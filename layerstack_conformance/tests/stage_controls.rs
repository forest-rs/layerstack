// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage-local controls compared with native OpenUSD. AOUSD Core §10–§11;
//! `UsdStageLoadRules`, `UsdStagePopulationMask`, layer muting.

use layerstack::{
    InMemoryStore, LayerId, LayerMuteError, LiveStage, LoadPolicy, Path, PayloadLoadRules,
    PayloadRule, PopulationMask, Stage, StageOptions,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::{Value as Json, json};
use std::process::Command;

fn names(
    paths: impl IntoIterator<Item = layerstack::PathId>,
    store: &InMemoryStore,
) -> Vec<String> {
    let mut result: Vec<_> = paths
        .into_iter()
        .map(|p| store.paths.resolve(p).display(&store.tokens))
        .collect();
    result.sort();
    result
}
fn snapshot(stage: &Stage, store: &mut InMemoryStore) -> Json {
    let root = store.path("/");
    let tint = store.property_path("/World.tint");
    json!({"prims": names(stage.traverse_all(root).filter(|p| *p != root), store),
        "loadable": names(stage.loadable_paths(&store.paths, root), store),
        "loaded": names(stage.loaded_payload_paths(&store.paths), store),
        "tint": stage.resolve_field_path(tint).map(|r| match r.value {layerstack::Value::Int(i)=>i,_=>panic!("int tint")})})
}
#[test]
fn controls_match_openusd_and_preserve_other_stage_snapshots() {
    let path = workspace_root().join("layerstack_conformance/fixtures/stage_controls/root.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let mut store = loaded.store;
    let root_layer = loaded.root_layer;
    let overlay = *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with("overlay.usda"))
        .unwrap()
        .0;
    let original = Stage::compose(&mut store, root_layer, StageOptions::default());
    let original_snapshot = snapshot(&original, &mut store);
    let mut live = LiveStage::compose(
        &mut store,
        root_layer,
        StageOptions {
            load_rules: PayloadLoadRules::load_none(),
            ..StageOptions::default()
        },
    );
    let world = store.path("/World");
    let nested = store.path("/World/Nested");
    let other_child = store.path("/Other/Child");
    let reference = store.path("/Reference");
    let mut actual = vec![snapshot(live.stage(), &mut store)];
    assert!(!live.stage().has_prim(nested));
    live.load(&store, world, LoadPolicy::WithoutDescendants);
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    assert!(live.stage().has_prim(nested));
    assert!(!live.stage().has_prim(store.path("/World/Nested/Leaf")));
    live.load(&store, nested, LoadPolicy::WithDescendants);
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    live.load_and_unload(&store, &[reference], &[world], LoadPolicy::WithDescendants);
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    assert!(live.mute_layer(overlay).unwrap());
    assert!(
        !live.mute_layer(overlay).unwrap(),
        "a duplicate mute is a no-op"
    );
    let changes = live.recompose_changes(&mut store);
    assert!(
        !changes.resynced.is_empty(),
        "control changes reach retained consumers"
    );
    assert!(live.stage().is_layer_muted(overlay));
    assert!(
        store.layers.contains_key(&overlay),
        "muting never evicts host data"
    );
    actual.push(snapshot(live.stage(), &mut store));
    assert_eq!(
        snapshot(&original, &mut store),
        original_snapshot,
        "other snapshots remain unchanged"
    );
    store.layers.get_mut(&overlay).unwrap().touch();
    assert!(
        live.notify_changed_layers(&store).is_empty(),
        "muted edits do not invalidate this stage"
    );
    live.notify_layer_edit(overlay);
    assert!(live.recompose_changes(&mut store).resynced.is_empty());
    assert_eq!(
        live.mute_and_unmute_layers(&[LayerId(999), root_layer], &[]),
        Err(LayerMuteError::RootLayer)
    );
    assert!(
        !live.options().muted_layers.contains(&LayerId(999)),
        "failed batch is atomic"
    );
    assert_eq!(
        live.mute_and_unmute_layers(&[overlay], &[overlay]),
        Err(LayerMuteError::ConflictingRequest)
    );
    assert!(live.unmute_layer(overlay));
    assert!(!live.unmute_layer(overlay));
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    live.set_population_mask(Some(PopulationMask {
        include: vec![world],
    }));
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    live.set_population_mask(Some(PopulationMask {
        include: vec![other_child],
    }));
    live.synchronize(&mut store);
    actual.push(snapshot(live.stage(), &mut store));
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped native oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let script = r#"
import json,sys
from pxr import Usd,Sdf
stage=Usd.Stage.Open(sys.argv[1],load=Usd.Stage.LoadNone)
def snapshot():
    attr=stage.GetAttributeAtPath('/World.tint')
    return dict(prims=sorted(str(p.GetPath()) for p in stage.TraverseAll()),
        loadable=sorted(str(p) for p in stage.FindLoadable()),
        loaded=sorted(str(p) for p in stage.GetLoadSet() if stage.GetPrimAtPath(p)),tint=attr.Get() if attr else None)
out=[snapshot()]
stage.Load('/World',Usd.LoadWithoutDescendants);out.append(snapshot())
stage.Load('/World/Nested');out.append(snapshot())
stage.LoadAndUnload(['/Reference'],['/World']);out.append(snapshot())
overlay=next(l for l in stage.GetLayerStack() if l.identifier.endswith('overlay.usda')).identifier
stage.MuteLayer(overlay);out.append(snapshot())
stage.UnmuteLayer(overlay);out.append(snapshot())
stage.SetPopulationMask(Usd.StagePopulationMask(['/World']));out.append(snapshot())
stage.SetPopulationMask(Usd.StagePopulationMask(['/Other/Child']));out.append(snapshot())
print(json.dumps(out))
"#;
    let result = Command::new(python)
        .args(["-c", script])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected: Vec<Json> = serde_json::from_slice(&result.stdout).unwrap();
    for (i, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a, b, "snapshot {i}");
    }
}

#[test]
fn population_masks_select_subtrees_ancestors_and_empty_namespace() {
    let path = workspace_root().join("layerstack_conformance/fixtures/stage_controls/root.usda");
    let loaded = load_entry_usda(&path);
    let mut store = loaded.store;
    let world = store.path("/World");
    let geometry = store.path("/World/Geometry");
    let root = store.path("/");
    let stage = Stage::compose(
        &mut store,
        loaded.root_layer,
        StageOptions {
            mask: Some(PopulationMask {
                include: vec![world, geometry],
            }),
            ..StageOptions::default()
        },
    );
    assert!(stage.has_prim(geometry));
    assert!(
        stage.has_prim(store.path("/World/Nested/Leaf")),
        "all descendants selected"
    );
    assert!(!stage.has_prim(store.path("/Other")));
    let empty = Stage::compose(
        &mut store,
        loaded.root_layer,
        StageOptions {
            mask: Some(PopulationMask::default()),
            ..StageOptions::default()
        },
    );
    assert_eq!(names(empty.traverse(root), &store), vec!["/"]);
    let all = Stage::compose(
        &mut store,
        loaded.root_layer,
        StageOptions {
            mask: Some(PopulationMask {
                include: vec![root],
            }),
            ..StageOptions::default()
        },
    );
    assert!(all.has_prim(store.path("/Reference/Nested/Leaf")));
}

#[test]
fn literal_rules_and_convenience_operations_match_native_effective_rules() {
    let mut store = InMemoryStore::default();
    let a = Path::parse_absolute("/A", &mut store.tokens).unwrap();
    let b = Path::parse_absolute("/A/B", &mut store.tokens).unwrap();
    let c = Path::parse_absolute("/A/B/C", &mut store.tokens).unwrap();
    let mut rules = PayloadLoadRules::load_none();
    rules.load_with_descendants(c.clone());
    assert_eq!(rules.effective_rule(&a), PayloadRule::Only);
    assert_eq!(rules.effective_rule(&b), PayloadRule::Only);
    assert_eq!(rules.effective_rule(&c), PayloadRule::All);
    rules.add_rule(b.clone(), PayloadRule::None);
    assert_eq!(
        rules.effective_rule(&a),
        PayloadRule::None,
        "literal intermediate exclusion blocks descendant promotion"
    );
    assert_eq!(rules.effective_rule(&b), PayloadRule::Only);
    rules.load_without_descendants(a.clone());
    assert_eq!(
        rules.rules().count(),
        2,
        "subtree convenience erases all deeper rules"
    );
    assert!(rules.is_loaded(&a));
    assert!(!rules.is_loaded(&b));
    rules.unload(a.clone());
    assert!(!rules.is_loaded(&a));
}

#[test]
fn inactive_roots_remain_inspectable_and_stage_inventories_ignore_host_residency() {
    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let a = store.path("/A");
    let child = store.path("/A/Hidden");
    let b = store.path("/B");
    let orphan = store.path("/Over");
    let class = store.path("/Class");
    let undef_child = store.path("/Over/Child");
    let abstract_child = store.path("/Class/Child");
    let value = store.tokens.intern("value");
    let default = store.tokens.intern("A");
    let mut layer = layerstack::Layer::new(LayerId(1));
    let mut inactive = layerstack::PrimSpec::def().with_field(value, layerstack::Value::Int(17));
    inactive.active = Some(false);
    layer.insert_prim(a, inactive);
    layer.insert_prim(child, layerstack::PrimSpec::def());
    layer.insert_prim(b, layerstack::PrimSpec::def());
    layer.insert_prim(orphan, layerstack::PrimSpec::over());
    layer.insert_prim(class, layerstack::PrimSpec::class());
    layer.insert_prim(undef_child, layerstack::PrimSpec::def());
    layer.insert_prim(abstract_child, layerstack::PrimSpec::def());
    layer.default_prim = Some(default);
    store.insert_layer(layer);
    store.insert_layer(layerstack::Layer::new(LayerId(99)));
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let stage = live.stage();
    assert!(stage.has_prim(a));
    assert!(!stage.is_active(a));
    assert!(!stage.has_prim(child));
    assert!(!stage.is_loaded(a, &store.paths));
    assert_eq!(
        stage.resolve_field(a, value).unwrap().value,
        layerstack::Value::Int(17)
    );
    assert_eq!(
        names(stage.traverse_all(root), &store),
        vec![
            "/",
            "/A",
            "/B",
            "/Class",
            "/Class/Child",
            "/Over",
            "/Over/Child"
        ]
    );
    assert_eq!(
        names(stage.traverse(root), &store),
        vec!["/", "/B", "/Class", "/Class/Child", "/Over", "/Over/Child"]
    );
    assert_eq!(
        names(stage.traverse_default(root, &store), &store),
        vec!["/", "/B"]
    );
    assert!(!stage.is_defined(undef_child, &store));
    assert!(stage.is_abstract(abstract_child, &store));
    assert_eq!(stage.traverse(store.path("/Missing")).count(), 0);
    assert_eq!(stage.layer_stack(), &[LayerId(1)]);
    assert_eq!(stage.used_layers(false), [LayerId(1)].into_iter().collect());
    assert!(stage.has_authored_default_prim(&store));
    assert_eq!(stage.default_prim(&mut store), Some(a));
    assert_eq!(stage.time_codes_per_second(&store), 24.0);
    let actual_flags: Vec<_> = stage
        .traverse_all(root)
        .map(|p| {
            json!([
                store.paths.resolve(p).display(&store.tokens),
                stage.is_active(p),
                stage.is_loaded(p, &store.paths),
                stage.is_defined(p, &store),
                stage.is_abstract(p, &store),
            ])
        })
        .collect();
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let oracle = Command::new(python)
        .args(["-c", r#"
import json
from pxr import Sdf,Usd
layer=Sdf.Layer.CreateAnonymous()
layer.ImportFromString('''#usda 1.0
(defaultPrim = "A")
def "A" (active = false)
{
    def "Hidden" {}
}
def "B" {}
over "Over"
{
    def "Child" {}
}
class "Class"
{
    def "Child" {}
}
''')
stage=Usd.Stage.Open(layer)
prims=[stage.GetPseudoRoot()]+list(stage.TraverseAll())
print(json.dumps([[str(p.GetPath()),p.IsActive(),p.IsLoaded(),p.IsDefined(),p.IsAbstract()] for p in prims]))
"#])
        .output();
    if std::env::var_os("LAYERSTACK_USD_PYTHON").is_some() {
        let result = oracle.as_ref().expect("configured native Python must run");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    if let Ok(result) = oracle
        && result.status.success()
    {
        let mut expected: Vec<Json> = serde_json::from_slice(&result.stdout).unwrap();
        let mut actual = actual_flags;
        expected.sort_by_key(|v| v[0].as_str().unwrap().to_owned());
        actual.sort_by_key(|v| v[0].as_str().unwrap().to_owned());
        assert_eq!(actual, expected, "native prim flags");
    } else {
        eprintln!("skipped native prim flags: set LAYERSTACK_USD_PYTHON");
    }
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .prims
        .get_mut(&a)
        .unwrap()
        .active = Some(true);
    live.notify_structural_change();
    live.synchronize(&mut store);
    assert!(live.stage().has_prim(child));
    assert!(live.stage().is_active(a));
}

#[test]
fn inactive_payload_is_inspectable_but_not_loadable() {
    let path = workspace_root()
        .join("layerstack_conformance/fixtures/stage_controls/inactive_payload.usda");
    let loaded = load_entry_usda(&path);
    let mut store = loaded.store;
    let root = store.path("/");
    let a = store.path("/A");
    for rules in [PayloadLoadRules::default(), PayloadLoadRules::load_none()] {
        let stage = Stage::compose(
            &mut store,
            loaded.root_layer,
            StageOptions {
                load_rules: rules,
                ..StageOptions::default()
            },
        );
        assert!(stage.has_prim(a));
        assert!(!stage.is_active(a));
        assert!(!stage.is_loaded(a, &store.paths));
        assert!(stage.loadable_paths(&store.paths, root).is_empty());
        assert_eq!(
            stage.loaded_payload_paths(&store.paths),
            if stage.load_rules().is_loaded(store.paths.resolve(a)) {
                vec![a]
            } else {
                vec![]
            }
        );
    }
    if let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") {
        let result = Command::new(python)
            .args([
                "-c",
                r#"
from pxr import Usd
import sys
for mode in [Usd.Stage.LoadAll,Usd.Stage.LoadNone]:
    s=Usd.Stage.Open(sys.argv[1],load=mode)
    assert [str(p.GetPath()) for p in s.TraverseAll()]==['/A']
    assert not s.FindLoadable()
    assert sorted(str(p) for p in s.GetLoadSet())==(['/A'] if mode==Usd.Stage.LoadAll else [])
"#,
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn all_children_never_restore_removed_relocation_sources() {
    let path = workspace_root().join("layerstack_conformance/fixtures/relocates/root.usda");
    let loaded = load_entry_usda(&path);
    let mut store = loaded.store;
    let plot = store.path("/Garden/Plot");
    store
        .layers
        .get_mut(&loaded.root_layer)
        .unwrap()
        .prims
        .get_mut(&plot)
        .unwrap()
        .active = Some(false);
    let stage = Stage::compose(&mut store, loaded.root_layer, StageOptions::default());
    assert!(stage.has_prim(plot));
    assert!(!stage.is_active(plot));
    for parent in stage.traverse_all(store.paths.lookup(&Path::root()).unwrap()) {
        for child in stage.all_children_of(parent).unwrap_or(&[]) {
            assert!(
                stage.has_prim(*child),
                "all children includes absent {} under {}",
                store.paths.resolve(*child).display(&store.tokens),
                store.paths.resolve(parent).display(&store.tokens)
            );
        }
    }
}
