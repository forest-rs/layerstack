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

use layerstack::{FieldValue, Layer, LayerId, NoAssets, PathInterner, TargetPath, TokenInterner};
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

/// Typed list ops under metadata keys no schema registers read from
/// OpenUSD's file, write as list-op statements and save as USDC, each as
/// its own list-op type, as OpenUSD saves them.
#[test]
fn unregistered_typed_list_ops_read_and_save_as_openusd() {
    assert_reads_as_openusd("unregistered_typed_list_ops");
}
