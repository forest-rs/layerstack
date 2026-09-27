// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `layerstack_schemas`'s views against OpenUSD, through every generated
//! getter, setter and enum.
//!
//! The generated table (`generated/schema_views.rs`) defines a
//! `/Fallback_<S>` and an `/Authored_<S>` prim for every schema and authors
//! a value through every setter on the second, with a [`SchemaEdit`]
//! applied to a [`LiveStage`]. The saved layer is
//! `fixtures/schema_views/scene.usda`; `scripts/schema_views_oracle.py`
//! records what OpenUSD 26.08 reads from it in `oracle.json`: every
//! authored property's value at the default time and at time 2 on both
//! prims (the authored value and the schema fallback), its
//! `allowedTokens`, and every relationship's targets. Every getter must
//! read the same, every enum must list the same tokens, and every property
//! OpenUSD records must be read by some getter.
//!
//! Spec: AOUSD Core §12.3 (value resolution), §13.3.2.3 (the prim
//! definition), §13.3.2.4 (fallbacks).

#![allow(missing_docs, reason = "integration tests")]

#[path = "generated/schema_views.rs"]
mod table;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Debug;
use std::sync::Arc;

use layerstack::edit::EditTarget;
use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, PathId, StageOptions, TargetPath};
use layerstack_schemas::{OPENUSD_VERSION, Scene, SchemaEdit};
use serde_json::{Value as Json, json};

const SCENE: &str = include_str!("../fixtures/schema_views/scene.usda");
const ORACLE: &str = include_str!("../fixtures/schema_views/oracle.json");

/// The paths the table authors, interned before the edit borrows the
/// store.
pub(crate) struct Harness {
    paths: HashMap<&'static str, PathId>,
    targets: Vec<TargetPath>,
}

impl Harness {
    pub(crate) fn path(&self, text: &str) -> PathId {
        self.paths[text]
    }

    /// What every relationship setter targets.
    pub(crate) fn targets(&self) -> &[TargetPath] {
        &self.targets
    }
}

/// Which value a getter reads.
#[derive(Clone, Copy)]
pub(crate) enum When {
    /// At the default time.
    Default,
    /// At the table's sample time.
    Sample,
}

/// A getter's value as the oracle records it.
pub(crate) trait Canon {
    fn canon(&self) -> Json;
}

fn float(v: f64) -> Json {
    if v.is_nan() {
        json!("nan")
    } else if v.is_infinite() {
        json!(if v > 0.0 { "inf" } else { "-inf" })
    } else {
        json!(v)
    }
}

impl Canon for f64 {
    fn canon(&self) -> Json {
        float(*self)
    }
}

impl Canon for f32 {
    fn canon(&self) -> Json {
        float(f64::from(*self))
    }
}

macro_rules! canon_plain {
    ($($ty:ty),*) => {
        $(impl Canon for $ty {
            fn canon(&self) -> Json {
                json!(self)
            }
        })*
    };
}

canon_plain!(bool, u8, i32, u32, i64, u64, &str);

impl Canon for Arc<str> {
    fn canon(&self) -> Json {
        json!(&**self)
    }
}

impl<T: Canon, const N: usize> Canon for [T; N] {
    fn canon(&self) -> Json {
        Json::Array(self.iter().map(Canon::canon).collect())
    }
}

impl<T: Canon> Canon for Vec<T> {
    fn canon(&self) -> Json {
        Json::Array(self.iter().map(Canon::canon).collect())
    }
}

/// Compares every reading with the oracle.
pub(crate) struct Readings<'a> {
    store: &'a InMemoryStore,
    oracle: &'a BTreeMap<String, Json>,
    read: BTreeSet<String>,
    failures: Vec<String>,
    checks: usize,
}

impl<'a> Readings<'a> {
    fn expected(&mut self, prim: &str, property: &str, field: &str) -> Option<&'a Json> {
        let key = format!("{prim}.{property}");
        let found = self.oracle.get(&key);
        self.read.insert(key.clone());
        let Some(found) = found else {
            self.failures
                .push(format!("{key}: OpenUSD records no such property"));
            return None;
        };
        self.checks += 1;
        Some(found.get(field).unwrap_or(&Json::Null))
    }

    fn compare(&mut self, prim: &str, property: &str, field: &str, got: Json) {
        if let Some(expected) = self.expected(prim, property, field)
            && *expected != got
        {
            self.failures.push(format!(
                "{prim}.{property} ({field}): layerstack {got}, OpenUSD {expected}"
            ));
        }
    }

    pub(crate) fn value<T: Canon>(
        &mut self,
        prim: &str,
        property: &str,
        when: When,
        got: Option<T>,
    ) {
        let field = match when {
            When::Default => "default",
            When::Sample => "sample",
        };
        let got = got.as_ref().map_or(Json::Null, Canon::canon);
        self.compare(prim, property, field, got);
    }

    pub(crate) fn targets(&mut self, prim: &str, property: &str, got: &[TargetPath]) {
        let got = got
            .iter()
            .map(|t| json!(t.display(&self.store.paths, &self.store.tokens)))
            .collect();
        self.compare(prim, property, "targets", Json::Array(got));
    }

    pub(crate) fn allowed(&mut self, prim: &str, property: &str, tokens: &[&str]) {
        self.compare(prim, property, "allowed", json!(tokens));
    }

    pub(crate) fn round_trip<E: PartialEq + Debug>(
        &mut self,
        tokens: &[&str],
        from_token: fn(&str) -> E,
        as_str: fn(&E) -> &str,
    ) {
        for token in tokens {
            let value = from_token(token);
            self.checks += 1;
            if as_str(&value) != *token {
                self.failures
                    .push(format!("{token}: round-trips as {value:?}"));
            }
            // A token the schema lists is never `Other`.
            if format!("{value:?}").starts_with("Other(") {
                self.failures.push(format!("{token}: reads as {value:?}"));
            }
        }
        let other = from_token("notAnAllowedToken");
        self.checks += 1;
        if as_str(&other) != "notAnAllowedToken" || !format!("{other:?}").starts_with("Other(") {
            self.failures
                .push(format!("{tokens:?}: an unlisted token reads as {other:?}"));
        }
    }
}

/// The stage the table authors, with its store and harness.
fn authored() -> (InMemoryStore, LiveStage, Harness) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let paths = table::PRIMS.iter().map(|p| (*p, store.path(p))).collect();
    let harness = Harness {
        paths,
        targets: vec![TargetPath::prim(store.path("/Target"))],
    };
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    table::author(&mut edit, &harness);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)
        .expect("every setter's edit applies");
    (store, live, harness)
}

/// The setters author `fixtures/schema_views/scene.usda`, which the oracle
/// records.
#[test]
fn setters_author_the_fixture_scene() {
    let (store, _, _) = authored();
    let layer = store.layers.get(&LayerId(1)).expect("the root layer");
    let text = layerstack_usda::save::save_usda(layer, &store.tokens, &store.paths)
        .expect("the authored layer saves");
    if text != SCENE {
        let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("schema_views.usda");
        std::fs::write(&out, &text).expect("writes the new scene");
        panic!(
            "the setters no longer author fixtures/schema_views/scene.usda; the new scene is \
             {}: copy it there and rerun scripts/schema_views_oracle.py",
            out.display()
        );
    }
}

/// Every getter reads what OpenUSD's `Get()` and `GetTargets()` read, and
/// every property OpenUSD records is read.
#[test]
fn getters_read_what_openusd_reads() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle["openusd_version"], OPENUSD_VERSION);
    let properties: BTreeMap<String, Json> =
        serde_json::from_value(oracle["properties"].clone()).expect("the oracle's properties");

    let (store, live, harness) = authored();
    let scene = Scene::new(live.stage(), &store);
    let mut readings = Readings {
        store: &store,
        oracle: &properties,
        read: BTreeSet::new(),
        failures: Vec::new(),
        checks: 0,
    };
    table::read(&scene, &harness, &mut readings);
    let unread: Vec<&String> = properties
        .keys()
        .filter(|key| !readings.read.contains(*key))
        .collect();
    let mut failures = readings.failures;
    failures.extend(
        unread
            .iter()
            .map(|key| format!("{key}: no getter reads it")),
    );
    eprintln!(
        "{} readings of {} properties against OpenUSD {OPENUSD_VERSION}",
        readings.checks,
        properties.len()
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every enum round-trips its tokens, and reads any other token as
/// `Other`.
#[test]
fn enums_round_trip() {
    let store = InMemoryStore::default();
    let oracle = BTreeMap::new();
    let mut readings = Readings {
        store: &store,
        oracle: &oracle,
        read: BTreeSet::new(),
        failures: Vec::new(),
        checks: 0,
    };
    table::enums(&mut readings);
    assert!(readings.checks > 0);
    assert!(
        readings.failures.is_empty(),
        "{}",
        readings.failures.join("\n")
    );
}

/// Path expression setters, which the table leaves out, author what their
/// getters read.
#[test]
fn path_expression_setters_author_what_getters_read() {
    use layerstack_schemas::usd::CollectionApi;
    use layerstack_schemas::usd_geom::Scope;

    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let path = store.path("/Group");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Scope::define(&mut edit, path);
    let collection = CollectionApi::apply(&mut edit, path, "lights").expect("applies to a scope");
    collection.set_membership_expression(&mut edit, "/World//Lights");
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).expect("applies");

    let scene = Scene::new(live.stage(), &store);
    let collection = CollectionApi::get(&scene, path, "lights").expect("a collection");
    assert_eq!(
        collection.membership_expression().as_deref(),
        Some("/World//Lights")
    );
}

const MAPPED: &str = include_str!("../fixtures/schema_views/mapped.usda");
const MAPPED_ASSET: &str = include_str!("../fixtures/schema_views/mapped_asset.usda");

/// The root layer and the asset layer of [`referenced`].
fn both_layers(store: &InMemoryStore) -> Vec<Layer> {
    [LayerId(1), LayerId(2)]
        .map(|id| store.layers[&id].clone())
        .to_vec()
}

/// A stage whose `/Instance` references `/Asset` (with a `Light` child)
/// in another layer, composed with the schemas, and the edit target of
/// that reference.
fn referenced() -> (InMemoryStore, LiveStage, EditTarget) {
    use layerstack::{ArcKind, PrimSpec, Reference};

    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let (asset, light) = (store.path("/Asset"), store.path("/Asset/Light"));
    let instance = store.path("/Instance");
    let (asset_name, instance_name, light_name) = (
        store.tokens.intern("Asset"),
        store.tokens.intern("Instance"),
        store.tokens.intern("Light"),
    );
    let mut asset_layer = Layer::new(LayerId(2));
    asset_layer.insert_prim(root, PrimSpec::default().with_children(vec![asset_name]));
    asset_layer.insert_prim(asset, PrimSpec::def().with_children(vec![light_name]));
    asset_layer.insert_prim(light, PrimSpec::def());
    store.insert_layer(asset_layer);
    let mut main = Layer::new(LayerId(1));
    main.insert_prim(root, PrimSpec::default().with_children(vec![instance_name]));
    let reference = Reference {
        asset: Some("mapped_asset.usda".into()),
        ..Reference::new(LayerId(2), asset)
    };
    main.insert_prim(instance, PrimSpec::def().with_reference(reference));
    store.insert_layer(main);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    let graph = live.stage().explain_prim_graph(instance).expect("a graph");
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|n| n.arc_kind() == ArcKind::References)
        })
        .expect("the reference node");
    let target = EditTarget::for_node(live.stage(), instance, node).expect("an edit target");
    (store, live, target)
}

/// Relationship targets are stage paths, mapped through the edit target
/// as the relationship's own path is: through the reference,
/// `/Instance/Light` and `/Instance/Light.intensity` are authored as
/// `/Asset/Light` and `/Asset/Light.intensity`, and read back as the stage
/// paths given. The saved layers are the fixtures `mapped.usda` and
/// `mapped_asset.usda`, whose composed targets OpenUSD records. Undo and
/// redo restore both layers exactly.
///
/// OpenUSD: `UsdRelationship::SetTargets` through a `UsdEditTarget`.
#[test]
fn targets_map_through_the_edit_target() {
    use layerstack::PropertyPath;
    use layerstack_schemas::usd::CollectionApi;

    let (mut store, mut live, target) = referenced();
    let instance = store.path("/Instance");
    let light = store.path("/Instance/Light");
    let intensity = store.tokens.intern("intensity");
    let wanted = vec![
        TargetPath::prim(light),
        TargetPath::Property(PropertyPath::new(light, intensity)),
    ];
    let before = both_layers(&store);

    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    CollectionApi::apply(&mut edit, instance, "lights")
        .expect("any prim may have a collection")
        .set_includes(&mut edit, &wanted);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).expect("applies");

    let scene = Scene::new(live.stage(), &store);
    let includes = CollectionApi::get(&scene, instance, "lights")
        .expect("applied")
        .includes();
    assert_eq!(includes, wanted);

    // The layers OpenUSD composes.
    let save = |id| {
        layerstack_usda::save::save_usda(&store.layers[&id], &store.tokens, &store.paths)
            .expect("saves")
    };
    let (main, asset) = (save(LayerId(1)), save(LayerId(2)));
    assert!(
        asset.contains("</Asset/Light>") && asset.contains("</Asset/Light.intensity>"),
        "the targets are authored in the asset's namespace:\n{asset}"
    );
    for (name, text, fixture) in [
        ("mapped.usda", &main, MAPPED),
        ("mapped_asset.usda", &asset, MAPPED_ASSET),
    ] {
        if text != fixture {
            let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
            std::fs::write(&out, text).expect("writes the new layer");
            panic!(
                "the edit no longer authors fixtures/schema_views/{name}; the new layer is {}: \
                 copy it there and rerun scripts/schema_views_oracle.py",
                out.display()
            );
        }
    }
    let oracle: Json = serde_json::from_str(ORACLE).expect("the oracle parses");
    let got: Vec<String> = includes
        .iter()
        .map(|t| t.display(&store.paths, &store.tokens))
        .collect();
    assert_eq!(
        json!(got),
        oracle["mapped"]["/Instance.collection:lights:includes"],
        "OpenUSD composes the same targets"
    );

    // Undo and redo.
    let after = both_layers(&store);
    let redo = live
        .apply(&mut store, &applied.inverse)
        .expect("undoes")
        .inverse;
    assert_eq!(both_layers(&store), before);
    live.apply(&mut store, &redo).expect("redoes");
    assert_eq!(both_layers(&store), after);
}

/// A target the edit target does not map rejects the transaction; nothing
/// is authored.
#[test]
fn unmappable_targets_are_rejected() {
    use layerstack::edit::{EditError, Rejection};
    use layerstack_schemas::usd::CollectionApi;

    let (mut store, mut live, target) = referenced();
    let instance = store.path("/Instance");
    let outside = store.path("/Elsewhere");
    let before = both_layers(&store);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    CollectionApi::apply(&mut edit, instance, "lights")
        .expect("any prim may have a collection")
        .set_includes(&mut edit, &[TargetPath::prim(outside)]);
    let transaction = edit.finish();
    let rejected = live.apply(&mut store, &transaction).err();
    assert!(
        matches!(
            rejected,
            Some(EditError::Rejected {
                reason: Rejection::UnmappableTarget(t),
                ..
            }) if t == TargetPath::prim(outside)
        ),
        "{rejected:?}"
    );
    assert_eq!(both_layers(&store), before);
}

/// Nothing authors a prim that does not exist: applying a schema to one
/// fails with `NoSuchPrim` and no edit handle is had for one, unless the
/// same edit defines it first.
///
/// OpenUSD: `UsdPrim::ApplyAPI` and attribute authoring fail on an
/// invalid prim.
#[test]
fn missing_prims_are_not_authored() {
    use layerstack::CannotApply;
    use layerstack_schemas::usd::{CollectionApi, CollectionApiEdit};
    use layerstack_schemas::usd_geom::{Mesh, MeshEdit, Xform};

    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let (missing, later) = (store.path("/Missing"), store.path("/Later"));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        CollectionApi::apply(&mut edit, missing, "lights").err(),
        Some(CannotApply::NoSuchPrim)
    );
    assert!(MeshEdit::new(&edit, missing).is_none());
    assert!(CollectionApiEdit::new(&edit, missing, "lights").is_none());
    assert_eq!(edit.transaction(), &layerstack::edit::Transaction::new());

    Xform::define(&mut edit, later);
    assert!(CollectionApi::apply(&mut edit, later, "lights").is_ok());
    assert!(MeshEdit::new(&edit, later).is_some());
    Mesh::define(&mut edit, missing);
    assert!(
        MeshEdit::new(&edit, missing).is_some(),
        "defined in the edit"
    );
}

const LOCAL_VARIANT: &str = include_str!("../fixtures/schema_views/local_variant.usda");

/// Through a local variant target, `/Rock{look=a}`, targets outside the
/// branch are kept as they are (`/Light`, `/Light.intensity`) and one
/// inside names no variant selection (`/Rock/Pebble`); the stage reads
/// them back as given. The saved layer is the fixture
/// `local_variant.usda`, whose composed targets OpenUSD records. Undo and
/// redo restore the layer exactly.
///
/// OpenUSD: `UsdEditTarget::ForLocalDirectVariant`, whose map function
/// has the root identity.
#[test]
fn local_variant_targets_keep_paths_outside_the_branch() {
    use layerstack::{PrimSpec, PropertyPath, SpecPath};
    use layerstack_schemas::usd::CollectionApi;

    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let (rock, light) = (store.path("/Rock"), store.path("/Light"));
    let pebble = store.path("/Rock/Pebble");
    let (rock_name, light_name) = (store.tokens.intern("Rock"), store.tokens.intern("Light"));
    let (look, a) = (store.tokens.intern("look"), store.tokens.intern("a"));
    let intensity = store.tokens.intern("intensity");
    let variant =
        SpecPath::parse("/Rock{look=a}", &mut store.tokens, &mut store.paths).expect("a path");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        root,
        PrimSpec::default().with_children(vec![rock_name, light_name]),
    );
    let mut rock_spec = PrimSpec::def();
    rock_spec.variant_set_order.push(look);
    rock_spec.variant_selections.insert(look, a);
    layer.insert_prim(rock, rock_spec);
    layer.insert_prim(light, PrimSpec::def());
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let wanted = vec![
        TargetPath::prim(light),
        TargetPath::Property(PropertyPath::new(light, intensity)),
        TargetPath::prim(pebble),
    ];
    let before = store.layers[&LayerId(1)].clone();

    let target = EditTarget::for_local_variant(LayerId(1), &variant);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    CollectionApi::apply(&mut edit, rock, "lights")
        .expect("any prim may have a collection")
        .set_includes(&mut edit, &wanted);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).expect("applies");

    let scene = Scene::new(live.stage(), &store);
    let includes = CollectionApi::get(&scene, rock, "lights")
        .expect("applied in the selected variant")
        .includes();
    assert_eq!(includes, wanted);

    let text =
        layerstack_usda::save::save_usda(&store.layers[&LayerId(1)], &store.tokens, &store.paths)
            .expect("saves");
    if text != LOCAL_VARIANT {
        let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("local_variant.usda");
        std::fs::write(&out, &text).expect("writes the new layer");
        panic!(
            "the edit no longer authors fixtures/schema_views/local_variant.usda; the new layer \
             is {}: copy it there and rerun scripts/schema_views_oracle.py",
            out.display()
        );
    }
    let oracle: Json = serde_json::from_str(ORACLE).expect("the oracle parses");
    let got: Vec<String> = includes
        .iter()
        .map(|t| t.display(&store.paths, &store.tokens))
        .collect();
    assert_eq!(
        json!(got),
        oracle["local_variant"]["/Rock.collection:lights:includes"],
        "OpenUSD composes the same targets"
    );

    let after = store.layers[&LayerId(1)].clone();
    let redo = live
        .apply(&mut store, &applied.inverse)
        .expect("undoes")
        .inverse;
    assert_eq!(store.layers[&LayerId(1)], before);
    live.apply(&mut store, &redo).expect("redoes");
    assert_eq!(store.layers[&LayerId(1)], after);
}
