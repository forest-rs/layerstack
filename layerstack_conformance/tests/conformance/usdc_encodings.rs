// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values the USDC writer once refused, saved as OpenUSD 26.11 saves them
//! or refused where OpenUSD cannot save them.
//!
//! `scripts/usdc_encodings_oracle.py` records, for each case under
//! `fixtures/usdc_encodings`, what OpenUSD does call by call: it opens the
//! case's USDA, exports the layer to USDC (keeping the file as
//! `<case>.usdc` when the export succeeds) and reads that file back. Here
//! each case is read with [`read_usda`] and saved with [`save_usdc`]: an
//! export OpenUSD accepts must give OpenUSD's file but for its LZ4 blocks
//! ([`crate_sections`]), and both files must read back with [`read_usdc`]
//! as the text OpenUSD reads back; an export OpenUSD refuses must be
//! refused before any output. A case without a USDA source, a layer the
//! text parser cannot make, is OpenUSD's file alone: it must read with
//! [`read_usdc`] as OpenUSD reads it, and save again.
//!
//! Spec: AOUSD Core §16.3 (USDC), §7.4 (metadata); OpenUSD 26.11
//! `pxr/usd/sdf/crateFile.cpp`.

use std::path::PathBuf;

use std::sync::Arc;

use layerstack::{
    FieldValue, FlattenRequirements, InMemoryStore, Layer, LayerId, ListOp, NoAssets, PathInterner,
    PropertyPath, Resolved, ResolvedValue, Stage, StageOptions, SublayerEntry, TargetPath,
    TokenInterner, Value, flatten_layer_stack,
};
use layerstack_conformance::usdc::crate_sections;
use layerstack_conformance::workspace_root;
use layerstack_usda::read_usda;
use layerstack_usda::save::save_usda;
use layerstack_usdc::writer::UsdcWriteError;
use layerstack_usdc::{read_usdc, save_usdc};
use serde_json::Value as Json;

fn fixtures() -> PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/usdc_encodings")
}

/// The oracle's record of `case`, after checking which OpenUSD made it.
fn oracle(case: &str) -> Json {
    let text = std::fs::read_to_string(fixtures().join("oracle.json")).unwrap();
    let oracle: Json = serde_json::from_str(&text).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    let record = oracle["cases"][case].clone();
    assert_eq!(
        record["open_usda"]["accepted"],
        Json::Bool(true),
        "{case}: OpenUSD reads the source"
    );
    record
}

/// Saves `case`'s USDA source as USDC.
fn save(case: &str) -> Result<Vec<u8>, UsdcWriteError> {
    let text = std::fs::read_to_string(fixtures().join(format!("{case}.usda"))).unwrap();
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = read_usda(&text, LayerId(1), &tokens, &paths, &mut NoAssets);
    let diagnostics: Vec<_> = read.diagnostics().collect();
    assert!(diagnostics.is_empty(), "{case}: {diagnostics:?}");
    save_usdc(&read.emitted.layer, &tokens, &paths)
}

/// The USDA text of the layer `data` reads back as.
fn read_back(data: &[u8]) -> String {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = read_usdc(data, LayerId(2), &tokens, &paths, &mut NoAssets).expect("reads");
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    save_usda(&read.layer, &tokens, &paths).expect("saves as USDA")
}

/// Asserts that `case` saves as OpenUSD exports it, and that OpenUSD's
/// file and ours both read back as OpenUSD reads its file.
fn assert_saves_as_openusd(case: &str) {
    let record = oracle(case);
    let export = &record["export_usdc"];
    assert_eq!(export["accepted"], Json::Bool(true), "{case}: {export}");
    let openusd = std::fs::read(fixtures().join(format!("{case}.usdc"))).unwrap();
    let ours = save(case).unwrap_or_else(|e| panic!("{case}: {e}"));
    assert!(
        crate_sections(&ours).unwrap() == crate_sections(&openusd).unwrap(),
        "{case}: the file OpenUSD writes"
    );
    let read = &record["open_usdc"];
    assert_eq!(read["same_text"], Json::Bool(true), "{case}: {read}");
    let text = read["text"].as_str().unwrap();
    assert_eq!(read_back(&openusd), text, "{case}: OpenUSD's file");
    assert_eq!(read_back(&ours), text, "{case}: our file");
}

/// List-op metadata no schema registers, on the layer, a prim, an
/// attribute and a relationship, is an `SdfUnregisteredValue` holding an
/// `SdfUnregisteredValueListOp` of each item's text; two equal ones share
/// one value, and an unregistered dictionary beside them is laid out as
/// OpenUSD lays it out.
#[test]
fn unregistered_list_ops_save_as_openusd_saves_them() {
    assert_saves_as_openusd("unregistered_list_ops");
}

/// Relationship targets and an attribute connection that are relational
/// attributes (`</A.b[/C].d>`) or mappers (`</A.b.mapper[/C]>`), some
/// nested (`</A.b[/C.d[/E].f].g>`) or holding a variant selection in a
/// bracketed path, save with every element below the property a path of
/// its own, as OpenUSD saves them, and read back as the same targets.
#[test]
fn relational_targets_save_as_openusd_saves_them() {
    assert_saves_as_openusd("relational_targets");
}

/// A relationship's legacy `default` is an `SdfPath`, which OpenUSD packs
/// as an invalid value and then fails to read; the save refuses it.
#[test]
fn relationship_defaults_are_refused_as_openusd_cannot_read_them() {
    let record = oracle("relationship_default");
    let export = &record["export_usdc"];
    assert_eq!(export["accepted"], Json::Bool(false), "{export}");
    let errors = export["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .all(|e| e.as_str().unwrap().contains("unsupported type 'SdfPath'")),
        "{errors:?}"
    );
    assert_eq!(record["open_usdc"]["accepted"], Json::Bool(false));
    assert!(
        matches!(
            save("relationship_default"),
            Err(UsdcWriteError::UnencodedField { ref path, ref key })
                if path == "/A.r" && key == "default"
        ),
        "{:?}",
        save("relationship_default")
    );
}

/// Relative targets whose bracketed paths the parser anchors with them
/// (`<C.b[../E].d>` on `/P/Q` is `</P/Q/C.b[/P/Q/E].d>`) save as OpenUSD
/// saves them.
#[test]
fn anchored_brackets_save_as_openusd_saves_them() {
    assert_saves_as_openusd("anchored_brackets");
}

/// A target that keeps a relative bracketed path (`</X/Y.b[Z].d>`) kills
/// OpenUSD's export, whose `CrateFile::_AddPath` walks the relative
/// path's parents without end; the save refuses it before any output.
#[test]
fn relative_brackets_are_refused_as_openusd_cannot_save_them() {
    let record = oracle("relative_brackets");
    let export = &record["export_usdc"];
    assert_eq!(export["crashed"], Json::Bool(true), "{export}");
    let refused = save("relative_brackets");
    assert!(
        matches!(
            refused,
            Err(UsdcWriteError::InvalidPath { ref path, .. })
                if path == "/P/Q.b[C].d.mapper[/P/Q/E]" || path == "/X/Y.b[Z].d"
        ),
        "{refused:?}"
    );
}

/// Asserts that OpenUSD's file of `case`, a layer the text parser cannot
/// make, reads with [`read_usdc`] as the text OpenUSD reads from it, that
/// the text reads with [`read_usda`] as OpenUSD's parser reads it, and that
/// the layer read saves as USDC, reading back the same. The save is not
/// compared with OpenUSD's file: `Sdf.CopySpec` made the layer OpenUSD
/// saved, which orders a spec's fields as no text parser does, and
/// [`save_usdc`] orders them as OpenUSD's text parser does.
fn assert_reads_as_openusd(case: &str) {
    let text = std::fs::read_to_string(fixtures().join("oracle.json")).unwrap();
    let oracle: Json = serde_json::from_str(&text).expect("oracle.json");
    let record = &oracle["cases"][case];
    assert_eq!(
        record["export_usdc"]["accepted"],
        Json::Bool(true),
        "{case}"
    );
    let read = &record["open_usdc"];
    assert_eq!(read["same_text"], Json::Bool(true), "{case}: {read}");
    let text = read["text"].as_str().unwrap();
    let openusd = std::fs::read(fixtures().join(format!("{case}.usdc"))).unwrap();
    assert_eq!(read_back(&openusd), text, "{case}: OpenUSD's file");

    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let reparsed = read_usda(text, LayerId(3), &tokens, &paths, &mut NoAssets);
    let diagnostics: Vec<_> = reparsed.diagnostics().collect();
    assert_eq!(
        diagnostics.is_empty(),
        record["open_text"]["accepted"] == Json::Bool(true),
        "{case}: the text read as OpenUSD's parser reads it: {diagnostics:?}"
    );

    let read = read_usdc(&openusd, LayerId(1), &tokens, &paths, &mut NoAssets).expect("reads");
    let ours = save_usdc(&read.layer, &tokens, &paths).unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(read_back(&ours), text, "{case}: our file");
    let types = record["metadata_types"].as_object().unwrap();
    for data in [&openusd, &ours] {
        let read = read_usdc(data, LayerId(4), &tokens, &paths, &mut NoAssets).expect("reads");
        for (field, expected) in types {
            let found = metadata_type(&read.layer, field, &tokens, &paths);
            assert_eq!(Some(expected.as_str().unwrap()), found, "{case}: {field}");
        }
    }
}

/// The type OpenUSD's Python names for the value of `field` (`<path>
/// <key>`) in `layer`: a typed list op's.
fn metadata_type(
    layer: &Layer,
    field: &str,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Option<&'static str> {
    let (path, key) = field.split_once(' ').unwrap();
    let key = tokens.intern(key);
    let value = match TargetPath::parse(path, tokens, paths).unwrap() {
        TargetPath::Prim(prim) => layer.prim(prim)?.field(key),
        TargetPath::Property(property) => layer.property(property)?.metadata(key),
    }?;
    Some(match value {
        FieldValue::IntListOp(_) => "IntListOp",
        FieldValue::UIntListOp(_) => "UIntListOp",
        FieldValue::Int64ListOp(_) => "Int64ListOp",
        FieldValue::UInt64ListOp(_) => "UInt64ListOp",
        FieldValue::TokenListOp(_) => "TokenListOp",
        FieldValue::StringListOp(_) => "StringListOp",
        _ => "another type",
    })
}

/// Attribute connections to mappers, which OpenUSD's authoring and text
/// parser refuse but its crate stores and reads as any path, read from
/// OpenUSD's file, write as the text OpenUSD writes (which neither text
/// reader then reads) and save as USDC that reads back the same.
#[test]
fn mapper_connections_read_and_save_as_openusd() {
    assert_reads_as_openusd("mapper_connections");
}

/// Connections and targets of the other paths below a property (a target
/// path, a relational attribute's target, a mapper argument, an
/// expression), which no text parser reads but OpenUSD's crate stores and
/// reads, read from OpenUSD's file, write as the text OpenUSD writes and
/// save as USDC that reads back the same.
#[test]
fn property_tail_targets_read_and_save_as_openusd() {
    assert_reads_as_openusd("property_tail_targets");
}

/// Typed list ops under metadata keys no schema registers read from
/// OpenUSD's file, write as list-op statements and save as USDC, each as
/// its own list-op type, as OpenUSD saves them.
#[test]
fn unregistered_typed_list_ops_read_and_save_as_openusd() {
    assert_reads_as_openusd("unregistered_typed_list_ops");
}

/// Prim specs with no specifier, as `Sdf.CreatePrimInLayer` makes them,
/// read from OpenUSD's file, write with OpenUSD's fallback (`over "Q"`) and
/// save as OpenUSD's file, with no `specifier` field.
#[test]
fn unspecified_prims_read_and_save_as_openusd() {
    assert_reads_as_openusd("unspecified_prims");
    let openusd = std::fs::read(fixtures().join("unspecified_prims.usdc")).unwrap();
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = read_usdc(&openusd, LayerId(1), &tokens, &paths, &mut NoAssets).expect("reads");
    let ours = save_usdc(&read.layer, &tokens, &paths).unwrap();
    assert!(
        crate_sections(&ours).unwrap() == crate_sections(&openusd).unwrap(),
        "the file OpenUSD writes"
    );
}

/// The oracle's record of the dictionary cases' entries and composition.
fn dictionary_composition() -> Json {
    let text = std::fs::read_to_string(fixtures().join("oracle.json")).unwrap();
    let oracle: Json = serde_json::from_str(&text).expect("oracle.json");
    oracle["dictionary_composition"].clone()
}

/// A dictionary entry as `usdc_encodings_oracle.py`'s `entry_json` writes
/// it: a dictionary by key, a list op as its kind and the lists it
/// authors, a value as itself.
fn entry_json(entry: &FieldValue, tokens: &TokenInterner, paths: &PathInterner) -> Json {
    fn op<T>(kind: &str, op: &ListOp<T>, item: impl Fn(&T) -> Json) -> Json {
        let mut found = serde_json::Map::new();
        found.insert("listOp".into(), Json::from(kind));
        let items = |items: &[T]| Json::Array(items.iter().map(&item).collect());
        if let Some(explicit) = &op.explicit {
            found.insert("explicitItems".into(), items(explicit));
        }
        for (name, list) in [
            ("prependedItems", &op.prepend),
            ("appendedItems", &op.append),
            ("deletedItems", &op.delete),
        ] {
            if !list.is_empty() {
                found.insert(name.into(), items(list));
            }
        }
        Json::Object(found)
    }
    match entry {
        FieldValue::Value(value) => value_json(value, tokens, paths),
        FieldValue::TokenListOp(o) => op("TokenListOp", o, |t| tokens.resolve(*t).into()),
        FieldValue::StringListOp(o) => op("StringListOp", o, |s| (**s).into()),
        FieldValue::IntListOp(o) => op("IntListOp", o, |x| (*x).into()),
        FieldValue::UIntListOp(o) => op("UIntListOp", o, |x| (*x).into()),
        FieldValue::Int64ListOp(o) => op("Int64ListOp", o, |x| (*x).into()),
        FieldValue::UInt64ListOp(o) => op("UInt64ListOp", o, |x| (*x).into()),
        other => panic!("no JSON for {other:?}"),
    }
}

/// A value of the dictionary cases as JSON.
fn value_json(value: &Value, tokens: &TokenInterner, paths: &PathInterner) -> Json {
    match value {
        Value::Dictionary(entries) => dictionary_json(entries, tokens, paths),
        Value::Int(x) => (*x).into(),
        Value::String(s) => (**s).into(),
        Value::Token(t) => tokens.resolve(*t).into(),
        other => panic!("no JSON for {other:?}"),
    }
}

/// A dictionary's entries as JSON, by key.
fn dictionary_json(
    entries: &[(Arc<str>, FieldValue)],
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Json {
    Json::Object(
        entries
            .iter()
            .map(|(key, entry)| (key.to_string(), entry_json(entry, tokens, paths)))
            .collect(),
    )
}

/// The entries of each dictionary field the dictionary cases author in
/// `layer`, keyed `<path> <field>` as the oracle's `layer_entries` keys them.
fn layer_entries(layer: &Layer, store: &InMemoryStore) -> Json {
    let (tokens, paths) = (store.tokens(), store.paths());
    let mut found = serde_json::Map::new();
    let mut add = |key: &str, field: Option<&FieldValue>| {
        if let Some(field) = field {
            found.insert(key.into(), entry_json(field, tokens, paths));
        }
    };
    add(
        "/ customLayerData",
        layer.metadata(tokens.intern("customLayerData")),
    );
    let custom = tokens.intern("customData");
    let prim = store.path("/P");
    add(
        "/P customData",
        layer.prim(prim).and_then(|spec| spec.field(custom)),
    );
    let attribute = PropertyPath::new(prim, tokens.intern("v"));
    add(
        "/P.v customData",
        layer
            .property(attribute)
            .and_then(|spec| spec.metadata(custom)),
    );
    Json::Object(found)
}

/// Reads the dictionary case `case`'s OpenUSD file into `store` as `id`.
fn read_case(store: &mut InMemoryStore, case: &str, id: LayerId) {
    let bytes = std::fs::read(fixtures().join(format!("{case}.usdc"))).unwrap();
    let read = read_usdc(&bytes, id, store.tokens(), store.paths(), &mut NoAssets).expect("reads");
    assert!(
        read.diagnostics.is_empty(),
        "{case}: {:?}",
        read.diagnostics
    );
    store.insert_layer(read.layer);
}

/// Dictionary entries holding list ops, which OpenUSD's crate stores and
/// reads as any value, read as OpenUSD reads them and save as USDC as
/// OpenUSD saves them again. USDA text cannot hold them (OpenUSD writes
/// `TfStringify` text its own parser refuses), so `save_usda` refuses the
/// layer, naming the field and the entry, and `read_usda` refuses that text.
#[test]
fn dictionary_list_ops_read_and_save_as_openusd() {
    let record = dictionary_composition();
    let mut store = InMemoryStore::default();
    for (id, case) in [(1, "dictionary_list_ops"), (2, "dictionary_list_ops_weak")] {
        assert_eq!(
            oracle_built(case)["export_usdc"]["accepted"],
            Json::Bool(true)
        );
        read_case(&mut store, case, LayerId(id));
        let layer = store.layer(LayerId(id)).unwrap();
        assert_eq!(layer_entries(layer, &store), record["read"][case], "{case}");
    }
    let layer = store.layer(LayerId(1)).unwrap();
    let ours = save_usdc(layer, store.tokens(), store.paths()).expect("saves as USDC");
    let again = read_usdc(
        &ours,
        LayerId(3),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    )
    .unwrap();
    assert!(again.diagnostics.is_empty(), "{:?}", again.diagnostics);
    assert_eq!(record["save_again"]["accepted"], Json::Bool(true));
    assert_eq!(
        layer_entries(&again.layer, &store),
        record["save_again"]["entries"]
    );
    let refused = save_usda(layer, store.tokens(), store.paths()).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "/#customLayerData: dictionary entry `layerOps` holds a list op, which USDA text cannot hold"
    );
    let openusd_text = oracle_built("dictionary_list_ops")["open_usdc"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        oracle_built("dictionary_list_ops")["open_text"]["accepted"],
        Json::Bool(false)
    );
    let text = read_usda(
        &openusd_text,
        LayerId(4),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    assert!(
        text.diagnostics().next().is_some(),
        "the text parser refuses OpenUSD's text"
    );
}

/// Stage metadata resolution composes a stronger list op over a weaker
/// one of its kind at each key, combines dictionaries, keeps the stronger
/// of any other pair, and reads each list op as the explicit list op of
/// what it gives an empty list, as `UsdStage` does (`MetadataValueComposer`,
/// `VtDictionaryOverRecursive`); flattening the layer stack composes them
/// without resolving them, as `UsdUtilsFlattenLayerStack` does.
#[test]
fn dictionary_list_ops_compose_as_openusd() {
    let record = dictionary_composition();
    let mut store = InMemoryStore::default();
    read_case(&mut store, "dictionary_list_ops", LayerId(2));
    read_case(&mut store, "dictionary_list_ops_weak", LayerId(3));
    let mut root = Layer::new(LayerId(1));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(3)));
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let view = stage.view(&store);
    let composed = |resolved: Option<Resolved<ResolvedValue>>| match resolved.map(|r| r.value) {
        Some(ResolvedValue::Dictionary(entries)) => {
            dictionary_json(&entries, store.tokens(), store.paths())
        }
        other => panic!("customData resolves to {other:?}"),
    };
    let prim = view.prim("/P").unwrap();
    assert_eq!(
        composed(prim.metadata("customData")),
        record["composed"]["/P"]
    );
    let attribute = view.attribute("/P.v").unwrap();
    assert_eq!(
        composed(attribute.metadata("customData")),
        record["composed"]["/P.v"]
    );
    let flattened = flatten_layer_stack(&store, LayerId(1).into(), LayerId(9), &mut |_, asset| {
        Some(asset.to_owned())
    })
    .expect("flattens");
    let mut entries = layer_entries(&flattened.layer, &store);
    // The root layer authors no `customLayerData`; OpenUSD's flatten
    // takes the stronger sublayer's.
    entries.as_object_mut().unwrap().remove("/ customLayerData");
    let mut expected = record["flattened_layer_stack"].clone();
    expected
        .as_object_mut()
        .unwrap()
        .remove("/ customLayerData");
    assert_eq!(entries, expected);
    let flat = stage
        .flatten(
            &mut store,
            &NoAssets,
            LayerId(1),
            LayerId(10),
            &FlattenRequirements::default(),
        )
        .expect("flattens")
        .layer;
    assert_eq!(layer_entries(&flat, &store), record["flattened_stage"]);
    let stage = Stage::compose(&mut store, LayerId(2), StageOptions::default());
    let layer_data = stage
        .layer_metadata(store.tokens().intern("customLayerData"), &store)
        .unwrap();
    assert_eq!(
        value_json(&layer_data, store.tokens(), store.paths()),
        record["stage_metadata"]
    );
}

/// Dictionary entries holding what no dictionary entry holds here (a path
/// or payload list op, a variability), which OpenUSD's crate stores and
/// reads, are each reported and left out; the rest of the dictionary reads.
#[test]
fn dictionary_unheld_entries_are_reported() {
    let record = oracle_built("dictionary_unheld_entries");
    assert_eq!(record["export_usdc"]["accepted"], Json::Bool(true));
    let bytes = std::fs::read(fixtures().join("dictionary_unheld_entries.usdc")).unwrap();
    let store = InMemoryStore::default();
    let read = read_usdc(
        &bytes,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    )
    .expect("reads");
    let reported: Vec<_> = read
        .diagnostics
        .iter()
        .map(|d| (d.spec_path.as_str(), d.field.as_deref(), d.message.as_str()))
        .collect();
    let unheld = |key: &str, kind: &str| {
        format!("unsupported: dictionary entry `{key}` holds {kind}, which is not read")
    };
    let expected = [
        unheld("paths", "a path list op"),
        unheld("payloads", "a payload list op"),
        unheld("variability", "a variability"),
    ];
    let expected: Vec<_> = expected
        .iter()
        .map(|message| ("/P", Some("customData"), message.as_str()))
        .collect();
    assert_eq!(reported, expected);
    let custom = layer_entries(&read.layer, &store);
    assert_eq!(custom["/P customData"], serde_json::json!({"kept": 1}));
}

/// The oracle's record of `case`, one with no USDA source.
fn oracle_built(case: &str) -> Json {
    let text = std::fs::read_to_string(fixtures().join("oracle.json")).unwrap();
    let oracle: Json = serde_json::from_str(&text).expect("oracle.json");
    oracle["cases"][case].clone()
}

/// An attribute's `displayUnit` is a `TfEnum` in OpenUSD, which its crate
/// writer packs as an invalid value and then fails to read; the save
/// refuses it.
#[test]
fn display_units_are_refused_as_openusd_cannot_read_them() {
    let record = oracle("display_unit");
    let export = &record["export_usdc"];
    assert_eq!(export["accepted"], Json::Bool(false), "{export}");
    let errors = export["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .all(|e| e.as_str().unwrap().contains("unsupported type 'TfEnum'")),
        "{errors:?}"
    );
    assert_eq!(record["open_usdc"]["accepted"], Json::Bool(false));
    assert!(
        matches!(
            save("display_unit"),
            Err(UsdcWriteError::UnencodedField { ref path, ref key })
                if path == "/P.a" && key == "displayUnit"
        ),
        "{:?}",
        save("display_unit")
    );
}
