// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim relocates, which OpenUSD 26.11 removed from Sdf, against usd-core.
//!
//! In USDA a `relocates` statement in prim or variant metadata no longer
//! parses (`PrimMetadataItem`, `pxr/usd/sdf/textFileFormatParser.h`): the
//! import fails with "Expected )" at the statement, where layerstack
//! reports its one parse error. Layer relocates still parse.
//!
//! A USDC prim spec with a `relocates` field still opens in OpenUSD 26.11,
//! which keeps the field as one its schema does not register and composes
//! without it. layerstack composes without it too, and reports the field
//! it leaves out.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, Specifier,
    TokenInterner,
};
use layerstack_usda::TextPosition;
use layerstack_usdc::writer::{Spec, SpecForm, Value, write_crate};

/// Layers whose prim or variant metadata authors relocates.
const REFUSED: [&str; 3] = [
    "#usda 1.0\ndef \"A\" (\n    relocates = {\n        <B>: <D>\n    }\n)\n{\n    def \"B\" {}\n}\n",
    "#usda 1.0\ndef \"A\" (\n    kind = \"group\"\n    relocates = {\n    }\n)\n{\n}\n",
    "#usda 1.0\nover \"A\" {\n    variantSet \"v\" = {\n        \"x\" (\n            relocates = { <B>: <C> }\n        ) {}\n    }\n}\n",
];

/// A layer whose layer metadata authors relocates.
const ACCEPTED: &str = "#usda 1.0\n(\n    relocates = {\n        </A/B>: </A/C>\n    }\n)\ndef \"A\" {\n    def \"B\" {}\n}\n";

/// For each USDA source on stdin (NUL separated), where its import fails
/// (`line:column`) or `imported`; then the prims of the USDC layer named by
/// the argument, as a stage composes them.
const ORACLE: &str = r#"
import re, sys
from pxr import Sdf, Usd
for source in sys.stdin.read().split("\0"):
    layer = Sdf.Layer.CreateAnonymous(".usda")
    try:
        layer.ImportFromString(source)
        print("imported")
    except Exception as error:
        print(re.search(r"'[^:']*:(\d+:\d+): Expected \)", str(error)).group(1))
stage = Usd.Stage.Open(Sdf.Layer.FindOrOpen(sys.argv[1]))
print(" ".join(str(prim.GetPath()) for prim in stage.Traverse()))
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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("prim_relocates")
}

/// Where layerstack's import of `source` fails (`line:column`), or
/// `imported`.
fn layerstack_import(source: &str) -> String {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
    match read.parse_diagnostics.as_slice() {
        [] => "imported".into(),
        [diagnostic] => {
            assert!(diagnostic.message.contains("relocates"), "{diagnostic:?}");
            let at = TextPosition::from_offset(source, diagnostic.span.start);
            format!("{}:{}", at.line, at.column)
        }
        diagnostics => panic!("{source}: {diagnostics:?}"),
    }
}

/// A crate layer `/A` (with a `relocates` field) over `/A/B`.
fn crate_with_prim_relocates() -> Vec<u8> {
    let prim = |path: &str| {
        Spec::new(path, SpecForm::Prim).with_field("specifier", Value::Specifier(Specifier::Def))
    };
    write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot)
            .with_field("primChildren", Value::TokenVector(vec!["A".into()])),
        prim("/A")
            .with_field(
                "relocates",
                Value::Relocates(vec![("/A/B".into(), "/A/C".into())]),
            )
            .with_field("primChildren", Value::TokenVector(vec!["B".into()])),
        prim("/A/B"),
    ])
    .unwrap()
}

/// layerstack refuses prim and variant relocates where OpenUSD does, keeps
/// layer relocates, and composes a USDC prim's relocates field away as
/// OpenUSD does.
#[test]
fn prim_relocates_are_refused_as_openusd_refuses_them() {
    let ours: Vec<String> = REFUSED
        .iter()
        .chain([&ACCEPTED])
        .map(|source| layerstack_import(source))
        .collect();
    assert_eq!(ours, ["3:5", "4:5", "5:13", "imported"]);

    let bytes = crate_with_prim_relocates();
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read =
        layerstack_usdc::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
    let reported: Vec<(&str, Option<&str>)> = read
        .diagnostics
        .iter()
        .map(|d| (d.spec_path.as_str(), d.field.as_deref()))
        .collect();
    assert_eq!(reported, [("/A", Some("relocates"))]);
    let text = layerstack_usda::save::save_usda(&read.layer, &tokens, &paths).unwrap();
    assert!(!text.contains("relocates"), "{text}");
    let our_prims: Vec<String> = ["/A", "/A/B"]
        .into_iter()
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap();
            text.contains(&format!("def \"{name}\""))
        })
        .map(String::from)
        .collect();

    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args(["-c", "from pxr import Sdf"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    if !available {
        eprintln!("skipped prim relocates oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let usdc = dir.join("prim_relocates.usdc");
    std::fs::write(&usdc, bytes).unwrap();
    let stdin: Vec<&str> = REFUSED.iter().copied().chain([ACCEPTED]).collect();
    let output = run_oracle(python, &usdc, &stdin.join("\0"));
    let mut lines = output.lines();
    for (source, ours) in stdin.iter().zip(&ours) {
        assert_eq!(lines.next(), Some(ours.as_str()), "{source}");
    }
    assert_eq!(lines.next(), Some(our_prims.join(" ").as_str()));
}

/// Runs [`ORACLE`] with `usdc` as its argument and `stdin`; its stdout.
fn run_oracle(python: &str, usdc: &Path, stdin: &str) -> String {
    let mut child = Command::new(python)
        .args(["-c", ORACLE])
        .arg(usdc)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
