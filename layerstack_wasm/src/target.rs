// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edit targets as the wrapper holds them: frozen values the stage mints,
//! resolved against the stage as it is composed whenever they are used.
//!
//! A target is identified by its mapping, as OpenUSD's `UsdEditTarget`
//! compares equal by its layer and `PcpMapFunction`: the layer written,
//! the stage prim whose namespace is mapped and the spec path it maps to
//! (its site), the arc it goes through, whether paths outside the prim map
//! to themselves, and the layer offset. Its record is
//! `[kind, layer, prim, site, arc, mapsOutside, offset, scale]`, `kind`
//! one of
//!
//! - `"layer"`: a layer of the stage's layer stack, every path mapped to
//!   itself, at the offset the stack reads it at
//!   ([`EditTarget::for_local_layer`]); `prim`, `site` and `arc` are
//!   `null`.
//! - `"variant"`: a variant branch of a layer of the stage's layer stack
//!   (`/World/Robot{finish=painted}`), on the prim that hosts it
//!   ([`EditTarget::for_local_variant`], OpenUSD's
//!   `ForLocalDirectVariant`), with no layer offset: times are written as
//!   given, even in a sublayer authored with an offset.
//! - `"arc"`: a layer reached through an arc of a composed prim, a node of
//!   its composition graph ([`EditTarget::for_node_layer`]).
//!
//! The wrapper sends a target back as the record's values. A `"layer"` or
//! `"variant"` target is rebuilt from its layer, which must still be in the
//! stage's layer stack. An `"arc"` target is found again among the nodes of
//! its prim's current graph: a node of the same arc kind and site whose
//! layer stack holds the layer at the same offset, with the same
//! root-identity. Targets equal by these values write identically, so it
//! does not matter which of several such nodes is found. A target no node
//! maps any more (the arc was removed, the prim renamed) is stale.
//!
//! Spec: AOUSD Core §10 (composition arcs map namespace), §10.3.1.1 and
//! §12.3.2.1 (layer offsets). OpenUSD: `UsdEditTarget`
//! (`pxr/usd/usd/editTarget.h`), `UsdStage::GetEditTargetForLocalLayer`,
//! `UsdEditTarget::ForLocalDirectVariant`.

use alloc::{format, string::String, vec, vec::Vec};

use layerstack::{ArcKind, EditTarget, LayerId, LayerOffset, NodeId, PathId, SpecPath};

use crate::{
    error::Error,
    stage::{Resolved, Stage, arc_name},
    wire::Wire,
};

/// A target as the wrapper sends it back: the values of its record.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TargetInput {
    pub(crate) kind: String,
    pub(crate) layer: u64,
    pub(crate) prim: Option<String>,
    pub(crate) site: Option<String>,
    pub(crate) arc: Option<String>,
    pub(crate) maps_outside: bool,
    pub(crate) offset: f64,
    pub(crate) scale: f64,
}

impl TargetInput {
    /// The local target of `layer`: what a bare `Layer` names.
    pub(crate) fn layer(layer: u64) -> Self {
        Self {
            kind: String::from("layer"),
            layer,
            prim: None,
            site: None,
            arc: None,
            maps_outside: true,
            offset: 0.0,
            scale: 1.0,
        }
    }
}

/// The record of a target: `[kind, layer, prim, site, arc, mapsOutside,
/// offset, scale]`.
fn record(
    kind: &str,
    layer: LayerId,
    prim: Option<String>,
    site: Option<String>,
    arc: Option<&str>,
    maps_outside: bool,
    offset: LayerOffset,
) -> Wire {
    Wire::List(vec![
        Wire::text(kind),
        Wire::UInt64(layer.0),
        Wire::text_or_null(prim),
        Wire::text_or_null(site),
        Wire::text_or_null(arc),
        Wire::Bool(maps_outside),
        Wire::Number(offset.offset),
        Wire::Number(offset.scale),
    ])
}

/// The stage path `path` names a target is found again by.
fn stale(prim: &str) -> Error {
    Error::new(
        "target.stale",
        "no arc of the prim maps the edit target any more (the arc was removed, or the prim \
         renamed or removed): take a new one from an opinion's editTarget or \
         prim.compositionArcs()",
    )
    .at_path(prim)
}

impl Stage {
    /// The record of `input` as `resolved` maps it now: its own values,
    /// with the offset and root-identity of the resolved target, so a bare
    /// layer's record carries the offset the stack reads it at.
    pub(crate) fn target_record(input: &TargetInput, resolved: &Resolved) -> Wire {
        record(
            &input.kind,
            resolved.target.layer(),
            input.prim.clone(),
            input.site.clone(),
            input.arc.as_deref(),
            resolved.target.has_root_identity(),
            resolved.target.layer_offset(),
        )
    }

    /// The record of the local target of `layer`
    /// ([`EditTarget::for_local_layer`], OpenUSD's
    /// `UsdStage::GetEditTargetForLocalLayer`): every path maps to itself,
    /// and times through the offset the stage's layer stack reads it at.
    ///
    /// # Errors
    ///
    /// `layer.unknown`, `layer.muted`, `layer.outside_stack`.
    pub(crate) fn edit_target_for_layer(&self, layer: u64) -> Result<Wire, Error> {
        let resolved = self.resolve_target(&TargetInput::layer(layer))?;
        Ok(record(
            "layer",
            LayerId(layer),
            None,
            None,
            None,
            true,
            resolved.target.layer_offset(),
        ))
    }

    /// Whether `layer` is in the stage's layer stack, unmuted: whether a
    /// bare layer can be edited (OpenUSD's `UsdStage::HasLocalLayer`).
    pub(crate) fn has_local_layer(&self, layer: u64) -> bool {
        self.document()
            .stage()
            .stage()
            .has_local_layer(LayerId(layer))
    }

    /// The record of the target inside the variant `variant` of the
    /// variant set `set` on the prim at `path`, in `layer`, a layer of the
    /// stage's layer stack: the prim and its namespace descendants map
    /// inside the branch (`/World/Robot{finish=painted}`), every other path
    /// to itself, and times are not mapped: the target has no layer offset,
    /// so in a sublayer authored with `(offset = 10)` a sample set at stage
    /// time 15 is written at the sublayer's time 15, where the stage reads
    /// it at 25. The variant does not have to be selected, nor to exist:
    /// edits through the target create it. A selected variant's own node
    /// ([`Self::composition_arcs`], an opinion's target) maps times
    /// through the sublayer's offset instead.
    ///
    /// OpenUSD: `UsdEditTarget::ForLocalDirectVariant`, whose map function
    /// has no time offset (`pxr/usd/usd/editTarget.cpp`).
    ///
    /// # Errors
    ///
    /// `path.invalid` when `path`, `set` or `variant` do not make a
    /// variant path; the layer errors of [`Self::edit_target_for_layer`].
    pub(crate) fn variant_edit_target(
        &self,
        path: &str,
        set: &str,
        variant: &str,
        layer: u64,
    ) -> Result<Wire, Error> {
        // Spec: AOUSD Core §8 (a variant set name is an identifier; a
        // variant name may also hold `|` and `-`, after an optional `.`).
        // OpenUSD: `SdfSchema::IsValidVariantIdentifier`.
        let identifier = !set.is_empty()
            && !set.starts_with(|c: char| c.is_ascii_digit())
            && set.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let name = variant.strip_prefix('.').unwrap_or(variant);
        let variant_name = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '|' | '-'));
        if !identifier || !variant_name {
            return Err(Error::new(
                "path.invalid",
                format!("`{set}` and `{variant}` do not name a variant set and a variant"),
            )
            .at_path(path));
        }
        let site = format!("{path}{{{set}={variant}}}");
        let input = TargetInput {
            kind: String::from("variant"),
            prim: Some(String::from(path)),
            site: Some(site.clone()),
            ..TargetInput::layer(layer)
        };
        let resolved = self.resolve_target(&input)?;
        Ok(record(
            "variant",
            LayerId(layer),
            Some(String::from(path)),
            Some(site),
            Some(arc_name(ArcKind::Variants)),
            true,
            resolved.target.layer_offset(),
        ))
    }

    /// The record of the target through the node `node` of the composed
    /// prim `prim`'s graph, in `layer`, one of the node's layers; `None`
    /// for a relocate node, or a layer outside its stack.
    pub(crate) fn node_target_record(
        &self,
        prim: PathId,
        node: NodeId,
        layer: LayerId,
    ) -> Option<Wire> {
        let document = self.document();
        let store = document.store();
        let stage = document.stage().stage();
        let target = EditTarget::for_node_layer(stage, store, prim, node, layer)?;
        let composed = document.view().prim(prim)?;
        let graph = composed.composition_graph();
        let found = graph.node(node)?;
        let root = graph.node(NodeId::ROOT)?;
        let offset = target.layer_offset();
        if found.parent().is_none() {
            return Some(record("layer", layer, None, None, None, true, offset));
        }
        let text = store.paths().display(prim, store.tokens());
        let site = found.site().display(store.paths(), store.tokens());
        // A variant node in the stage's own layer stack, on the prim itself,
        // in a layer read with no offset, maps as a local variant target of
        // its site does (`ForLocalDirectVariant`, which has no offset). In
        // a sublayer with an offset it maps times as well: an arc target.
        let local_variant = found.arc_kind() == ArcKind::Variants
            && found.layer_stack_identifier() == root.layer_stack_identifier()
            && found.site().prim_path() == prim
            && target.has_root_identity()
            && offset.is_identity();
        let kind = if local_variant { "variant" } else { "arc" };
        Some(record(
            kind,
            layer,
            Some(text),
            Some(site),
            Some(arc_name(found.arc_kind())),
            target.has_root_identity(),
            offset,
        ))
    }

    /// Resolves `input` against the stage as it is composed now.
    ///
    /// # Errors
    ///
    /// - `layer.unknown` for a layer the stage does not have, `layer.muted`
    ///   for a muted one (checked before anything else of the target).
    /// - `layer.outside_stack` for a `"layer"` or `"variant"` target whose
    ///   layer is not in the stage's layer stack.
    /// - `path.invalid` for a malformed prim path or site;
    ///   `argument.invalid` for an unknown kind.
    /// - `target.stale` (with the prim's path) for an `"arc"` target no
    ///   node of its prim's graph maps any more.
    pub(crate) fn resolve_target(&self, input: &TargetInput) -> Result<Resolved, Error> {
        let layer = LayerId(input.layer);
        let document = self.document();
        let store = document.store();
        let stage = document.stage().stage();
        if store.layer(layer).is_none() {
            return Err(Error::new(
                "layer.unknown",
                "the stage has no layer with this id",
            ));
        }
        let identifier = document.identifier(layer).map(String::from);
        if stage.is_layer_muted(layer) {
            return Err(Error {
                identifier,
                ..Error::new(
                    "layer.muted",
                    "the layer is muted, so the stage does not compose it: unmute it to edit it",
                )
            });
        }
        let local = || -> Result<EditTarget, Error> {
            EditTarget::for_local_layer(stage, store, layer).ok_or_else(|| Error {
                identifier: identifier.clone(),
                ..Error::new(
                    "layer.outside_stack",
                    "the layer is not in the stage's layer stack: the stage reaches it only \
                     through an arc; edit it through one, with an opinion's editTarget or \
                     prim.compositionArcs()",
                )
            })
        };
        let prim_text = input.prim.as_deref().unwrap_or("");
        match input.kind.as_str() {
            "layer" => Ok(Resolved {
                target: local()?,
                stack: stage.layer_stack().to_vec(),
            }),
            "variant" => {
                local()?;
                let site = input.site.as_deref().unwrap_or("");
                let path = SpecPath::parse(site, store.tokens(), store.paths())
                    .ok()
                    .filter(|path| {
                        path.has_variant_selections()
                            && path.property().is_none()
                            && store.paths().display(path.prim_path(), store.tokens()) == prim_text
                    })
                    .ok_or_else(|| {
                        Error::new(
                            "path.invalid",
                            "not a variant of the prim, such as /World/Robot{finish=painted}",
                        )
                        .at_path(site)
                    })?;
                Ok(Resolved {
                    target: EditTarget::for_local_variant(layer, &path),
                    stack: stage.layer_stack().to_vec(),
                })
            }
            "arc" => {
                let prim = store.try_path(prim_text).map_err(|_| {
                    Error::new("path.invalid", "not a prim path").at_path(prim_text)
                })?;
                let composed = document.view().prim(prim).ok_or_else(|| stale(prim_text))?;
                let graph = composed.composition_graph();
                let wanted = LayerOffset {
                    offset: input.offset,
                    scale: input.scale,
                };
                let arc = input.arc.as_deref().unwrap_or("");
                let site = input.site.as_deref().unwrap_or("");
                for (id, node) in graph.nodes() {
                    if arc_name(node.arc_kind()) != arc
                        || node.layer_offset_for_layer(layer) != Some(wanted)
                        || node.site().display(store.paths(), store.tokens()) != site
                    {
                        continue;
                    }
                    let Some(target) = EditTarget::for_node_layer(stage, store, prim, id, layer)
                    else {
                        continue;
                    };
                    if target.has_root_identity() == input.maps_outside {
                        return Ok(Resolved {
                            target,
                            stack: node.layers().collect(),
                        });
                    }
                }
                Err(stale(prim_text))
            }
            other => Err(Error::new(
                "argument.invalid",
                format!("`{other}` is not an edit target kind: layer, variant or arc"),
            )),
        }
    }

    /// The spec path `input` maps the stage prim or property path `path`
    /// to in its layer (`/Robot.height`), or `None` when it does not map it.
    ///
    /// OpenUSD: `UsdEditTarget::MapToSpecPath`.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::resolve_target`]; `path.invalid`.
    pub(crate) fn map_edit_target(
        &self,
        input: &TargetInput,
        path: &str,
    ) -> Result<Option<String>, Error> {
        let resolved = self.resolve_target(input)?;
        let store = self.document().store();
        let mapped = if path.contains('.') {
            let property = store.try_property_path(path).map_err(|_| {
                Error::new(
                    "path.invalid",
                    "not a property path, such as /World/Rock.size",
                )
                .at_path(path)
            })?;
            resolved
                .target
                .map_property_to_spec_path(property, store.paths())
        } else {
            let prim = store.try_path(path).map_err(|_| {
                Error::new("path.invalid", "not a prim path, such as /World/Rock").at_path(path)
            })?;
            resolved.target.map_to_spec_path(prim, store.paths())
        };
        Ok(mapped.map(|spec| spec.display(store.paths(), store.tokens())))
    }

    /// Whether the stage composes what `input` writes now: a `"layer"`
    /// target's layer is in the stage's layer stack, a `"variant"` target's
    /// variant is selected, and an `"arc"` target's arc still reaches its
    /// prim. An edit through a target that is not composed changes no
    /// composed value until it is.
    pub(crate) fn edit_target_composed(&self, input: &TargetInput) -> bool {
        let Ok(resolved) = self.resolve_target(input) else {
            return false;
        };
        if input.kind != "variant" {
            return true;
        }
        let document = self.document();
        let store = document.store();
        let Some(prim) = input
            .prim
            .as_deref()
            .and_then(|path| document.view().prim(path))
        else {
            return false;
        };
        let site = input.site.as_deref().unwrap_or("");
        let layer = resolved.target.layer();
        prim.composition_graph().nodes().any(|(_, node)| {
            node.arc_kind() == ArcKind::Variants
                && node.layers().any(|id| id == layer)
                && node.site().display(store.paths(), store.tokens()) == site
        })
    }

    /// The composition arcs of the prim at `path`, strongest first, each
    /// `[arc, site, layerStack, layers, implied, parent, kind, mapsOutside]`:
    /// the arc kind reaching the node, the spec path it reads, the root
    /// layer of its layer stack, each layer of that stack as
    /// `[layer, offset, scale, kind]` (the offset from that layer's times
    /// to stage times, and the kind of a target through the arc in that
    /// layer: a local variant's node is a `"variant"` target in a layer
    /// with no offset and an `"arc"` one in a sublayer with one), whether
    /// the arc is implied, the index of the arc it is authored beneath
    /// (`null` for the prim's own, `local`, node), and the kind and
    /// root-identity of a target through it in the stack's first layer
    /// (`null` and `false` for a relocate node, whose specs never
    /// contribute).
    ///
    /// Spec: AOUSD Core §10.4 (the prim index and strength ordering).
    /// OpenUSD: `UsdPrimCompositionQuery::GetCompositionArcs`.
    ///
    /// # Errors
    ///
    /// `path.invalid`; `prim.not_found` when no composed prim is there.
    pub(crate) fn composition_arcs(&self, path: &str) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?;
        let document = self.document();
        let store = document.store();
        let graph = prim.composition_graph();
        let ids: Vec<NodeId> = graph.nodes().map(|(id, _)| id).collect();
        let rows = graph
            .nodes()
            .map(|(id, node)| {
                let layers: Vec<Wire> = node
                    .layers()
                    .filter_map(|layer| {
                        let offset = node.layer_offset_for_layer(layer)?;
                        let kind = match self.node_target_record(prim.path(), id, layer) {
                            Some(Wire::List(fields)) => fields[0].clone(),
                            _ => Wire::Null,
                        };
                        Some(Wire::List(vec![
                            Wire::UInt64(layer.0),
                            Wire::Number(offset.offset),
                            Wire::Number(offset.scale),
                            kind,
                        ]))
                    })
                    .collect();
                let target = node
                    .layers()
                    .next()
                    .and_then(|layer| self.node_target_record(prim.path(), id, layer));
                let (kind, maps_outside) = match &target {
                    Some(Wire::List(fields)) => (fields[0].clone(), fields[5].clone()),
                    _ => (Wire::Null, Wire::Bool(false)),
                };
                Wire::List(vec![
                    Wire::text(arc_name(node.arc_kind())),
                    Wire::Text(node.site().display(store.paths(), store.tokens())),
                    Wire::UInt64(node.layer_stack().0),
                    Wire::List(layers),
                    Wire::Bool(node.is_implied()),
                    node.parent()
                        .and_then(|parent| ids.iter().position(|id| *id == parent))
                        .map_or(Wire::Null, |index| Wire::Number(index as f64)),
                    kind,
                    maps_outside,
                ])
            })
            .collect();
        Ok(Wire::List(rows))
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use super::TargetInput;
    use crate::{
        edit::EditBatch,
        stage::{BROWSER_LIMITS, Stage},
        wire::Wire,
    };

    const SCENE: &str = r#"#usda 1.0
(
    subLayers = [
        @./shot.usda@ (offset = 10)
    ]
)
def Xform "World" {
    def Xform "Robot" (
        prepend references = @./robot.usda@ (offset = 10; scale = 2)
        variants = {
            string finish = "painted"
        }
        prepend variantSets = "finish"
    ) {
        double height = 2
        variantSet "finish" = {
            "chrome" {
            }
            "painted" {
                color3f color = (0.8, 0.2, 0.1)
            }
        }
    }
    def Xform "Spare" (
        prepend references = @./robot.usda@
    ) {
    }
    def Xform "Twin" (
        prepend references = [@./robot.usda@, @./robot.usda@</Robot>]
    ) {
    }
}
"#;
    const ROBOT: &str = "#usda 1.0\n(\n    defaultPrim = \"Robot\"\n)\ndef Xform \"Robot\" {\n    double height = 1.2\n    color3f color = (0.5, 0.5, 0.5)\n}\n";
    const SHOT: &str = "#usda 1.0\nover \"World\" {\n    over \"Robot\" {\n        variantSet \"finish\" = {\n            \"painted\" {\n                double glow.timeSamples = {\n                    0: 1,\n                }\n            }\n        }\n    }\n}\n";

    fn open() -> Stage {
        let mut stage = Stage::open(
            "scene.usda",
            SCENE.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let storage = stage.document_mut().storage_mut();
        storage.supply("robot.usda", ROBOT.as_bytes().to_vec());
        storage.supply("shot.usda", SHOT.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        stage
    }

    fn layer(stage: &Stage, identifier: &str) -> u64 {
        let document = stage.document();
        document
            .store()
            .layer_ids()
            .into_iter()
            .find(|id| document.identifier(*id) == Some(identifier))
            .unwrap_or_else(|| panic!("no layer {identifier}"))
            .0
    }

    /// The target record of each opinion of the attribute at `path`.
    fn targets(stage: &Stage, path: &str) -> Vec<Wire> {
        let why = stage.explain(path, None).unwrap();
        why.as_list().unwrap()[4]
            .as_list()
            .unwrap()
            .iter()
            .map(|row| row.as_list().unwrap()[7].clone())
            .collect()
    }

    /// A target as the wrapper sends a record back.
    fn input(record: &Wire) -> TargetInput {
        let fields = record.as_list().unwrap();
        let text = |wire: &Wire| wire.as_text().map(String::from);
        TargetInput {
            kind: text(&fields[0]).unwrap(),
            layer: match fields[1] {
                Wire::UInt64(id) => id,
                ref other => panic!("{other:?}"),
            },
            prim: text(&fields[2]),
            site: text(&fields[3]),
            arc: text(&fields[4]),
            maps_outside: fields[5] == Wire::Bool(true),
            offset: number(&fields[6]),
            scale: number(&fields[7]),
        }
    }

    fn number(wire: &Wire) -> f64 {
        match wire {
            Wire::Number(value) => *value,
            other => panic!("{other:?}"),
        }
    }

    fn target(
        kind: &str,
        layer: u64,
        prim: Option<&str>,
        site: Option<&str>,
        arc: Option<&str>,
        maps_outside: bool,
        offset: f64,
        scale: f64,
    ) -> Wire {
        Wire::List(vec![
            Wire::text(kind),
            Wire::UInt64(layer),
            Wire::text_or_null(prim),
            Wire::text_or_null(site),
            Wire::text_or_null(arc),
            Wire::Bool(maps_outside),
            Wire::Number(offset),
            Wire::Number(scale),
        ])
    }

    fn set_height(stage: &mut Stage, record: &Wire, path: &str, value: f64) -> crate::EditReceipt {
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![value]);
        batch.set_value(path.into(), None, slot);
        stage.apply_target(&input(record), &mut batch).unwrap()
    }

    fn height(stage: &Stage, path: &str) -> Wire {
        stage.get(path, None).unwrap().as_list().unwrap()[2].clone()
    }

    #[test]
    fn explain_rows_name_the_target_that_writes_each_opinion() {
        let stage = open();
        let (scene, robot) = (layer(&stage, "scene.usda"), layer(&stage, "robot.usda"));
        assert_eq!(
            targets(&stage, "/World/Robot.height"),
            [
                target("layer", scene, None, None, None, true, 0.0, 1.0),
                target(
                    "arc",
                    robot,
                    Some("/World/Robot"),
                    Some("/Robot"),
                    Some("references"),
                    false,
                    10.0,
                    2.0
                ),
            ]
        );
        assert_eq!(
            targets(&stage, "/World/Robot.color")[0],
            target(
                "variant",
                scene,
                Some("/World/Robot"),
                Some("/World/Robot{finish=painted}"),
                Some("variants"),
                true,
                0.0,
                1.0
            ),
            "a local variant on the prim itself"
        );
        assert_eq!(
            stage
                .variant_edit_target("/World/Robot", "finish", "painted", scene)
                .unwrap(),
            targets(&stage, "/World/Robot.color")[0],
            "the same target as the selected variant's opinion"
        );
        let shot = layer(&stage, "shot.usda");
        assert_eq!(
            stage.edit_target_for_layer(shot).unwrap(),
            target("layer", shot, None, None, None, true, 10.0, 1.0),
            "a sublayer's target carries its offset"
        );
        assert!(stage.has_local_layer(shot) && !stage.has_local_layer(robot));
        assert_eq!(
            stage.edit_target_for_layer(robot).unwrap_err().code,
            "layer.outside_stack"
        );
    }

    /// A variant target has no layer offset, as OpenUSD's
    /// `ForLocalDirectVariant`: in `shot.usda`, a sublayer authored with
    /// `(offset = 10)`, a sample set at stage time 15 lands at its time 15.
    /// The selected variant's node in that sublayer, an opinion's target,
    /// is an arc target that maps times through the offset: 15 lands at 5.
    /// (`variant_target_offsets_oracle.py` records both against OpenUSD.)
    #[test]
    fn variant_targets_have_no_layer_offset_and_variant_nodes_do() {
        let mut stage = open();
        let shot = layer(&stage, "shot.usda");
        let direct = stage
            .variant_edit_target("/World/Robot", "finish", "painted", shot)
            .unwrap();
        assert_eq!(
            direct,
            target(
                "variant",
                shot,
                Some("/World/Robot"),
                Some("/World/Robot{finish=painted}"),
                Some("variants"),
                true,
                0.0,
                1.0
            )
        );
        let why = stage.explain("/World/Robot.glow", Some(0.0)).unwrap();
        let node = why.as_list().unwrap()[4].as_list().unwrap()[0]
            .as_list()
            .unwrap()[7]
            .clone();
        assert_eq!(
            node,
            target(
                "arc",
                shot,
                Some("/World/Robot"),
                Some("/World/Robot{finish=painted}"),
                Some("variants"),
                true,
                10.0,
                1.0
            ),
            "the variant node reads the sublayer at its offset"
        );
        let arcs = stage.composition_arcs("/World/Robot").unwrap();
        let variant = arcs.as_list().unwrap()[1].as_list().unwrap()[3].clone();
        let kinds: Vec<Wire> = variant
            .as_list()
            .unwrap()
            .iter()
            .map(|entry| entry.as_list().unwrap()[3].clone())
            .collect();
        assert_eq!(
            kinds,
            [Wire::text("variant"), Wire::text("arc")],
            "per layer"
        );
        let set = |stage: &mut Stage, record: &Wire, value: f64| {
            let mut batch = EditBatch::new();
            let slot = batch.value_numbers("double".into(), vec![value]);
            batch.set_value("/World/Robot.glow".into(), Some(15.0), slot);
            stage.apply_target(&input(record), &mut batch).unwrap()
        };
        let mut receipt = set(&mut stage, &direct, 2.0);
        assert_eq!(
            stage.time_samples("/World/Robot.glow").unwrap(),
            [10.0, 25.0],
            "written at the sublayer's time 15, read at 25"
        );
        stage.undo(&mut receipt).unwrap();
        set(&mut stage, &node, 2.0);
        assert_eq!(
            stage.time_samples("/World/Robot.glow").unwrap(),
            [10.0, 15.0],
            "written at the sublayer's time 5, read at 15"
        );
    }

    #[test]
    fn composition_arcs_list_the_graph_and_their_targets() {
        let stage = open();
        let robot = layer(&stage, "robot.usda");
        let arcs = stage.composition_arcs("/World/Robot").unwrap();
        let rows = arcs.as_list().unwrap();
        let summary: Vec<(Wire, Wire, Wire, Wire)> = rows
            .iter()
            .map(|row| {
                let row = row.as_list().unwrap();
                (
                    row[0].clone(),
                    row[1].clone(),
                    row[5].clone(),
                    row[6].clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    Wire::text("local"),
                    Wire::text("/World/Robot"),
                    Wire::Null,
                    Wire::text("layer")
                ),
                (
                    Wire::text("variants"),
                    Wire::text("/World/Robot{finish=painted}"),
                    Wire::Number(0.0),
                    Wire::text("variant")
                ),
                (
                    Wire::text("references"),
                    Wire::text("/Robot"),
                    Wire::Number(0.0),
                    Wire::text("arc")
                ),
            ]
        );
        let reference = rows[2].as_list().unwrap();
        assert_eq!(reference[2], Wire::UInt64(robot));
        assert_eq!(
            reference[3],
            Wire::List(vec![Wire::List(vec![
                Wire::UInt64(robot),
                Wire::Number(10.0),
                Wire::Number(2.0),
                Wire::text("arc"),
            ])])
        );
        assert_eq!(
            reference[7],
            Wire::Bool(false),
            "a reference maps only its prim"
        );
        assert_eq!(
            stage.composition_arcs("/Nowhere").unwrap_err().code,
            "prim.not_found"
        );
    }

    #[test]
    fn edits_through_an_opinion_target_write_the_asset_and_map_paths() {
        let mut stage = open();
        let robot = layer(&stage, "robot.usda");
        let through = targets(&stage, "/World/Robot.height")[1].clone();
        assert_eq!(
            stage
                .map_edit_target(&input(&through), "/World/Robot.height")
                .unwrap()
                .as_deref(),
            Some("/Robot.height")
        );
        assert_eq!(
            stage
                .map_edit_target(&input(&through), "/World/Spare")
                .unwrap(),
            None
        );
        let receipt = set_height(&mut stage, &through, "/World/Robot.height", 1.5);
        let record = receipt.record.as_list().unwrap();
        assert_eq!(record[0], through, "the receipt names the target");
        assert_eq!(
            record[5],
            Wire::List(vec![Wire::UInt64(robot)]),
            "the layers written"
        );
        assert!(stage.layer_dirty(robot).unwrap());
        assert_eq!(
            height(&stage, "/World/Robot.height"),
            Wire::Number(2.0),
            "masked by the local opinion"
        );
        assert_eq!(
            height(&stage, "/World/Spare.height"),
            Wire::Number(1.5),
            "every reference changes"
        );
        assert!(stage.edit_target_composed(&input(&through)));
        // The target survives its own edit and unrelated recomposition.
        set_height(&mut stage, &through, "/World/Robot.height", 1.6);
        let mut batch = EditBatch::new();
        let one = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/World/Lamp.glow".into(), None, one);
        let error = stage
            .apply_target(&input(&through), &mut batch)
            .unwrap_err();
        assert_eq!(
            (error.code, error.edit, error.path.as_deref()),
            (
                "edit.rejected.unmappable",
                Some(0),
                Some("/World/Lamp.glow")
            )
        );
    }

    #[test]
    fn equal_targets_through_shared_and_duplicate_arcs_write_alike() {
        let mut stage = open();
        let twin = targets(&stage, "/World/Twin.height");
        assert_eq!(
            twin[0], twin[1],
            "two references to one site map alike: one target"
        );
        set_height(&mut stage, &twin[0], "/World/Twin.height", 1.7);
        assert_eq!(height(&stage, "/World/Spare.height"), Wire::Number(1.7));
        let spare = targets(&stage, "/World/Spare.height")[0].clone();
        assert_ne!(spare, twin[0], "a target is about its own prim");
        set_height(&mut stage, &spare, "/World/Spare.height", 1.8);
        assert_eq!(height(&stage, "/World/Twin.height"), Wire::Number(1.8));
    }

    #[test]
    fn a_target_goes_stale_when_its_arc_or_prim_goes() {
        let mut stage = open();
        let through = targets(&stage, "/World/Spare.height")[0].clone();
        let mut receipt = set_height(&mut stage, &through, "/World/Spare.height", 1.5);
        let mut batch = EditBatch::new();
        batch.remove_arc(
            false,
            "/World/Spare".into(),
            Some("./robot.usda".into()),
            None,
        );
        let mut removal = stage
            .apply(layer(&stage, "scene.usda"), &mut batch)
            .unwrap();
        let error = stage
            .map_edit_target(&input(&through), "/World/Spare.height")
            .unwrap_err();
        assert_eq!(
            (error.code, error.path.as_deref()),
            ("target.stale", Some("/World/Spare"))
        );
        assert!(!stage.edit_target_composed(&input(&through)));
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![2.0]);
        batch.set_value("/World/Spare.height".into(), None, slot);
        assert_eq!(
            stage
                .apply_target(&input(&through), &mut batch)
                .unwrap_err()
                .code,
            "target.stale"
        );
        stage
            .undo(&mut receipt)
            .expect("undo is spec-addressed: it works on a stale target");
        stage.undo(&mut removal).unwrap();
        assert!(
            stage.edit_target_composed(&input(&through)),
            "the arc is back"
        );
        let robot = targets(&stage, "/World/Robot.height")[1].clone();
        let scene = layer(&stage, "scene.usda");
        stage
            .move_object(scene, "/World/Robot", "/World/Rover")
            .unwrap();
        assert_eq!(
            stage
                .map_edit_target(&input(&robot), "/World/Robot")
                .unwrap_err()
                .code,
            "target.stale",
            "renamed"
        );
    }

    #[test]
    fn namespace_edits_refuse_targets_through_arcs_and_variants() {
        let mut stage = open();
        let through = input(&targets(&stage, "/World/Robot.height")[1]);
        let error = stage
            .move_through(&through, "/World/Robot", "/World/Rover")
            .unwrap_err();
        assert_eq!(
            (error.code, error.path.as_deref()),
            ("namespace.unmappable", Some("/World/Robot"))
        );
        let variant = input(&targets(&stage, "/World/Robot.color")[0]);
        assert_eq!(
            stage
                .delete_through(&variant, "/World/Robot")
                .unwrap_err()
                .code,
            "namespace.unmappable"
        );
    }

    #[test]
    fn a_variant_target_writes_its_branch_selected_or_not() {
        let mut stage = open();
        let scene = layer(&stage, "scene.usda");
        let chrome = stage
            .variant_edit_target("/World/Robot", "finish", "chrome", scene)
            .unwrap();
        assert!(!stage.edit_target_composed(&input(&chrome)), "not selected");
        let mut batch = EditBatch::new();
        let slot = batch.value_floats("color3f".into(), vec![0.9, 0.9, 0.95]);
        batch.create_attribute(
            "/World/Robot.color".into(),
            "color3f".into(),
            false,
            false,
            Some(slot),
        );
        stage.apply_target(&input(&chrome), &mut batch).unwrap();
        let store = stage.document().store();
        let text = layerstack_usda::save_usda(
            store.layer(layerstack::LayerId(scene)).unwrap(),
            store.tokens(),
            store.paths(),
        )
        .unwrap();
        assert!(
            text.contains("\"chrome\" {\n                color3f color = (0.9, 0.9, 0.95)"),
            "{text}"
        );
        assert_eq!(
            stage
                .variant_edit_target(
                    "/World/Robot",
                    "finish",
                    "chrome",
                    layer(&stage, "robot.usda")
                )
                .unwrap_err()
                .code,
            "layer.outside_stack"
        );
        assert_eq!(
            stage
                .variant_edit_target("/World/Robot", "finish", "{bad", scene)
                .unwrap_err()
                .code,
            "path.invalid"
        );
    }

    #[test]
    fn muted_and_read_only_layers_are_refused_with_a_way_on() {
        let mut stage = open();
        let robot = layer(&stage, "robot.usda");
        let through = targets(&stage, "/World/Robot.height")[1].clone();
        stage.set_layer_muted(robot, true).unwrap();
        assert_eq!(
            stage
                .map_edit_target(&input(&through), "/World/Robot")
                .unwrap_err()
                .code,
            "layer.muted",
            "muting is named before staleness"
        );
        stage.set_layer_muted(robot, false).unwrap();

        // A package member: the robot packaged, then referenced.
        let mut asset = Stage::open(
            "robot.usda",
            ROBOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let record = asset.export_package().unwrap();
        let Wire::Bytes(package) = record.as_list().unwrap()[0].clone() else {
            panic!("a package");
        };
        let scene =
            "#usda 1.0\ndef \"Packed\" (\n    prepend references = @./robot.usdz@\n) {\n}\n";
        let mut packed = Stage::open(
            "scene.usda",
            scene.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        packed
            .document_mut()
            .storage_mut()
            .supply("robot.usdz", package);
        packed.load_supplied().unwrap();
        let member = targets(&packed, "/Packed.height")[0].clone();
        assert_eq!(input(&member).kind, "arc");
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![2.0]);
        batch.set_value("/Packed.height".into(), None, slot);
        let error = packed
            .apply_target(&input(&member), &mut batch)
            .unwrap_err();
        assert_eq!(error.code, "layer.read_only");
        assert!(
            error.message.contains("editTargetForLayer"),
            "{}",
            error.message
        );
    }
}
