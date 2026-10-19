// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reverse collection membership against forward membership and OpenUSD.
//!
//! OpenUSD has no reverse membership API, so the reverse answer is checked
//! against the forward one: for every prim and property path (and the
//! pseudo-root), [`MembershipIndex::collections_including`] must name
//! exactly the collections whose `MembershipQuery::is_included` includes
//! it, with the same `Membership`. The forward answers are checked against
//! OpenUSD 26.08 (`ComputeMembershipQuery`, `IsPathIncluded`) through
//! `scripts/binding_oracle.py`'s `oracle.json`.
//!
//! `fixtures/membership/scene.usda` sublayers `strong.usda` over
//! `weak.usda`. `/Parts/Stones/Granite/Pebble` is authored into collections
//! on four prims: explicitly (`/Parts.collection:smooth`, `explicitOnly`),
//! through `expandPrims` on an ancestor (`/Parts.collection:heavy` and
//! `/Sets.collection:stones`), and by `/Garden.collection:path`, which the
//! stronger layer excludes it from. Beside those: an include deleted by
//! the stronger layer, `includeRoot` with excludes,
//! `expandPrimsAndProperties` with an excluded property, a collection
//! included by another, a `membershipExpression`, and collection bindings.
//!
//! The reverse answer is also checked on the `binding` and
//! `path_expression` fixtures, after each edit of a live stage against a
//! fresh build, and between the USDA and USDC forms of the generated parts
//! library (`layerstack_conformance::library_scene`), with its bindings.
//!
//! Spec: AOUSD Core §15 (collections); OpenUSD's `UsdCollectionAPI`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use layerstack::edit::{EditTarget, Transaction};
use layerstack::{
    InMemoryStore, LayerId, ListOp, LiveStage, PathId, PropertyPath, Specifier, Stage,
    StageOptions, TargetPath, Value,
};
use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};
use layerstack_conformance::usda_real::{LoadedStage, load_entry_usda};
use layerstack_conformance::usdc::load_entry_usdc;
use layerstack_conformance::workspace_root;
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::{
    BindingCache, BindingOptions, MaterialPurpose, Membership, MembershipIndex,
    MembershipIndexUpdate, Scene,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/membership/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    collections: BTreeMap<String, CollectionRecord>,
}

#[derive(Deserialize)]
struct CollectionRecord {
    uses_rule_map: bool,
    included: Vec<String>,
}

fn options(store: &mut InMemoryStore) -> StageOptions {
    StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    }
}

fn load(fixture: &str) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(&workspace_root().join(format!(
        "layerstack_conformance/fixtures/{fixture}/scene.usda"
    )));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = options(&mut loaded.store);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, stage)
}

fn root(scene: &Scene<'_>) -> PathId {
    scene
        .store()
        .paths()
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root")
}

/// The pseudo-root, every prim on the stage and each of its properties.
fn all_paths(scene: &Scene<'_>) -> Vec<TargetPath> {
    let stage = scene.stage();
    let mut paths = Vec::new();
    for prim in stage.traverse_all(root(scene)) {
        paths.push(TargetPath::Prim(prim));
        for property in stage
            .prim(prim, scene.store())
            .map(|prim| prim.properties())
            .unwrap_or_default()
        {
            paths.push(TargetPath::Property(property.path()));
        }
    }
    paths
}

type Answer = Vec<(String, String, Membership)>;

/// The reverse answer, in its order.
fn reverse(index: &MembershipIndex, scene: &Scene<'_>, path: TargetPath) -> Answer {
    let store = scene.store();
    index
        .collections_including(scene, path)
        .into_iter()
        .map(|inclusion| {
            (
                store
                    .paths()
                    .display(inclusion.collection.owner, store.tokens()),
                inclusion.collection.name.clone(),
                inclusion.membership,
            )
        })
        .collect()
}

/// Every path where the index and the forward queries disagree, or where
/// the index's order is not namespace order.
fn disagreements(index: &MembershipIndex, scene: &Scene<'_>, paths: &[TargetPath]) -> Vec<String> {
    let store = scene.store();
    // Forward queries, computed once per collection.
    let mut queries = Vec::new();
    for prim in scene.stage().traverse_all(root(scene)) {
        for collection in CollectionApi::instances(scene, prim) {
            queries.push((
                store.paths().display(prim, store.tokens()),
                collection.instance().to_string(),
                collection.membership_query(),
            ));
        }
    }
    let mut out = Vec::new();
    for path in paths {
        let expected: BTreeSet<(String, String, String)> = queries
            .iter()
            .filter_map(|(owner, name, query)| {
                let membership = query.is_included(scene, *path);
                membership
                    .is_included()
                    .then(|| (owner.clone(), name.clone(), format!("{membership:?}")))
            })
            .collect();
        let got = reverse(index, scene, *path);
        let got_set: BTreeSet<(String, String, String)> = got
            .iter()
            .map(|(o, n, m)| (o.clone(), n.clone(), format!("{m:?}")))
            .collect();
        let text = path.display(store.paths(), store.tokens());
        if got_set != expected || got_set.len() != got.len() {
            out.push(format!("{text}: reverse {got_set:?}, forward {expected:?}"));
        }
        let inclusions = index.collections_including(scene, *path);
        let ordered = inclusions.windows(2).all(|w| {
            store
                .paths()
                .cmp_paths(w[0].collection.owner, w[1].collection.owner, store.tokens())
                .then_with(|| w[0].collection.name.cmp(&w[1].collection.name))
                .is_lt()
        });
        if !ordered {
            out.push(format!("{text}: not in namespace order: {got:?}"));
        }
    }
    out
}

#[test]
fn forward_membership_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (loaded, stage) = load("membership");
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    // The oracle's paths: the pseudo-root, and the prims `Traverse` visits
    // with their properties.
    let mut paths = vec![TargetPath::Prim(root(&scene))];
    for prim in stage.traverse(root(&scene)).skip(1) {
        paths.push(TargetPath::Prim(prim));
        for property in stage
            .prim(prim, store)
            .map(|prim| prim.properties())
            .unwrap_or_default()
        {
            paths.push(TargetPath::Property(property.path()));
        }
    }
    let mut failures = Vec::new();
    let mut seen = 0;
    for prim in stage.traverse(root(&scene)) {
        for collection in CollectionApi::instances(&scene, prim) {
            seen += 1;
            let name = format!(
                "{}.collection:{}",
                store.paths.display(prim, &store.tokens),
                collection.instance()
            );
            let expected = &oracle.collections[&name];
            let query = collection.membership_query();
            assert_eq!(query.uses_rule_map(), expected.uses_rule_map, "{name}");
            let included: BTreeSet<&String> = expected.included.iter().collect();
            for path in &paths {
                let text = path.display(&store.paths, &store.tokens);
                let got = query.is_included(&scene, *path).is_included();
                if got != included.contains(&text) {
                    failures.push(format!("{name} {text}: included {got}"));
                }
            }
        }
    }
    assert_eq!(seen, oracle.collections.len(), "every collection");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_reverse_answer_is_the_forward_answer() {
    for fixture in ["membership", "binding", "path_expression"] {
        let (loaded, stage) = load(fixture);
        let scene = Scene::new(&stage, &loaded.store);
        let index = MembershipIndex::build(&scene);
        let paths = all_paths(&scene);
        let failures = disagreements(&index, &scene, &paths);
        assert!(
            failures.is_empty(),
            "{fixture}: {} differences:\n{}",
            failures.len(),
            failures.join("\n")
        );
        let stats = index.stats();
        assert!(stats.collections > 0 && stats.compiled == stats.collections as u64);
        eprintln!("{fixture}: {} paths, {stats:?}", paths.len());
    }
}

#[test]
fn the_pebble_is_in_the_collections_openusd_puts_it_in() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = load("membership");
    let scene = Scene::new(&stage, &loaded.store);
    let index = MembershipIndex::build(&scene);
    let pebble = loaded
        .store
        .paths
        .lookup(&path(&loaded.store, "/Parts/Stones/Granite/Pebble"));
    let got = reverse(
        &index,
        &scene,
        TargetPath::Prim(pebble.expect("the pebble")),
    );
    let expected: Vec<String> = oracle
        .collections
        .iter()
        .filter(|(_, record)| {
            record
                .included
                .iter()
                .any(|p| p == "/Parts/Stones/Granite/Pebble")
        })
        .map(|(name, _)| name.clone())
        .collect();
    let got_names: Vec<String> = got
        .iter()
        .map(|(owner, name, _)| format!("{owner}.collection:{name}"))
        .collect();
    assert_eq!(got_names, expected);
    // Explicitly, and through an ancestor's `expandPrims`; never by the
    // collection whose stronger layer excludes it.
    assert!(got.contains(&(
        "/Parts".into(),
        "smooth".into(),
        Membership::Included(layerstack_schemas::ExpansionRule::ExplicitOnly)
    )));
    assert!(got.contains(&(
        "/Sets".into(),
        "stones".into(),
        Membership::Included(layerstack_schemas::ExpansionRule::ExpandPrims)
    )));
    assert!(
        !got.iter()
            .any(|(owner, name, _)| owner == "/Garden" && name == "path")
    );
    // Each inclusion names the entry that decided it: the pebble itself, an
    // ancestor, or the pseudo-root for `includeRoot`.
    let decided: Vec<(String, Option<String>)> = index
        .collections_including(&scene, TargetPath::Prim(pebble.expect("the pebble")))
        .into_iter()
        .map(|inclusion| {
            let store = scene.store();
            (
                inclusion.collection.name.clone(),
                inclusion
                    .decided_at
                    .map(|at| at.display(store.paths(), store.tokens()).to_string()),
            )
        })
        .collect();
    for (name, at) in [
        ("smooth", "/Parts/Stones/Granite/Pebble"),
        ("stones", "/Parts/Stones"),
        ("everything", "/"),
    ] {
        assert!(
            decided.contains(&(name.into(), Some(at.into()))),
            "{name} at {at}: {decided:?}"
        );
    }
}

fn path(store: &InMemoryStore, text: &str) -> layerstack::Path {
    layerstack::Path::parse_absolute(text, &store.tokens).expect("a path")
}

type Names = &'static [&'static str];

/// The edits of [`incremental_updates_match_a_fresh_build`], each with the
/// update it must report as `(added, removed, changed)` collection names,
/// and the collections it compiles again when that is not every one.
struct Step {
    name: &'static str,
    edit: fn(&mut InMemoryStore, &Layers) -> Transaction,
    expect: (Names, Names, Names),
    recompiled: Option<Names>,
}

struct Layers {
    root: LayerId,
    strong: LayerId,
    weak: LayerId,
}

fn property(
    store: &mut InMemoryStore,
    layer: LayerId,
    prim: &str,
    name: &str,
) -> layerstack::Address {
    let prim = store.path(prim);
    EditTarget::for_layer(layer).property(PropertyPath::new(prim, store.tokens.intern(name)))
}

fn prim_at(store: &mut InMemoryStore, layer: LayerId, prim: &str) -> layerstack::Address {
    let prim = store.path(prim);
    EditTarget::for_layer(layer).prim(prim)
}

fn names(update: &[layerstack_schemas::CollectionIdentity]) -> Vec<&str> {
    update.iter().map(|c| c.name.as_str()).collect()
}

const STEPS: &[Step] = &[
    Step {
        name: "an unrelated attribute",
        edit: |store, layers| {
            let mut tx = Transaction::new();
            let at = property(store, layers.weak, "/Parts/Stones/Granite/Boulder", "mass");
            tx.set_default(at, Value::Float(4.));
            tx
        },
        expect: (&[], &[], &[]),
        recompiled: Some(&[]),
    },
    Step {
        name: "an included collection's includes",
        edit: |store, layers| {
            let pebble = store.path("/Parts/Stones/Granite/Pebble");
            let acorn = store.path("/Parts/Seeds/Acorn");
            let mut tx = Transaction::new();
            let at = property(store, layers.weak, "/Parts", "collection:smooth:includes");
            tx.set_targets(
                at,
                ListOp::appended(vec![TargetPath::Prim(pebble), TargetPath::Prim(acorn)]),
            );
            tx
        },
        // An existing property: a precise inventory, no resync. `nested`
        // includes `smooth`.
        expect: (&[], &[], &["nested", "smooth"]),
        recompiled: Some(&["nested", "smooth"]),
    },
    Step {
        name: "a stronger exclude on an included collection",
        edit: |store, layers| {
            let pebble = store.path("/Parts/Stones/Granite/Pebble");
            let mut tx = Transaction::new();
            let at = property(store, layers.strong, "/Parts", "collection:smooth:excludes");
            tx.set_targets(at, ListOp::explicit(vec![TargetPath::Prim(pebble)]));
            tx
        },
        // A new property spec resyncs `/Parts`: its collections compile
        // again, `heavy` unchanged.
        expect: (&[], &[], &["nested", "smooth"]),
        recompiled: Some(&["heavy", "nested", "smooth"]),
    },
    Step {
        name: "an include deleted in the stronger layer",
        edit: |store, layers| {
            let stones = store.path("/Parts/Stones");
            let mut tx = Transaction::new();
            let at = property(store, layers.strong, "/Sets", "collection:stones:includes");
            tx.set_targets(at, ListOp::deleted(vec![TargetPath::Prim(stones)]));
            tx
        },
        expect: (&[], &[], &["stones"]),
        recompiled: Some(&["everything", "stones"]),
    },
    Step {
        name: "an expression",
        edit: |store, layers| {
            let mut tx = Transaction::new();
            let at = property(
                store,
                layers.weak,
                "/Terrain",
                "collection:rocky:membershipExpression",
            );
            tx.set_default(at, Value::PathExpression("/Parts//Pine".into()));
            tx
        },
        expect: (&[], &[], &["rocky"]),
        recompiled: Some(&["rocky"]),
    },
    Step {
        name: "a new member under an included ancestor",
        edit: |store, layers| {
            let xform = store.tokens.intern("Xform");
            let mut tx = Transaction::new();
            let at = prim_at(store, layers.weak, "/Parts/Stones/Granite/Cobble");
            tx.create_prim(at, Specifier::Def, Some(xform));
            tx
        },
        expect: (&[], &[], &[]),
        recompiled: Some(&[]),
    },
    Step {
        name: "a newly applied collection",
        edit: |store, layers| {
            let seeds = store.path("/Parts/Seeds");
            let api = store.tokens.intern("CollectionAPI:loose");
            let mut tx = Transaction::new();
            let at = prim_at(store, layers.root, "/Terrain");
            tx.add_applied_schema(at, api);
            let at = property(store, layers.root, "/Terrain", "collection:loose:includes");
            tx.set_targets(at, ListOp::explicit(vec![TargetPath::Prim(seeds)]));
            tx
        },
        // Its prim resyncs: `rocky` there compiles again, unchanged.
        expect: (&["loose"], &[], &[]),
        recompiled: Some(&["rocky"]),
    },
    Step {
        name: "a removed member",
        edit: |store, layers| {
            let mut tx = Transaction::new();
            tx.remove_spec(prim_at(store, layers.weak, "/Parts/Seeds/Pine"));
            tx
        },
        expect: (&[], &[], &[]),
        recompiled: Some(&[]),
    },
    Step {
        name: "an owner's defining spec removed",
        edit: |store, layers| {
            let mut tx = Transaction::new();
            tx.remove_spec(prim_at(store, layers.weak, "/Garden"));
            tx
        },
        // The stronger layer's `over` keeps `/Garden`, without its schemas.
        expect: (&[], &["nested", "path", "props"], &[]),
        recompiled: Some(&[]),
    },
    Step {
        name: "the stronger sublayer removed",
        edit: |_, layers| {
            let mut tx = Transaction::new();
            tx.remove_sublayer(layers.root, 0);
            tx
        },
        expect: (&[], &[], &["heavy", "smooth", "stones"]),
        recompiled: None,
    },
];

/// After each edit, the index brought up to date by the edit's changes
/// answers as a fresh build does, for every path before and after the
/// edits, and reports the collections the step names.
#[test]
fn incremental_updates_match_a_fresh_build() {
    let (mut loaded, _) = load("membership");
    let layer = |name: &str| {
        *loaded
            .layer_names
            .iter()
            .find(|(_, n)| n.ends_with(name))
            .expect("the layer")
            .0
    };
    let layers = Layers {
        root: loaded.root_layer,
        strong: layer("strong.usda"),
        weak: layer("weak.usda"),
    };
    let store = &mut loaded.store;
    let options = options(store);
    let mut live = LiveStage::compose(store, layers.root, options);
    let scene = Scene::new(live.stage(), store);
    let mut index = MembershipIndex::build(&scene);
    let mut paths = all_paths(&scene);
    let mut failures = Vec::new();
    for step in STEPS {
        let tx = (step.edit)(store, &layers);
        let applied = live.apply(store, &tx).expect(step.name);
        let scene = Scene::new(live.stage(), store);
        let update: MembershipIndexUpdate = index.apply_changes(&scene, &applied.changes);
        let report = (
            names(&update.added),
            names(&update.removed),
            names(&update.changed),
        );
        let sorted = |v: &[&str]| {
            let mut v: Vec<String> = v.iter().map(|s| (*s).to_string()).collect();
            v.sort_unstable();
            v
        };
        if (sorted(&report.0), sorted(&report.1), sorted(&report.2))
            != (
                sorted(step.expect.0),
                sorted(step.expect.1),
                sorted(step.expect.2),
            )
        {
            failures.push(format!("{}: reported {report:?}, {update:?}", step.name));
        }
        if let Some(recompiled) = step.recompiled
            && sorted(&names(&update.recompiled)) != sorted(recompiled)
        {
            failures.push(format!(
                "{}: compiled again {:?}",
                step.name, update.recompiled
            ));
        }
        for path in all_paths(&scene) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        let fresh = MembershipIndex::build(&scene);
        let kept: Vec<_> = index.collections().collect();
        let built: Vec<_> = fresh.collections().collect();
        if kept != built {
            failures.push(format!("{}: compiled collections differ", step.name));
        }
        for path in &paths {
            if reverse(&index, &scene, *path) != reverse(&fresh, &scene, *path) {
                failures.push(format!(
                    "{}: {} kept {:?}, fresh {:?}",
                    step.name,
                    path.display(&store.paths, &store.tokens),
                    reverse(&index, &scene, *path),
                    reverse(&fresh, &scene, *path)
                ));
            }
        }
        failures.extend(
            disagreements(&index, &scene, &paths)
                .into_iter()
                .map(|f| format!("{}: {f}", step.name)),
        );
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// What a loaded library answers, as text: each prim's bound material and
/// binding relationship, and each prim's including collections.
fn library_answers(loaded: LoadedStage) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut store = loaded.store;
    let options = options(&mut store);
    let stage = Stage::compose(&mut store, loaded.root_layer, options);
    let scene = Scene::new(&stage, &store);
    let display = |path: PathId| store.paths.display(path, &store.tokens);
    let prims: Vec<PathId> = stage.traverse_all(root(&scene)).skip(1).collect();
    let mut cache = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let bindings = cache
        .compute_bound_materials(&scene, &prims)
        .into_iter()
        .zip(&prims)
        .filter_map(|(bound, prim)| {
            bound.material.map(|material| {
                format!(
                    "{} {} {}",
                    display(*prim),
                    display(material),
                    bound
                        .binding
                        .map(|b| b.relationship.display(&store.paths, &store.tokens))
                        .unwrap_or_default()
                )
            })
        })
        .collect();
    let index = MembershipIndex::build(&scene);
    let mut members = Vec::new();
    for prim in &prims {
        let answer = reverse(&index, &scene, TargetPath::Prim(*prim));
        members.push(format!("{} {answer:?}", display(*prim)));
    }
    let failures = disagreements(&index, &scene, &all_paths(&scene));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    (
        prims.iter().map(|p| display(*p)).collect(),
        bindings,
        members,
    )
}

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    base.join(format!("membership-index-{name}"))
}

/// The generated parts library composes, binds and answers membership the
/// same from its USDA and its USDC files.
#[test]
fn library_usda_and_usdc_agree() {
    let scene = LibraryScene::generate(&LibraryShape::SMALL);
    let dir = scratch_dir("library-scene");
    // Start clean if an earlier run was interrupted.
    let _ = std::fs::remove_dir_all(&dir);
    let usda = scene
        .write(&dir.join("usda"), LibraryFormat::Usda)
        .expect("writes");
    let usdc = scene
        .write(&dir.join("usdc"), LibraryFormat::Usdc)
        .expect("writes");
    let from_usda = load_entry_usda(&usda.root);
    let from_usdc = load_entry_usdc(&usdc.root);
    usda.remove();
    usdc.remove();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(from_usda.invalid.is_empty(), "{:?}", from_usda.invalid);
    let (prims, bindings, members) = library_answers(from_usda);
    assert_eq!(prims.len(), scene.counts.prims, "{}", scene.summary());
    assert!(bindings.len() > scene.counts.leaves / 2, "most leaves bind");
    assert!(
        members.iter().filter(|m| !m.ends_with("[]")).count() > scene.counts.leaves / 2,
        "most leaves are members"
    );
    let (usdc_prims, usdc_bindings, usdc_members) = library_answers(from_usdc);
    assert_eq!(prims, usdc_prims);
    assert_eq!(bindings, usdc_bindings);
    assert_eq!(members, usdc_members);
}
