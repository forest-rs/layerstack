// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sublayers authored by asset path, call by call, against usd-core 26.11.
//!
//! An anonymous session layer sublayers an absolute path, a path relative
//! to no layer and a missing path; the stage opens with it, then a
//! sublayer relative to the root layer is inserted on the root, one
//! relative to a referenced layer is appended to that layer, and two are
//! removed. OpenUSD opens each sublayer as it computes the layer stack
//! (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`) and
//! reports one it cannot open as `PcpErrorInvalidSublayerPath`; after each
//! step, layerstack composes the same prims, layer stack, values and
//! errors. AOUSD Core §9.4 (relative asset paths), §10.3.1 (sublayers),
//! §10.6.
#![cfg(not(target_arch = "wasm32"))]

use crate::support::scratch::scratch_dir;
use std::path::Path;
use std::process::Command;

use layerstack::{
    CompositionError, InMemoryStore, Layer, LayerId, LayerOffset, StageOptions, SublayerEntry,
    Time, Transaction,
};
use layerstack_io::{Filesystem, OpenOptions, StageDocument};

/// The files, by path relative to the scratch directory.
const FILES: [(&str, &str); 7] = [
    (
        "scene/root.usda",
        "#usda 1.0\ndef \"Root\" {\n    int value = 1\n}\ndef \"Model\" (prepend references = @./model.usda@</Asset>) {}\n",
    ),
    ("scene/model.usda", "#usda 1.0\ndef \"Asset\" {}\n"),
    (
        "scene/parts/rootsub.usda",
        "#usda 1.0\ndef \"FromRootSub\" {}\n",
    ),
    (
        "scene/parts/modelsub.usda",
        "#usda 1.0\nover \"Asset\" {\n    int extra = 3\n}\n",
    ),
    (
        "sub.usda",
        "#usda 1.0\ndef \"FromSub\" {}\nover \"Root\" {\n    int value = 2\n}\n",
    ),
    (
        "rel.usda",
        "#usda 1.0\n(\n    subLayers = [@./nested.usda@]\n)\ndef \"FromRelative\" {}\n",
    ),
    ("nested.usda", "#usda 1.0\ndef \"FromNested\" {}\n"),
];

/// The same steps in OpenUSD, run in the scratch directory, which a path
/// relative to the anonymous session layer resolves against. Prints one
/// line per step, as [`step`] does.
const ORACLE: &str = r#"
import os, sys
from pxr import Sdf, Usd

def name(layer):
    return os.path.basename(layer.realPath) if layer.realPath else "session"

def report(stage):
    prims = ",".join(sorted(str(p.GetPath()) for p in stage.Traverse()))
    stack = ",".join(name(l) for l in stage.GetLayerStack(includeSessionLayers=True))
    def value(path):
        attribute = stage.GetAttributeAtPath(path)
        v = attribute.Get() if attribute else None
        return "none" if v is None else str(v)
    missing = []
    for error in stage.GetCompositionErrors():
        if str(error.errorType) != "Pcp.ErrorType_InvalidSublayerPath":
            missing.append("other:" + str(error))
            continue
        text = str(error)
        asset = text.split("@")[1]
        layer = text.split("@")[3]
        layer = "session" if layer.startswith("anon:") else os.path.basename(layer)
        missing.append(f"{layer}:{asset}")
    print(f"prims={prims};stack={stack};value={value('/Root.value')};extra={value('/Model.extra')};missing={','.join(missing)}")

dir = os.getcwd()
session = Sdf.Layer.CreateAnonymous("session")
session.subLayerPaths = [os.path.join(dir, "sub.usda"), "rel.usda", "missing.usda"]
stage = Usd.Stage.Open(Sdf.Layer.FindOrOpen(os.path.join(dir, "scene/root.usda")), session)
report(stage)
stage.GetRootLayer().subLayerPaths.insert(0, "./parts/rootsub.usda")
report(stage)
Sdf.Layer.Find(os.path.join(dir, "scene/model.usda")).subLayerPaths.append("./parts/modelsub.usda")
report(stage)
session.subLayerPaths.remove("missing.usda")
report(stage)
stage.GetRootLayer().subLayerPaths.remove("./parts/rootsub.usda")
report(stage)
"#;

const SESSION: LayerId = LayerId(7);

fn base_name(identifier: &str) -> String {
    Path::new(identifier)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The stage after a step, in the oracle's words.
fn step(doc: &StageDocument<Filesystem>) -> String {
    let name = |layer: LayerId| doc.identifier(layer).map_or("session".into(), base_name);
    let view = doc.view();
    let mut prims: Vec<String> = view
        .traverse()
        .map(|prim| format!("/{}", prim.name()))
        .collect();
    prims.sort();
    let stage = doc.stage().stage();
    let stack: Vec<String> = stage
        .layer_stack()
        .iter()
        .map(|&layer| name(layer))
        .collect();
    let value = |path: &str| {
        view.attribute(path)
            .and_then(|a| a.value::<i32>(Time::Default))
            .map_or("none".into(), |v| v.to_string())
    };
    let missing: Vec<String> = stage
        .composition_errors()
        .iter()
        .map(|error| match error {
            CompositionError::UnresolvedSublayer(error) => {
                format!("{}:{}", name(error.layer), error.asset_path)
            }
            other => format!("other:{other:?}"),
        })
        .collect();
    format!(
        "prims={};stack={};value={};extra={};missing={}",
        prims.join(","),
        stack.join(","),
        value("/Root.value"),
        value("/Model.extra"),
        missing.join(",")
    )
}

fn by_path(asset: &str) -> SublayerEntry {
    SublayerEntry::unresolved(asset, LayerOffset::IDENTITY)
}

/// layerstack's lines for the oracle's steps.
fn layerstack_steps(dir: &Path) -> Vec<String> {
    let mut store = InMemoryStore::default();
    let mut session = Layer::new(SESSION);
    let sub = dir.join("sub.usda");
    session.sublayers_mut().extend([
        by_path(sub.to_str().unwrap()),
        by_path("rel.usda"),
        by_path("missing.usda"),
    ]);
    store.insert_layer(session);
    let options = StageOptions {
        session_layer: Some(SESSION),
        ..StageOptions::default()
    };
    let storage = Filesystem::new(dir, []).unwrap();
    let mut doc = StageDocument::open(
        storage,
        "scene/root.usda",
        OpenOptions {
            stage: options,
            store: Some(store),
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let mut steps = vec![step(&doc)];

    let root = doc.root_layer();
    let mut edit = Transaction::new();
    edit.insert_sublayer(root, 0, by_path("./parts/rootsub.usda"));
    doc.apply(&edit).unwrap();
    steps.push(step(&doc));

    let model = doc
        .store()
        .layer_ids()
        .into_iter()
        .find(|&layer| {
            doc.identifier(layer)
                .is_some_and(|id| id.ends_with("model.usda"))
        })
        .unwrap();
    let mut edit = Transaction::new();
    edit.insert_sublayer(model, 0, by_path("./parts/modelsub.usda"));
    doc.apply(&edit).unwrap();
    steps.push(step(&doc));

    let mut edit = Transaction::new();
    edit.remove_sublayer(SESSION, 2);
    doc.apply(&edit).unwrap();
    steps.push(step(&doc));

    let mut edit = Transaction::new();
    edit.remove_sublayer(root, 0);
    doc.apply(&edit).unwrap();
    steps.push(step(&doc));
    steps
}

/// Each step composes what usd-core 26.11 composes for it.
#[test]
fn authored_sublayers_load_as_openusd_loads_them() {
    let dir = scratch_dir("authored_sublayers");
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in FILES {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let ours = layerstack_steps(&dir.canonicalize().unwrap());
    let opened = "prims=/FromNested,/FromRelative,/FromSub,/Model,/Root;\
                  stack=session,sub.usda,rel.usda,nested.usda,root.usda;\
                  value=2;extra=none;missing=session:missing.usda";
    assert_eq!(ours[0], opened);
    assert_eq!(
        ours[2],
        "prims=/FromNested,/FromRelative,/FromRootSub,/FromSub,/Model,/Root;\
         stack=session,sub.usda,rel.usda,nested.usda,root.usda,rootsub.usda;\
         value=2;extra=3;missing=session:missing.usda"
    );

    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    if !available {
        eprintln!("skipped authored sublayer oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let output = Command::new(python)
        .args(["-c", ORACLE])
        .current_dir(dir.canonicalize().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let theirs = String::from_utf8(output.stdout).unwrap();
    let theirs: Vec<&str> = theirs.lines().collect();
    assert_eq!(ours, theirs);
}
