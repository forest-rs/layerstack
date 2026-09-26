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
    composition_error::{
        CompositionError, InconsistentPropertyType, InvalidExternalTargetPath, UnresolvedPrimPath,
    },
    doc::{FieldValue, LayerId, LayerStore, Reference, ReferenceTarget},
    interner::TokenId,
    path::{Path, PathId, PropertyPath, TargetPath},
    prim_index::{ArcKind, FieldKey, OpinionKey, OpinionValue, PrimIndex},
    relocates::Walk,
    spec_path::SpecPath,
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

/// How an arc maps paths authored beneath its target onto its destination.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArcPathMap<'a> {
    /// The arc.
    pub(crate) arc: ArcKind,
    /// The arc's target prim, in the namespace its specs are authored in.
    pub(crate) source: &'a Path,
    /// The arc's destination prim.
    pub(crate) dest: &'a Path,
    /// How a path beneath `source` maps.
    pub(crate) inside: Inside<'a>,
    /// What the arc does with a path outside `source`.
    pub(crate) outside: Outside,
    /// The relocations of the arc target's layer stack, as `(target,
    /// source)` pairs in the namespace paths are authored in. Under
    /// [`Outside::Identity`], a path at or beneath a target whose source
    /// lies beneath `dest` does not map: it is content of the destination
    /// that a relocation moved, which would not map back.
    pub(crate) relocated: &'a [(PathId, PathId)],
}

/// How an [`ArcPathMap`] maps a path beneath the arc's target.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Inside<'a> {
    /// Onto the same path beneath the destination.
    Join,
    /// Through the relocations the arc's walk passes (see [`Walk::map`]),
    /// beneath the destination prim with this path: relocates first, then
    /// the arc's scope.
    Relocate(&'a Walk<'a>, PathId),
    /// Left as authored, for a later pass over the arc's opinions to map;
    /// only a path outside the target is checked.
    Keep,
}

/// What an [`ArcPathMap`] does with a path outside the arc's target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outside {
    /// It does not map: a reference or payload to another layer stack.
    Unmapped,
    /// It maps to itself unless it is beneath the destination, which is in
    /// the same namespace: an internal reference or payload, an inherit or
    /// a specializes authored in the stage's namespace.
    Identity,
    /// It maps to itself; the destination is in another namespace, so a
    /// path beneath it is not recognized. Nested class and internal arcs
    /// under-report rather than drop a path that maps.
    IdentityUnchecked,
}

impl ArcPathMap<'_> {
    /// Maps `path`, or returns `None` when the arc cannot map it.
    fn map(&self, store: &mut dyn LayerStore, path: PathId) -> Option<PathId> {
        let resolved = store.paths().resolve(path);
        if let Some(rel) = resolved.strip_prefix(self.source) {
            return Some(match self.inside {
                Inside::Join => {
                    let mapped = self.dest.join(rel);
                    store.paths_mut().intern(mapped)
                }
                Inside::Relocate(walk, dest_root) => {
                    let rel = rel.to_vec();
                    walk.map(store, dest_root, &rel)
                }
                Inside::Keep => path,
            });
        }
        match self.outside {
            Outside::Unmapped => None,
            Outside::Identity if resolved.strip_prefix(self.dest).is_some() => None,
            Outside::Identity if self.moved_from_dest(store, path) => None,
            Outside::Identity | Outside::IdentityUnchecked => Some(path),
        }
    }

    /// Whether `path` lies at or beneath a relocation target whose source
    /// lies beneath `dest` (see [`Self::relocated`]).
    fn moved_from_dest(&self, store: &dyn LayerStore, path: PathId) -> bool {
        let paths = store.paths();
        let resolved = paths.resolve(path);
        self.relocated.iter().any(|&(target, source)| {
            paths.resolve(target).is_prefix_of(resolved)
                && self.dest.is_prefix_of(paths.resolve(source))
        })
    }

    fn map_target(&self, store: &mut dyn LayerStore, target: TargetPath) -> Option<TargetPath> {
        match target {
            TargetPath::Prim(path) => self.map(store, path).map(TargetPath::Prim),
            TargetPath::Property(path) => self
                .map(store, path.prim_path())
                .map(|prim| TargetPath::Property(PropertyPath::new(prim, path.property()))),
        }
    }
}

/// The property spec a set of target paths is authored on: `property` of
/// the composed prim `prim`, from `layer`.
#[derive(Clone, Debug)]
pub(crate) struct TargetOwner {
    pub(crate) prim: PathId,
    pub(crate) property: TokenId,
    pub(crate) layer: LayerId,
    /// The spec's path in the property's stack.
    pub(crate) spec: SpecPath,
}

/// Maps the relationship targets or attribute connections of `value`, a
/// spec brought in across `map`'s arc, into the arc's destination.
///
/// A path the arc cannot map is removed and reported as
/// [`InvalidExternalTargetPath`]; a deleted path that cannot be mapped is
/// removed silently, since it removes nothing. Values other than a
/// property spec's targets are mapped where they can be and otherwise kept.
///
/// Spec: AOUSD Core §10.3.2, §10.6. OpenUSD: `_PathTranslateCallback` in
/// `pxr/usd/pcp/targetIndex.cpp` and `PcpMapFunction`'s bijection check in
/// `pxr/usd/pcp/mapFunction.cpp`.
pub(crate) fn map_arc_targets(
    store: &mut dyn LayerStore,
    value: &mut OpinionValue,
    map: ArcPathMap<'_>,
    owner: TargetOwner,
    cycles: &mut CycleDetector,
) {
    let list = match value {
        OpinionValue::Property(spec) => match spec.targets.as_mut() {
            Some(list) => list,
            None => return,
        },
        OpinionValue::Field(FieldValue::PathListOp(list)) => {
            let keep = ArcPathMap {
                outside: Outside::IdentityUnchecked,
                ..map
            };
            for items in [&mut list.prepend, &mut list.append, &mut list.delete]
                .into_iter()
                .chain(list.explicit.as_mut())
            {
                for item in items.iter_mut() {
                    *item = keep.map_target(store, *item).unwrap_or(*item);
                }
            }
            return;
        }
        OpinionValue::Field(_) => return,
    };
    let mut report = |target: TargetPath| {
        cycles.report(CompositionError::InvalidExternalTargetPath(
            InvalidExternalTargetPath {
                prim: owner.prim,
                property: owner.property,
                target,
                spec: owner.spec.clone(),
                arc: map.arc,
                layer: owner.layer,
            },
        ));
    };
    for items in [&mut list.prepend, &mut list.append]
        .into_iter()
        .chain(list.explicit.as_mut())
    {
        *items = items
            .iter()
            .filter_map(|item| {
                let mapped = map.map_target(store, *item);
                if mapped.is_none() {
                    report(*item);
                }
                mapped
            })
            .collect();
    }
    list.delete = list
        .delete
        .iter()
        .filter_map(|item| map.map_target(store, *item))
        .collect();
}

/// The property spec a target path error was found in, or `None` for an
/// error of another kind.
fn target_error_spec(error: &CompositionError) -> Option<&SpecPath> {
    match error {
        CompositionError::InvalidExternalTargetPath(error) => Some(&error.spec),
        _ => None,
    }
}

/// Whether a target path error applies to the composed property: an
/// explicit target list authored stronger than the spec the error is found
/// in replaces that spec's targets, so its errors are not reported. Other
/// errors always apply.
///
/// `prims` must be ranked. The error applies when the opinion of the spec
/// it was found in ranks no weaker than the property's strongest explicit
/// target list: an error in that list itself applies, and one in any
/// weaker opinion, from the same layer or not, does not.
///
/// Spec: AOUSD Core §12.4 (an explicit list op replaces weaker opinions).
/// OpenUSD: `PcpBuildFilteredTargetIndex` in `pxr/usd/pcp/targetIndex.cpp`
/// clears the target path errors found so far at each explicit list op.
pub(crate) fn target_error_applies(
    error: &CompositionError,
    prims: &HashMap<PathId, PrimIndex>,
) -> bool {
    let (prim, property, layer) = match error {
        CompositionError::InvalidExternalTargetPath(error) => {
            (error.prim, error.property, error.layer)
        }
        _ => return true,
    };
    let Some(spec) = target_error_spec(error) else {
        return true;
    };
    let Some(opinions) = prims
        .get(&prim)
        .and_then(|index| index.property_opinions(property))
    else {
        return true;
    };
    for opinion in opinions {
        let Some(targets) = opinion.value.targets() else {
            continue;
        };
        if opinion.key.layer_id == layer && opinion.key.spec_path == *spec {
            return true;
        }
        if targets.explicit.is_some() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::{ArcPathMap, Inside, Outside};
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

    /// Spec: AOUSD Core §10.3.2; OpenUSD's `PcpMapFunction` maps an arc's
    /// target onto its destination and, for internal and class arcs, every
    /// other path to itself unless that would not map back.
    #[test]
    fn arcs_map_target_paths_like_openusd() {
        let mut store = InMemoryStore::default();
        let [tools, saw, shed, shed_saw, door, pebble] = [
            "/Tools",
            "/Tools/Saw",
            "/Shed",
            "/Shed/Saw",
            "/Shed/Door",
            "/Pebble",
        ]
        .map(|path| store.path(path));
        let (source, dest) = (
            store.paths.resolve(tools).clone(),
            store.paths.resolve(shed).clone(),
        );
        let map = |outside| ArcPathMap {
            arc: ArcKind::References,
            source: &source,
            dest: &dest,
            inside: Inside::Join,
            outside,
            relocated: &[],
        };
        let mapped = |store: &mut InMemoryStore, outside, path| map(outside).map(store, path);
        for outside in [
            Outside::Unmapped,
            Outside::Identity,
            Outside::IdentityUnchecked,
        ] {
            assert_eq!(mapped(&mut store, outside, saw), Some(shed_saw));
        }
        assert_eq!(mapped(&mut store, Outside::Unmapped, pebble), None);
        assert_eq!(mapped(&mut store, Outside::Identity, pebble), Some(pebble));
        assert_eq!(mapped(&mut store, Outside::Identity, door), None);
        assert_eq!(
            mapped(&mut store, Outside::IdentityUnchecked, door),
            Some(door)
        );
    }
}
