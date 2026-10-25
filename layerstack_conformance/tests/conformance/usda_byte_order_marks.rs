// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA layers that begin with a byte order mark, against usd-core 26.11.
//!
//! OpenUSD 26.11 refuses a `.usda` file that begins with a UTF-8, UTF-16 or
//! UTF-32 byte order mark before parsing it, naming the mark
//! (`_CheckBOM`, `pxr/usd/sdf/usdaFileFormat.cpp`); a string imported with
//! a UTF-8 mark fails the header rule at 1:1. layerstack refuses the file
//! with the same reason and reports the string's mark as its one parse
//! error.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, StageOptions,
    TokenInterner,
};
use layerstack_io::{Filesystem, IoErrorKind, StageDocument};
use layerstack_usda::diagnostic::Severity;

/// Each mark's file name and leading bytes.
const MARKS: [(&str, &[u8]); 5] = [
    ("utf8", b"\xEF\xBB\xBF"),
    ("utf16be", b"\xFE\xFF"),
    ("utf16le", b"\xFF\xFE"),
    ("utf32be", b"\0\0\xFE\xFF"),
    ("utf32le", b"\xFF\xFE\0\0"),
];

/// The layer after each mark.
const BODY: &str = "#usda 1.0\ndef \"A\" {}\n";

/// Prints, for each file, OpenUSD's reason for refusing it (or `opened`),
/// then whether a string with a UTF-8 mark imports.
const ORACLE: &str = r#"
import sys
from pxr import Sdf
for path in sys.argv[1:]:
    try:
        Sdf.Layer.FindOrOpen(path)
        print("opened")
    except Exception as error:
        text = str(error)
        print(text[text.index("is not a valid usda layer"):].rstrip("'\n"))
layer = Sdf.Layer.CreateAnonymous(".usda")
try:
    print("imported" if layer.ImportFromString("﻿" + sys.stdin.read()) else "refused")
except Exception:
    print("refused")
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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("usda_byte_order_marks")
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

/// layerstack's reason for refusing `name`, in OpenUSD's words after the
/// layer's identifier.
fn layerstack_refusal(dir: &Path, name: &str) -> String {
    let storage = Filesystem::new(dir, []).unwrap();
    let Err(error) = StageDocument::open(storage, name, StageOptions::default()) else {
        panic!("{name} opened");
    };
    assert_eq!(error.kind, IoErrorKind::Rejected, "{name}");
    let at = error
        .message
        .find("is not a valid usda layer")
        .unwrap_or_else(|| panic!("{name}: {}", error.message));
    error.message[at..].to_owned()
}

/// Each marked file is refused for the reason OpenUSD gives.
#[test]
fn marked_files_are_refused_as_openusd_refuses_them() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (name, mark) in MARKS {
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, [mark, BODY.as_bytes()].concat()).unwrap();
        paths.push(path);
    }
    let ours: Vec<String> = MARKS
        .iter()
        .map(|(name, _)| layerstack_refusal(&dir, &format!("{name}.usda")))
        .collect();
    assert_eq!(
        ours[0],
        "is not a valid usda layer: Asset starts with UTF-8 BOM which is not supported, \
         please convert the file to UTF-8 without the BOM."
    );
    let Some(python) = openusd_python() else {
        eprintln!("skipped byte order mark oracle: set LAYERSTACK_USD_PYTHON");
        return;
    };
    let mut child = Command::new(python)
        .args(["-c", ORACLE])
        .args(&paths)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(BODY.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    for ((name, _), ours) in MARKS.iter().zip(&ours) {
        assert_eq!(lines.next(), Some(ours.as_str()), "{name}");
    }
    assert_eq!(lines.next(), Some("refused"), "OpenUSD's string import");
    assert!(string_import_is_refused(), "layerstack's string import");
}

/// Whether layerstack's string import reports the UTF-8 mark as its one
/// error, which a strict open refuses.
fn string_import_is_refused() -> bool {
    let source = format!("\u{feff}{BODY}");
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read = layerstack_usda::read_usda(&source, LayerId(1), &tokens, &paths, &mut NoAssets);
    let [diagnostic] = read.parse_diagnostics.as_slice() else {
        panic!("diagnostics: {:?}", read.parse_diagnostics);
    };
    assert_eq!(
        (diagnostic.span.start, diagnostic.span.end),
        (0, 3),
        "the mark's bytes"
    );
    assert!(
        diagnostic.message.contains("byte order mark"),
        "{diagnostic:?}"
    );
    diagnostic.severity == Severity::Error
}
