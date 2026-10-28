// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Names with bytes from 0x80 up, in the orders OpenUSD 26.11 writes and
//! sorts them, against usd-core.
//!
//! OpenUSD 26.11 orders tokens by unsigned byte, as `strcmp` does
//! (`TfToken::operator<`, `pxr/base/tf/token.h`), where 26.08 sorted UTF-8
//! names apart from it. That order sorts paths (`SdfPath::operator<` compares
//! prim names as tokens) and the USDA layer metadata section; dictionary
//! keys and variant selections (`std::map` of `std::string`) were already
//! in byte order, and properties go in `TfDictionaryLessThan` order, which
//! did not change. layerstack orders all of these by byte, or by
//! `dictionary_cmp`, as each is.
//!
//! OpenUSD 26.11 also makes a token the text before any NUL, so a USDA
//! `token` value with an escaped NUL reads as its prefix (26.08 refused
//! the layer); `TokenInterner::intern` does the same.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::process::{Command, Stdio};

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, Path, PathInterner, ResolvedAsset, TokenInterner,
};

/// A layer whose dictionary keys, variant sets and properties mix ASCII
/// and UTF-8 names, as usd-core 26.11 writes it.
const SOURCE: &str = r#"#usda 1.0

def "Root" (
    customData = {
        int Z = 5
        int a = 3
        int z = 1
        int "é" = 2
        int "ü" = 6
        int "Ω" = 4
    }
    variants = {
        string alpha = "w"
        string zinc = "y"
        string "ébène" = "x"
    }
    prepend variantSets = ["zinc", "ébène", "alpha"]
)
{
    custom int a = 3
    custom int e = 7
    custom int Z = 5
    custom int z = 1
    custom int é = 2
    custom int ü = 6
    custom int Ω = 4

    def "Ωmega"
    {
    }

    def "émile"
    {
    }

    def "zed"
    {
    }
}

"#;

/// Root prim names to sort as paths.
const NAMES: [&str; 12] = [
    "z", "é", "a", "Ω", "Z", "ü", "e", "ab", "日本", "aé", "ÿ", "Ā",
];

/// Prints usd-core's USDA of the layer on stdin, then each argument's root
/// prim path, as `sorted` orders `Sdf.Path`s.
const ORACLE: &str = r#"
import sys
from pxr import Sdf
layer = Sdf.Layer.CreateAnonymous(".usda")
layer.ImportFromString(sys.stdin.read())
print(layer.ExportToString(), end="\0")
print(" ".join(str(path) for path in sorted(Sdf.Path("/" + name) for name in sys.argv[1:])))
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

/// layerstack's root prim paths for [`NAMES`], in namespace order.
fn sorted_paths() -> String {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let mut ids: Vec<_> = NAMES
        .iter()
        .map(|name| paths.intern(Path::parse_absolute(&format!("/{name}"), &tokens).unwrap()))
        .collect();
    paths.sort_paths(&mut ids, &tokens);
    ids.iter()
        .map(|id| paths.display(*id, &tokens))
        .collect::<Vec<_>>()
        .join(" ")
}

/// layerstack writes the layer in usd-core 26.11's order and sorts paths
/// with UTF-8 names by byte, as usd-core 26.11 does.
#[test]
fn utf8_names_order_as_openusd_orders_them() {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read = layerstack_usda::read_usda(SOURCE, LayerId(1), &tokens, &paths, &mut NoAssets);
    assert!(
        read.parse_diagnostics.is_empty(),
        "{:?}",
        read.parse_diagnostics
    );
    let text = layerstack_usda::save_usda(&read.emitted.layer, &tokens, &paths).unwrap();
    assert_eq!(text, SOURCE, "layerstack's USDA");
    let ours = sorted_paths();
    assert_eq!(ours, "/Z /a /ab /aé /e /z /é /ü /ÿ /Ā /Ω /日本");

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
        eprintln!("skipped token order oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let mut child = Command::new(python)
        .args(["-c", ORACLE])
        .args(NAMES)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(SOURCE.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (usda, sorted) = stdout.split_once('\0').unwrap();
    assert_eq!(usda, SOURCE, "OpenUSD's USDA");
    assert_eq!(sorted.trim_end(), ours, "OpenUSD's path order");
}

/// Token values with escaped NULs: OpenUSD 26.11 reads each as the token
/// of its text before the NUL, where 26.08 refused the layer.
const WITH_NULS: &str = "#usda 1.0\n\ndef \"R\" (\n    customData = {\n        token tk = \"t\\x00k\"\n    }\n)\n{\n    token t = \"a\\x00b\"\n    token[] ts = [\"x\\x00y\", \"x\"]\n}\n";

/// [`WITH_NULS`] as usd-core 26.11 writes it.
const WITHOUT_NULS: &str = r#"#usda 1.0

def "R" (
    customData = {
        token tk = "t"
    }
)
{
    token t = "a"
    token[] ts = ["x", "x"]
}

"#;

/// A NUL ends a token, as it ends usd-core 26.11's.
#[test]
fn embedded_nuls_end_tokens_as_openusd_ends_them() {
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read = layerstack_usda::read_usda(WITH_NULS, LayerId(1), &tokens, &paths, &mut NoAssets);
    assert!(
        read.parse_diagnostics.is_empty(),
        "{:?}",
        read.parse_diagnostics
    );
    let text = layerstack_usda::save_usda(&read.emitted.layer, &tokens, &paths).unwrap();
    assert_eq!(text, WITHOUT_NULS, "layerstack's USDA");

    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args(["-c", "from pxr import Sdf"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped embedded NUL oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let mut child = Command::new(python)
        .args([
            "-c",
            "import sys\nfrom pxr import Sdf\nlayer = Sdf.Layer.CreateAnonymous('.usda')\nlayer.ImportFromString(sys.stdin.read())\nprint(layer.ExportToString(), end='')",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(WITH_NULS.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        WITHOUT_NULS,
        "OpenUSD's USDA"
    );
}
