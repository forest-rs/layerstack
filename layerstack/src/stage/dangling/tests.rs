// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{vec, vec::Vec};

use super::*;
use crate::{
    InMemoryStore, Layer, LayerId, ListOp, PayloadLoadRules, PopulationMask, PrimSpec,
    PropertySpec, Reference, StageOptions,
};

const ROOT: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);

/// The kinds, as `UsdStage::GetObjectAtPath` with `IsActive` and
/// `IsLoaded` tell them apart in OpenUSD 26.08 (checked against the C++
/// library through `fixtures/dangling_targets` in the conformance crate).
#[test]
fn each_kind_of_target() {
    let mut store = InMemoryStore::default();
    let size = store.tokens.intern("size");
    let grain = store.tokens.intern("grain");
    let mut asset = Layer::new(ASSET);
    let quarry = store.path("/Quarry");
    asset.insert_prim(
        quarry,
        PrimSpec::def().with_property(grain, PropertySpec::attribute()),
    );
    asset.insert_prim(store.path("/Quarry/Stone"), PrimSpec::def());
    let fern = store.path("/Fern");
    asset.insert_prim(fern, PrimSpec::def());
    asset.insert_prim(
        store.path("/Fern/Frond"),
        PrimSpec::def().with_property(size, PropertySpec::attribute()),
    );
    store.insert_layer(asset);

    let mut root = Layer::new(ROOT);
    root.insert_prim(store.path("/Terrain"), PrimSpec::def());
    root.insert_prim(
        store.path("/Terrain/Rock"),
        PrimSpec::def().with_property(size, PropertySpec::attribute()),
    );
    root.insert_prim(
        store.path("/Terrain/Off"),
        PrimSpec::def()
            .with_active(false)
            .with_property(size, PropertySpec::attribute()),
    );
    root.insert_prim(store.path("/Terrain/Off/Pebble"), PrimSpec::def());
    let terrain_quarry = store.path("/Terrain/Quarry");
    root.insert_prim(
        terrain_quarry,
        PrimSpec::def().with_payload(Reference::new(ASSET, quarry)),
    );
    root.insert_prim(store.path("/Terrain/Quarry/Local"), PrimSpec::def());
    root.insert_prim(store.path("/Plants"), PrimSpec::def());
    root.insert_prim(
        store.path("/Plants/Fern"),
        PrimSpec::def()
            .with_instanceable(true)
            .with_reference(Reference::new(ASSET, fern)),
    );
    let cases: [(&str, &str, Option<DanglingKind>); 13] = [
        ("rock", "/Terrain/Rock", None),
        ("rock_size", "/Terrain/Rock.size", None),
        ("gone", "/Terrain/Gone", Some(DanglingKind::NoPrim)),
        ("gone_deep", "/Gone/Deeper", Some(DanglingKind::NoPrim)),
        (
            "rock_gone",
            "/Terrain/Rock.gone",
            Some(DanglingKind::NoProperty),
        ),
        ("off", "/Terrain/Off", Some(DanglingKind::Inactive)),
        (
            "off_size",
            "/Terrain/Off.size",
            Some(DanglingKind::Inactive),
        ),
        (
            "pebble",
            "/Terrain/Off/Pebble",
            Some(DanglingKind::Inactive),
        ),
        (
            "stone",
            "/Terrain/Quarry/Stone",
            Some(DanglingKind::Unloaded),
        ),
        (
            "grain",
            "/Terrain/Quarry.grain",
            Some(DanglingKind::Unloaded),
        ),
        ("local", "/Terrain/Quarry/Local", None),
        ("frond", "/Plants/Fern/Frond.size", None),
        (
            "frond_gone",
            "/Plants/Fern/Gone",
            Some(DanglingKind::NoPrim),
        ),
    ];
    let source = store.path("/Source");
    let mut spec = PrimSpec::def();
    let mut expected = Vec::new();
    for (name, target, kind) in cases {
        let name = store.tokens.intern(name);
        let target = if target.contains('.') {
            TargetPath::Property(store.property_path(target))
        } else {
            TargetPath::Prim(store.path(target))
        };
        spec = spec.with_property(
            name,
            PropertySpec::relationship().with_targets(ListOp::explicit(vec![target])),
        );
        expected.push((name, target, kind));
    }
    let input = store.tokens.intern("input");
    let missing_output = TargetPath::Property(store.property_path("/Terrain/Gone.output"));
    spec = spec.with_property(
        input,
        PropertySpec::attribute().with_targets(ListOp::explicit(vec![
            missing_output,
            TargetPath::Property(store.property_path("/Terrain/Rock.size")),
        ])),
    );
    root.insert_prim(source, spec);
    store.insert_layer(root);

    let mut load_rules = PayloadLoadRules::default();
    load_rules.unload(store.paths().resolve(terrain_quarry).clone());
    let options = StageOptions {
        load_rules,
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, ROOT, options);
    for (_, target, kind) in &expected {
        assert_eq!(
            stage.dangling_kind(*target, store.paths()),
            *kind,
            "{}",
            target.display(store.paths(), store.tokens())
        );
    }

    let report = stage.dangling_targets(&store, 1);
    assert_eq!(report.relationships.len(), expected.len());
    for (name, target, kind) in &expected {
        let group = report.relationship(*name).expect("a group per name");
        assert_eq!((group.properties, group.targets), (1, 1));
        assert_eq!(group.dangling.total(), usize::from(kind.is_some()));
        let examples: Vec<_> = group.examples.iter().map(|e| (e.target, e.kind)).collect();
        assert_eq!(
            examples,
            kind.map(|kind| (*target, kind))
                .into_iter()
                .collect::<Vec<_>>()
        );
        if let Some(kind) = kind {
            assert_eq!(group.dangling.of(*kind), 1);
            assert_eq!(group.examples[0].source, PropertyPath::new(source, *name));
        }
    }
    let connections = report.connection(input).expect("the connection");
    assert_eq!((connections.targets, connections.dangling.no_prim), (2, 1));
    assert_eq!(report.connections.len(), 1);
    assert_eq!(
        report.total(),
        DanglingCounts {
            no_prim: 4,
            no_property: 1,
            inactive: 3,
            unloaded: 2,
            masked: 0,
        }
    );
    assert!(!report.is_clean());
    // Names in property-name order.
    let names: Vec<&str> = report
        .relationships
        .iter()
        .map(|group| store.tokens().resolve(group.name))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_by(|a, b| crate::stage::dictionary_cmp(a, b));
    assert_eq!(names, sorted);

    // A population mask leaves `/Terrain` out: what is there is masked,
    // what is not is still missing beneath the pseudo-root.
    let options = StageOptions {
        mask: Some(PopulationMask {
            include: vec![source],
        }),
        ..StageOptions::default()
    };
    let masked = Stage::compose(&mut store, ROOT, options);
    let rock = TargetPath::Prim(store.path("/Terrain/Rock"));
    assert_eq!(
        masked.dangling_kind(rock, store.paths()),
        Some(DanglingKind::Masked)
    );
    let report = masked.dangling_targets(&store, 0);
    assert!(report.relationships.iter().all(|g| g.examples.is_empty()));
    assert_eq!(report.total().masked, report.targets);
}

/// A stage whose targets all resolve reports each name with nothing
/// dangling, and a property whose list is empty is read but not grouped.
#[test]
fn clean_stage() {
    let mut store = InMemoryStore::default();
    let rock = store.path("/Rock");
    let rests_on = store.tokens.intern("restsOn");
    let empty = store.tokens.intern("empty");
    let mut root = Layer::new(ROOT);
    root.insert_prim(
        rock,
        PrimSpec::def()
            .with_property(
                rests_on,
                PropertySpec::relationship()
                    .with_targets(ListOp::explicit(vec![TargetPath::Prim(rock)])),
            )
            .with_property(empty, PropertySpec::relationship()),
    );
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    let report = stage.dangling_targets(&store, 4);
    assert!(report.is_clean());
    assert_eq!(report.relationships.len(), 1);
    assert_eq!(report.relationships[0].name, rests_on);
    assert_eq!((report.prims, report.properties, report.targets), (2, 2, 1));
}
