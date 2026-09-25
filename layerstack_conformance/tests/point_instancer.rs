// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `PointInstancer` export checked by OpenUSD.
//!
//! A scattered field of three prototypes ([`Field`]) is written as USDA,
//! USDC and a generic USDZ package (`ARKit` packages write instanced
//! references instead; `tests/instanced_references.rs` checks those).
//! OpenUSD's Python bindings (the Python named
//! by `LAYERSTACK_USD_PYTHON`, or `python3`, when it imports `pxr`) read
//! each file through `scripts/point_instancer_oracle.py`: every instance
//! transform `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`
//! computes must match the one the inputs define, and the prototype order,
//! ids, extent and prototype material bindings must be as authored.
//! `usdchecker`, when on `PATH`, must accept each file. Tools older than
//! the release a check
//! needs (`export_fixtures::minimum_openusd`) are skipped with the reason
//! printed; without the tools the tests report that they skipped and pass.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use layerstack::half;
use layerstack_conformance::export_fixtures::{
    OpenUsdRelease, minimum_openusd, parse_openusd_release,
};
use layerstack_conformance::instancer_fixtures::{Field, PINE, SHRUB};
use layerstack_mesh_export::{Node, OrientationPrecision, UsdzProfile};
use serde::Deserialize;

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    let dir = base.join(format!("point-instancer-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Why a tool reporting `version` cannot check fixture `name`, or `None`
/// when it can. An unreadable version counts as too old.
fn too_old(version: &str, name: &str) -> Option<String> {
    let (minimum, reason) = minimum_openusd(name)?;
    let release: Option<OpenUsdRelease> = parse_openusd_release(version);
    match release {
        Some(release) if release >= minimum => None,
        _ => Some(format!("{version:?} is older than {minimum:?}: {reason}")),
    }
}

/// Writes the field as USDA, USDC and a generic USDZ package into `dir`.
fn write_field(field: &Field, dir: &Path) -> Vec<PathBuf> {
    let scene = field.scene();
    let files = [
        ("field.usda", scene.to_usda().unwrap().into_bytes()),
        ("field.usdc", scene.to_usdc().unwrap()),
        (
            "field.usdz",
            scene.to_usdz(UsdzProfile::Generic, &[]).unwrap(),
        ),
    ];
    files
        .into_iter()
        .map(|(name, bytes)| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect()
}

fn usd_python() -> Option<String> {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .ok()?;
    out.status.success().then_some(python)
}

#[derive(Deserialize)]
struct Report {
    version: String,
    layers: Vec<LayerReport>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayerReport {
    prototypes: Vec<String>,
    proto_indices: Vec<u32>,
    ids: Vec<i64>,
    extent: Vec<[f64; 3]>,
    computed_extent: Vec<[f64; 3]>,
    transforms: Vec<[f64; 16]>,
    bindings: Vec<[String; 2]>,
    orientationsf: Vec<[f64; 4]>,
    orientations: Vec<[f64; 4]>,
}

const PROTOTYPES: [&str; 3] = [
    "/Root/Field/Prototypes/Pine",
    "/Root/Field/Prototypes/Boulder",
    "/Root/Field/Prototypes/Shrub",
];

/// The prototype prims that bind a material, and the material each one
/// binds: the boulder binds none.
const BINDINGS: [[&str; 2]; 4] = [
    ["/Root/Field/Prototypes/Pine/Trunk", "/Root/Materials/Bark"],
    [
        "/Root/Field/Prototypes/Pine/Crown/Sides",
        "/Root/Materials/Needles",
    ],
    [
        "/Root/Field/Prototypes/Pine/Crown/Base",
        "/Root/Materials/Shade",
    ],
    [
        "/Root/Field/Prototypes/Shrub/Leaves",
        "/Root/Materials/Needles",
    ],
];

#[test]
fn instance_transforms_match_openusd() {
    let Some(python) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    let field = Field::new(8, 8);
    assert!(
        field.proto_indices.contains(&PINE) && field.proto_indices.contains(&SHRUB),
        "the field mixes prototypes"
    );
    let dir = scratch_dir("oracle");
    let mut layers = write_field(&field, &dir);
    // Both orientation attributes: readers must prefer `orientationsf`.
    let mut both = field.scene();
    let Node::PointInstancer(instancer) = &mut both.root.children[0] else {
        unreachable!("the field is one instancer");
    };
    instancer.orientation_precision = OrientationPrecision::FloatAndHalf;
    let both_path = dir.join("field_both.usda");
    std::fs::write(&both_path, both.to_usda().unwrap()).unwrap();
    layers.push(both_path.clone());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/point_instancer_oracle.py");
    let out = Command::new(&python)
        .arg(&script)
        .arg("/Root/Field")
        .args(&layers)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Report = serde_json::from_slice(&out.stdout).unwrap();
    if let Some(reason) = too_old(&report.version, "point_instancer") {
        eprintln!("skipped: OpenUSD {reason}");
        return;
    }
    eprintln!("OpenUSD {} via {python}", report.version);

    let expected = field.instance_transforms();
    let authored = field.scene().to_usda().unwrap();
    for (layer, got) in layers.iter().zip(&report.layers) {
        let name = layer.display();
        assert_eq!(got.prototypes, PROTOTYPES, "{name}: prototype order");
        assert_eq!(
            got.proto_indices, field.proto_indices,
            "{name}: protoIndices"
        );
        assert_eq!(got.ids, field.ids, "{name}: ids");
        // `orientationsf` round-trips exactly; `orientations` is authored
        // only alongside it, rounded to `half`, and does not win.
        let exact: Vec<[f64; 4]> = field
            .orientations
            .iter()
            .map(|q| q.map(f64::from))
            .collect();
        assert_eq!(got.orientationsf, exact, "{name}: orientationsf");
        if *layer == both_path {
            let rounded: Vec<[f64; 4]> = field
                .orientations
                .iter()
                .map(|q| q.map(|c| f64::from(half::to_f32(half::from_f32(c)))))
                .collect();
            assert_eq!(got.orientations, rounded, "{name}: orientations");
            assert_ne!(rounded, exact, "{name}: the half values differ");
        } else {
            assert!(got.orientations.is_empty(), "{name}: no quath");
        }
        assert_eq!(got.transforms.len(), expected.len(), "{name}: instances");
        for (i, (m, want)) in got.transforms.iter().zip(&expected).enumerate() {
            for (k, value) in m.iter().enumerate() {
                let want = want[k / 4][k % 4];
                assert!(
                    (value - want).abs() <= 1e-12 * want.abs().max(1.0),
                    "{name}: instance {i} matrix element {k}: OpenUSD {value}, expected {want}"
                );
            }
        }
        // The authored extent is exactly what the USDA says, and agrees
        // with OpenUSD's own computation from the prototype bounds, up to
        // the rounding of `float` extents.
        let text_extent = authored_extent(&authored);
        assert_eq!(got.extent, text_extent, "{name}: authored extent");
        assert_eq!(got.computed_extent.len(), 2, "{name}: computed extent");
        for (a, c) in got
            .extent
            .iter()
            .flatten()
            .zip(got.computed_extent.iter().flatten())
        {
            assert!(
                (a - c).abs() <= 1e-5 * c.abs().max(1.0),
                "{name}: extent {:?}, OpenUSD computes {:?}",
                got.extent,
                got.computed_extent
            );
        }
        let bindings: Vec<[&str; 2]> = got
            .bindings
            .iter()
            .map(|[p, m]| [p.as_str(), m.as_str()])
            .collect();
        assert_eq!(bindings, BINDINGS, "{name}: prototype material bindings");
    }
}

/// The instancer's `extent` in USDA text, as numbers: the first one
/// written, since the instancer's properties precede its prototypes.
fn authored_extent(text: &str) -> Vec<[f64; 3]> {
    let line = text
        .lines()
        .find(|l| l.contains("float3[] extent"))
        .unwrap();
    let numbers: Vec<f64> = line
        .split_once('=')
        .unwrap()
        .1
        .split(['[', ']', '(', ')', ',', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| f64::from(w.parse::<f32>().unwrap()))
        .collect();
    numbers.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
}

fn usdchecker(args: &[&Path]) -> Result<(), String> {
    let out = Command::new("usdchecker").args(args).output().unwrap();
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

#[test]
fn usdchecker_accepts_the_field() {
    let Some(version) = Command::new("usdchecker")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    else {
        eprintln!("skipped: usdchecker is not on PATH");
        return;
    };
    if let Some(reason) = too_old(&version, "point_instancer") {
        eprintln!("skipped: usdchecker {reason}");
        return;
    }
    eprintln!("usdchecker: {version}");
    let dir = scratch_dir("checker");
    let layers = write_field(&Field::new(8, 8), &dir);
    let mut failures = Vec::new();
    for layer in &layers {
        if let Err(e) = usdchecker(&[layer]) {
            failures.push(format!("usdchecker {}: {e}", layer.display()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Instancing writes each prototype once: the files are much smaller than
/// the same placements with duplicated geometry, and cheaper to build.
/// Sizes and times are printed; only sizes are asserted.
#[test]
fn instancing_is_smaller_than_duplicated_geometry() {
    let field = Field::new(20, 20);
    let names = field.instance_names();
    let instanced = field.scene();
    let duplicated = field.duplicated_scene(&names);

    let start = Instant::now();
    let instanced_usdc = instanced.to_usdc().unwrap();
    let instanced_usdc_time = start.elapsed();
    let start = Instant::now();
    let duplicated_usdc = duplicated.to_usdc().unwrap();
    let duplicated_usdc_time = start.elapsed();
    let instanced_usda = instanced.to_usda().unwrap().len();
    let duplicated_usda = duplicated.to_usda().unwrap().len();
    eprintln!(
        "{} instances: USDC {} vs {} bytes ({:?} vs {:?}); USDA {} vs {} bytes",
        field.proto_indices.len(),
        instanced_usdc.len(),
        duplicated_usdc.len(),
        instanced_usdc_time,
        duplicated_usdc_time,
        instanced_usda,
        duplicated_usda,
    );
    assert!(
        instanced_usdc.len() * 3 < duplicated_usdc.len(),
        "USDC size"
    );
    assert!(instanced_usda * 3 < duplicated_usda, "USDA size");
}
