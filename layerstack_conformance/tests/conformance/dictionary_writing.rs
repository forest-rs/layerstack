// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dictionaries holding path expressions and empty values, through the
//! USDA and USDC writers, against usd-core 26.11.
//!
//! OpenUSD 26.11 quotes a path expression inside a dictionary as it does
//! an attribute's (issue 4182; `testSdfParsing` baseline
//! `233_dictionary_pathExpressions.usda`), and its USDA writer skips a
//! dictionary entry with no value, warning, where 26.08 wrote `key =`
//! (PR 4142; `Sdf_FileIOUtility::_WriteDictionary`,
//! `pxr/usd/sdf/fileIO_Common.cpp`). Its USDC writer still refuses such
//! an entry.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner, Value,
};
use layerstack_usdc::writer::UsdcWriteError;

/// OpenUSD 26.11's `233_dictionary_pathExpressions.usda` baseline, as its
/// USDA writer writes it.
const SOURCE: &str = r#"#usda 1.0

def Scope "Root"
{
    custom double attr (
        customData = {
            asset assetPath = @/some/asset.usda@
            dictionary nested = {
                pathExpression nestedPexpr = "/nested//expr"
            }
            pathExpression pexpr = "/foo//bar"
            pathExpression[] pexprArray = ["/a//b", "~/c", "/d /e"]
            pathExpression pexprComplement = "~/foo/bar"
            pathExpression pexprMulti = "/a//b /c/d"
            pathExpression pexprRelative = "foo ../bar baz//qux"
            string str = "plain"
            token tok = "atoken"
        }
    )
}

"#;

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

/// A per-test directory under Cargo's integration-test scratch space.
fn scratch_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("dictionary_writing")
}

/// OpenUSD's Python, or `None` (skipping) when none is configured.
fn openusd_python() -> Option<String> {
    let explicit = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit.clone().unwrap_or_else(|| "python3".into());
    let available = Command::new(&python)
        .args(["-c", "from pxr import Sdf"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    available.then_some(python)
}

/// Runs `script` with OpenUSD's Python, `args` after it; its stdout.
fn openusd(python: &str, script: &str, args: &[&Path]) -> String {
    let output = Command::new(python)
        .args(["-c", script])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Path expressions in a dictionary read and write as usd-core 26.11's:
/// layerstack's USDA of the text and of usd-core's USDC is that text, and
/// usd-core reads layerstack's USDA and USDC as it.
#[test]
fn dictionary_path_expressions_round_trip_with_openusd() {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read = layerstack_usda::read_usda(SOURCE, LayerId(1), &tokens, &paths, &mut NoAssets);
    assert!(
        read.parse_diagnostics.is_empty(),
        "{:?}",
        read.parse_diagnostics
    );
    let text = layerstack_usda::save_usda(&read.emitted.layer, &tokens, &paths).unwrap();
    assert_eq!(text, SOURCE, "layerstack's USDA of the text");

    let Some(python) = openusd_python() else {
        eprintln!("skipped dictionary oracle: set LAYERSTACK_USD_PYTHON");
        return;
    };
    let dir = scratch_dir().join("path_expressions");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.usda");
    let theirs = dir.join("openusd.usdc");
    std::fs::write(&source, SOURCE).unwrap();
    let exported = openusd(
        python.as_str(),
        "import sys\nfrom pxr import Sdf\nlayer = Sdf.Layer.FindOrOpen(sys.argv[1])\nassert layer.Export(sys.argv[2])\nprint(layer.ExportToString(), end='')",
        &[&source, &theirs],
    );
    assert_eq!(exported, SOURCE, "OpenUSD's USDA of the text");

    let bytes = std::fs::read(&theirs).unwrap();
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read =
        layerstack_usdc::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let text = layerstack_usda::save_usda(&read.layer, &tokens, &paths).unwrap();
    assert_eq!(text, SOURCE, "layerstack's USDA of OpenUSD's USDC");

    let ours_usda = dir.join("layerstack.usda");
    let ours_usdc = dir.join("layerstack.usdc");
    std::fs::write(&ours_usda, &text).unwrap();
    let crate_bytes = layerstack_usdc::save_usdc(&read.layer, &tokens, &paths).unwrap();
    std::fs::write(&ours_usdc, crate_bytes).unwrap();
    let exported = openusd(
        python.as_str(),
        "import sys\nfrom pxr import Sdf\nfor path in sys.argv[1:]:\n    print(Sdf.Layer.FindOrOpen(path).ExportToString(), end='\\0')",
        &[&ours_usda, &ours_usdc],
    );
    for (format, text) in ["usda", "usdc"].iter().zip(exported.split_terminator('\0')) {
        assert_eq!(text, SOURCE, "OpenUSD's USDA of layerstack's {format}");
    }
}

/// `customLayerData` holding entries with no value, at the top level and
/// in nested dictionaries, as usd-core 26.11's USDA writer writes it.
const WITHOUT_EMPTY_ENTRIES: &str = r#"#usda 1.0
(
    customLayerData = {
        dictionary allEmpty = {
        }
        int keep = 1
        dictionary nested = {
            int kept = 2
        }
    }
)

"#;

/// Builds `customLayerData` with entries with no value, in OpenUSD's
/// Python syntax; prints its USDA, then whether its USDC exported.
const EMPTY_ENTRIES_ORACLE: &str = r#"
from pxr import Sdf, Tf
layer = Sdf.Layer.CreateAnonymous(".usda")
layer.customLayerData = {"allEmpty": {"gone": None}, "empty": None, "keep": 1,
                         "nested": {"alsoEmpty": None, "kept": 2}}
print(layer.ExportToString(), end="\0")
import sys
try:
    print(layer.Export(sys.argv[1]))
except Tf.ErrorException:
    print("refused")
"#;

/// The USDA writer leaves out dictionary entries with no value, as
/// usd-core 26.11 does, and the USDC writer refuses them, as it does.
#[test]
fn empty_dictionary_entries_are_skipped_as_openusd_skips_them() {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let mut layer =
        layerstack_usda::read_usda("#usda 1.0\n", LayerId(1), &tokens, &paths, &mut NoAssets)
            .emitted
            .layer;
    let entry = |key: &str, value: Value| (key.into(), value);
    layer.set_metadata(
        tokens.intern("customLayerData"),
        Value::Dictionary(vec![
            entry(
                "allEmpty",
                Value::Dictionary(vec![entry("gone", Value::Null)]),
            ),
            entry("empty", Value::Null),
            entry("keep", Value::Int(1)),
            entry(
                "nested",
                Value::Dictionary(vec![
                    entry("alsoEmpty", Value::Null),
                    entry("kept", Value::Int(2)),
                ]),
            ),
        ]),
    );
    let text = layerstack_usda::save_usda(&layer, &tokens, &paths).unwrap();
    assert_eq!(text, WITHOUT_EMPTY_ENTRIES);
    let refused = layerstack_usdc::save_usdc(&layer, &tokens, &paths);
    assert!(
        matches!(refused, Err(UsdcWriteError::Save(_))),
        "{refused:?}"
    );

    let Some(python) = openusd_python() else {
        eprintln!("skipped dictionary oracle: set LAYERSTACK_USD_PYTHON");
        return;
    };
    let dir = scratch_dir().join("empty_entries");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let output = openusd(
        python.as_str(),
        EMPTY_ENTRIES_ORACLE,
        &[&dir.join("openusd.usdc")],
    );
    let (usda, usdc) = output.split_once('\0').unwrap();
    assert_eq!(usda, WITHOUT_EMPTY_ENTRIES, "OpenUSD's USDA");
    assert_eq!(usdc.trim(), "refused", "OpenUSD's USDC");
}
