// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material bindings and collection membership against OpenUSD.
//!
//! `fixtures/binding/scene.usda` has direct and collection bindings at
//! several depths, both binding strengths and their conflicts, purposes and
//! their fallback, bindings on prims without `MaterialBindingAPI`,
//! collections with each expansion rule, excludes, `includeRoot`, nested and
//! circular inclusions and a `relationship` mode, collections decided by
//! their `membershipExpression` (by path, by the `kind` and `isa`
//! predicates, through references to other collections and a reference
//! cycle, on `/Exprs` and `/Predicates`), bindings to a non-`Material`, a missing prim
//! and through relationship forwarding, and overrides through a reference
//! (`asset.usda`). `scripts/binding_oracle.py` records what OpenUSD 26.08
//! computes in `oracle.json`:
//!
//! - for every prim, purpose and legacy mode, `ComputeBoundMaterial`'s
//!   material and winning relationship;
//! - for every collection, which prim and property paths (and the
//!   pseudo-root) `IsPathIncluded` includes.
//!
//! Collection bindings on one prim are tried in its property order
//! (`reorder properties` on `/Ordered`). `binding_caches_follow_edits`
//! also edits the scene through a `LiveStage` and checks, after each edit
//! and the invalidation `BindingCache` documents for it, that a long-lived
//! cache agrees with a fresh one.
//!
//! Every value must agree, through one `BindingCache` per purpose and mode
//! and through the views, bindings through collections decided by their
//! `membershipExpression` included.
//!
//! Spec: AOUSD Core §13 (applied schemas); OpenUSD's `UsdShade` and
//! `UsdCollectionAPI` define binding and membership.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use layerstack::{PathId, PropertyPath, Stage, StageOptions, TargetPath};
use layerstack_conformance::usda_real::load_entry_usda;
use layerstack_conformance::workspace_root;
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::{
    BindingCache, BindingKind, BindingOptions, MaterialPurpose, PrimView, Scene,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/binding/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    bindings: BTreeMap<String, BTreeMap<String, Bound>>,
    collections: BTreeMap<String, CollectionRecord>,
}

#[derive(Deserialize)]
struct Bound {
    material: Option<String>,
    relationship: Option<String>,
}

#[derive(Deserialize)]
struct CollectionRecord {
    uses_rule_map: bool,
    included: Vec<String>,
}

fn load() -> (layerstack_conformance::usda_real::LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/binding/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, stage)
}

/// The prims of the stage, in traversal order.
fn prims(stage: &Stage, loaded: &layerstack_conformance::usda_real::LoadedStage) -> Vec<PathId> {
    let root = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    stage.traverse(root).skip(1).collect()
}

#[test]
fn bound_materials_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (loaded, stage) = load();
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let prims = prims(&stage, &loaded);
    let display = |path: PathId| store.paths.display(path, &store.tokens);
    let property = |path: PropertyPath| path.display(&store.paths, &store.tokens);
    assert_eq!(prims.len(), oracle.bindings.len(), "the same prims");

    let mut failures = Vec::new();
    let mut checks = 0;
    for (purpose, purpose_key) in [
        (MaterialPurpose::All, "all"),
        (MaterialPurpose::Preview, "preview"),
        (MaterialPurpose::Full, "full"),
    ] {
        for (legacy, mode) in [(true, "legacy"), (false, "strict")] {
            let options = BindingOptions {
                support_legacy_bindings: legacy,
            };
            let mut cache = BindingCache::new(purpose.clone(), options);
            let results = cache.compute_bound_materials(&scene, &prims);
            for (path, bound) in prims.iter().zip(results) {
                let text = display(*path);
                let key = format!("{purpose_key}@{mode}");
                let expected = &oracle.bindings[&text][&key];
                let context = format!("{text} {key}");
                // The one-shot view agrees with the cache.
                if PrimView::new(scene, *path).compute_bound_material(&purpose, options) != bound {
                    failures.push(format!("{context}: the view and the cache differ"));
                }
                checks += 1;
                let material = bound.material.map(display);
                let relationship = bound.binding.as_ref().map(|b| property(b.relationship));
                if material != expected.material || relationship != expected.relationship {
                    failures.push(format!(
                        "{context}: {material:?} by {relationship:?}, OpenUSD {:?} by {:?}",
                        expected.material, expected.relationship
                    ));
                }
                if let Some(binding) = &bound.binding {
                    if binding.without_binding_api && !legacy {
                        failures.push(format!("{context}: a legacy binding in strict mode"));
                    }
                    if let BindingKind::Collection { collection } = binding.kind
                        && !property(binding.relationship).contains(":collection:")
                    {
                        failures.push(format!(
                            "{context}: {} is no collection binding",
                            property(collection)
                        ));
                    }
                }
            }
            assert!(cache.stats().hits > 0, "ancestors' bindings are shared");
        }
    }
    eprintln!(
        "{checks} bindings against OpenUSD {}",
        oracle.openusd_version
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn collection_membership_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = load();
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let prims = prims(&stage, &loaded);
    // The pseudo-root too, which `includeRoot` never makes a member.
    let root = store.paths.lookup(&layerstack::Path::root()).expect("root");
    let mut paths: Vec<TargetPath> = vec![TargetPath::Prim(root)];
    for prim in &prims {
        paths.push(TargetPath::Prim(*prim));
        for name in stage.property_names(*prim, store) {
            paths.push(TargetPath::Property(PropertyPath::new(*prim, name)));
        }
    }
    let display = |path: TargetPath| path.display(&store.paths, &store.tokens);

    let mut failures = Vec::new();
    let mut checks = 0;
    let mut seen = 0;
    for prim in &prims {
        for collection in CollectionApi::instances(&scene, *prim) {
            seen += 1;
            let name = format!(
                "{}.collection:{}",
                store.paths.display(*prim, &store.tokens),
                collection.instance()
            );
            let Some(expected) = oracle.collections.get(&name) else {
                failures.push(format!("{name}: OpenUSD has no such collection"));
                continue;
            };
            let query = collection.membership_query();
            if query.uses_rule_map() != expected.uses_rule_map {
                failures.push(format!(
                    "{name}: uses its rule map {}",
                    query.uses_rule_map()
                ));
            }
            let included: BTreeSet<&String> = expected.included.iter().collect();
            for path in &paths {
                let text = display(*path);
                checks += 1;
                let got = query.is_included(&scene, *path).is_included();
                if got != included.contains(&text) {
                    failures.push(format!("{name} {text}: included {got}, OpenUSD {}", !got));
                }
            }
        }
    }
    assert_eq!(seen, oracle.collections.len(), "every collection");
    eprintln!(
        "{checks} memberships against OpenUSD {}",
        oracle.openusd_version
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every binding a long-lived cache computes, and every membership it
/// holds, next to a fresh cache's: the differences.
fn stale(cache: &mut BindingCache, scene: &Scene<'_>, prims: &[PathId], step: &str) -> Vec<String> {
    let store = scene.store();
    let display = |path: PathId| store.paths().display(path, store.tokens());
    let mut fresh = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let mut out = Vec::new();
    for prim in prims {
        let (kept, now) = (
            cache.compute_bound_material(scene, *prim),
            fresh.compute_bound_material(scene, *prim),
        );
        if kept != now {
            out.push(format!(
                "{step}: {} bound {kept:?}, fresh {now:?}",
                display(*prim)
            ));
        }
        for collection in CollectionApi::instances(scene, *prim) {
            let Some(path) = PrimView::new(*scene, *prim)
                .property_path(&format!("collection:{}", collection.instance()))
            else {
                continue;
            };
            let now = collection.membership_query();
            let kept = cache.membership_query(scene, path).cloned();
            for member in prims {
                let target = TargetPath::Prim(*member);
                let kept = kept.as_ref().map(|q| q.is_included(scene, target));
                if kept != Some(now.is_included(scene, target)) {
                    out.push(format!(
                        "{step}: {}.collection:{} of {} is {kept:?}",
                        display(*prim),
                        collection.instance(),
                        display(*member)
                    ));
                }
            }
        }
    }
    out
}

/// A cache kept across edits agrees with a fresh one after each edit, once
/// the edit is invalidated as `BindingCache` documents: a binding
/// retargeted, a binding made stronger, an include removed from a
/// collection another collection includes, an include added, the
/// expression of a collection another's expression references changed,
/// and `reorder properties` changed.
#[test]
fn binding_caches_follow_edits() {
    use layerstack::edit::{EditTarget, Transaction};
    use layerstack::{FieldValue, InMemoryStore, ListOp, Value};

    let (mut loaded, _) = load();
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let layer = loaded.root_layer;
    let mut live = layerstack::LiveStage::compose(&mut loaded.store, layer, options);
    let prims = prims(live.stage(), &loaded);
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let mut failures = Vec::new();

    let store = &mut loaded.store;
    let property = |store: &mut InMemoryStore, prim: &str, name: &str| {
        let prim = store.path(prim);
        PropertyPath::new(prim, store.tokens.intern(name))
    };
    let target = EditTarget::for_layer(layer);

    // Warm the cache, and check it before any edit.
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "unedited"));

    // Retarget `/Direct`'s binding.
    let green = store.path("/Looks/Green");
    let direct = property(store, "/Direct", "material:binding");
    let mut tx = Transaction::new();
    tx.set_targets(
        target.property(direct),
        ListOp::explicit(vec![TargetPath::Prim(green)]),
    );
    live.apply(store, &tx).expect("retargets");
    cache.invalidate(direct.prim_path());
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "retargeted"));

    // Make it stronger than its descendants' bindings.
    let key = store.tokens.intern("bindMaterialAs");
    let stronger = store.tokens.intern("strongerThanDescendants");
    let mut tx = Transaction::new();
    tx.set_metadata(
        target.property(direct),
        key,
        FieldValue::Value(Value::Token(stronger)),
    );
    live.apply(store, &tx).expect("strengthens");
    cache.invalidate(direct.prim_path());
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "stronger"));

    // `/Nest.collection:outer` includes `inner`: dropping `/Nest/X` from
    // `inner` changes `outer`, which only `inner`'s invalidation names.
    let nest_z = store.path("/Nest/Z");
    let inner_includes = property(store, "/Nest", "collection:inner:includes");
    let inner = property(store, "/Nest", "collection:inner");
    let mut tx = Transaction::new();
    tx.set_targets(
        target.property(inner_includes),
        ListOp::explicit(vec![TargetPath::Prim(nest_z)]),
    );
    live.apply(store, &tx).expect("drops an include");
    cache.invalidate_collection(inner);
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "include removed"));

    // Add `/Collections/D` to `/Collections.collection:other`.
    let c = store.path("/Collections/C");
    let d = store.path("/Collections/D");
    let other_includes = property(store, "/Collections", "collection:other:includes");
    let other = property(store, "/Collections", "collection:other");
    let mut tx = Transaction::new();
    tx.set_targets(
        target.property(other_includes),
        ListOp::explicit(vec![TargetPath::Prim(c), TargetPath::Prim(d)]),
    );
    live.apply(store, &tx).expect("adds an include");
    cache.invalidate_collection(other);
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "include added"));

    // `/Predicates.collection:joined` references `parts`: changing `parts`'s
    // expression changes `joined`, which only `parts`'s invalidation names.
    let parts_expression = property(
        store,
        "/Predicates",
        "collection:parts:membershipExpression",
    );
    let parts = property(store, "/Predicates", "collection:parts");
    let leaf = store.path("/Predicates/Plain/Leaf");
    let red = store.path("/Looks/Red");
    let mut tx = Transaction::new();
    tx.set_default(
        target.property(parts_expression),
        Value::PathExpression("/Predicates/Plain//{isa:Mesh}".into()),
    );
    live.apply(store, &tx).expect("edits an expression");
    cache.invalidate_collection(parts);
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "expression edited"));
    let bound = cache.compute_bound_material(&scene, leaf);
    assert_eq!(bound.material, Some(red), "`joined` reads `parts`");

    // Put `/Ordered`'s `a` binding before `z`.
    let ordered = store.path("/Ordered");
    let a = store.tokens.intern("material:binding:collection:a");
    let z = store.tokens.intern("material:binding:collection:z");
    store
        .layers
        .get_mut(&layer)
        .and_then(|layer| layer.prims.get_mut(&ordered))
        .expect("the `/Ordered` spec")
        .property_order = Some(vec![a, z]);
    live.notify_layer_prim_edits(layer, &[ordered]);
    live.recompose(store);
    cache.invalidate(ordered);
    let scene = Scene::new(live.stage(), store);
    failures.extend(stale(&mut cache, &scene, &prims, "reordered"));
    let bound = cache.compute_bound_material(&scene, ordered);
    assert_eq!(bound.material, Some(red), "`a` comes first");

    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
