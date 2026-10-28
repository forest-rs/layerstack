// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD's own USDA parsing corpus, read and written as OpenUSD 26.11
//! reads and writes it.
//!
//! `pxr/usd/sdf/testenv/testSdfParsing.testenv` holds the layers OpenUSD's
//! `testSdfParsing` reads: some it reads, some it refuses. With
//! `LAYERSTACK_OPENUSD_SOURCE` naming an OpenUSD 26.11 checkout and a
//! Python with OpenUSD's `pxr` (`LAYERSTACK_USD_PYTHON`, or `python3`), the
//! test has OpenUSD export each corpus file (`Sdf.Layer.OpenAsAnonymous`,
//! `ExportToString`, without the corpus's `plugInfo.json`, so its test
//! metadata is unregistered) and requires of layerstack, file by file:
//!
//! - a file OpenUSD reads: [`read_usda`] reports no error, so a strict
//!   import reads it, and [`save_usda`] writes OpenUSD's text byte for
//!   byte;
//! - a file OpenUSD refuses: the reader reports an error, so a strict
//!   import refuses it.
//!
//! [`DIFFERENCES`] lists the files layerstack deliberately or knowingly
//! handles otherwise, each with its reason and outcome. A listed file
//! whose outcome changes fails the test too, so the list stays current.
//! The corpus is not copied into this repository; without the checkout or
//! the Python the test is skipped.
//!
//! Spec: AOUSD Core §16 (the USDA text format).
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset,
    TokenInterner,
};
use layerstack_usda::diagnostic::Severity;
use layerstack_usda::read_usda;
use layerstack_usda::save::save_usda;

/// What became of one corpus file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// OpenUSD reads it; layerstack reads it and writes OpenUSD's text.
    Same,
    /// OpenUSD reads it; layerstack reads it and writes other text.
    Differs,
    /// OpenUSD reads it; layerstack reports an error or cannot save it.
    Error,
    /// OpenUSD and layerstack both refuse it.
    Refuses,
    /// OpenUSD refuses it; layerstack reads it without an error.
    Accepts,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Same => "same",
            Self::Differs => "differs",
            Self::Error => "error",
            Self::Refuses => "refuses",
            Self::Accepts => "accepts",
        })
    }
}

/// A corpus file layerstack handles otherwise than OpenUSD.
struct Difference {
    /// The corpus file name.
    file: &'static str,
    /// What layerstack does with it.
    outcome: Outcome,
    /// Why.
    reason: &'static str,
}

/// The corpus files layerstack handles otherwise than OpenUSD 26.11, in
/// name order.
const DIFFERENCES: &[Difference] = &[
    Difference {
        file: "127_varyingRelationship.usda",
        outcome: Outcome::Error,
        reason: "a relationship's legacy `.default = <path>` is reported: the layer \
                 has no path value to keep it",
    },
    Difference {
        file: "185_namespaced_properties.usda",
        outcome: Outcome::Error,
        reason: "a relationship's legacy `.default = <path>`, as in \
                 `127_varyingRelationship`",
    },
    Difference {
        file: "46_weirdStringContent.usda",
        outcome: Outcome::Error,
        reason: "string escapes that make bytes that are not UTF-8 are reported: a \
                 layerstack string is UTF-8",
    },
];

/// Exports each `.usda` file of the directory `argv[1]` into `argv[2]`, as
/// `<name>` with OpenUSD's text or `<name>.error` when OpenUSD refuses it.
const EXPORT: &str = r#"
import os, sys
from pxr import Sdf, Tf
src, out = sys.argv[1], sys.argv[2]
for name in sorted(os.listdir(src)):
    if not name.endswith(".usda"):
        continue
    try:
        layer = Sdf.Layer.OpenAsAnonymous(os.path.join(src, name))
        text = layer.ExportToString() if layer else None
    except Tf.ErrorException:
        text = None
    if text is None:
        open(os.path.join(out, name + ".error"), "w").close()
    else:
        with open(os.path.join(out, name), "w") as f:
            f.write(text)
"#;

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

/// The corpus directory and OpenUSD's Python, or `None` (skipping).
fn setup() -> Option<(PathBuf, String)> {
    let Some(source) = std::env::var_os("LAYERSTACK_OPENUSD_SOURCE").map(PathBuf::from) else {
        eprintln!("skipped: set LAYERSTACK_OPENUSD_SOURCE to an OpenUSD 26.11 checkout");
        return None;
    };
    let corpus = source.join("pxr/usd/sdf/testenv/testSdfParsing.testenv");
    assert!(corpus.is_dir(), "{} is not a directory", corpus.display());
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
    if !available {
        eprintln!("skipped: set LAYERSTACK_USD_PYTHON to a Python with OpenUSD's pxr");
        return None;
    }
    Some((corpus, python))
}

/// layerstack's text of `source`, or `None` when it reports an error or
/// cannot save the layer.
fn layerstack_text(source: &[u8]) -> Option<String> {
    let source = std::str::from_utf8(source).ok()?;
    let store = InMemoryStore::default();
    let read = read_usda(
        source,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    );
    let errors = read
        .parse_diagnostics
        .iter()
        .chain(&read.lower_diagnostics)
        .chain(&read.emitted.diagnostics)
        .any(|d| d.severity == Severity::Error);
    if errors {
        return None;
    }
    save_usda(&read.emitted.layer, &store.tokens, &store.paths).ok()
}

fn outcome(file: &Path, openusd: Option<&str>) -> Outcome {
    let source = std::fs::read(file).unwrap();
    match (openusd, layerstack_text(&source)) {
        (Some(expected), Some(text)) if text == expected => Outcome::Same,
        (Some(_), Some(_)) => Outcome::Differs,
        (Some(_), None) => Outcome::Error,
        (None, Some(_)) => Outcome::Accepts,
        (None, None) => Outcome::Refuses,
    }
}

/// Every corpus file reads and writes as OpenUSD's, or is refused as
/// OpenUSD refuses it, but for [`DIFFERENCES`]. All files run; the
/// failure lists every one that does not match.
#[test]
fn corpus_reads_and_writes_as_openusd() {
    assert!(
        DIFFERENCES.windows(2).all(|w| w[0].file < w[1].file),
        "DIFFERENCES is in name order"
    );
    assert!(DIFFERENCES.iter().all(|d| !d.reason.is_empty()));
    let Some((corpus, python)) = setup() else {
        return;
    };
    let exported = Path::new(env!("CARGO_TARGET_TMPDIR")).join("usda_corpus");
    let _ = std::fs::remove_dir_all(&exported);
    std::fs::create_dir_all(&exported).unwrap();
    let status = Command::new(&python)
        .args(["-c", EXPORT])
        .arg(&corpus)
        .arg(&exported)
        .status()
        .unwrap();
    assert!(status.success(), "OpenUSD's export failed");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "usda"))
        .collect();
    files.sort();
    assert!(files.len() > 150, "{} corpus files", files.len());

    let expected: BTreeMap<&str, Outcome> =
        DIFFERENCES.iter().map(|d| (d.file, d.outcome)).collect();
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_str().unwrap();
        let openusd = std::fs::read_to_string(exported.join(name)).ok();
        let got = outcome(file, openusd.as_deref());
        *tally.entry(got.to_string()).or_default() += 1;
        let want = expected.get(name).copied().unwrap_or(if openusd.is_some() {
            Outcome::Same
        } else {
            Outcome::Refuses
        });
        if got != want {
            failures.push(format!("{name}: {got}, expected {want}"));
        }
    }
    for name in expected.keys() {
        if !files.iter().any(|f| f.file_name().unwrap() == *name) {
            failures.push(format!("{name}: listed in DIFFERENCES, not in the corpus"));
        }
    }
    eprintln!("corpus outcomes: {tally:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
