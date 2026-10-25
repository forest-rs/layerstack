// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which flattened prototype each instance shares, against usd-core 26.11
//! with `USD_ASSIGN_PROTOTYPES_DETERMINISTICALLY` set.
//!
//! OpenUSD numbers its prototypes `/__Prototype_N` in the order of its
//! instance cache, which only that setting makes stable across runs: 26.11
//! orders new instance keys by the least path, in `SdfPath` order, of the
//! instances sharing each (`Usd_InstanceCache::ProcessChanges`,
//! `pxr/usd/usd/instanceCache.cpp`, PR 4143). `UsdStage::Flatten` then
//! names a `/Flattened_Prototype_N` for each prototype `GetPrototypes`
//! returns, in path order (`_GenerateFlattenedPrototypePath`,
//! `pxr/usd/usd/stage.cpp`), so `/__Prototype_10` precedes
//! `/__Prototype_2`. The scenes' instances are not in path order, one has
//! more than nine prototypes, and each nests instances in another's
//! prototype, which OpenUSD knows by their path under the outer instance.
//!
//! Spec: AOUSD Core §11.3.3 (instances share a prototype).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::stage::flatten::FlattenRequirements;
use layerstack::{LayerId, ReferenceTarget, Stage, StageOptions};
use layerstack_conformance::usda_real::load_entry_usda;

/// The scene: instances of three assets, the first named last, and an
/// instance nested in the third asset.
const SCENE: &str = r#"#usda 1.0
def "Assets"
{
    def "X"
    {
        def "Leaf"
        {
        }
    }

    def "Y"
    {
        def "Leaf"
        {
        }
    }

    def "Z"
    {
        def "Inner" (
            instanceable = true
            references = </Assets/X>
        )
        {
        }
    }
}

def "Second" (
    instanceable = true
    references = </Assets/Y>
)
{
}

def "First" (
    instanceable = true
    references = </Assets/X>
)
{
}

def "Third" (
    instanceable = true
    references = </Assets/Z>
)
{
}

def "Another" (
    instanceable = true
    references = </Assets/Y>
)
{
}
"#;

/// A second scene, its assets in another layer: more than nine
/// prototypes, so path order puts `/__Prototype_10` before
/// `/__Prototype_2`, and an asset instanced only inside another asset,
/// whose prototype OpenUSD numbers after every other.
const LIBRARY: &str = r#"#usda 1.0
def "Nest"
{
    def "Inner" (
        instanceable = true
        references = </Hidden>
    )
    {
    }

    def "Shared" (
        instanceable = true
        references = </Part3>
    )
    {
    }
}

def "Hidden"
{
    def "Leaf"
    {
    }
}
"#;

/// The second scene's root layer, generated with its parts.
fn many_scene() -> String {
    let mut library = String::from(LIBRARY);
    let mut root = String::from("#usda 1.0\n");
    for part in 0..11 {
        library.push_str(&format!(
            "def \"Part{part}\"\n{{\n    def \"Leaf{part}\"\n    {{\n    }}\n}}\n"
        ));
    }
    // Instance names that sort in neither part nor authoring order.
    for (name, part) in [
        ("m", 4),
        ("c", 9),
        ("q", 0),
        ("a", 7),
        ("k", 10),
        ("e", 2),
        ("o", 5),
        ("g", 1),
        ("s", 8),
        ("i", 6),
        ("b", 3),
        ("t", 4),
    ] {
        root.push_str(&format!(
            "def \"{name}\" (\n    instanceable = true\n    references = @library.usda@</Part{part}>\n)\n{{\n}}\n"
        ));
    }
    root.push_str(
        "def \"n\" (\n    instanceable = true\n    references = @library.usda@</Nest>\n)\n{\n}\n",
    );
    std::fs::write(scratch_dir().join("library.usda"), library).unwrap();
    root
}

/// usd-core's flattened layer: each prim with an internal reference, and
/// its target.
const ORACLE: &str = r#"
import json, sys
from pxr import Sdf, Usd

layer = Usd.Stage.Open(sys.argv[1]).Flatten()
out = {}
def visit(path):
    spec = layer.GetObjectAtPath(path)
    if isinstance(spec, Sdf.PrimSpec):
        for ref in spec.referenceList.GetAddedOrExplicitItems():
            if not ref.assetPath:
                out[str(path)] = str(ref.primPath)
layer.Traverse(Sdf.Path.absoluteRootPath, visit)
print(json.dumps(out, sort_keys=True))
"#;

/// A per-test directory under Cargo's integration-test scratch space.
fn scratch_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("prototype_order")
}

/// usd-core's answer, or `None` without usd-core.
fn oracle(scene: &Path) -> Option<BTreeMap<String, String>> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    if !available {
        eprintln!("skipped prototype order oracle: set LAYERSTACK_USD_PYTHON");
        return None;
    }
    let output = Command::new(&python)
        .env("USD_ASSIGN_PROTOTYPES_DETERMINISTICALLY", "1")
        .args(["-c", ORACLE])
        .arg(scene)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// Each instance, and each nested in a prototype, shares the flattened
/// prototype usd-core's deterministic assignment gives it.
#[test]
fn flattened_prototypes_follow_deterministic_assignment() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in [
        ("scene.usda", SCENE.to_owned()),
        ("many.usda", many_scene()),
    ] {
        let scene = dir.join(name);
        std::fs::write(&scene, text).unwrap();
        check(&scene);
    }
}

/// Flattens `scene` with layerstack and compares it with usd-core.
fn check(scene: &Path) {
    let Some(want) = oracle(scene) else {
        return;
    };
    let mut loaded = load_entry_usda(scene);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let id = LayerId(
        loaded
            .store
            .layer_ids()
            .iter()
            .map(|id| id.0)
            .max()
            .unwrap()
            + 1,
    );
    let flat = stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            id,
            &FlattenRequirements::default(),
        )
        .unwrap();
    let store = &loaded.store;
    let text = |path| store.paths.display(path, &store.tokens).to_string();
    let mut got = BTreeMap::new();
    for (&path, spec) in flat.layer.prims() {
        for reference in spec.references.explicit.iter().flatten() {
            if let ReferenceTarget::Prim(target) = reference.target {
                got.insert(text(path), text(target));
            }
        }
    }
    assert_eq!(got, want, "{}", scene.display());
}
