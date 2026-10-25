// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `duration` and `duration[]` values (OpenUSD 26.11's `GfDuration` and
//! `VtArray<GfDuration>`) through the USDA and USDC readers and writers,
//! against usd-core.
//!
//! OpenUSD writes a duration as its double: in USDA with `#usda 1.4`
//! (`_StringFromVtValueHelper`, `pxr/usd/sdf/fileIO_Common.cpp`), in USDC
//! as value type 61 with crate version 0.16.0 (`Write(GfDuration)`,
//! `pxr/usd/sdf/crateFile.cpp`). `time_values` reads them through layer
//! offsets and value clips.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
};

/// The layer usd-core writes as USDA and USDC, and the text it writes.
const SOURCE: &str = r#"#usda 1.4

def "P" (
    customData = {
        duration dur = 4
        dictionary nested = {
            duration[] spans = [1, 0.0999756]
        }
    }
)
{
    custom duration d = 2.5
    duration d.timeSamples = {
        3: -1,
    }
    custom duration[] ds = [1, 0.125, 1e+300, -0, inf]
    custom duration small = 24
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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("durations")
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

/// usd-core's USDC of [`SOURCE`] reads as the same values, and the USDA
/// and USDC layerstack writes from it open in usd-core as that layer.
#[test]
fn durations_round_trip_with_openusd() {
    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args(["-c", "from pxr import Gf; Gf.Duration"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr with Gf.Duration"
    );
    if !available {
        eprintln!("skipped duration oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.usda");
    let theirs = dir.join("openusd.usdc");
    std::fs::write(&source, SOURCE.replace("#usda 1.4", "#usda 1.0")).unwrap();
    openusd(
        python,
        "import sys\nfrom pxr import Sdf\nassert Sdf.Layer.FindOrOpen(sys.argv[1]).Export(sys.argv[2])",
        &[&source, &theirs],
    );

    let bytes = std::fs::read(&theirs).unwrap();
    assert_eq!(&bytes[8..11], &[0, 16, 0], "OpenUSD writes crate 0.16.0");
    let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    let read =
        layerstack_usdc::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
    let text = layerstack_usda::save::save_usda(&read.layer, &tokens, &paths).unwrap();
    assert_eq!(text, SOURCE, "layerstack's USDA of OpenUSD's USDC");

    let ours_usda = dir.join("layerstack.usda");
    let ours_usdc = dir.join("layerstack.usdc");
    std::fs::write(&ours_usda, &text).unwrap();
    let crate_bytes = layerstack_usdc::writer::save_layer(&read.layer, &tokens, &paths).unwrap();
    assert_eq!(
        &crate_bytes[8..11],
        &[0, 16, 0],
        "layerstack writes crate 0.16.0"
    );
    std::fs::write(&ours_usdc, crate_bytes).unwrap();
    let exported = openusd(
        python,
        "import sys\nfrom pxr import Sdf\nfor path in sys.argv[1:]:\n    print(Sdf.Layer.FindOrOpen(path).ExportToString(), end='\\0')",
        &[&ours_usda, &ours_usdc],
    );
    for (format, text) in ["usda", "usdc"].iter().zip(exported.split_terminator('\0')) {
        assert_eq!(text, SOURCE, "OpenUSD's USDA of layerstack's {format}");
    }
}
