// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Builds a namespace with two targets per leaf, one prim and one property.
pub(super) fn fixture(depth: usize) -> (layerstack::InMemoryStore, layerstack::Layer) {
    use layerstack::{
        InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertyPath, PropertySpec, TargetPath,
    };
    let store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let child = store.tokens().intern("Group");
    let mut parent = String::from("/");
    for level in 0..depth {
        let spec = if level == 0 {
            PrimSpec::default()
        } else {
            PrimSpec::def()
        };
        layer.insert_prim(store.path(&parent), spec.with_children(vec![child]));
        if level != 0 {
            parent.push('/');
        }
        parent.push_str("Group");
    }
    let relation = store.tokens().intern("links");
    let leaves: Vec<_> = (0..2048)
        .map(|i| {
            let name = format!("Leaf{i}");
            (
                store.tokens().intern(&name),
                store.path(&format!("{parent}/{name}")),
            )
        })
        .collect();
    layer.insert_prim(
        store.path(&parent),
        PrimSpec::def().with_children(leaves.iter().map(|&(n, _)| n).collect()),
    );
    for (i, &(_, path)) in leaves.iter().enumerate() {
        let next = leaves[(i + 1) % leaves.len()].1;
        let previous = leaves[(i + leaves.len() - 1) % leaves.len()].1;
        let targets = ListOp::explicit(vec![
            TargetPath::Prim(next),
            TargetPath::Property(PropertyPath::new(previous, relation)),
        ]);
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_property(relation, PropertySpec::relationship().with_targets(targets)),
        );
    }
    (store, layer)
}
