// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sublayers authored by asset path, on a host-owned session layer at open
//! and through [`StageDocument::apply`] on any layer of the stage, load
//! when the stage recomposes, as OpenUSD loads them; one that is not found
//! is reported (AOUSD Core §10.3.1, §10.6).
//!
//! Each step is checked against OpenUSD 26.11, which, for the same files,
//! composes the prims, layer stack, values and errors asserted here
//! (`Usd.Stage.Open` with an anonymous session layer, then
//! `subLayerPaths` edits on the session, root and referenced layers).

use super::*;
use layerstack::{
    ArcKind, CompositionError, LayerOffset, SublayerEntry, Time, Transaction, UnresolvedSublayer,
};

const SESSION: LayerId = LayerId(7);

fn files() -> Memory {
    Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"Root\" {\n int value = 1\n}\n\
             def \"Model\" (prepend references = @model.usda@</Asset>) {}\n",
        ),
        ("model.usda", "#usda 1.0\ndef \"Asset\" {}\n"),
        (
            "sub.usda",
            "#usda 1.0\ndef \"FromSub\" {}\nover \"Root\" {\n int value = 2\n int fromSub = 2\n}\n",
        ),
        (
            "rel.usda",
            "#usda 1.0\n(subLayers = [@nested.usda@])\ndef \"FromRelative\" {}\n",
        ),
        ("nested.usda", "#usda 1.0\ndef \"FromNested\" {}\n"),
        (
            "rootsub.usda",
            "#usda 1.0\ndef \"FromRootSub\" {}\nover \"Root\" {\n int value = 3\n}\n",
        ),
        (
            "modelsub.usda",
            "#usda 1.0\nover \"Asset\" {\n int extra = 3\n}\n",
        ),
    ])
}

/// The sublayers the oracle authors on the session layer.
fn session_sublayers() -> Vec<SublayerEntry> {
    ["sub.usda", "rel.usda", "missing.usda"]
        .into_iter()
        .map(|asset| SublayerEntry::unresolved(asset, LayerOffset::IDENTITY))
        .collect()
}

/// Opens `root.usda` with a host-owned session layer authoring `sublayers`.
fn open(sublayers: Vec<SublayerEntry>) -> StageDocument<Memory> {
    let mut store = InMemoryStore::default();
    let mut session = Layer::new(SESSION);
    session.sublayers_mut().extend(sublayers);
    store.insert_layer(session);
    let options = StageOptions {
        session_layer: Some(SESSION),
        ..StageOptions::default()
    };
    StageDocument::open_in(files(), store, "root.usda", options, ImportPolicy::Strict).unwrap()
}

/// What one oracle step prints.
#[derive(Debug, PartialEq)]
struct Composed {
    prims: Vec<String>,
    layer_stack: Vec<String>,
    value: Option<i32>,
    from_sub: Option<i32>,
    extra: Option<i32>,
    /// The asset paths of the sublayers reported as not loaded, by the
    /// identifier of the layer naming each.
    unresolved_sublayers: Vec<(String, String)>,
}

fn layer_name(doc: &StageDocument<Memory>, layer: LayerId) -> String {
    doc.identifier(layer).unwrap_or("session").into()
}

fn composed(doc: &StageDocument<Memory>) -> Composed {
    let view = doc.view();
    let mut prims: Vec<String> = view
        .traverse()
        .map(|prim| format!("/{}", prim.name()))
        .collect();
    prims.sort();
    let value = |path: &str| {
        view.attribute(path)
            .and_then(|a| a.value::<i32>(Time::Default))
    };
    let stage = doc.stage().stage();
    let errors = stage.composition_errors();
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, CompositionError::UnresolvedSublayer(_))),
        "{errors:?}"
    );
    Composed {
        prims,
        layer_stack: stage
            .layer_stack()
            .iter()
            .map(|&layer| layer_name(doc, layer))
            .collect(),
        value: value("/Root.value"),
        from_sub: value("/Root.fromSub"),
        extra: value("/Model.extra"),
        unresolved_sublayers: errors
            .iter()
            .filter_map(|error| match error {
                CompositionError::UnresolvedSublayer(UnresolvedSublayer {
                    layer, asset, ..
                }) => Some((layer_name(doc, *layer), asset.clone())),
                _ => None,
            })
            .collect(),
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| String::from(*name)).collect()
}

/// OpenUSD after the session layer's sublayers load: the missing one is
/// skipped and reported (`PcpErrorInvalidSublayerPath`).
fn with_session_sublayers() -> Composed {
    Composed {
        prims: names(&[
            "/FromNested",
            "/FromRelative",
            "/FromSub",
            "/Model",
            "/Root",
        ]),
        layer_stack: names(&[
            "session",
            "sub.usda",
            "rel.usda",
            "nested.usda",
            "root.usda",
        ]),
        value: Some(2),
        from_sub: Some(2),
        extra: None,
        unresolved_sublayers: vec![("session".into(), "missing.usda".into())],
    }
}

fn layer_named(doc: &StageDocument<Memory>, identifier: &str) -> LayerId {
    doc.store()
        .layer_ids()
        .into_iter()
        .find(|&layer| doc.identifier(layer) == Some(identifier))
        .unwrap()
}

/// A host-owned session layer's sublayers load at open, a missing one is
/// reported, and the failure is kept in the load report.
#[test]
fn session_sublayers_load_at_open() {
    let doc = open(session_sublayers());
    assert_eq!(composed(&doc), with_session_sublayers());
    let failures: Vec<&str> = doc
        .load_report()
        .asset_failures
        .iter()
        .map(|failure| failure.asset.as_str())
        .collect();
    assert_eq!(failures, ["missing.usda"]);
    assert!(doc.load_report().asset_failures[0].is_missing());
}

/// Call by call, as the oracle authors them: sublayers set on the session
/// layer, inserted on the root layer, appended to a referenced layer, and
/// removed, each loaded or dropped when `apply` recomposes.
#[test]
fn sublayers_authored_through_apply_load() {
    let mut doc = open(Vec::new());
    assert_eq!(
        composed(&doc),
        Composed {
            prims: names(&["/Model", "/Root"]),
            layer_stack: names(&["session", "root.usda"]),
            value: Some(1),
            from_sub: None,
            extra: None,
            unresolved_sublayers: Vec::new(),
        }
    );

    let mut edit = Transaction::new();
    edit.set_sublayers(SESSION, session_sublayers());
    let (applied, loaded) = doc.apply(&edit).unwrap();
    let requested: Vec<(&str, ArcKind)> = applied
        .asset_requests
        .iter()
        .map(|request| (request.asset_path.as_str(), request.arc))
        .collect();
    assert_eq!(
        requested,
        [
            ("missing.usda", ArcKind::Local),
            ("rel.usda", ArcKind::Local),
            ("sub.usda", ArcKind::Local),
        ]
    );
    let read: Vec<&str> = loaded
        .layers
        .iter()
        .map(|&layer| doc.identifier(layer).unwrap())
        .collect();
    assert_eq!(read.len(), 3, "{read:?}");
    for identifier in ["sub.usda", "rel.usda", "nested.usda"] {
        assert!(read.contains(&identifier), "{read:?}");
    }
    assert_eq!(loaded.asset_failures.len(), 1);
    assert!(loaded.asset_failures[0].is_missing());
    assert_eq!(composed(&doc), with_session_sublayers());
    assert!(doc.stage().stage().asset_requests().is_empty());

    let root = doc.stage().stage().root_layer().unwrap();
    let mut edit = Transaction::new();
    edit.insert_sublayer(
        root,
        0,
        SublayerEntry::unresolved("rootsub.usda", LayerOffset::IDENTITY),
    );
    let (_, loaded) = doc.apply(&edit).unwrap();
    assert_eq!(loaded.layers.len(), 1);
    let mut expected = with_session_sublayers();
    expected.prims.insert(2, "/FromRootSub".into());
    expected.layer_stack.push("rootsub.usda".into());
    assert_eq!(composed(&doc), expected);

    let model = layer_named(&doc, "model.usda");
    let mut edit = Transaction::new();
    edit.insert_sublayer(
        model,
        0,
        SublayerEntry::unresolved("modelsub.usda", LayerOffset::IDENTITY),
    );
    let (applied, loaded) = doc.apply(&edit).unwrap();
    assert_eq!(applied.asset_requests.len(), 1);
    assert_eq!(applied.asset_requests[0].arc, ArcKind::Local);
    assert_eq!(
        doc.view()
            .prim(applied.asset_requests[0].prim)
            .map(|prim| prim.name()),
        Some("Model")
    );
    assert_eq!(loaded.layers.len(), 1);
    expected.extra = Some(3);
    assert_eq!(composed(&doc), expected);

    let mut edit = Transaction::new();
    edit.remove_sublayer(SESSION, 2);
    let (applied, loaded) = doc.apply(&edit).unwrap();
    assert!(applied.asset_requests.is_empty());
    assert!(loaded.layers.is_empty());
    expected.unresolved_sublayers.clear();
    assert_eq!(composed(&doc), expected);

    let mut edit = Transaction::new();
    edit.remove_sublayer(root, 0);
    doc.apply(&edit).unwrap();
    expected.prims.remove(2);
    expected.layer_stack.pop();
    assert_eq!(composed(&doc), expected);
}

/// A sublayer that is not found is asked for once: composing again reads
/// nothing, and the stage keeps reporting it.
#[test]
fn a_missing_authored_sublayer_is_read_once() {
    let mut doc = open(Vec::new());
    let mut edit = Transaction::new();
    edit.set_sublayers(
        SESSION,
        vec![SublayerEntry::unresolved(
            "missing.usda",
            LayerOffset::IDENTITY,
        )],
    );
    doc.apply(&edit).unwrap();
    let reads = doc.storage().reads.len();
    let requested = doc.load_requested_assets().unwrap();
    assert!(requested.layers.is_empty() && requested.asset_failures.is_empty());
    assert_eq!(doc.storage().reads.len(), reads);
    assert_eq!(
        composed(&doc).unresolved_sublayers,
        [("session".into(), "missing.usda".into())]
    );
}

/// A new stage loads its host-owned session layer's sublayers too, as
/// `UsdStage::CreateNew` does, and reports the one that is not found.
#[test]
fn session_sublayers_load_when_creating() {
    let mut store = InMemoryStore::default();
    let mut session = Layer::new(SESSION);
    session.sublayers_mut().extend(
        ["sub.usda", "missing.usda"]
            .into_iter()
            .map(|asset| SublayerEntry::unresolved(asset, LayerOffset::IDENTITY)),
    );
    store.insert_layer(session);
    let options = StageOptions {
        session_layer: Some(SESSION),
        ..StageOptions::default()
    };
    let doc = StageDocument::create_in(files(), store, "created.usda", options).unwrap();
    let composed = composed(&doc);
    assert_eq!(composed.prims, ["/FromSub"]);
    assert_eq!(
        composed.layer_stack,
        ["session", "sub.usda", "created.usda"]
    );
    assert_eq!(
        composed.unresolved_sublayers,
        [("session".into(), "missing.usda".into())]
    );
    assert_eq!(doc.load_report().asset_failures.len(), 1);
    assert!(doc.is_dirty(doc.stage().stage().root_layer().unwrap()));
}
