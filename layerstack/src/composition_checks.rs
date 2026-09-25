// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validation of composed scene description.
//!
//! These checks find the composition errors that do not stop an arc from
//! being followed, but make some of the scene description it brings in
//! invalid. Each reports a [`CompositionError`] and, where OpenUSD ignores
//! the offending specs, drops them the same way, so a check never changes a
//! resolved value that OpenUSD keeps.
//!
//! Spec: AOUSD Core §10.6 (composition errors).

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{
    arc_cycle::CycleDetector,
    composition_error::{CompositionError, InconsistentPropertyType, UnresolvedPrimPath},
    doc::{LayerStore, Reference, ReferenceTarget},
    path::PathId,
    prim_index::{ArcKind, FieldKey, OpinionKey, PrimIndex},
};

/// Checks that a reference or payload followed while composing `prim`
/// brings in specs, reporting [`UnresolvedPrimPath`] when it does not.
///
/// The arc's node has specs when its target site, or any site beneath its
/// node (the arcs authored on the target and on its ancestors), provides a
/// prim spec: [`Self::begin`] notes the sources `prim` has before the arc is
/// expanded and [`Self::finish`] reports the arc when the expansion added
/// none. An arc to a target that exists only through a cycle adds none and
/// is reported, as in OpenUSD.
///
/// Only arcs authored on `prim` itself are checked; an arc its ancestors
/// author is checked when composing them, where it is introduced.
///
/// Spec: AOUSD Core §10.3.2.1 (a reference to a path without specs in the
/// referenced layer stack is a composition error), §10.3.2.2. OpenUSD:
/// `_EvalUnresolvedPrimPathError` and `_PrimSpecExistsUnderNodeAtIntroduction`
/// in `pxr/usd/pcp/primIndex.cpp`.
pub(crate) struct TargetSpecsCheck {
    error: UnresolvedPrimPath,
    sources: usize,
}

impl TargetSpecsCheck {
    /// Starts the check of `reference`, which targets `path` for `prim` and
    /// is authored at namespace depth `namespace_depth`; `None` when it is
    /// not checked (a `defaultPrim` target, reported as
    /// [`crate::CompositionError::UnresolvedDefaultPrim`], or an arc
    /// authored on an ancestor).
    pub(crate) fn begin(
        store: &dyn LayerStore,
        out: &HashMap<PathId, PrimIndex>,
        reference: &Reference,
        prim: PathId,
        arc: ArcKind,
        path: PathId,
        namespace_depth: u16,
    ) -> Option<Self> {
        if reference.target == ReferenceTarget::DefaultPrim
            || usize::from(namespace_depth) != store.paths().resolve(prim).depth()
        {
            return None;
        }
        Some(Self {
            error: UnresolvedPrimPath {
                prim,
                arc,
                layer: reference.layer,
                path,
            },
            sources: out.get(&prim).map_or(0, |index| index.sources.len()),
        })
    }

    /// Reports the arc when expanding it added no source to the prim.
    pub(crate) fn finish(self, out: &HashMap<PathId, PrimIndex>, cycles: &mut CycleDetector) {
        let sources = out
            .get(&self.error.prim)
            .map_or(0, |index| index.sources.len());
        if sources == self.sources {
            cycles.report(CompositionError::UnresolvedPrimPath(self.error));
        }
    }
}

/// Drops each property spec of `prim`'s index whose kind differs from the
/// kind of the property's strongest spec, reporting
/// [`InconsistentPropertyType`] for each. `index` must be ranked
/// ([`PrimIndex::finalize`]).
///
/// Spec: AOUSD Core §7.6.3, §10.6. OpenUSD: `_GetPrimProperty` in
/// `pxr/usd/pcp/propertyIndex.cpp` ignores a spec whose `SdfSpecType`
/// differs from the first spec's. Attribute type and variability mismatches
/// are not checked: OpenUSD ignores them in USD mode.
pub(crate) fn drop_inconsistent_property_kinds(
    prim: PathId,
    index: &mut PrimIndex,
    cycles: &mut CycleDetector,
) {
    for (field, opinions) in &mut index.opinions_by_field {
        let FieldKey::Property(property) = *field else {
            continue;
        };
        let mut specs = opinions.iter().filter_map(|opinion| {
            opinion
                .value
                .as_property()
                .map(|spec| (&opinion.key, spec.kind))
        });
        let Some((defining, defining_kind)) = specs.next() else {
            continue;
        };
        let conflicting: Vec<OpinionKey> = specs
            .filter(|(_, kind)| *kind != defining_kind)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &conflicting {
            cycles.report(CompositionError::InconsistentPropertyType(
                InconsistentPropertyType {
                    prim,
                    property,
                    defining_layer: defining.layer_id,
                    defining_spec: defining.spec_path.clone(),
                    defining_kind,
                    conflicting_layer: key.layer_id,
                    conflicting_spec: key.spec_path.clone(),
                },
            ));
        }
        if conflicting.is_empty() {
            continue;
        }
        opinions.retain(|opinion| {
            opinion.value.as_property().is_none() || !conflicting.contains(&opinion.key)
        });
        if let Some(declarations) = index.property_types_by_field.get_mut(&property) {
            declarations.retain(|(key, _)| !conflicting.contains(key));
        }
    }
    index
        .property_types_by_field
        .retain(|_, declarations| !declarations.is_empty());
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use crate::{
        composition_error::{CompositionError, InconsistentPropertyType, UnresolvedPrimPath},
        doc::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Value},
        listop::ListOp,
        path::{PropertyPath, TargetPath},
        prim_index::ArcKind,
        property::{PropertyKind, PropertySpec},
        spec_path::SpecPath,
        stage::{Stage, StageOptions},
    };

    const ROOT: LayerId = LayerId(1);
    const ASSET: LayerId = LayerId(2);

    /// Spec: AOUSD Core §10.3.2.1; OpenUSD reports `PcpErrorUnresolvedPrimPath`
    /// on the prim whose composition reaches the arc, and keeps the rest.
    #[test]
    fn arc_to_a_prim_without_specs_is_reported() {
        let mut store = InMemoryStore::default();
        let (stone, pebble, missing, brook, spring, nowhere) = (
            store.path("/Stone"),
            store.path("/Pebble"),
            store.path("/Missing"),
            store.path("/Brook"),
            store.path("/Spring"),
            store.path("/Nowhere"),
        );
        let mut root = Layer::new(ROOT);
        let mut stone_spec = PrimSpec::def();
        stone_spec.payloads = ListOp {
            explicit: Some(vec![
                Reference::new(ROOT, pebble),
                Reference::new(ROOT, missing),
            ]),
            ..ListOp::default()
        };
        root.insert_prim(stone, stone_spec);
        root.insert_prim(pebble, PrimSpec::def());
        let mut brook_spec = PrimSpec::def();
        brook_spec.references = ListOp {
            explicit: Some(vec![
                Reference::with_asset(ASSET, nowhere, "asset.usda"),
                Reference::with_asset(ASSET, spring, "asset.usda"),
            ]),
            ..ListOp::default()
        };
        root.insert_prim(brook, brook_spec);
        store.insert_layer(root);
        let mut asset = Layer::new(ASSET);
        asset.insert_prim(spring, PrimSpec::def());
        store.insert_layer(asset);

        let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
        let mut errors = stage.composition_errors().to_vec();
        errors.sort_by_key(|error| error.prim());
        let mut expected = vec![
            CompositionError::UnresolvedPrimPath(UnresolvedPrimPath {
                prim: stone,
                arc: ArcKind::Payloads,
                layer: ROOT,
                path: missing,
            }),
            CompositionError::UnresolvedPrimPath(UnresolvedPrimPath {
                prim: brook,
                arc: ArcKind::References,
                layer: ASSET,
                path: nowhere,
            }),
        ];
        expected.sort_by_key(|error| error.prim());
        assert_eq!(errors, expected);
        let sites: Vec<_> = stage
            .explain_prim(stone)
            .expect("composed")
            .iter()
            .map(|key| key.lookup_path)
            .collect();
        assert_eq!(sites, [stone, pebble]);
    }

    /// Spec: AOUSD Core §7.6.3, §10.6; OpenUSD ignores a weaker spec of the
    /// other kind and reports `PcpErrorInconsistentPropertyType`.
    #[test]
    fn weaker_spec_of_the_other_kind_is_dropped() {
        let mut store = InMemoryStore::default();
        let (lantern, lamp) = (store.path("/Lantern"), store.path("/Lamp"));
        let (glow, wick) = (store.tokens.intern("glow"), store.tokens.intern("wick"));
        let mut root = Layer::new(ROOT);
        let mut lantern_spec = PrimSpec::def()
            .with_property(glow, PropertySpec::attribute().with_default(Value::Int(1)))
            .with_property(
                wick,
                PropertySpec::relationship().with_targets(ListOp {
                    explicit: Some(vec![TargetPath::Prim(lamp)]),
                    ..ListOp::default()
                }),
            );
        lantern_spec.references = ListOp {
            explicit: Some(vec![Reference::with_asset(ASSET, lamp, "lamp.usda")]),
            ..ListOp::default()
        };
        root.insert_prim(lantern, lantern_spec);
        store.insert_layer(root);
        let mut asset = Layer::new(ASSET);
        asset.insert_prim(
            lamp,
            PrimSpec::def()
                .with_property(glow, PropertySpec::relationship())
                .with_property(wick, PropertySpec::attribute().with_default(Value::Int(4))),
        );
        store.insert_layer(asset);

        let stage = Stage::compose(
            &mut store,
            ROOT,
            StageOptions {
                with_provenance: true,
                ..StageOptions::default()
            },
        );
        let stack = |name| -> Vec<_> {
            stage
                .explain_property_path(PropertyPath::new(lantern, name))
                .expect("composed property")
                .iter()
                .map(|opinion| opinion.key.layer_id)
                .collect()
        };
        assert_eq!(stack(glow), [ROOT]);
        assert_eq!(stack(wick), [ROOT]);
        assert_eq!(
            stage
                .resolve_property_declaration(lantern, wick)
                .map(|d| d.kind),
            Some(PropertyKind::Relationship)
        );
        let lantern_spec =
            |name| SpecPath::from_prim_path(lantern, &store.paths).with_property(name);
        let lamp_spec = |name| SpecPath::from_prim_path(lamp, &store.paths).with_property(name);
        let mut errors = stage.composition_errors().to_vec();
        errors.sort_by_key(|error| match error {
            CompositionError::InconsistentPropertyType(error) => error.defining_kind as u8,
            _ => u8::MAX,
        });
        assert_eq!(
            errors,
            [
                CompositionError::InconsistentPropertyType(InconsistentPropertyType {
                    prim: lantern,
                    property: glow,
                    defining_layer: ROOT,
                    defining_spec: lantern_spec(glow),
                    defining_kind: PropertyKind::Attribute,
                    conflicting_layer: ASSET,
                    conflicting_spec: lamp_spec(glow),
                }),
                CompositionError::InconsistentPropertyType(InconsistentPropertyType {
                    prim: lantern,
                    property: wick,
                    defining_layer: ROOT,
                    defining_spec: lantern_spec(wick),
                    defining_kind: PropertyKind::Relationship,
                    conflicting_layer: ASSET,
                    conflicting_spec: lamp_spec(wick),
                }),
            ]
        );
    }
}
