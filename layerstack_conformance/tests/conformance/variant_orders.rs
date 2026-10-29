// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The child and property orders a variant authors, read, written,
//! composed, flattened and authored as OpenUSD 26.11 does.
//!
//! `scripts/variant_orders_oracle.py` writes the layers of
//! `fixtures/variant_orders`, `root.usdc` (OpenUSD's crate-format copy of
//! `root.usda`) and `oracle.json`, which records, for OpenUSD:
//!
//! - every prim spec and variant prim spec of each layer, with the
//!   `primOrder` (`reorder nameChildren`) and `propertyOrder`
//!   (`reorder properties`) it authors, and each layer's
//!   `ExportToString`. Layerstack reads the same orders from the USDA
//!   layers and from OpenUSD's `root.usdc`, and writes each layer back to
//!   OpenUSD's text, directly and after a USDC round trip;
//! - every composed prim's children, property names and property order,
//!   before and after `/S` switches its selection, which a [`LiveStage`]
//!   recomposes as a fresh composition does. The composed `primOrder`, the
//!   strongest opinion, a variant's included, is what a stage flatten
//!   writes;
//! - the prim specs `UsdStage::Flatten` and `UsdUtils.FlattenLayerStack`
//!   write, with their children and orders, which layerstack's
//!   [`Stage::flatten`] and [`flatten_layer_stack`] must write too;
//! - `UsdPrim::SetChildrenReorder`, `SetPropertyOrder` and their clears
//!   through an edit target into `/E`'s variant (`v=x`, and `n=y` nested in
//!   it), and `SdfPrimSpec::nameChildrenOrder` on the variant's prim spec.
//!   The same edits through a [`Transaction`] are accepted, author the same
//!   orders on the variant spec, compose the same children and write the
//!   same root layer text.
//!
//! Spec: AOUSD Core §7.6.7 (variant specs hold every prim spec field),
//! §7.6.2.4.5 (`primOrder`), §11.3.1 and §11.3.2 (ordered prim and
//! property children). OpenUSD: `PcpComposeSiteChildNames`
//! (`pxr/usd/pcp/composeSite.cpp`), `UsdPrim::_GetPropertyNames`
//! (`pxr/usd/usd/prim.cpp`).

use std::collections::BTreeMap;

use layerstack::{
    Address, AssetResolveError, AssetResolver, EditTarget, FlattenRequirements, InMemoryStore,
    Layer, LayerId, LiveStage, Path, PathId, PathInterner, ResolvedAsset, SpecPath, Stage,
    StageOptions, TokenId, TokenInterner, Transaction, flatten_layer_stack,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/variant_orders/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    /// By layer file name, by spec path.
    specs: BTreeMap<String, BTreeMap<String, Orders>>,
    /// Each layer's `ExportToString`, by file name.
    text: BTreeMap<String, String>,
    composed: BTreeMap<String, Composed>,
    switched: Switched,
    flatten: BTreeMap<String, Flattened>,
    layer_stack_flatten: BTreeMap<String, Flattened>,
    edits: Vec<Edit>,
}

/// The orders a spec authors.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Orders {
    prim_order: Option<Vec<String>>,
    property_order: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Composed {
    children: Vec<String>,
    properties: Vec<String>,
    /// The strongest `primOrder` opinion, as `GetMetadata` resolves it.
    prim_order: Option<Vec<String>>,
    property_order: Vec<String>,
}

#[derive(Deserialize)]
struct Switched {
    /// `(prim, variant set, variant)`, authored on the root layer.
    selection: (String, String, String),
    composed: BTreeMap<String, Composed>,
}

/// A spec of a flattened layer.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Flattened {
    children: Vec<String>,
    prim_order: Option<Vec<String>>,
    property_order: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Edit {
    name: String,
    /// The variant's spec path, `/E{v=x}`.
    variant: String,
    calls: Vec<(String, Option<Vec<String>>)>,
    accepted: Vec<bool>,
    spec: Orders,
    composed: Composed,
    /// The root layer's `ExportToString` after the last call.
    text: String,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("26."),
        "oracle from OpenUSD {}",
        oracle.openusd_version
    );
    oracle
}

fn fixture(name: &str) -> std::path::PathBuf {
    workspace_root()
        .join("layerstack_conformance/fixtures/variant_orders")
        .join(name)
}

/// Resolves no asset: arcs keep their authored asset paths.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn names(tokens: &TokenInterner, order: Option<&[TokenId]>) -> Option<Vec<String>> {
    order.map(|order| order.iter().map(|&t| tokens.resolve(t).into()).collect())
}

/// The orders `layer` authors at the prim or variant spec `path`, `None`
/// when it holds no spec there.
fn orders_at(store: &InMemoryStore, layer: &Layer, path: &str) -> Option<Orders> {
    let (tokens, paths) = (store.tokens(), store.paths());
    let path = SpecPath::parse(path, tokens, paths).expect("spec path");
    if let Some(variant) = layer.variant_spec_at(&path, paths) {
        return Some(Orders {
            prim_order: names(tokens, variant.prim_order.as_deref()),
            property_order: names(tokens, variant.property_order.as_deref()),
        });
    }
    let sites: Vec<_> = path.variant_selection_sites().collect();
    let spec = layer.prim_spec_in(path.prim_path(), &sites)?;
    Some(Orders {
        prim_order: names(tokens, spec.prim_order()),
        property_order: names(tokens, spec.property_order()),
    })
}

/// The children `layer` lists at the prim or variant spec `path`, with its
/// orders.
fn flattened_at(store: &InMemoryStore, layer: &Layer, path: &str) -> Option<Flattened> {
    let (tokens, paths) = (store.tokens(), store.paths());
    let orders = orders_at(store, layer, path)?;
    let spec_path = SpecPath::parse(path, tokens, paths).expect("spec path");
    let children = match layer.variant_spec_at(&spec_path, paths) {
        Some(variant) => &variant.authored_children,
        None => {
            let sites: Vec<_> = spec_path.variant_selection_sites().collect();
            &layer
                .prim_spec_in(spec_path.prim_path(), &sites)?
                .authored_children
        }
    };
    Some(Flattened {
        children: children.iter().map(|&t| tokens.resolve(t).into()).collect(),
        prim_order: orders.prim_order,
        property_order: orders.property_order,
    })
}

/// Reads one layer of the fixture on its own.
fn read_layer(store: &mut InMemoryStore, id: LayerId, name: &str) -> Layer {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    let read = layerstack_usda::read_usda(&text, id, store.tokens(), store.paths(), &mut NoAssets);
    assert!(
        !read.has_errors(),
        "{name}: {:?}",
        read.diagnostics().collect::<Vec<_>>()
    );
    read.emitted.layer
}

fn usda(store: &InMemoryStore, layer: &Layer) -> String {
    layerstack_usda::save::save_usda(layer, store.tokens(), store.paths()).expect("save usda")
}

/// Every layer authors each spec's orders as OpenUSD reads them, and saves
/// as OpenUSD writes it, directly and through USDC; OpenUSD's own USDC of
/// the root layer reads the same.
#[test]
fn variant_orders_read_and_write_as_openusd() {
    let oracle = oracle();
    let mut store = InMemoryStore::default();
    for (i, (name, specs)) in oracle.specs.iter().enumerate() {
        let id = LayerId(i as u64 + 1);
        let layer = read_layer(&mut store, id, name);
        for (path, want) in specs {
            let got = orders_at(&store, &layer, path);
            assert_eq!(got.as_ref(), Some(want), "{name}: {path}");
        }
        assert_eq!(
            usda(&store, &layer),
            oracle.text[name],
            "{name}: saved text"
        );
        let bytes = layerstack_usdc::save_usdc(&layer, store.tokens(), store.paths())
            .unwrap_or_else(|e| panic!("{name}: save usdc: {e:?}"));
        let back =
            layerstack_usdc::read_usdc(&bytes, id, store.tokens(), store.paths(), &mut NoAssets)
                .unwrap_or_else(|e| panic!("{name}: read usdc: {e:?}"));
        assert_eq!(
            usda(&store, &back.layer),
            oracle.text[name],
            "{name}: through usdc"
        );
    }
    let bytes = std::fs::read(fixture("root.usdc")).unwrap();
    let crate_layer = layerstack_usdc::read_usdc(
        &bytes,
        LayerId(90),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    )
    .expect("OpenUSD's root.usdc");
    for (path, want) in &oracle.specs["root.usda"] {
        let got = orders_at(&store, &crate_layer.layer, path);
        assert_eq!(got.as_ref(), Some(want), "root.usdc: {path}");
    }
    assert_eq!(
        usda(&store, &crate_layer.layer),
        oracle.text["root.usda"],
        "OpenUSD's root.usdc saved as USDA"
    );
}

/// OpenUSD's Python, or `None` (skipping) when none is configured.
fn openusd_python() -> Option<String> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = std::process::Command::new(&python)
        .args(["-c", "from pxr import Sdf"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    available.then_some(python)
}

/// OpenUSD reads layerstack's USDC of each layer as the text it writes
/// for the USDA.
#[test]
fn openusd_reads_variant_orders_from_layerstack_usdc() {
    let Some(python) = openusd_python() else {
        eprintln!("skipped variant orders USDC check: set LAYERSTACK_USD_PYTHON");
        return;
    };
    let oracle = oracle();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("variant_orders");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = InMemoryStore::default();
    for (i, name) in oracle.text.keys().enumerate() {
        let layer = read_layer(&mut store, LayerId(i as u64 + 1), name);
        let bytes = layerstack_usdc::save_usdc(&layer, store.tokens(), store.paths())
            .unwrap_or_else(|e| panic!("{name}: save usdc: {e:?}"));
        let path = dir.join(name.replace(".usda", ".usdc"));
        std::fs::write(&path, bytes).unwrap();
        let output = std::process::Command::new(&python)
            .args([
                "-c",
                "import sys\nfrom pxr import Sdf\n\
                 print(Sdf.Layer.OpenAsAnonymous(sys.argv[1]).ExportToString(), end='')",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            oracle.text[name],
            "{name}: OpenUSD's text of layerstack's USDC"
        );
    }
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(&fixture("root.usda"));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

/// Every composed prim's children, property names and property order, by
/// path; `prim_order` from `flattened` when given, as the composed
/// metadata a stage flatten writes, and `None` otherwise.
fn composed(
    stage: &Stage,
    store: &InMemoryStore,
    flattened: Option<&BTreeMap<String, Flattened>>,
) -> BTreeMap<String, Composed> {
    let root = store.paths().lookup(&Path::root()).unwrap();
    stage
        .traverse_all(root)
        .filter(|&path| path != root)
        .filter_map(|path| stage.prim(path, store))
        .map(|prim| {
            let path = prim.to_string();
            let prim_order = flattened
                .and_then(|flat| flat.get(&path))
                .and_then(|spec| spec.prim_order.clone());
            let composed = Composed {
                children: prim.all_children().map(|c| c.name().into()).collect(),
                properties: prim.property_names().into_iter().map(Into::into).collect(),
                prim_order,
                property_order: prim
                    .property_order()
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            };
            (path, composed)
        })
        .collect()
}

/// The prim and variant specs of `layer` at the paths `want` names.
fn flattened(
    store: &InMemoryStore,
    layer: &Layer,
    want: &BTreeMap<String, Flattened>,
) -> BTreeMap<String, Flattened> {
    want.keys()
        .filter_map(|path| Some((path.clone(), flattened_at(store, layer, path)?)))
        .collect()
}

fn stage_flatten(loaded: &mut LoadedStage, stage: &Stage) -> Layer {
    let id = LayerId(200);
    stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            id,
            &FlattenRequirements::default(),
        )
        .expect("flatten")
        .layer
}

/// The stage composes every prim's children and properties in OpenUSD's
/// order, and its flatten writes OpenUSD's children and orders, the
/// composed `primOrder` among them; a selection switch recomposes the same
/// as a fresh composition and as OpenUSD.
#[test]
fn variant_orders_compose_and_flatten_as_openusd() {
    let oracle = oracle();
    let mut loaded = load();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let flat = stage_flatten(&mut loaded, &stage);
    let flat = flattened(&loaded.store, &flat, &oracle.flatten);
    assert_eq!(flat, oracle.flatten, "UsdStage::Flatten");
    assert_eq!(
        composed(&stage, &loaded.store, Some(&flat)),
        oracle.composed,
        "composed"
    );

    let consolidated = flatten_layer_stack(
        &loaded.store,
        loaded.root_layer.into(),
        LayerId(300),
        &mut |_, asset| Some(asset.into()),
    )
    .expect("flatten layer stack");
    assert_eq!(
        flattened(
            &loaded.store,
            &consolidated.layer,
            &oracle.layer_stack_flatten
        ),
        oracle.layer_stack_flatten,
        "UsdUtils.FlattenLayerStack"
    );

    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let (prim, set, variant) = &oracle.switched.selection;
    let prim = loaded.store.path(prim);
    let set = loaded.store.tokens().intern(set);
    let variant = loaded.store.tokens().intern(variant);
    let mut txn = Transaction::new();
    txn.set_variant_selection(
        EditTarget::for_layer(loaded.root_layer).prim(prim),
        set,
        Some(variant),
    );
    live.apply(&mut loaded.store, &txn).expect("switch");
    let fresh = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let flat = stage_flatten(&mut loaded, &fresh);
    let flat = flattened(&loaded.store, &flat, &oracle.flatten);
    let switched = composed(live.stage(), &loaded.store, Some(&flat));
    assert_eq!(
        switched,
        composed(&fresh, &loaded.store, Some(&flat)),
        "live switch"
    );
    assert_eq!(switched, oracle.switched.composed, "switched");
}

/// Each authoring call through a variant edit target is accepted as
/// OpenUSD accepts it, authors the variant spec's orders, recomposes `/E`
/// in OpenUSD's order and leaves OpenUSD's root layer text.
#[test]
fn variant_order_edits_agree_with_openusd_call_by_call() {
    let oracle = oracle();
    assert!(!oracle.edits.is_empty());
    for case in &oracle.edits {
        let name = &case.name;
        let mut loaded = load();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let variant = SpecPath::parse(&case.variant, loaded.store.tokens(), loaded.store.paths())
            .expect("variant path");
        let target = EditTarget::for_local_variant(loaded.root_layer, &variant);
        let prim: PathId = variant.prim_path();
        let mut accepted = Vec::new();
        for (call, value) in &case.calls {
            let order = value.as_ref().map(|names| {
                names
                    .iter()
                    .map(|n| loaded.store.tokens().intern(n))
                    .collect::<Vec<_>>()
            });
            let mut txn = Transaction::new();
            match (call.as_str(), order) {
                ("set_prim_order", Some(order)) => txn.set_prim_order(target.prim(prim), order),
                ("set_prim_order", None) => txn.clear_prim_order(target.prim(prim)),
                ("set_property_order", Some(order)) => {
                    txn.set_property_order(target.prim(prim), order)
                }
                ("set_property_order", None) => txn.clear_property_order(target.prim(prim)),
                // `SdfPrimSpec::nameChildrenOrder` of the variant's prim
                // spec: the spec addressed directly.
                ("sdf_name_children_order", Some(order)) => {
                    txn.set_prim_order(Address::spec(loaded.root_layer, variant.clone()), order)
                }
                other => panic!("{name}: no call {other:?}"),
            };
            accepted.push(live.apply(&mut loaded.store, &txn).is_ok());
            let fresh = Stage::compose(
                &mut loaded.store,
                loaded.root_layer,
                StageOptions::default(),
            );
            assert_eq!(
                composed(live.stage(), &loaded.store, None),
                composed(&fresh, &loaded.store, None),
                "{name}: call {} recomposes",
                accepted.len()
            );
        }
        assert_eq!(accepted, case.accepted, "{name}: accepted");
        let root = loaded.store.layer(loaded.root_layer).unwrap();
        let spec = orders_at(&loaded.store, root, &case.variant);
        assert_eq!(spec.as_ref(), Some(&case.spec), "{name}: variant spec");
        assert_eq!(
            usda(&loaded.store, root),
            case.text,
            "{name}: root layer text"
        );

        let flat = stage_flatten(&mut loaded, live.stage());
        let mut want = BTreeMap::new();
        want.insert(
            String::from("/E"),
            Flattened {
                children: Vec::new(),
                prim_order: None,
                property_order: None,
            },
        );
        let flat = flattened(&loaded.store, &flat, &want);
        let got = composed(live.stage(), &loaded.store, Some(&flat));
        assert_eq!(got["/E"], case.composed, "{name}: composed /E");
    }
}
