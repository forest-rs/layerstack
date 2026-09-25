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

use hashbrown::HashMap;

use crate::{
    arc_cycle::CycleDetector,
    composition_error::{CompositionError, UnresolvedPrimPath},
    doc::{LayerStore, Reference, ReferenceTarget},
    path::PathId,
    prim_index::{ArcKind, PrimIndex},
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

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use crate::{
        composition_error::{CompositionError, UnresolvedPrimPath},
        doc::{InMemoryStore, Layer, LayerId, PrimSpec, Reference},
        listop::ListOp,
        prim_index::ArcKind,
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
}
