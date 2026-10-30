// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edit targets: where stage-level edits are written.

use alloc::{string::String, sync::Arc, vec::Vec};

use crate::{
    doc::{FieldValue, LayerId, LayerOffset, LayerStore, Value},
    interner::{TokenId, TokenInterner},
    path::{PathId, PathInterner, PropertyPath, TargetPath},
    path_expression::{PathExpression, value::anchor_and_map_with},
    prim_index::ArcKind,
    prim_index_graph::{NodeId, PrimIndexGraph, PrimNode},
    spec_path::SpecPath,
    stage::Stage,
    target_translation::{PrimMap, is_relational, map_relational},
};

/// How an [`EditTarget`] maps stage namespace to spec paths in its layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NamespaceMap {
    /// Every stage path is the spec path of the same name.
    Identity,
    /// `stage_root` and its namespace descendants map to `spec_root` and
    /// the same descendants beneath it. With `root_identity`, every other
    /// path maps to itself, unless that lands beneath a plain `spec_root`
    /// (which `stage_root` already maps to); without it nothing else maps.
    ///
    /// OpenUSD: a `PcpMapFunction` with one path pair, and the root
    /// identity (`/` → `/`) when it has one: a local variant target's
    /// (`UsdEditTarget::ForLocalDirectVariant`), and a node's map to the
    /// root when every arc on the way keeps it (variants, inherits and
    /// specializes do; references and payloads do not). A path is mapped by
    /// its most specific pair, and one whose image falls in the range of a
    /// more specific pair does not map.
    Prefix {
        stage_root: PathId,
        spec_root: SpecPath,
        root_identity: bool,
    },
}

impl NamespaceMap {
    /// Whether every path the map's pair does not cover maps to itself
    /// (`PcpMapFunction::HasRootIdentity`).
    fn has_root_identity(&self) -> bool {
        match self {
            Self::Identity => true,
            Self::Prefix { root_identity, .. } => *root_identity,
        }
    }

    /// The spec path of the stage prim `path`, if the map covers it.
    pub(crate) fn map_prim(&self, path: PathId, paths: &PathInterner) -> Option<SpecPath> {
        match self {
            Self::Identity => Some(SpecPath::from_prim_path(path)),
            Self::Prefix {
                stage_root,
                spec_root,
                root_identity,
            } => {
                if let Some(names) = paths
                    .resolve(path)
                    .strip_prefix(paths.resolve(*stage_root))
                    .map(<[_]>::to_vec)
                {
                    return Some(spec_root.join_prims(&names, paths));
                }
                if !root_identity {
                    return None;
                }
                let plain_root = !spec_root.has_variant_selections();
                let shadowed = plain_root
                    && paths
                        .resolve(path)
                        .strip_prefix(paths.resolve(spec_root.prim_path()))
                        .is_some();
                (!shadowed).then(|| SpecPath::from_prim_path(path))
            }
        }
    }

    /// Whether the map takes the stage prim `path` to the spec path of
    /// the same name, as [`Self::map_prim`] would, without interning the
    /// image.
    fn maps_to_itself(&self, path: PathId, paths: &PathInterner) -> bool {
        match self {
            Self::Identity => true,
            Self::Prefix {
                stage_root,
                spec_root,
                root_identity,
            } => {
                let stage_path = paths.resolve(path);
                if stage_path
                    .strip_prefix(paths.resolve(*stage_root))
                    .is_some()
                {
                    // The names below `stage_root` are kept, so the image
                    // is `path` exactly when `spec_root` is `stage_root`.
                    return *spec_root == SpecPath::from_prim_path(*stage_root);
                }
                *root_identity
                    && (spec_root.has_variant_selections()
                        || stage_path
                            .strip_prefix(paths.resolve(spec_root.prim_path()))
                            .is_none())
            }
        }
    }
}

/// Where stage-level edits are written: a layer, a map from stage
/// namespace to spec paths in that layer, and the layer offset between the
/// layer's times and stage times.
///
/// An edit target is how a host authors *through* composition. Targeting
/// the node of a reference makes an edit of `/World/RockB.size` write
/// `/Rock.size` in the referenced layer; targeting a variant node writes
/// inside the variant (`/Rock{shape=jagged}.roughness`); and a time sample
/// set at stage time 14 through a reference with `(offset = 10; scale = 2)`
/// is written at layer time 2.
///
/// Edits name their target through an [`Address`] ([`EditTarget::prim`],
/// [`EditTarget::property`]), which is mapped when the edit is applied: a
/// path the target does not map is rejected then.
///
/// OpenUSD: `UsdEditTarget` (`pxr/usd/usd/editTarget.h`), constructed from
/// a layer, a layer and a `PcpNodeRef`, or `ForLocalDirectVariant`; its
/// `MapToSpecPath` and the time offset of its `PcpMapFunction`.
///
/// Spec: AOUSD Core §8 (spec paths, including variant selections),
/// §10.3.1.1 and §12.3.2.1 (layer offsets).
#[derive(Clone, Debug, PartialEq)]
#[doc(alias = "UsdEditTarget")]
pub struct EditTarget {
    layer: LayerId,
    map: NamespaceMap,
    offset: LayerOffset,
}

impl EditTarget {
    /// Targets `layer` directly: stage paths are spec paths and stage times
    /// are layer times.
    ///
    /// OpenUSD: `UsdEditTarget(layer)`.
    #[must_use]
    pub fn for_layer(layer: LayerId) -> Self {
        Self {
            layer,
            map: NamespaceMap::Identity,
            offset: LayerOffset::IDENTITY,
        }
    }

    /// Targets `layer`, a layer of the stage's own layer stack (its session
    /// layer, root layer and their sublayers), as [`Self::for_layer`] does,
    /// with the layer offset the stack reads it at: stage paths are spec
    /// paths, and a time sample set at stage time 15 in a sublayer authored
    /// with `(offset = 10)` is written at layer time 5. Time codes per
    /// second conversions between the layers of the stack are included.
    ///
    /// Returns `None` if `layer` is not in the stage's layer stack (a layer
    /// reached only through an arc, or a muted one): target it through a
    /// node of a prim that reads it ([`Self::for_node_layer`]).
    ///
    /// OpenUSD: `UsdStage::GetEditTargetForLocalLayer`, the layer with
    /// `PcpLayerStack::GetLayerOffsetForLayer`. The edit target a bare
    /// layer converts to (`UsdEditTarget(layer)`) has no offset instead.
    ///
    /// Spec: AOUSD Core §10.3.1.1 (sublayer offsets), §12.3.2.1 (layer
    /// offsets).
    #[must_use]
    pub fn for_local_layer(stage: &Stage, store: &dyn LayerStore, layer: LayerId) -> Option<Self> {
        if !stage.has_local_layer(layer) {
            return None;
        }
        // The pseudo-root's root node reads the stage's own layer stack,
        // with each layer's offset from the stack's composition.
        let root = store.paths().intern_segments(&[]);
        let offset = stage
            .explain_prim_graph(root)
            .and_then(|graph| graph.node(NodeId::ROOT))
            .and_then(|node| node.layer_offset_for_layer(layer))
            .unwrap_or(LayerOffset::IDENTITY);
        Some(Self {
            offset,
            ..Self::for_layer(layer)
        })
    }

    /// Targets the variant branch `variant` of `layer`, such as
    /// `/Rock{shape=jagged}`: the prim hosting the branch and its namespace
    /// descendants map inside the branch, and every other path maps to
    /// itself, outside the branch (`/Light` is `/Light`). Any property
    /// suffix of `variant` is ignored.
    ///
    /// The branch does not have to be selected, or to exist: edits through
    /// the target create it.
    ///
    /// OpenUSD: `UsdEditTarget::ForLocalDirectVariant`.
    #[must_use]
    pub fn for_local_variant(layer: LayerId, variant: &SpecPath) -> Self {
        Self {
            layer,
            map: NamespaceMap::Prefix {
                stage_root: variant.prim_path(),
                spec_root: variant.prim_spec(),
                root_identity: true,
            },
            offset: LayerOffset::IDENTITY,
        }
    }

    /// Targets the node `node` of the composed prim `prim`'s index graph
    /// ([`Prim::composition_graph`](crate::Prim::composition_graph)), in the root layer of the node's
    /// layer stack.
    ///
    /// `prim` and its namespace descendants map to the node's site and the
    /// same descendants beneath it, including the variant selections of a
    /// variant node. Other paths map to themselves when every arc from the
    /// root to the node is a variant, inherit or specialize arc, whose
    /// namespace maps keep the root identity; through a reference or
    /// payload they do not map. The prim's root node maps every path to
    /// itself. Times map through the offsets of every arc from the root to
    /// the node.
    ///
    /// Returns `None` if `prim` is not on the stage, `node` is not a node
    /// of its graph, or `node` is a relocate node: its site is the
    /// relocation source, whose own specs never contribute, so an edit
    /// there would change nothing (AOUSD Core §10.3.2.6). A relocated prim
    /// is edited through its other nodes, which map it to the specs it
    /// reads, or through its root node at the relocation target. Opinions
    /// authored at a relocation source through the root node are not
    /// rejected; composition ignores them.
    ///
    /// OpenUSD: `UsdEditTarget(layer, node)` with the node's layer stack's
    /// root layer.
    #[must_use]
    pub fn for_node(stage: &Stage, prim: PathId, node: NodeId) -> Option<Self> {
        let (graph, node_ref) = editable_node(stage, prim, node)?;
        Some(Self::from_node(
            prim,
            graph,
            node_ref,
            node_ref.layer_stack(),
            node_ref.root_layer_offset(),
        ))
    }

    /// Targets the node `node` of `prim`'s index graph, like
    /// [`EditTarget::for_node`], in `layer`, one of the layers of the node's
    /// layer stack: times also map through that layer's sublayer offset.
    ///
    /// Returns `None` if `prim` is not on the stage, `node` is not a node of
    /// its graph or is a relocate node, or `layer` is not in the node's
    /// layer stack.
    ///
    /// OpenUSD: `UsdEditTarget(layer, node)` with a sublayer of the node's
    /// layer stack.
    #[must_use]
    pub fn for_node_layer(
        stage: &Stage,
        store: &dyn LayerStore,
        prim: PathId,
        node: NodeId,
        layer: LayerId,
    ) -> Option<Self> {
        let (graph, node_ref) = editable_node(stage, prim, node)?;
        store.layer(layer)?;
        let offset = node_ref.layer_offset_for_layer(layer)?;
        Some(Self::from_node(prim, graph, node_ref, layer, offset))
    }

    fn from_node(
        prim: PathId,
        graph: &PrimIndexGraph,
        node: PrimNode<'_>,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Self {
        let map = if node.parent().is_none() && node.arc_kind() == ArcKind::Local {
            NamespaceMap::Identity
        } else {
            // OpenUSD: `PcpMapExpression::AddRootIdentity` for class arcs;
            // a variant arc's map is the identity; references and payloads
            // map only their target's namespace (`primIndex.cpp`).
            let mut root_identity = true;
            let mut at = Some(node);
            while let Some(current) = at {
                root_identity &= matches!(
                    current.arc_kind(),
                    ArcKind::Local | ArcKind::Variants | ArcKind::Inherits | ArcKind::Specializes
                );
                at = current.parent().and_then(|parent| graph.node(parent));
            }
            NamespaceMap::Prefix {
                stage_root: prim,
                spec_root: node.site().clone(),
                root_identity,
            }
        };
        Self { layer, map, offset }
    }

    /// The layer edits are written to.
    #[must_use]
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The offset from the target layer's times to stage times.
    #[must_use]
    pub fn layer_offset(&self) -> LayerOffset {
        self.offset
    }

    /// Whether the target maps paths outside the namespace it is about to
    /// themselves: true for a layer target, a local variant target and a
    /// node reached only through variant, inherit and specialize arcs;
    /// false through a reference or payload, which maps only the referenced
    /// prim's namespace, so an edit there of a path outside the prim is
    /// rejected as unmappable.
    ///
    /// OpenUSD: `UsdEditTarget::GetMapFunction().HasRootIdentity()`.
    #[must_use]
    pub fn has_root_identity(&self) -> bool {
        self.map.has_root_identity()
    }

    /// Maps the stage prim path `path` to its spec path in the target
    /// layer; `None` if the target does not map it.
    ///
    /// OpenUSD: `UsdEditTarget::MapToSpecPath`.
    #[must_use]
    pub fn map_to_spec_path(&self, path: PathId, paths: &PathInterner) -> Option<SpecPath> {
        self.map.map_prim(path, paths)
    }

    /// Whether the target maps the stage prim `path` to the spec path of
    /// the same name in its layer, as [`Self::map_to_spec_path`] would.
    /// It interns nothing.
    ///
    /// OpenUSD: `GetMapFunction().IsIdentityPathMapping() ||
    /// MapToSpecPath(path) == path`, the test `UsdStage::_ValidateEditPrim`
    /// makes before it refuses authoring to an instance proxy or prototype.
    #[must_use]
    pub fn maps_to_itself(&self, path: PathId, paths: &PathInterner) -> bool {
        self.map.maps_to_itself(path, paths)
    }

    /// Maps the stage property path `path` to its spec path in the target
    /// layer; `None` if the target does not map its prim.
    #[must_use]
    pub fn map_property_to_spec_path(
        &self,
        path: PropertyPath,
        paths: &PathInterner,
    ) -> Option<SpecPath> {
        self.map
            .map_prim(path.prim_path(), paths)
            .map(|prim| prim.with_property(path.property()))
    }

    /// Maps the stage time `time` to a time in the target layer.
    ///
    /// OpenUSD: `UsdEditTarget::GetMapFunction().GetTimeOffset()`, inverted,
    /// applied to the time (`UsdStage::_SetValueImpl`).
    #[must_use]
    pub fn map_to_spec_time(&self, time: f64) -> f64 {
        self.offset.map_time(time)
    }

    /// Maps `value`, authored on the stage prim `prim` or one of its
    /// properties, to the value written in the target layer: what
    /// [`Transaction::set_default`](crate::Transaction::set_default),
    /// [`set_time_sample`](crate::Transaction::set_time_sample) and
    /// [`set_metadata`](crate::Transaction::set_metadata) write through
    /// the target's addresses.
    ///
    /// A `timecode` is mapped to layer time through the inverse of the
    /// target's layer offset, and a `duration` is divided by its scale. A
    /// `pathExpression` is made absolute at `prim`, then each pattern
    /// prefix and expression reference path is mapped to its spec path
    /// in the target layer, without variant selections; one the target
    /// does not map matches nothing, and a pattern with a leading `//`
    /// is kept. Arrays and dictionaries, at any depth, are mapped
    /// element by element. Asset paths and other values are written as
    /// given.
    ///
    /// OpenUSD: `_StageValueToFieldXf` and `_PathExprToField` in
    /// `pxr/usd/usd/stage.cpp`, applied through `VtValueTryTransform` by
    /// `UsdStage::_SetValue` and `UsdStage::_SetMetadataImpl`;
    /// `SdfPathExpression::MakeAbsolute`, `PcpMapFunction::MapTargetToSource`.
    /// OpenUSD keeps a variant selection in a mapped prefix
    /// (`/Rock{look=a}Child`), which names no prim; this strips it, as
    /// relationship targets are stripped. OpenUSD cannot read that output
    /// back: `SdfPathExpression` parses the `{` as a predicate, so its
    /// USDA reader rejects the saved layer ("Ill-formed path expression
    /// ... expected '}' to close predicate expression").
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets), §10 (arcs map
    /// namespace).
    #[must_use]
    pub fn map_to_spec_value(&self, prim: PathId, value: &Value, store: &dyn LayerStore) -> Value {
        map_value(&self.map, self.offset, prim, value, store).unwrap_or_else(|| value.clone())
    }

    /// Addresses the stage prim `path` through this target.
    #[must_use]
    pub fn prim(&self, path: PathId) -> Address {
        Address {
            layer: self.layer,
            offset: self.offset,
            path: AddressPath::Stage {
                map: self.map.clone(),
                prim: path,
                property: None,
            },
        }
    }

    /// Addresses the stage property `path` through this target.
    #[must_use]
    pub fn property(&self, path: PropertyPath) -> Address {
        Address {
            layer: self.layer,
            offset: self.offset,
            path: AddressPath::Stage {
                map: self.map.clone(),
                prim: path.prim_path(),
                property: Some(path.property()),
            },
        }
    }
}

/// The node `node` of `prim`'s graph, unless it is a relocate node, whose
/// own specs never contribute (see [`EditTarget::for_node`]).
fn editable_node(
    stage: &Stage,
    prim: PathId,
    node: NodeId,
) -> Option<(&PrimIndexGraph, PrimNode<'_>)> {
    let graph = stage.explain_prim_graph(prim)?;
    let node = graph.node(node)?;
    (node.arc_kind() != ArcKind::Relocates).then_some((graph, node))
}

/// The spec an edit applies to: a spec path in a layer, given directly
/// ([`Address::spec`]) or as a stage path through an [`EditTarget`]
/// ([`EditTarget::prim`], [`EditTarget::property`]).
///
/// A stage address is mapped when the edit is applied, and its times are
/// stage times, mapped through the target's layer offset. A spec address
/// names the spec itself, and its times are layer times.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    layer: LayerId,
    offset: LayerOffset,
    path: AddressPath,
}

#[derive(Clone, Debug, PartialEq)]
enum AddressPath {
    Spec(SpecPath),
    Stage {
        map: NamespaceMap,
        prim: PathId,
        property: Option<TokenId>,
    },
}

impl Address {
    /// Addresses the spec at `path` in `layer`: a prim spec (`/Rock`), a
    /// variant spec (`/Rock{shape=jagged}`), or a property spec of either
    /// (`/Rock{shape=jagged}.roughness`).
    #[must_use]
    pub fn spec(layer: LayerId, path: SpecPath) -> Self {
        Self {
            layer,
            offset: LayerOffset::IDENTITY,
            path: AddressPath::Spec(path),
        }
    }

    /// The layer the addressed spec is in.
    #[must_use]
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The stage prim and property this address names, for a stage
    /// address.
    pub(crate) fn stage_path(&self) -> Option<(PathId, Option<TokenId>)> {
        match self.path {
            AddressPath::Spec(_) => None,
            AddressPath::Stage { prim, property, .. } => Some((prim, property)),
        }
    }

    /// The path an address names, as an edit that cannot map it reports
    /// it: a stage address's stage path (`/World/Rock.height`), or a spec
    /// address's own prim or property path, which needs no mapping.
    pub(crate) fn named_path(&self) -> TargetPath {
        let (prim, property) = match &self.path {
            AddressPath::Spec(path) => (path.prim_path(), path.property()),
            AddressPath::Stage { prim, property, .. } => (*prim, *property),
        };
        match property {
            Some(name) => TargetPath::Property(PropertyPath::new(prim, name)),
            None => TargetPath::Prim(prim),
        }
    }

    /// The addressed spec path; `None` if the target does not map the
    /// stage path.
    pub(crate) fn resolve(&self, paths: &PathInterner) -> Option<SpecPath> {
        match &self.path {
            AddressPath::Spec(path) => Some(path.clone()),
            AddressPath::Stage {
                map,
                prim,
                property,
            } => {
                let spec = map.map_prim(*prim, paths)?;
                Some(match property {
                    Some(name) => spec.with_property(*name),
                    None => spec,
                })
            }
        }
    }

    /// Maps a relationship or connection target given with this address
    /// to the path authored in its layer; `None` if the target does not
    /// map it.
    ///
    /// A stage address's targets are stage paths, mapped through the edit
    /// target's namespace map as its own path is. The authored path is the
    /// concrete path: a target never names a variant selection, so one
    /// written inside a variant names the prims the variant's opinions
    /// apply to. A spec address's targets are the layer's own paths,
    /// authored as given.
    ///
    /// A relational attribute or mapper target maps the paths in its
    /// brackets too, and does not map unless they all do.
    ///
    /// OpenUSD: `UsdEditTarget::MapToSpecPath` on each target
    /// (`UsdRelationship::_GetTargetForAuthoring`), which maps each path
    /// `SdfPath::GetAllTargetPathsRecursively` finds, strips variant
    /// selections from it and puts it back with `SdfPath::ReplacePrefix`
    /// (see [`crate::target_translation`]), then strips them from the
    /// result.
    pub(crate) fn map_target(
        &self,
        target: TargetPath,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Option<TargetPath> {
        let AddressPath::Stage { map, .. } = &self.path else {
            return Some(target);
        };
        if let TargetPath::Property(property) = target
            && is_relational(tokens, property.property())
        {
            let map_prim = |prim: PathId| map.map_prim(prim, paths).map(|spec| spec.prim_path());
            let map = PrimMap {
                map: &map_prim,
                root_identity: map.has_root_identity(),
            };
            return map_relational(tokens, paths, property, &map).map(TargetPath::Property);
        }
        let prim = map.map_prim(target.prim_path(), paths)?.prim_path();
        Some(match target {
            TargetPath::Prim(_) => TargetPath::Prim(prim),
            TargetPath::Property(property) => {
                TargetPath::Property(PropertyPath::new(prim, property.property()))
            }
        })
    }

    /// Maps a time given with this address to a time in its layer.
    pub(crate) fn layer_time(&self, time: f64) -> f64 {
        self.offset.map_time(time)
    }

    /// Maps a value given with a stage address to the value written in its
    /// layer, as [`EditTarget::map_to_spec_value`] does; `None` when
    /// nothing changes, and always for a spec address, whose values are
    /// the layer's own.
    pub(crate) fn layer_value(&self, value: &Value, store: &dyn LayerStore) -> Option<Value> {
        match &self.path {
            AddressPath::Spec(_) => None,
            AddressPath::Stage { map, prim, .. } => {
                map_value(map, self.offset, *prim, value, store)
            }
        }
    }

    /// [`Self::layer_value`] for a field value: only a plain value maps.
    pub(crate) fn layer_field(
        &self,
        value: &FieldValue,
        store: &dyn LayerStore,
    ) -> Option<FieldValue> {
        match value {
            FieldValue::Value(value) => self.layer_value(value, store).map(FieldValue::Value),
            _ => None,
        }
    }
}

/// `value`, authored at the stage prim `prim`, in the layer `map` and
/// `offset` take stage namespace and time to; `None` when nothing changes
/// (see [`EditTarget::map_to_spec_value`]).
fn map_value(
    map: &NamespaceMap,
    offset: LayerOffset,
    prim: PathId,
    value: &Value,
    store: &dyn LayerStore,
) -> Option<Value> {
    let mut anchor: Option<Vec<String>> = None;
    crate::stage::stage_time::map_leaves(value, &mut |leaf| match leaf {
        Value::TimeCode(time) if !offset.is_identity() => {
            Some(Value::TimeCode(offset.map_time(*time)))
        }
        // `SdfLayerOffset::operator*(GfDuration)` of the inverse offset
        // scales by its scale alone.
        Value::Duration(length) if offset.scale != 1.0 => {
            Some(Value::Duration(length / offset.scale))
        }
        Value::PathExpression(text) => {
            let anchor = anchor.get_or_insert_with(|| prim_names(store, prim));
            map_expression(text, anchor, map, store)
        }
        _ => None,
    })
}

/// The names of the prim path `prim`, root first.
fn prim_names(store: &dyn LayerStore, prim: PathId) -> Vec<String> {
    store
        .paths()
        .resolve(prim)
        .segments()
        .iter()
        .map(|name| String::from(store.tokens().resolve(*name)))
        .collect()
}

/// The expression `text`, made absolute at `anchor` and mapped through
/// `map` to spec paths without variant selections; `None` when its text
/// does not change or does not parse.
fn map_expression(
    text: &str,
    anchor: &[String],
    map: &NamespaceMap,
    store: &dyn LayerStore,
) -> Option<Value> {
    let expression = PathExpression::parse(text).ok()?;
    let (paths, tokens) = (store.paths(), store.tokens());
    let mapped = anchor_and_map_with(expression, anchor, &mut |names| {
        let segments: Vec<TokenId> = names.iter().map(|name| tokens.intern(name)).collect();
        let spec = map.map_prim(paths.intern_segments(&segments), paths)?;
        Some(prim_names(store, spec.prim_path()))
    })
    .lossless_text();
    (mapped.as_str() != text).then(|| Value::PathExpression(Arc::from(mapped)))
}
