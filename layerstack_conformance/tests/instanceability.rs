// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Direct contributing arcs govern scene graph instancing (AOUSD Core §11.3.3).
//! Empty class targets and inherited metadata must not discard local content.

use layerstack::{
    EditTarget, InMemoryStore, LiveStage, PrimSpec, Specifier, Stage, StageOptions, Transaction,
    Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::json;
use std::process::Command;

fn fixture() -> std::path::PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/instanceability/root.usda")
}

const CASES: &[(&str, bool, bool, bool)] = &[
    ("Inherited/A", false, true, true),
    ("Referenced/A", false, true, true),
    ("Payloaded/A", false, true, true),
    ("DirectInherit", true, false, true),
    ("DirectReference", true, false, true),
    ("DirectPayload", true, false, true),
    ("DirectSpecialize", true, false, true),
    ("EmptyTarget", true, false, false),
    ("MissingTarget", false, true, false),
    ("EmptyClassAndReference", true, false, true),
    ("Subroot", true, false, true),
    ("NestedAncestral", true, true, true),
    ("TaggedTarget", true, false, true),
    ("Outer/Inner", false, false, true),
    ("VariantOnly", true, false, true),
];

#[test]
fn only_contributing_direct_arc_subtrees_make_instances() {
    let mut loaded = load_entry_usda(&fixture());
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let view = stage.view(&loaded.store);
    for &(name, instance, local, geo) in CASES {
        let root = view.prim(format!("/{name}").as_str()).unwrap();
        assert_eq!(root.is_instance(), instance, "instance status at {name}");
        assert_eq!(
            view.prim(format!("/{name}/Local").as_str()).is_some(),
            local,
            "local child at {name}"
        );
        assert_eq!(
            view.prim(format!("/{name}/Geo").as_str()).is_some(),
            geo,
            "arc child at {name}"
        );
        assert_eq!(root.prototype().is_some(), instance, "prototype at {name}");
    }
    assert_eq!(
        view.attribute("/VariantOnly/Geo.value")
            .unwrap()
            .authored_value(layerstack::Time::Default)
            .unwrap()
            .value,
        Value::Int(1),
        "local opinions below an instance do not override variant data"
    );
}

#[test]
fn native_instancing_agrees_on_direct_ancestral_and_empty_sites() {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .is_ok_and(|out| out.status.success())
    {
        eprintln!("skipped native oracle: pxr unavailable");
        return;
    }
    let names: Vec<_> = CASES.iter().map(|case| case.0).collect();
    let code = r#"
import json,sys
from pxr import Usd
s=Usd.Stage.Open(sys.argv[1])
names=json.loads(sys.argv[2])
print(json.dumps([[n,s.GetPrimAtPath('/'+n).IsInstance(),bool(s.GetPrimAtPath('/'+n+'/Local')),bool(s.GetPrimAtPath('/'+n+'/Geo'))] for n in names]))
assert s.GetAttributeAtPath('/VariantOnly/Geo.value').Get() == 1
assert s.GetPrimAtPath('/EmptyClassAndReference').GetPrototype() == s.GetPrimAtPath('/DirectReference').GetPrototype()
s.CreateClassPrim('/Missing')
assert s.GetPrimAtPath('/EmptyClassAndReference').GetPrototype() != s.GetPrimAtPath('/DirectReference').GetPrototype()
s.RemovePrim('/Missing')
assert s.GetPrimAtPath('/EmptyClassAndReference').GetPrototype() == s.GetPrimAtPath('/DirectReference').GetPrototype()
s.Unload('/PayloadUnloaded')
assert s.GetPrimAtPath('/PayloadLoaded').GetPrototype() != s.GetPrimAtPath('/PayloadUnloaded').GetPrototype()
s.Load('/PayloadUnloaded')
assert s.GetPrimAtPath('/PayloadLoaded').GetPrototype() == s.GetPrimAtPath('/PayloadUnloaded').GetPrototype()
"#;
    let out = Command::new(python)
        .args(["-c", code])
        .arg(fixture())
        .arg(serde_json::to_string(&names).unwrap())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut loaded = load_entry_usda(&fixture());
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let view = stage.view(&loaded.store);
    let actual: Vec<_> = names
        .iter()
        .map(|name| {
            let root = view.prim(format!("/{name}").as_str());
            json!([
                name,
                root.is_some_and(|root| root.is_instance()),
                view.prim(format!("/{name}/Local").as_str()).is_some(),
                view.prim(format!("/{name}/Geo").as_str()).is_some()
            ])
        })
        .collect();
    assert_eq!(json!(actual), expected);
}

#[test]
fn first_and_last_class_specs_change_instanceability_incrementally() {
    let mut loaded = load_entry_usda(&fixture());
    let root = loaded.root_layer;
    let target = loaded.store.path("/Missing");
    let occurrence = loaded.store.path("/MissingTarget");
    let local = loaded.store.path("/MissingTarget/Local");
    let direct_reference = loaded.store.path("/DirectReference");
    let class_and_reference = loaded.store.path("/EmptyClassAndReference");
    let is_instance = |stage: &Stage, store: &InMemoryStore, path| {
        stage
            .prim(path, store)
            .is_some_and(|prim| prim.is_instance())
    };
    let has_prim = |stage: &Stage, store: &InMemoryStore, path| stage.prim(path, store).is_some();
    let mut live = LiveStage::compose(&mut loaded.store, root, StageOptions::default());
    assert!(!is_instance(live.stage(), &loaded.store, occurrence));
    let mut create = Transaction::new();
    create.create_prim(
        EditTarget::for_layer(root).prim(target),
        Specifier::Class,
        None,
    );
    let mut edit = create;
    for expected in [true, false, true, false] {
        edit = edit.apply(&mut loaded.store).unwrap();
        live.synchronize(&mut loaded.store);
        let fresh = Stage::compose(&mut loaded.store, root, StageOptions::default());
        let store = &loaded.store;
        assert_eq!(is_instance(live.stage(), store, occurrence), expected);
        assert_eq!(has_prim(live.stage(), store, local), !expected);
        let prim_stack =
            |stage: &Stage| stage.prim(occurrence, store).map(|prim| prim.prim_stack());
        assert_eq!(prim_stack(live.stage()), prim_stack(&fresh));
        assert_eq!(
            is_instance(live.stage(), store, occurrence),
            is_instance(&fresh, store, occurrence)
        );
        assert_eq!(
            has_prim(live.stage(), store, local),
            has_prim(&fresh, store, local)
        );
        let share_prototype = |stage: &Stage| {
            let prototype = |path| stage.prim(path, store).and_then(|prim| prim.prototype());
            prototype(direct_reference) == prototype(class_and_reference)
        };
        assert_eq!(
            share_prototype(live.stage()),
            !expected,
            "an empty class node must not split prototypes; its first spec must"
        );
        assert_eq!(share_prototype(live.stage()), share_prototype(&fresh));
        assert_eq!(
            live.stage().prototypes().count(),
            fresh.prototypes().count()
        );
    }
    // Replacing a class with an empty over still leaves a contributing spec.
    loaded
        .store
        .layer_mut(root)
        .unwrap()
        .insert_prim(target, PrimSpec::over());
    live.synchronize(&mut loaded.store);
    assert!(is_instance(live.stage(), &loaded.store, occurrence));
}

#[test]
fn nested_payload_participation_separates_empty_prototypes() {
    let mut loaded = load_entry_usda(&fixture());
    let loaded_instance = loaded.store.path("/PayloadLoaded");
    let unloaded_instance = loaded.store.path("/PayloadUnloaded");
    let mut options = StageOptions::default();
    options
        .load_rules
        .unload(loaded.store.paths.resolve(unloaded_instance).clone());
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    let instance = |stage: &Stage, store: &InMemoryStore, path| {
        let prim = stage.prim(path, store).unwrap();
        assert!(prim.is_instance());
        prim.prototype()
    };
    assert_ne!(
        instance(live.stage(), &loaded.store, loaded_instance),
        instance(live.stage(), &loaded.store, unloaded_instance),
        "payload participation affects the key even without descendant prims"
    );
    live.load(
        &loaded.store,
        unloaded_instance,
        layerstack::LoadPolicy::WithDescendants,
    );
    live.synchronize(&mut loaded.store);
    let prototype = |path| {
        live.stage()
            .prim(path, &loaded.store)
            .and_then(|prim| prim.prototype())
    };
    assert_eq!(prototype(loaded_instance), prototype(unloaded_instance));
    let fresh = Stage::compose(&mut loaded.store, loaded.root_layer, live.options().clone());
    assert_eq!(
        live.stage().prototypes().count(),
        fresh.prototypes().count()
    );
}
