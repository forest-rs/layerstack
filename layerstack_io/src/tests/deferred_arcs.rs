// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arcs inside variant branches the authoring layer does not select are
//! loaded only when composition reaches them (AOUSD Core §10.3.2.5).

use super::*;
use layerstack::{ArcKind, CompositionError};

/// A model whose variant set `look` selects `a`; each branch references
/// its own asset.
const MODEL: &str = r#"#usda 1.0
def "First" {}
def "Second" {}
def "Model" (
    variants = { string look = "a" }
    prepend variantSets = "look"
) {
    variantSet "look" = {
        "a" (prepend references = @a.usda@</Look>) {}
        "b" (prepend references = @b.usda@</Look>) {}
    }
}
"#;

fn look(name: &str) -> String {
    format!("#usda 1.0\ndef \"Look\" {{\n string name = \"{name}\"\n}}\n")
}

fn name(doc: &StageDocument<Memory>, prim: &str) -> Option<Value> {
    let segments = prim
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| doc.store.tokens.lookup(segment))
        .collect::<Option<Vec<_>>>()?;
    let prim = doc
        .store
        .paths
        .lookup(&layerstack::Path::root().join(&segments))?;
    let property = layerstack::PropertyPath::new(prim, doc.store.tokens.lookup("name")?);
    doc.view()
        .attribute(property)
        .and_then(|a| a.authored_value(layerstack::Time::Default))
        .map(|resolved| resolved.value)
}

fn read(doc: &StageDocument<Memory>, identifier: &str) -> bool {
    doc.storage.reads.iter().any(|read| read == identifier)
}

fn select(doc: &mut StageDocument<Memory>, variant: &str) {
    let root = doc.stage.stage().root_layer().unwrap();
    let model = doc.store.path("/Model");
    let set = doc.store.tokens.intern("look");
    let variant = doc.store.tokens.intern(variant);
    let mut edit = Transaction::new();
    edit.set_variant_selection(EditTarget::for_layer(root).prim(model), set, Some(variant));
    let (store, stage) = doc.parts_mut();
    stage.apply(store, &edit).unwrap();
}

#[test]
fn opening_reads_only_the_branches_the_stage_selects() {
    let a = look("a");
    let b = look("b");
    let backend = Memory::with(&[("model.usda", MODEL), ("a.usda", &a), ("b.usda", &b)]);
    let doc = StageDocument::open(backend, "model.usda", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), Some(Value::string("a")));
    assert!(read(&doc, "a.usda"));
    assert!(!read(&doc, "b.usda"));
    assert!(doc.stage.stage().asset_requests().is_empty());
    assert!(doc.stage.stage().composition_errors().is_empty());

    // Every branch's arcs, when asked for.
    let backend = Memory::with(&[("model.usda", MODEL), ("a.usda", &a), ("b.usda", &b)]);
    let loading = LoadOptions {
        variant_arcs: VariantArcLoading::All,
        ..Default::default()
    };
    let doc =
        StageDocument::open_with(backend, "model.usda", StageOptions::default(), loading).unwrap();
    assert!(read(&doc, "b.usda"));
    assert_eq!(name(&doc, "/Model"), Some(Value::string("a")));
}

#[test]
fn opening_loads_branches_a_stronger_layer_selects() {
    let a = look("a");
    let b = look("b");
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\n(\n subLayers = [@shot.usda@, @model.usda@]\n)\n",
        ),
        (
            "shot.usda",
            "#usda 1.0\nover \"Model\" (\n variants = { string look = \"b\" }\n) {}\n",
        ),
        ("model.usda", MODEL),
        ("a.usda", &a),
        ("b.usda", &b),
    ]);
    let doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), Some(Value::string("b")));
    assert!(read(&doc, "b.usda"));
    assert!(doc.stage.stage().asset_requests().is_empty());
    assert!(doc.stage.stage().composition_errors().is_empty());
    let b_layer = doc
        .store
        .layer_ids()
        .into_iter()
        .find(|&id| doc.identifier(id) == Some("b.usda"))
        .unwrap();
    assert!(doc.load_report().layers.contains(&b_layer));
    assert!(doc.stage.stage().used_layers(true).contains(&b_layer));
    // Recording where the asset resolved leaves its anchor clean.
    let model = doc
        .store
        .layer_ids()
        .into_iter()
        .find(|&id| doc.identifier(id) == Some("model.usda"))
        .unwrap();
    assert!(!doc.is_dirty(model));
}

#[test]
fn selecting_a_deferred_branch_asks_for_its_assets_until_they_load() {
    let a = look("a");
    let b = look("b");
    let backend = Memory::with(&[("model.usda", MODEL), ("a.usda", &a), ("b.usda", &b)]);
    let mut doc = StageDocument::open(backend, "model.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let model = doc.store.path("/Model");
    select(&mut doc, "b");

    // Nothing is read implicitly: the arc contributes nothing yet, and the
    // stage names what it needs.
    assert!(!read(&doc, "b.usda"));
    assert_eq!(name(&doc, "/Model"), None);
    let requests = doc.stage.stage().asset_requests().to_vec();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].anchor, root);
    assert_eq!(requests[0].asset_path, "b.usda");
    assert_eq!(requests[0].prim, model);
    assert_eq!(requests[0].arc, ArcKind::References);
    assert!(doc.stage.stage().composition_errors().is_empty());

    let report = doc.load_requested_assets().unwrap();
    assert_eq!(report.layers.len(), 1);
    assert_eq!(doc.identifier(report.layers[0]), Some("b.usda"));
    assert_eq!(name(&doc, "/Model"), Some(Value::string("b")));
    assert!(doc.stage.stage().asset_requests().is_empty());
    assert!(
        doc.stage
            .stage()
            .used_layers(true)
            .contains(&report.layers[0])
    );
    // Loading composes only the prim whose arc names the asset, and the
    // pseudo-root holding it, not its siblings.
    let work = doc.stage.recomposition_work();
    assert!(!work.full_rebuild, "{work:?}");
    assert_eq!(work.replaced_prim_indexes, 2, "{work:?}");

    // Switching back needs no load: `a.usda` is already bound.
    select(&mut doc, "a");
    assert_eq!(name(&doc, "/Model"), Some(Value::string("a")));
    assert!(doc.stage.stage().asset_requests().is_empty());
}

#[test]
fn loaded_deferred_assets_load_the_branches_they_select_in_turn() {
    // `b.usda`, loaded on request, selects a branch of its own whose arc it
    // defers to a stronger selection.
    let nested = r#"#usda 1.0
def "Look" (
    variants = { string tint = "light" }
    prepend variantSets = "tint"
) {
    variantSet "tint" = {
        "light" (prepend references = @light.usda@</Tint>) {}
        "dark" (prepend references = @dark.usda@</Tint>) {}
    }
}
"#;
    let tint = |name: &str| format!("#usda 1.0\ndef \"Tint\" {{\n string name = \"{name}\"\n}}\n");
    let light = tint("light");
    let dark = tint("dark");
    let a = look("a");
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\n(\n subLayers = [@shot.usda@, @model.usda@]\n)\n",
        ),
        (
            "shot.usda",
            "#usda 1.0\nover \"Model\" (\n variants = { string look = \"b\"\n string tint = \"dark\" }\n) {}\n",
        ),
        ("model.usda", MODEL),
        ("a.usda", &a),
        ("b.usda", nested),
        ("light.usda", &light),
        ("dark.usda", &dark),
    ]);
    let doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), Some(Value::string("dark")));
    assert!(read(&doc, "dark.usda"));
    assert!(read(&doc, "light.usda"));
    assert!(doc.stage.stage().asset_requests().is_empty());
}

#[test]
fn missing_assets_in_unselected_branches_are_not_read_or_reported() {
    let a = look("a");
    for import_policy in [ImportPolicy::Strict, ImportPolicy::AllowRecovery] {
        let backend = Memory::with(&[("model.usda", MODEL), ("a.usda", &a)]);
        let loading = LoadOptions {
            import_policy,
            ..Default::default()
        };
        let mut doc =
            StageDocument::open_with(backend, "model.usda", StageOptions::default(), loading)
                .unwrap();
        assert!(doc.load_report().asset_failures.is_empty());
        assert!(doc.stage.stage().composition_errors().is_empty());

        // Either policy records the asset missing once composition asks
        // for it, and composition reports it (AOUSD Core §10.6).
        select(&mut doc, "b");
        let report = doc.load_requested_assets().unwrap();
        assert_eq!(report.asset_failures.len(), 1);
        assert_eq!(report.asset_failures[0].asset, "b.usda");
        assert!(report.asset_failures[0].is_missing());
        assert!(doc.stage.stage().asset_requests().is_empty());
        let root = doc.stage.stage().root_layer().unwrap();
        let model = doc.store.path("/Model");
        let [CompositionError::UnresolvedAsset(error)] = doc.stage.stage().composition_errors()
        else {
            panic!("{:?}", doc.stage.stage().composition_errors());
        };
        assert_eq!(
            (error.asset.as_str(), error.prim, error.introducing_layer),
            ("b.usda", model, root)
        );
        let spec = error
            .introducing_spec
            .display(&doc.store.paths, &doc.store.tokens);
        assert_eq!(spec, "/Model{look=b}");
    }
}

#[test]
fn a_missing_deferred_asset_that_appears_loads_after_a_reload() {
    // `shot.usda` selects the branch `model.usda` defers, whose asset is
    // missing when the stage opens.
    let a = look("a");
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\n(\n subLayers = [@shot.usda@, @model.usda@]\n)\n",
        ),
        (
            "shot.usda",
            "#usda 1.0\nover \"Model\" (\n variants = { string look = \"b\" }\n) {}\n",
        ),
        ("model.usda", MODEL),
        ("a.usda", &a),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), None);
    assert!(doc.load_report().asset_failures[0].is_missing());
    assert!(matches!(
        doc.stage.stage().composition_errors(),
        [CompositionError::UnresolvedAsset(error)] if error.asset == "b.usda"
    ));

    // The miss is recorded: asking again reads nothing.
    doc.storage
        .files
        .insert("b.usda".into(), look("b").into_bytes());
    doc.storage.reads.clear();
    assert!(doc.load_requested_assets().unwrap().layers.is_empty());
    assert!(!read(&doc, "b.usda"));

    // A reload retries it, as `UsdStage::Reload` retries every invalid
    // asset: composition asks for it again, and loading it composes it.
    doc.reload(ReloadPolicy::PreserveDirty).unwrap();
    assert!(doc.stage.stage().composition_errors().is_empty());
    assert_eq!(doc.stage.stage().asset_requests().len(), 1);
    let report = doc.load_requested_assets().unwrap();
    assert_eq!(report.layers.len(), 1);
    assert!(report.asset_failures.is_empty());
    assert_eq!(name(&doc, "/Model"), Some(Value::string("b")));
    assert!(doc.stage.stage().composition_errors().is_empty());
}

#[test]
fn binary_layers_defer_unselected_branch_arcs() {
    let a = look("a");
    let b = look("b");
    let mut doc = StageDocument::open(
        Memory::with(&[("model.usda", MODEL), ("a.usda", &a), ("b.usda", &b)]),
        "model.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    doc.export_layer(root, "model.usdc").unwrap();
    let usdc = doc.storage_mut().files.remove("model.usdc").unwrap();
    let mut backend = Memory::with(&[("a.usda", &a), ("b.usda", &b)]);
    backend.files.insert("model.usdc".into(), usdc);
    let doc = StageDocument::open(backend, "model.usdc", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), Some(Value::string("a")));
    assert!(read(&doc, "a.usda"));
    assert!(!read(&doc, "b.usda"));
}

#[test]
fn packages_hold_deferred_assets_and_load_their_members_on_request() {
    let a = look("a");
    let b = look("b");
    let mut doc = StageDocument::open(
        Memory::with(&[("model.usda", MODEL), ("a.usda", &a), ("b.usda", &b)]),
        "model.usda",
        StageOptions::default(),
    )
    .unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    // Packaging takes every branch's assets, deferred or not.
    let prepared = doc
        .prepare_package(
            root,
            "model.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(prepared.report().is_complete(), "{:?}", prepared.report());
    assert_eq!(prepared.plan().unwrap().layers.len(), 3);
    doc.export_package("out.usdz", &prepared).unwrap();

    let mut output = Memory::default();
    output
        .files
        .insert("out.usdz".into(), doc.storage.files["out.usdz"].clone());
    let mut doc = StageDocument::open(output, "out.usdz", StageOptions::default()).unwrap();
    assert_eq!(name(&doc, "/Model"), Some(Value::string("a")));
    // Localization renames the members; `b`'s is the one left unread.
    let loaded = doc.store.layer_count();
    assert_eq!(loaded, 2);
    select(&mut doc, "b");
    let report = doc.load_requested_assets().unwrap();
    assert_eq!(report.layers.len(), 1);
    assert!(
        doc.identifier(report.layers[0])
            .is_some_and(|identifier| identifier.starts_with("out.usdz["))
    );
    assert_eq!(name(&doc, "/Model"), Some(Value::string("b")));
}

#[test]
fn switching_an_instance_to_a_deferred_branch_uses_the_loaded_payload() {
    let asset = r#"#usda 1.0
(
    defaultPrim = "Asset"
)
def "Asset" (
    variants = { string geo = "render" }
    prepend variantSets = "geo"
) {
    def "Geo" {}
    variantSet "geo" = {
        "render" {
            over "Geo" (payload = @render.usda@) {}
        }
        "deform" {
            over "Geo" (payload = @deform.usda@) {}
        }
    }
}
"#;
    let mesh = |name: &str| {
        format!(
            "#usda 1.0\n(\n defaultPrim = \"Mesh\"\n)\ndef \"Mesh\" {{\n string name = \"{name}\"\n}}\n"
        )
    };
    let render = mesh("render");
    let deform = mesh("deform");
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"A\" (instanceable = true\n references = @asset.usda@) {}\ndef \"B\" (instanceable = true\n references = @asset.usda@) {}\n",
        ),
        ("asset.usda", asset),
        ("render.usda", &render),
        ("deform.usda", &deform),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert!(!read(&doc, "deform.usda"));
    let root = doc.stage.stage().root_layer().unwrap();
    let prim = doc.store.path("/A");
    let set = doc.store.tokens.intern("geo");
    let variant = doc.store.tokens.intern("deform");
    let mut edit = Transaction::new();
    edit.set_variant_selection(EditTarget::for_layer(root).prim(prim), set, Some(variant));
    let (store, stage) = doc.parts_mut();
    stage.apply(store, &edit).unwrap();
    let report = doc.load_requested_assets().unwrap();
    assert_eq!(report.layers.len(), 1);
    assert_eq!(doc.identifier(report.layers[0]), Some("deform.usda"));
    assert_eq!(name(&doc, "/A/Geo"), Some(Value::string("deform")));
    assert_eq!(name(&doc, "/B/Geo"), Some(Value::string("render")));
    assert!(
        doc.stage
            .stage()
            .used_layers(true)
            .contains(&report.layers[0])
    );
}

#[test]
fn layers_loaded_beneath_a_referenced_ancestor_count_as_used() {
    // The model sits inside a referenced set, so composing the set's root
    // reaches the model's arcs too.
    let a = look("a");
    let b = look("b");
    let backend = Memory::with(&[
        (
            "root.usda",
            "#usda 1.0\ndef \"Set\" (references = @set.usda@</Set>) {}\n",
        ),
        (
            "set.usda",
            "#usda 1.0\ndef \"Set\" {\n def \"Tier\" {\n def \"Model\" (references = @model.usda@</Model>) {}\n }\n}\n",
        ),
        ("model.usda", MODEL),
        ("a.usda", &a),
        ("b.usda", &b),
    ]);
    let mut doc = StageDocument::open(backend, "root.usda", StageOptions::default()).unwrap();
    assert!(!read(&doc, "b.usda"));
    let root = doc.stage.stage().root_layer().unwrap();
    let prim = doc.store.path("/Set/Tier/Model");
    let set = doc.store.tokens.intern("look");
    let variant = doc.store.tokens.intern("b");
    let mut edit = Transaction::new();
    edit.set_variant_selection(EditTarget::for_layer(root).prim(prim), set, Some(variant));
    let (store, stage) = doc.parts_mut();
    stage.apply(store, &edit).unwrap();
    let report = doc.load_requested_assets().unwrap();
    assert_eq!(report.layers.len(), 1);
    assert_eq!(name(&doc, "/Set/Tier/Model"), Some(Value::string("b")));
    assert!(!doc.stage.recomposition_work().full_rebuild);
    let fresh = layerstack::Stage::compose(&mut doc.store, root, StageOptions::default());
    assert_eq!(doc.stage.stage().used_layers(true), fresh.used_layers(true));
}

#[test]
fn edit_results_name_the_assets_a_transaction_asks_for() {
    use layerstack::{LayerOffset, ListPosition, Reference, ReferenceTarget, Specifier};
    let backend = Memory::with(&[("a.usda", &look("a")), ("b.usda", &look("b"))]);
    let mut doc = StageDocument::create(backend, "root.usda", StageOptions::default()).unwrap();
    let root = doc.stage.stage().root_layer().unwrap();
    let at = EditTarget::for_layer(root);
    let look_prim = doc.store.path("/Look");
    let payload = |asset: &str| {
        Reference::deferred(
            root,
            asset,
            ReferenceTarget::Prim(look_prim),
            LayerOffset::IDENTITY,
        )
    };
    let (a, b) = (doc.store.path("/A"), doc.store.path("/B"));

    // Applied on the live stage alone, the payload is only asked for.
    let mut edit = Transaction::new();
    edit.create_prim(at.prim(a), Specifier::Def, None);
    edit.add_payload(at.prim(a), payload("a.usda"), ListPosition::BackOfPrepend);
    let (store, stage) = doc.parts_mut();
    let applied = stage.apply(store, &edit).unwrap();
    let asked = |requests: &[layerstack::AssetRequest]| {
        let [request] = requests else {
            return false;
        };
        (
            request.anchor,
            request.asset_path.as_str(),
            request.prim,
            request.arc,
        ) == (root, "a.usda", a, ArcKind::Payloads)
    };
    assert!(asked(&applied.asset_requests));
    assert!(asked(doc.stage.stage().asset_requests()));
    assert!(!read(&doc, "a.usda"));

    // A request pending before an edit is not reported again.
    let mut edit = Transaction::new();
    edit.create_prim(at.prim(b), Specifier::Def, None);
    let (store, stage) = doc.parts_mut();
    assert!(stage.apply(store, &edit).unwrap().asset_requests.is_empty());

    // `StageDocument::apply` loads what is pending, its own requests and
    // earlier ones alike.
    let mut edit = Transaction::new();
    edit.add_payload(at.prim(b), payload("b.usda"), ListPosition::BackOfPrepend);
    let (applied, loaded) = doc.apply(&edit).unwrap();
    assert_eq!(applied.asset_requests.len(), 1);
    assert_eq!(applied.asset_requests[0].prim, b);
    assert_eq!(loaded.layers.len(), 2);
    assert!(doc.stage.stage().asset_requests().is_empty());
    assert_eq!(name(&doc, "/A"), Some(Value::string("a")));
    assert_eq!(name(&doc, "/B"), Some(Value::string("b")));

    // A rejected edit applies nothing and loads nothing.
    let mut edit = Transaction::new();
    edit.set_default(
        at.property(doc.store.property_path("/A.name")),
        Value::Int(1),
    );
    assert!(matches!(doc.apply(&edit), Err(ApplyError::Edit(_))));
}
