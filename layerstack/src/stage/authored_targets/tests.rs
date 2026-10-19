// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{string::String, vec, vec::Vec};

use super::*;
use crate::{
    InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertySpec, Reference, StageOptions,
    SublayerEntry,
};

const ROOT: LayerId = LayerId(1);
const STRONG: LayerId = LayerId(2);
const WEAK: LayerId = LayerId(3);
const ASSET: LayerId = LayerId(4);

fn prim(store: &mut InMemoryStore, path: &str) -> TargetPath {
    TargetPath::Prim(store.path(path))
}

fn rel(targets: ListOp<TargetPath>) -> PropertySpec {
    PropertySpec::relationship().with_targets(targets)
}

/// Each authored path with its spec, list and read, as text.
fn shown(store: &InMemoryStore, walk: &AuthoredTargets) -> Vec<(LayerId, String, String)> {
    let (paths, tokens) = (&store.paths, &store.tokens);
    walk.targets()
        .map(|target| {
            let read = match target.read {
                TargetRead::Mapped(path) => path.display(paths, tokens),
                TargetRead::Dropped(reason) => alloc::format!("{reason:?}"),
                TargetRead::Replaced => "Replaced".into(),
            };
            (
                target.layer,
                alloc::format!(
                    "{} as {}",
                    target.spec.display(paths, tokens),
                    target.source.display(paths, tokens)
                ),
                alloc::format!(
                    "{} {} -> {read}",
                    target.position.as_str(),
                    target.authored.display(paths, tokens)
                ),
            )
        })
        .collect()
}

/// Sublayers author in the stage's namespace: each spec's paths read as
/// authored, by layer in strength order, and a deleted path is not
/// walked.
#[test]
fn sublayers_read_as_authored() {
    let mut store = InMemoryStore::default();
    let shade = store.tokens.intern("shade");
    let (rock, gone, moss) = (
        prim(&mut store, "/Rock"),
        prim(&mut store, "/Gone"),
        prim(&mut store, "/Moss"),
    );
    let spot = store.path("/Spot");
    let mut weak = Layer::new(WEAK);
    weak.insert_prim(store.path("/Rock"), PrimSpec::def());
    weak.insert_prim(
        spot,
        PrimSpec::def().with_property(
            shade,
            rel(ListOp::appended(vec![gone, rock]).with_deleted(vec![moss])),
        ),
    );
    let mut strong = Layer::new(STRONG);
    strong.insert_prim(
        spot,
        PrimSpec::over().with_property(
            shade,
            rel(ListOp::prepended(vec![moss]).with_deleted(vec![gone])),
        ),
    );
    let mut root = Layer::new(ROOT);
    root.sublayers_mut()
        .extend([SublayerEntry::new(STRONG), SublayerEntry::new(WEAK)]);
    for layer in [root, strong, weak] {
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    let walk = stage.authored_targets(&store);
    let layers: Vec<(LayerId, usize)> = walk.layers.iter().map(|l| (l.layer, l.specs)).collect();
    assert_eq!(layers, vec![(ROOT, 0), (STRONG, 1), (WEAK, 1)]);
    assert!(walk.layers.iter().all(|layer| layer.unread.is_empty()));
    let on = |layer| (layer, "/Spot.shade as /Spot.shade".into());
    let expected: Vec<(LayerId, String, String)> = [
        (on(STRONG), "prepended /Moss -> /Moss"),
        (on(WEAK), "appended /Gone -> /Gone"),
        (on(WEAK), "appended /Rock -> /Rock"),
    ]
    .into_iter()
    .map(|((layer, spec), read)| (layer, spec, read.into()))
    .collect();
    assert_eq!(shown(&store, &walk), expected);
    assert!(walk.targets().all(|t| t.node == NodeId::ROOT));
}

/// A referenced layer's paths are in its own namespace: each prim that
/// references it reads them mapped to its own namespace, a path outside
/// the referenced prim is dropped, and a stronger explicit list replaces
/// the referenced opinion. OpenUSD 26.08 gives the same `GetTargets` and
/// warns of `</Outside>` once per referencing prim (checked through
/// `fixtures/layer_dangling_targets` in the conformance crate).
#[test]
fn references_map_each_read() {
    let mut store = InMemoryStore::default();
    let shade = store.tokens.intern("shade");
    let (asset_gone, outside, b_part) = (
        prim(&mut store, "/Asset/Gone"),
        prim(&mut store, "/Outside"),
        prim(&mut store, "/World/B/Part"),
    );
    let asset_prim = store.path("/Asset");
    let mut asset = Layer::new(ASSET);
    asset.insert_prim(
        asset_prim,
        PrimSpec::def().with_property(shade, rel(ListOp::explicit(vec![asset_gone, outside]))),
    );
    asset.insert_prim(store.path("/Asset/Part"), PrimSpec::def());
    // A prim spec no reference brings in.
    asset.insert_prim(
        store.path("/Unused"),
        PrimSpec::def().with_property(shade, rel(ListOp::appended(vec![outside]))),
    );
    let mut root = Layer::new(ROOT);
    root.insert_prim(store.path("/World"), PrimSpec::def());
    root.insert_prim(
        store.path("/World/A"),
        PrimSpec::def().with_reference(Reference::new(ASSET, asset_prim)),
    );
    root.insert_prim(
        store.path("/World/B"),
        PrimSpec::def()
            .with_reference(Reference::new(ASSET, asset_prim))
            .with_property(shade, rel(ListOp::explicit(vec![b_part]))),
    );
    // Beneath an inactive prim, which the stage leaves out.
    root.insert_prim(store.path("/Retired"), PrimSpec::def().with_active(false));
    root.insert_prim(
        store.path("/Retired/Rock"),
        PrimSpec::def().with_property(shade, rel(ListOp::appended(vec![outside]))),
    );
    for layer in [root, asset] {
        store.insert_layer(layer);
    }
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    let walk = stage.authored_targets(&store);
    let layers: Vec<(LayerId, usize)> = walk.layers.iter().map(|l| (l.layer, l.specs)).collect();
    assert_eq!(layers, vec![(ROOT, 2), (ASSET, 2)]);
    let expected: Vec<(LayerId, String, String)> = [
        (
            ROOT,
            "/World/B.shade as /World/B.shade",
            "explicit /World/B/Part -> /World/B/Part",
        ),
        (
            ASSET,
            "/Asset.shade as /World/A.shade",
            "explicit /Asset/Gone -> /World/A/Gone",
        ),
        (
            ASSET,
            "/Asset.shade as /World/A.shade",
            "explicit /Outside -> OutsideArc(References)",
        ),
        (
            ASSET,
            "/Asset.shade as /World/B.shade",
            "explicit /Asset/Gone -> Replaced",
        ),
        (
            ASSET,
            "/Asset.shade as /World/B.shade",
            "explicit /Outside -> Replaced",
        ),
    ]
    .into_iter()
    .map(|(layer, spec, read)| (layer, spec.into(), read.into()))
    .collect();
    assert_eq!(shown(&store, &walk), expected);
    let reference = walk.targets().nth(1).map(|t| t.node).expect("a read");
    assert_eq!(
        stage
            .explain_prim_graph(store.path("/World/A"))
            .and_then(|graph| graph.node(reference))
            .map(|node| node.arc_kind()),
        Some(ArcKind::References)
    );
    let unread: Vec<(LayerId, String, usize, UnreadReason)> = walk
        .layers
        .iter()
        .flat_map(|layer| &layer.unread)
        .map(|spec| {
            (
                spec.layer,
                spec.spec.display(&store.paths, &store.tokens),
                spec.targets,
                spec.reason,
            )
        })
        .collect();
    assert_eq!(
        unread,
        vec![
            (
                ROOT,
                "/Retired/Rock.shade".into(),
                1,
                UnreadReason::PrimNotComposed(DanglingKind::Inactive)
            ),
            (ASSET, "/Unused.shade".into(), 1, UnreadReason::NotReached),
        ]
    );
}

/// Pairing keeps authored order, and takes a dropped path from the
/// composition errors only while the list has more paths than the mapped
/// one.
#[test]
fn pairing_skips_only_dropped_paths() {
    let mut store = InMemoryStore::default();
    let (a, b, c, out) = (
        prim(&mut store, "/A"),
        prim(&mut store, "/B"),
        prim(&mut store, "/C"),
        prim(&mut store, "/Out"),
    );
    let (ma, mc) = (prim(&mut store, "/W/A"), prim(&mut store, "/W/C"));
    let drop = DropReason::OutsideArc(ArcKind::References);
    assert_eq!(
        pair(&[a, b, c], &[ma, mc], &[(b, drop)], || ArcKind::References),
        vec![
            TargetRead::Mapped(ma),
            TargetRead::Dropped(drop),
            TargetRead::Mapped(mc)
        ]
    );
    // The same path authored where it maps elsewhere is not taken for
    // the dropped one once the lists are even.
    assert_eq!(
        pair(&[out, a], &[ma], &[(out, drop)], || ArcKind::References),
        vec![TargetRead::Dropped(drop), TargetRead::Mapped(ma)]
    );
    assert_eq!(
        pair(&[a, out], &[ma], &[], || ArcKind::Payloads),
        vec![
            TargetRead::Mapped(ma),
            TargetRead::Dropped(DropReason::OutsideArc(ArcKind::Payloads))
        ]
    );
}

/// Ordering by first read gives the same order placed or sorted.
#[test]
fn first_reads_order_either_way() {
    let firsts = [7_u32, 2, 9, 0, 4];
    assert_eq!(by_first(firsts.into_iter(), 10), vec![3, 1, 4, 0, 2]);
    assert_eq!(by_first(firsts.into_iter(), 1_000), vec![3, 1, 4, 0, 2]);
    assert!(by_first(core::iter::empty(), 0).is_empty());
}
