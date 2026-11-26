// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, vec::Vec};
use core::{mem, ops::Range};

use layerstack::{Changes, PathId, sort};

use super::{Matrix4, Scene, Slot, Time, XformCache, transform_change_paths};

/// World matrices for a fixed, ordered selection evaluated across many frames.
///
/// The selection owns a transform cache and the current output rows. Each
/// frame updates only rows whose world transform can vary. After a live edit,
/// apply the complete change report before reading or advancing the selection.
/// The edit path refreshes rows in affected subtrees and updates their
/// time dependencies. Reset boundaries still control world composition.
///
/// As with [`XformCache`], paths and cached data belong to one scene. Create a
/// new selection when switching to a different stage or store.
///
/// Spec: AOUSD Core §11 (population), §12.3 (values), §12.5 (interpolation).
#[derive(Debug)]
pub struct XformSelection {
    cache: XformCache,
    paths: Vec<PathId>,
    matrices: Vec<Option<Matrix4>>,
    // Row indices in namespace order, for bounded subtree lookup after edits.
    ordered: Vec<usize>,
    // Dense time-dependent rows. Reverse positions permit O(1) changes
    // in membership without requiring a time update to visit static rows.
    varying: Vec<(usize, Slot)>,
    varying_positions: Vec<Option<Slot>>,
    affected: Vec<Range<usize>>,
}

impl XformSelection {
    /// Prepares and evaluates these paths at `time`.
    /// Duplicate paths keep separate output rows in their original order.
    #[must_use]
    pub fn new(scene: &Scene<'_>, paths: impl IntoIterator<Item = PathId>, time: Time) -> Self {
        let paths: Vec<_> = paths.into_iter().collect();
        let count = paths.len();
        let interner = scene.store().paths();
        let mut ordered: Vec<_> = (0..count).collect();
        sort::sort_unstable_by(&mut ordered, |&a, &b| {
            interner.resolve(paths[a]).cmp(interner.resolve(paths[b]))
        });
        let mut cache = XformCache::new(time);
        cache.samples = Some(Box::default());
        let mut selection = Self {
            cache,
            paths,
            matrices: alloc::vec![None; count],
            ordered,
            varying: Vec::new(),
            varying_positions: alloc::vec![None; count],
            affected: Vec::new(),
        };
        selection.refresh_all(scene);
        selection
    }

    /// The paths in output order.
    #[must_use]
    pub fn paths(&self) -> &[PathId] {
        &self.paths
    }

    /// World matrices at the current time in selection order.
    /// Rows for absent prims are `None`.
    #[must_use]
    pub fn matrices(&self) -> &[Option<[[f64; 4]; 4]>] {
        &self.matrices
    }

    /// Time at which the world matrices are evaluated.
    #[must_use]
    pub fn time(&self) -> Time {
        self.cache.time()
    }

    /// Updates the output rows whose transform might vary with `time`.
    pub fn set_time(&mut self, scene: &Scene<'_>, time: Time) {
        if time == self.time() {
            return;
        }
        let epoch = self.cache.epoch;
        self.cache.set_time(time);
        if self.cache.epoch < epoch {
            self.refresh_all(scene);
            return;
        }
        for &(row, slot) in &self.varying {
            self.matrices[row] = self.cache.world_at_slot(scene, slot);
        }
    }

    /// Applies a complete live change report against the resulting scene and
    /// refreshes affected rows at the held time.
    ///
    /// Finds affected rows by binary search over the selected paths and
    /// refreshes each affected row once. Pass every report in order.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) {
        let epoch = self.cache.epoch;
        self.cache.apply_changes(scene, changes);
        if self.cache.epoch < epoch {
            self.refresh_all(scene);
            return;
        }

        let interner = scene.store().paths();
        let sorted_path = |row: usize| interner.resolve(self.paths[row]);
        let roots = changes
            .created
            .iter()
            .chain(&changes.removed)
            .chain(&changes.resynced)
            .copied()
            .chain(transform_change_paths(scene, changes));
        for root in roots {
            let prefix = interner.resolve(root);
            let left = self
                .ordered
                .partition_point(|&row| sorted_path(row) < prefix);
            let count =
                self.ordered[left..].partition_point(|&row| prefix.is_prefix_of(sorted_path(row)));
            if count != 0 {
                self.affected.push(left..left + count);
            }
        }

        let mut affected = mem::take(&mut self.affected);
        sort::sort_unstable_by_key(&mut affected, |range| range.start);
        let mut refreshed = 0;
        for range in affected.drain(..) {
            for index in refreshed.max(range.start)..range.end {
                self.refresh(scene, self.ordered[index]);
            }
            refreshed = refreshed.max(range.end);
        }
        self.affected = affected;
    }

    fn refresh_all(&mut self, scene: &Scene<'_>) {
        self.varying.clear();
        self.varying_positions.fill(None);
        for row in 0..self.paths.len() {
            self.refresh(scene, row);
        }
    }

    fn refresh(&mut self, scene: &Scene<'_>, row: usize) {
        let path = self.paths[row];
        self.matrices[row] = self.cache.local_to_world(scene, path);
        let slot = (self.matrices[row].is_some() && self.cache.world_might_vary(path))
            .then(|| self.cache.entries[&path]);

        match (slot, self.varying_positions[row]) {
            (Some(slot), Some(position)) => self.varying[position.get() - 1].1 = slot,
            (Some(slot), None) => {
                self.varying.push((row, slot));
                self.varying_positions[row] = Slot::new(self.varying.len());
            }
            (None, Some(position)) => {
                let index = position.get() - 1;
                self.varying.swap_remove(index);
                if let Some(&(moved, _)) = self.varying.get(index) {
                    self.varying_positions[moved] = Some(position);
                }
                self.varying_positions[row] = None;
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec};
    use layerstack::{
        EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec,
        PropertyType, Specifier, StageOptions, TokenId, Transaction, Value,
    };

    use super::*;

    struct Fixture {
        store: InMemoryStore,
        live: LiveStage,
        paths: [PathId; 5],
        order: TokenId,
        translate: TokenId,
        rotate: TokenId,
        reset: TokenId,
    }

    fn fixture() -> Fixture {
        let mut store = InMemoryStore::default();
        let [root, leaf, detached, other, missing] = [
            "/World",
            "/World/Leaf",
            "/World/Reset",
            "/WorldElse/Leaf",
            "/Missing",
        ]
        .map(|path| store.path(path));
        let [xform, order, translate, rotate, reset] = [
            "Xform",
            "xformOpOrder",
            "xformOp:translate",
            "xformOp:rotateZ",
            "!resetXformStack!",
        ]
        .map(|name| store.tokens().intern(name));
        let token_array = |tokens: &[TokenId]| {
            PropertySpec::typed_attribute(PropertyType::parse("token[]").unwrap()).with_default(
                Value::Array(tokens.iter().copied().map(Value::Token).collect()),
            )
        };
        let prim = |tokens: &[TokenId]| {
            PrimSpec::def()
                .with_type_name(xform)
                .with_property(order, token_array(tokens))
        };
        let translate_default =
            PropertySpec::typed_attribute(PropertyType::parse("double3").unwrap())
                .with_default(Value::Vec3d([1., 3., 0.]));
        let rotation = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
            .with_default(Value::Double(20.));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            root,
            prim(&[translate, rotate])
                .with_property(translate, translate_default.clone())
                .with_property(rotate, rotation.clone()),
        );
        layer.insert_prim(
            leaf,
            prim(&[translate, rotate])
                .with_property(
                    translate,
                    translate_default.clone().with_time_samples(vec![
                        (0., Value::Vec3d([0., 1., 0.])),
                        (2., Value::Vec3d([5., 1., 2.])),
                    ]),
                )
                .with_property(rotate, rotation),
        );
        layer.insert_prim(
            detached,
            prim(&[reset, translate]).with_property(translate, translate_default.clone()),
        );
        layer.insert_prim(
            other,
            prim(&[translate]).with_property(translate, translate_default),
        );
        store.insert_layer(layer);
        let opts = StageOptions {
            schemas: Some(Arc::new(crate::openusd(store.tokens()))),
            ..Default::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), opts);
        Fixture {
            store,
            live,
            paths: [root, leaf, detached, other, missing],
            order,
            translate,
            rotate,
            reset,
        }
    }

    fn compare_frames(selection: &mut XformSelection, scene: &Scene<'_>) {
        for time in [
            Time::Default,
            Time::at(-1.),
            Time::held(1.),
            Time::at(1.),
            Time::at(4.),
        ] {
            selection.set_time(scene, time);
            let mut fresh = XformCache::new(time);
            for (&path, expected) in selection.paths().iter().zip(selection.matrices()) {
                assert_eq!(*expected, fresh.local_to_world(scene, path));
            }
        }
    }

    #[test]
    fn retained_selection_tracks_bindings_membership_and_slot_retirement() {
        let mut f = fixture();
        let [root, leaf, detached, other, missing] = f.paths;
        let scene = Scene::new(f.live.stage(), &f.store);
        let mut selection = XformSelection::new(
            &scene,
            [other, detached, leaf, leaf, missing, root],
            Time::Default,
        );
        assert_eq!(selection.varying.len(), 2);
        assert_eq!(selection.cache.samples.as_ref().unwrap().len(), 1);
        let initial = selection.matrices().to_vec();
        assert_eq!(initial[2], initial[3]);
        assert_eq!(initial[4], None);
        compare_frames(&mut selection, &scene);

        let target = EditTarget::for_layer(LayerId(1));
        let mut sample = Transaction::new();
        sample.set_time_sample(
            target.property(PropertyPath::new(root, f.rotate)),
            1.,
            Value::Double(45.),
        );
        let mut order = Transaction::new();
        order.set_default(
            target.property(PropertyPath::new(leaf, f.order)),
            Value::Array(vec![Value::Token(f.reset), Value::Token(f.rotate)]),
        );
        let mut fallback = Transaction::new();
        fallback.set_time_sample(
            target.property(PropertyPath::new(leaf, f.translate)),
            2.,
            Value::Blocked,
        );
        let mut remove = Transaction::new();
        remove.remove_spec(target.prim(root));

        let mut create = Transaction::new();
        create.create_prim(target.prim(missing), Specifier::Def, None);

        for (tx, varying, bindings) in [
            (create, 2, 1),
            (sample, 3, 2),
            (order, 0, 0),
            (fallback, 2, 0),
            (remove, 0, 0),
        ] {
            let applied = f.live.apply(&mut f.store, &tx).unwrap();
            let scene = Scene::new(f.live.stage(), &f.store);
            selection.apply_changes(&scene, &applied.changes);
            assert_eq!(selection.varying.len(), varying);
            assert_eq!(selection.cache.samples.as_ref().unwrap().len(), bindings);
            compare_frames(&mut selection, &scene);
            let undone = f.live.apply(&mut f.store, &applied.inverse).unwrap();
            let scene = Scene::new(f.live.stage(), &f.store);
            selection.apply_changes(&scene, &undone.changes);
            compare_frames(&mut selection, &scene);
            assert_eq!(selection.varying.len(), 2);
            assert_eq!(selection.cache.samples.as_ref().unwrap().len(), 1);
        }
        selection.set_time(&Scene::new(f.live.stage(), &f.store), Time::Default);
        assert_eq!(selection.matrices(), initial);
    }

    #[test]
    fn local_edits_refresh_only_selected_descendants() {
        let mut f = fixture();
        let [root, leaf, detached, other, missing] = f.paths;
        let scene = Scene::new(f.live.stage(), &f.store);
        let mut selection = XformSelection::new(
            &scene,
            [detached, other, leaf, missing, root, leaf],
            Time::at(1.),
        );
        let processed = |s: &XformSelection| {
            let stats = s.cache.stats();
            (stats.hits, stats.local_computed, stats.world_computed)
        };
        let before = processed(&selection);
        let mut edit = Transaction::new();
        edit.set_time_sample(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(leaf, f.translate)),
            2.,
            Value::Vec3d([7., 9., 11.]),
        );
        let applied = f.live.apply(&mut f.store, &edit).unwrap();
        let scene = Scene::new(f.live.stage(), &f.store);
        selection.apply_changes(&scene, &applied.changes);
        assert_eq!(
            processed(&selection),
            (before.0 + 3, before.1 + 1, before.2 + 1)
        );
        compare_frames(&mut selection, &scene);
    }
}
