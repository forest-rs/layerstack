// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prototype grouping and occurrence target mapping against OpenUSD.
//! AOUSD Core §11.4; OpenUSD `PcpInstanceKey`.

use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::json;
use std::process::Command;

#[test]
fn native_prototypes_preserve_occurrence_targets() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/flatten/instance_targets/root.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty());
    let mut store = loaded.store;
    let stage = Stage::compose(&mut store, loaded.root_layer, StageOptions::default());
    let mut groups: Vec<Vec<String>> = stage
        .prototypes()
        .map(|prototype| {
            assert!(
                prototype
                    .prims()
                    .iter()
                    .all(|prim| !prim.relative_path().is_empty())
            );
            let mut instances: Vec<_> = prototype
                .instances()
                .iter()
                .map(|p| store.paths.resolve(*p).display(&store.tokens).to_string())
                .collect();
            instances.sort();
            instances
        })
        .collect();
    groups.sort();
    let mut targets = serde_json::Map::new();
    for instance in ["First", "Second"] {
        for (child, field) in [
            ("Anchor", "next"),
            ("Anchor", "missing"),
            ("Branch", "back"),
        ] {
            let path = format!("/Grove/{instance}/{child}.{field}");
            let property = store.property_path(&path);
            let value = stage.resolve_target_list_path(property).unwrap();
            targets.insert(
                path,
                json!(
                    value
                        .value
                        .iter()
                        .map(|p| p.display(&store.paths, &store.tokens))
                        .collect::<Vec<_>>()
                ),
            );
        }
    }
    let actual = json!({"groups": groups, "targets": targets});
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let code = r#"
import json,sys
from pxr import Usd
s=Usd.Stage.Open(sys.argv[1])
groups=sorted(sorted(str(p.GetPath()) for p in prototype.GetInstances()) for prototype in s.GetPrototypes())
targets={}
for i in ['First','Second']:
    for child,field in [('Anchor','next'),('Anchor','missing'),('Branch','back')]:
        path=f'/Grove/{i}/{child}.{field}'
        targets[path]=[str(p) for p in s.GetRelationshipAtPath(path).GetTargets()]
print(json.dumps(dict(groups=groups,targets=targets)))
"#;
    let available = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output();
    if !available.is_ok_and(|out| out.status.success()) {
        eprintln!("skipped native oracle: pxr unavailable");
        return;
    }
    let out = Command::new(python)
        .args(["-c", code])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn native_clip_contexts_have_distinct_prototypes_and_animation() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/flatten/instance_clips/root.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty());
    let mut store = loaded.store;
    let stage = Stage::compose(&mut store, loaded.root_layer, StageOptions::default());
    assert!(stage.clip_issues().is_empty());
    assert_eq!(stage.prototypes().count(), 2);
    let values: Vec<_> = ["First", "Second"]
        .map(|name| store.property_path(&format!("/{name}/Child.x")))
        .iter()
        .map(|path| {
            let value = stage.resolve_property_path_at_time(
                *path,
                5.,
                layerstack::InterpolationType::Linear,
            );
            assert!(
                stage
                    .property_sample_times(path.prim_path(), path.property())
                    .is_empty()
            );
            value.map(|resolved| {
                let layerstack::Value::Double(value) = resolved.value else {
                    panic!("double")
                };
                value
            })
        })
        .collect();
    assert_eq!(values, [None, None]);
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output();
    if !available.is_ok_and(|out| out.status.success()) {
        eprintln!("skipped native oracle: pxr unavailable");
        return;
    }
    let out = Command::new(python).args(["-c", "import json,sys; from pxr import Usd; s=Usd.Stage.Open(sys.argv[1]); print(json.dumps([len(s.GetPrototypes()),[s.GetAttributeAtPath('/'+n+'/Child.x').Get(5) for n in ['First','Second']]]))"]).arg(path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json!([stage.prototypes().count(), values]), expected);
}

#[test]
fn incremental_clip_edits_rebuild_prototype_identity() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/flatten/instance_clips/root.usda");
    let loaded = load_entry_usda(&path);
    let root = loaded.root_layer;
    let mut store = loaded.store;
    let original = Stage::compose(&mut store, root, StageOptions::default());
    let mut live = layerstack::LiveStage::compose(&mut store, root, StageOptions::default());
    assert_eq!(original.prototypes().count(), 2);
    let clips = store.tokens.lookup("clips").unwrap();
    for name in ["First", "Second"] {
        let prim = store.path(&format!("/{name}"));
        let layer = store.layers.get_mut(&root).unwrap();
        let mut spec = layer.prims[&prim].clone();
        spec.fields.retain(|field| field.name != clips);
        layer.insert_prim(prim, spec);
        live.synchronize(&mut store);
        let fresh = Stage::compose(&mut store, root, StageOptions::default());
        assert_eq!(
            live.stage().prototypes().count(),
            fresh.prototypes().count()
        );
    }
    assert_eq!(live.stage().prototypes().count(), 1);
    assert_eq!(original.prototypes().count(), 2);
}
