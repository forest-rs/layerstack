// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composing over a store that is still loading some of its layers
//! ([`LayerStore::is_layer_pending`]).

use alloc::{format, string::String, vec, vec::Vec};

use hashbrown::HashSet;

use super::{Boundaries, ChildIndex};
use crate::{
    InMemoryStore, Layer, LayerId, LayerStore, PathId, PathInterner, PendingPrim, PrimSpec,
    Reference, Stage, StageOptions, SublayerEntry, TokenInterner,
};

const ROOT: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);
const OUTER: LayerId = LayerId(3);
const INNER: LayerId = LayerId(4);
const ROOT_SUBLAYER: LayerId = LayerId(5);
const ASSET_SUBLAYER: LayerId = LayerId(6);

/// A store that holds every layer but those still loading, which it
/// reports pending.
struct LoadingStore {
    inner: InMemoryStore,
    pending: HashSet<LayerId>,
}

impl LayerStore for LoadingStore {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        if self.pending.contains(&id) {
            None
        } else {
            self.inner.layer(id)
        }
    }
    fn is_layer_pending(&self, id: LayerId) -> bool {
        self.pending.contains(&id)
    }
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        if self.pending.contains(&id) {
            None
        } else {
            self.inner.layer_mut(id)
        }
    }
    fn tokens(&self) -> &TokenInterner {
        self.inner.tokens()
    }
    fn paths(&self) -> &PathInterner {
        self.inner.paths()
    }
}

/// A stage of
///
/// - `/World/A` and `/World/E`, referencing `/Asset` of the layer stack
///   `ASSET`, which sublayers `ASSET_SUBLAYER`, and has a child `Child`;
/// - `/World/B`, with a child `C`, reading only the stage's layers;
/// - `/World/D`, referencing `/Outer` of `OUTER`, which references
///   `/Inner` of `INNER` in turn;
///
/// and a root layer that sublayers `ROOT_SUBLAYER`, which authors `/Extra`.
fn store() -> LoadingStore {
    let mut store = InMemoryStore::default();
    let asset = store.path("/Asset");
    let outer = store.path("/Outer");
    let inner = store.path("/Inner");
    let size = store.tokens.intern("size");
    let attribute = |value: i64| crate::PropertySpec::attribute().with_default(value);

    let mut root = Layer::new(ROOT);
    root.sublayers_mut().push(SublayerEntry::new(ROOT_SUBLAYER));
    for (path, spec) in [
        ("/World", PrimSpec::def()),
        (
            "/World/A",
            PrimSpec::def().with_reference(Reference::new(ASSET, asset)),
        ),
        ("/World/B", PrimSpec::def()),
        ("/World/B/C", PrimSpec::def()),
        (
            "/World/D",
            PrimSpec::def().with_reference(Reference::new(OUTER, outer)),
        ),
        (
            "/World/E",
            PrimSpec::def().with_reference(Reference::new(ASSET, asset)),
        ),
    ] {
        let path = store.path(path);
        root.insert_prim(path, spec);
    }
    store.insert_layer(root);

    let mut extra = Layer::new(ROOT_SUBLAYER);
    extra.insert_prim(store.path("/Extra"), PrimSpec::def());
    store.insert_layer(extra);

    let mut layer = Layer::new(ASSET);
    layer
        .sublayers_mut()
        .push(SublayerEntry::new(ASSET_SUBLAYER));
    layer.insert_prim(asset, PrimSpec::def().with_property(size, attribute(1)));
    layer.insert_prim(store.path("/Asset/Child"), PrimSpec::def());
    store.insert_layer(layer);
    let mut weaker = Layer::new(ASSET_SUBLAYER);
    weaker.insert_prim(asset, PrimSpec::over().with_property(size, attribute(2)));
    store.insert_layer(weaker);

    let mut layer = Layer::new(OUTER);
    layer.insert_prim(
        outer,
        PrimSpec::def().with_reference(Reference::new(INNER, inner)),
    );
    store.insert_layer(layer);
    let mut layer = Layer::new(INNER);
    layer.insert_prim(inner, PrimSpec::def().with_property(size, attribute(3)));
    layer.insert_prim(store.path("/Inner/Deep"), PrimSpec::def());
    store.insert_layer(layer);

    LoadingStore {
        inner: store,
        pending: HashSet::new(),
    }
}

/// Everything a composed stage holds, in a form two stages over one
/// store compare by: each prim's index and children, by path, and the
/// stage's errors and layers.
fn summary(stage: &Stage, store: &dyn LayerStore) -> Vec<String> {
    let mut prims: Vec<PathId> = stage.prim_paths().collect();
    prims.sort_by_cached_key(|path| store.paths().resolve(*path).display(store.tokens()));
    let mut out: Vec<String> = prims
        .into_iter()
        .map(|path| {
            format!(
                "{} {:?} {:?}",
                store.paths().resolve(path).display(store.tokens()),
                stage.index_of(path),
                stage.all_children_of(path),
            )
        })
        .collect();
    out.push(format!("{:?}", stage.composition_errors()));
    out.push(format!("{:?}", stage.used_layers(true)));
    out.push(format!("{:?}", stage.layer_stack()));
    out.push(format!("{:?}", stage.pending_prims()));
    out
}

fn compose(store: &mut LoadingStore, child_index: &ChildIndex) -> Stage {
    Stage::compose_retained(
        store,
        ROOT,
        StageOptions::default(),
        false,
        &Boundaries::default(),
        child_index,
    )
}

/// The stage composed once every layer has loaded, over one child index
/// with `stage`, is the stage composed with every layer loaded from the
/// start: nothing composed while layers were pending was kept.
fn assert_complete_once_loaded(store: &mut LoadingStore, child_index: &ChildIndex) {
    store.pending.clear();
    let loaded = compose(store, child_index);
    let fresh = compose(store, &ChildIndex::default());
    assert!(loaded.pending_prims().is_empty());
    assert_eq!(summary(&loaded, store), summary(&fresh, store));
}

/// A prim whose reference targets a layer still loading is not composed,
/// nor are its descendants: the stage lists it with the layer, and every
/// prim that does not read the layer composes as it would with it.
///
/// Spec: AOUSD Core §10.3.2.1 (references); the stage composes the prims
/// whose indexes it can complete.
#[test]
fn a_reference_to_a_pending_layer_leaves_its_prim_pending() {
    let mut store = store();
    let complete = compose(&mut store, &ChildIndex::default());
    let complete = summary(&complete, &store);
    store.pending.insert(ASSET);
    let child_index = ChildIndex::default();
    let stage = compose(&mut store, &child_index);
    let path = |store: &LoadingStore, path: &str| store.inner.path(path);
    let (a, e) = (path(&store, "/World/A"), path(&store, "/World/E"));
    // Both prims wait: the incomplete layer stack is not kept for the
    // second one to read as an empty one.
    assert_eq!(
        stage.pending_prims(),
        [
            PendingPrim {
                prim: a,
                layers: vec![ASSET],
            },
            PendingPrim {
                prim: e,
                layers: vec![ASSET],
            },
        ]
    );
    for pending in ["/World/A", "/World/A/Child", "/World/E", "/World/E/Child"] {
        assert!(!stage.has_prim(path(&store, pending)), "{pending}");
    }
    let world = path(&store, "/World");
    assert_eq!(
        stage.all_children_of(world),
        Some(&[path(&store, "/World/B"), path(&store, "/World/D")][..])
    );
    // The prims that read no pending layer compose as they do with it.
    let kept: Vec<String> = summary(&stage, &store)
        .into_iter()
        .filter(|line| line.starts_with("/World/B") || line.starts_with("/World/D"))
        .collect();
    let expected: Vec<String> = complete
        .into_iter()
        .filter(|line| line.starts_with("/World/B") || line.starts_with("/World/D"))
        .collect();
    assert_eq!(kept, expected);
    assert_complete_once_loaded(&mut store, &child_index);
}

/// A sublayer of a referenced layer stack that is still loading leaves the
/// stack incomplete: the prims referencing it wait for the sublayer.
///
/// Spec: AOUSD Core §10.3.1 (sublayers).
#[test]
fn a_pending_sublayer_of_a_referenced_stack_leaves_its_prims_pending() {
    let mut store = store();
    store.pending.insert(ASSET_SUBLAYER);
    let child_index = ChildIndex::default();
    let stage = compose(&mut store, &child_index);
    let pending: Vec<(PathId, Vec<LayerId>)> = stage
        .pending_prims()
        .iter()
        .map(|pending| (pending.prim, pending.layers.clone()))
        .collect();
    assert_eq!(
        pending,
        [
            (store.inner.path("/World/A"), vec![ASSET_SUBLAYER]),
            (store.inner.path("/World/E"), vec![ASSET_SUBLAYER]),
        ]
    );
    assert_complete_once_loaded(&mut store, &child_index);
}

/// An arc authored in a referenced layer that targets a layer still loading
/// leaves the prim whose index follows it pending, with the inner layer.
///
/// Spec: AOUSD Core §10.3.2.1 (references are followed transitively).
#[test]
fn a_nested_arc_to_a_pending_layer_leaves_the_outer_prim_pending() {
    let mut store = store();
    store.pending.insert(INNER);
    let child_index = ChildIndex::default();
    let stage = compose(&mut store, &child_index);
    let d = store.inner.path("/World/D");
    assert_eq!(
        stage.pending_prims(),
        [PendingPrim {
            prim: d,
            layers: vec![INNER],
        }]
    );
    assert!(!stage.has_prim(d));
    assert!(stage.has_prim(store.inner.path("/World/A/Child")));
    assert_complete_once_loaded(&mut store, &child_index);
}

/// A layer of the stage's own layer stack still loading leaves the whole
/// stage pending: it holds no prim, and lists the pseudo-root with the
/// layer.
///
/// Spec: AOUSD Core §9 (layer stacks), §10.3.1 (sublayers).
#[test]
fn a_pending_layer_of_the_stage_stack_leaves_the_stage_pending() {
    for layer in [ROOT_SUBLAYER, ROOT] {
        let mut store = store();
        store.pending.insert(layer);
        let child_index = ChildIndex::default();
        let stage = compose(&mut store, &child_index);
        let root = store.inner.path("/");
        assert_eq!(
            stage.pending_prims(),
            [PendingPrim {
                prim: root,
                layers: vec![layer],
            }]
        );
        assert_eq!(stage.prim_paths().count(), 0);
        assert_complete_once_loaded(&mut store, &child_index);
    }
}

/// A store with no pending layer composes what it composes without the
/// question: a layer it does not hold is missing, and contributes nothing.
#[test]
fn a_missing_layer_is_not_pending() {
    let mut store = store();
    store.inner.layers.remove(&ASSET);
    let stage = compose(&mut store, &ChildIndex::default());
    assert!(stage.pending_prims().is_empty());
    assert!(stage.has_prim(store.inner.path("/World/A")));
}

/// A live stage composed while layers load lists the same pending prims as
/// a stage, whether its own layer stack or an arc's is incomplete.
#[test]
fn a_live_stage_lists_pending_prims() {
    for (layer, prim) in [(ASSET, "/World/A"), (ROOT_SUBLAYER, "/")] {
        let mut store = store();
        store.pending.insert(layer);
        let live = crate::LiveStage::compose(&mut store, ROOT, StageOptions::default());
        let prim = store.inner.path(prim);
        assert_eq!(live.stage().pending_prims()[0].prim, prim);
        assert_eq!(live.stage().pending_prims()[0].layers, [layer]);
    }
}

/// Instances whose prototype reads a pending layer beneath the instance
/// root each list the prim: the second does not copy the first's
/// descendants, which are incomplete.
///
/// Spec: AOUSD Core §11.3.3 (instances share prototypes).
#[test]
fn instances_with_a_pending_descendant_each_list_it() {
    let mut store = store();
    let asset = store.inner.path("/Asset");
    let inner = store.inner.path("/Inner");
    let mut layer = Layer::new(ROOT);
    for (path, spec) in [
        ("/World", PrimSpec::def()),
        (
            "/World/I1",
            PrimSpec::def()
                .with_reference(Reference::new(ASSET, asset))
                .with_instanceable(true),
        ),
        (
            "/World/I2",
            PrimSpec::def()
                .with_reference(Reference::new(ASSET, asset))
                .with_instanceable(true),
        ),
    ] {
        let path = store.inner.path(path);
        layer.insert_prim(path, spec);
    }
    store.inner.insert_layer(layer);
    let child = store.inner.path("/Asset/Child");
    let asset_layer = store.inner.layer_mut(ASSET).expect("the asset layer");
    asset_layer.insert_prim(
        child,
        PrimSpec::def().with_reference(Reference::new(INNER, inner)),
    );
    store.pending.insert(INNER);
    let child_index = ChildIndex::default();
    let stage = compose(&mut store, &child_index);
    let pending: Vec<PathId> = stage
        .pending_prims()
        .iter()
        .map(|pending| pending.prim)
        .collect();
    assert_eq!(
        pending,
        [
            store.inner.path("/World/I1/Child"),
            store.inner.path("/World/I2/Child"),
        ]
    );
    assert!(stage.has_prim(store.inner.path("/World/I2")));
    assert_complete_once_loaded(&mut store, &child_index);
}
