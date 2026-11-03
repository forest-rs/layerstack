// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A test helper: whether a live stage holds what a fresh live
//! composition of its layers holds.

use alloc::{format, vec::Vec};

use hashbrown::{HashMap, HashSet};

use super::{LiveStage, OPINION_EDIT};
use crate::{InMemoryStore, LayerId, Path, PathId, Stage};

/// The live stage holds what a fresh live composition of its layers
/// holds: every prim index node for node and record for record (layer
/// strengths and offsets included), child lists, instancing, errors,
/// asset requests and layers used, and the dependency and source
/// indexes it recomposes by.
pub(crate) fn assert_as_fresh(live: &LiveStage, store: &mut InMemoryStore, context: &str) {
    let fresh = LiveStage::compose(store, live.root, live.options.clone());
    let (a, b) = (live.stage(), fresh.stage());
    let root = store.paths().intern(Path::root());
    let order = |stage: &Stage| {
        stage
            .traverse_all(root)
            .map(|prim| store.paths().display(prim, store.tokens()))
            .collect::<Vec<_>>()
    };
    assert_eq!(order(a), order(b), "{context}: prims");
    let mut prims: Vec<PathId> = b.prim_paths().collect();
    let mut held: Vec<PathId> = a.prim_paths().collect();
    prims.sort_unstable();
    held.sort_unstable();
    assert_eq!(held, prims, "{context}: prim indexes");
    for prim in prims {
        let (x, y) = (
            a.index_of(prim).expect("held"),
            b.index_of(prim).expect("held"),
        );
        let at = format!("{context}: {}", store.paths().display(prim, store.tokens()));
        assert!(
            x.data.same_records(&y.data),
            "{at}: records {:?} vs {:?}",
            x.sources,
            y.sources
        );
        assert_eq!(x.graph.len(), y.graph.len(), "{at}: nodes");
        for ((_, m), (_, n)) in x.graph.nodes().zip(y.graph.nodes()) {
            assert_eq!(m.arc_kind(), n.arc_kind(), "{at}");
            assert_eq!(m.parent(), n.parent(), "{at}");
            assert_eq!(m.origin(), n.origin(), "{at}");
            assert_eq!(
                m.layer_stack_identifier(),
                n.layer_stack_identifier(),
                "{at}"
            );
            assert_eq!(m.site(), n.site(), "{at}");
            assert_eq!(m.namespace_depth(), n.namespace_depth(), "{at}");
            assert_eq!(m.sibling_index(), n.sibling_index(), "{at}");
            assert_eq!(m.is_implied(), n.is_implied(), "{at}");
            let layers = |node: crate::PrimNode<'_>| {
                node.layers()
                    .map(|layer| (layer, node.layer_offset_for_layer(layer)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(layers(m), layers(n), "{at}: layers");
        }
        assert_eq!(x.type_name, y.type_name, "{at}");
        assert_eq!(x.type_info.is_some(), y.type_info.is_some(), "{at}");
        assert_eq!(x.dynamic.is_some(), y.dynamic.is_some(), "{at}");
        assert_eq!(a.all_children_of(prim), b.all_children_of(prim), "{at}");
        assert_eq!(
            a.children_of(prim),
            b.children_of(prim),
            "{at}: active children"
        );
        assert_eq!(a.is_active(prim), b.is_active(prim), "{at}: active");
        assert_eq!(a.is_instance(prim), b.is_instance(prim), "{at}");
        assert_eq!(
            a.instance_prototype(prim),
            b.instance_prototype(prim),
            "{at}"
        );
        assert_eq!(a.prim_stack(prim), b.prim_stack(prim), "{at}");
    }
    assert_eq!(a.composition_errors(), b.composition_errors(), "{context}");
    assert_eq!(a.asset_requests(), b.asset_requests(), "{context}");
    assert_eq!(a.used_layers(true), b.used_layers(true), "{context}");
    assert_eq!(a.layer_stack(), b.layer_stack(), "{context}");
    assert_eq!(
        a.prototypes().count(),
        b.prototypes().count(),
        "{context}: prototypes"
    );
    let edges = |live: &LiveStage| {
        let graph = live.tracker.graph();
        let mut edges: Vec<(PathId, PathId)> = graph
            .keys()
            .flat_map(|key| {
                graph
                    .dependencies(key, OPINION_EDIT)
                    .map(move |source| (key, source))
            })
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges
    };
    assert_eq!(edges(live), edges(&fresh), "{context}: arc edges");
    let non_empty = |map: &HashMap<LayerId, HashSet<PathId>>| {
        map.iter()
            .filter(|(_, prims)| !prims.is_empty())
            .map(|(layer, prims)| (*layer, prims.clone()))
            .collect::<HashMap<_, _>>()
    };
    assert_eq!(
        non_empty(&live.layer_to_prims),
        non_empty(&fresh.layer_to_prims),
        "{context}: layer → prims"
    );
    assert_eq!(
        live.prim_to_layers, fresh.prim_to_layers,
        "{context}: prim → layers"
    );
    assert_eq!(
        non_empty(&live.default_prim_dependents),
        non_empty(&fresh.default_prim_dependents),
        "{context}"
    );
    assert_eq!(
        live.generations, fresh.generations,
        "{context}: generations"
    );
    let sources = |live: &LiveStage| {
        let mut found: Vec<(PathId, Vec<(LayerId, PathId)>)> = live
            .stage
            .prim_paths()
            .map(|prim| (prim, live.stage.source_sites(prim)))
            .collect();
        found.sort_unstable();
        found
    };
    assert_eq!(sources(live), sources(&fresh), "{context}: source sites");
}
