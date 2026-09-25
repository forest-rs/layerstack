// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Independent check of exporter output with the OpenUSD tools `usdcat` and
//! `usdchecker`, when they are on `PATH`.
//!
//! The tools are optional: without them these tests report that they
//! skipped and pass, so CI needs no USD installation. The fuller gate (tool
//! versions, `--arkit` validators, ZIP layout checks, a render) is
//! `layerstack_conformance/scripts/export_interop.sh`.
//!
//! The writers target OpenUSD 26.08 (`export_fixtures::TARGET_OPENUSD`):
//! metadata is stored with the types that release registers. A fixture
//! whose metadata an older release does not register (see
//! `export_fixtures::minimum_openusd`) is compared only with a tool at
//! least that new; with an older one, such as the `usdcat` a system image
//! provides, it is skipped with the reason printed. Every other comparison
//! is exact whatever the tool's version.
//!
//! The authored-layer save round trip uses OpenUSD's Python bindings
//! instead: the Python named by `LAYERSTACK_USD_PYTHON`, or `python3`, when
//! it imports `pxr`, under the same version rule.

use std::path::Path;
use std::process::Command;

use layerstack_conformance::export_fixtures::{
    Expect, OpenUsdRelease, documents, minimum_openusd, parse_openusd_release, write_all,
};
use layerstack_conformance::save_corpus::{Imported, cases};
use layerstack_conformance::usdc::crate_structure;
use layerstack_usdc::writer::{
    Spec as UsdcSpec, SpecForm, Specifier, Value as UsdcValue, Variability, write_crate,
};

fn tool(name: &str) -> Option<String> {
    Command::new(name)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> std::path::PathBuf {
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
    base.join(format!("export-interop-{name}"))
}

#[test]
fn external_usd_tools_accept_exporter_output() {
    let Some(usdcat) = tool("usdcat") else {
        eprintln!("skipped: usdcat is not on PATH");
        return;
    };
    let usdchecker = tool("usdchecker");
    eprintln!("usdcat: {usdcat}; usdchecker: {usdchecker:?}");

    let dir = scratch_dir("fixtures");
    let _ = std::fs::remove_dir_all(&dir);
    let fixtures = write_all(&dir);
    let mut failures = Vec::new();
    for fixture in &fixtures {
        let path = fixture.path.display().to_string();
        let cat = Command::new("usdcat").arg(&fixture.path).output().unwrap();
        let checked = usdchecker.as_ref().map(|_| {
            Command::new("usdchecker")
                .arg(&fixture.path)
                .output()
                .unwrap()
        });
        match fixture.expect {
            Expect::Valid | Expect::ValidArkit => {
                if !cat.status.success() {
                    failures.push(format!(
                        "usdcat rejected {path}: {}",
                        String::from_utf8_lossy(&cat.stderr)
                    ));
                }
                if let Some(out) = checked.filter(|o| !o.status.success()) {
                    failures.push(format!(
                        "usdchecker rejected {path}: {}",
                        String::from_utf8_lossy(&out.stdout)
                    ));
                }
            }
            Expect::Invalid(validator) => {
                if let Some(out) = checked {
                    let report = String::from_utf8_lossy(&out.stdout);
                    if out.status.success() || !report.contains(validator) {
                        failures.push(format!(
                            "usdchecker did not report {validator} for control {path}"
                        ));
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Why a tool of `release` cannot check a fixture that needs `minimum`, or
/// `None` when it can. An unreadable version counts as too old.
fn too_old(
    release: Option<OpenUsdRelease>,
    minimum: Option<(OpenUsdRelease, &'static str)>,
) -> Option<String> {
    let (needed, reason) = minimum?;
    match release {
        Some(release) if release >= needed => None,
        Some((year, month)) => Some(format!("{reason}; the tool is OpenUSD {year}.{month:02}")),
        None => Some(format!("{reason}; the tool's version is unknown")),
    }
}

#[test]
fn reads_openusd_releases() {
    assert_eq!(
        parse_openusd_release("Apple USD Tools (0.25.11)"),
        Some((25, 11))
    );
    assert_eq!(parse_openusd_release("0.26.8\n"), Some((26, 8)));
    assert_eq!(parse_openusd_release("usdcat 24.08"), Some((24, 8)));
    assert_eq!(parse_openusd_release("no version"), None);
    assert!(too_old(Some((25, 8)), minimum_openusd("metadata_dictionaries")).is_some());
    assert!(too_old(None, minimum_openusd("metadata_dictionaries")).is_some());
    assert!(too_old(Some((25, 11)), minimum_openusd("metadata_dictionaries")).is_none());
    assert!(too_old(Some((20, 2)), minimum_openusd("cube")).is_none());
}

fn usdcat(args: &[&Path]) -> Result<String, String> {
    let out = Command::new("usdcat")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Differential check of the USDC writer against OpenUSD, for every
/// authored document of the fixture set:
///
/// 1. `usdcat` prints the same text for our USDC as for our USDA of the
///    same document, so OpenUSD reads one layer from both;
/// 2. OpenUSD's own USDC for that USDA (`usdcat -o`) decodes, with this
///    workspace's reader, to the same version, specs, fields (in order) and
///    values as ours. Bytes may differ (LZ4 output, deduplication), so the
///    comparison is structural.
#[test]
fn usdc_writer_matches_openusd() {
    let Some(version) = tool("usdcat") else {
        eprintln!("skipped: usdcat is not on PATH");
        return;
    };
    let release = parse_openusd_release(&version);
    let dir = scratch_dir("usdc-differential");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for (name, doc) in documents() {
        if let Some(reason) = too_old(release, minimum_openusd(name)) {
            eprintln!("skipped {name} with usdcat {version:?}: {reason}");
            continue;
        }
        let usda = dir.join(format!("{name}.usda"));
        let usdc = dir.join(format!("{name}.usdc"));
        let reference = dir.join(format!("{name}.openusd.usdc"));
        std::fs::write(&usda, doc.to_usda().unwrap()).unwrap();
        let ours = layerstack_usdc::writer::write_document(&doc).unwrap();
        std::fs::write(&usdc, &ours).unwrap();

        match (usdcat(&[&usdc]), usdcat(&[&usda])) {
            (Ok(from_usdc), Ok(from_usda)) if from_usdc == from_usda => {}
            (Ok(from_usdc), Ok(from_usda)) => failures.push(format!(
                "{name}: usdcat text differs\n--- usdc\n{from_usdc}\n--- usda\n{from_usda}"
            )),
            (from_usdc, from_usda) => failures.push(format!(
                "{name}: usdcat failed: usdc {:?}, usda {:?}",
                from_usdc.err(),
                from_usda.err()
            )),
        }

        let output = Path::new("-o");
        if let Err(e) = usdcat(&[&usda, output, &reference]) {
            failures.push(format!("{name}: usdcat -o failed: {e}"));
            continue;
        }
        let theirs = std::fs::read(&reference).unwrap();
        let ours = crate_structure(&ours).expect("our USDC decodes");
        let theirs = crate_structure(&theirs).expect("OpenUSD's USDC decodes");
        if ours != theirs {
            failures.push(format!(
                "{name}: structure differs from OpenUSD's USDC\n--- ours\n{ours:#?}\n--- OpenUSD\n{theirs:#?}"
            ));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Values OpenUSD always inlines (`_IsAlwaysInlined`,
/// `pxr/usd/sdf/crateFile.cpp`) or inlines when they fit (`_EncodeInline`,
/// `crateValueInliners.h`), with the out-of-line cases beside them: the
/// USDA type and literal OpenUSD parses, and the crate value this writer is
/// given for it.
fn inline_cases() -> Vec<(&'static str, &'static str, &'static str, UsdcValue)> {
    use UsdcValue as V;
    let identity = |n: usize| -> [[f64; 4]; 4] {
        core::array::from_fn(|i| core::array::from_fn(|j| if i == j && i < n { 1.0 } else { 0.0 }))
    };
    let mut moved = identity(4);
    moved[3][0] = 5.0;
    vec![
        // Always inlined: bitwise types of at most four bytes, and indexes.
        ("bool", "bool", "true", V::Bool(true)),
        ("uchar", "uchar", "200", V::UChar(200)),
        ("int", "int", "-7", V::Int(-7)),
        ("uint", "uint", "4294967295", V::UInt(u32::MAX)),
        ("half", "half", "1", V::Half(0x3c00)),
        ("halfFrac", "half", "0.333251953125", V::Half(0x3555)),
        ("halfNegZero", "half", "-0", V::Half(0x8000)),
        (
            "halfSubnormal",
            "half",
            "5.960464477539063e-8",
            V::Half(0x0001),
        ),
        ("halfMax", "half", "65504", V::Half(0x7bff)),
        ("half2", "half2", "(0, 1)", V::Vec2h([0x0000, 0x3c00])),
        ("half2Int", "half2", "(1, -2)", V::Vec2h([0x3c00, 0xc000])),
        (
            "half2Frac",
            "half2",
            "(0.5, 0.333251953125)",
            V::Vec2h([0x3800, 0x3555]),
        ),
        ("float", "float", "0.1", V::Float(0.1)),
        ("floatNegZero", "float", "-0", V::Float(-0.0)),
        ("token", "token", "\"tok\"", V::Token("tok".into())),
        ("string", "string", "\"text\"", V::String("text".into())),
        ("asset", "asset", "@a.png@", V::Asset("a.png".into())),
        ("blocked", "int", "None", V::Block),
        // Inlined when they fit 32 bits, or are exactly a `float`.
        ("int64", "int64", "-5", V::Int64(-5)),
        ("int64Big", "int64", "-1099511627776", V::Int64(-(1 << 40))),
        (
            "uint64",
            "uint64",
            "4294967295",
            V::UInt64(u64::from(u32::MAX)),
        ),
        (
            "uint64Big",
            "uint64",
            "18446744073709551615",
            V::UInt64(u64::MAX),
        ),
        ("double", "double", "-0.5", V::Double(-0.5)),
        ("doubleNegZero", "double", "-0", V::Double(-0.0)),
        ("doubleFrac", "double", "0.1", V::Double(0.1)),
        // Wider vectors: inlined as `int8` components when integral.
        (
            "half3",
            "half3",
            "(0, -1, 2)",
            V::Vec3h([0x0000, 0xbc00, 0x4000]),
        ),
        (
            "half3Frac",
            "half3",
            "(0.5, 0, 0)",
            V::Vec3h([0x3800, 0, 0]),
        ),
        (
            "half4",
            "half4",
            "(0, 1, 2, 3)",
            V::Vec4h([0, 0x3c00, 0x4000, 0x4200]),
        ),
        (
            "half4Frac",
            "half4",
            "(0.5, 1, 2, 3)",
            V::Vec4h([0x3800, 0x3c00, 0x4000, 0x4200]),
        ),
        ("float2", "float2", "(-128, 127)", V::Vec2f([-128.0, 127.0])),
        ("float2Wide", "float2", "(128, 0)", V::Vec2f([128.0, 0.0])),
        ("float3", "float3", "(0, -1, 2)", V::Vec3f([0.0, -1.0, 2.0])),
        (
            "float3NegZero",
            "float3",
            "(-0, 1, 2)",
            V::Vec3f([-0.0, 1.0, 2.0]),
        ),
        (
            "float4",
            "float4",
            "(1, 2, 3, 4.5)",
            V::Vec4f([1.0, 2.0, 3.0, 4.5]),
        ),
        ("double2", "double2", "(1, 2)", V::Vec2d([1.0, 2.0])),
        (
            "double3",
            "double3",
            "(1, 2, 0.1)",
            V::Vec3d([1.0, 2.0, 0.1]),
        ),
        (
            "double4",
            "double4",
            "(1, 2, 3, 4)",
            V::Vec4d([1.0, 2.0, 3.0, 4.0]),
        ),
        ("int2", "int2", "(-3, 4)", V::Vec2i([-3, 4])),
        ("int3", "int3", "(1, 2, 300)", V::Vec3i([1, 2, 300])),
        ("int4", "int4", "(1, 2, 3, 4)", V::Vec4i([1, 2, 3, 4])),
        // Quaternions are never inlined (USDA writes the real part first).
        (
            "quath",
            "quath",
            "(1, 0, 0, 0)",
            V::Quath([0, 0, 0, 0x3c00]),
        ),
        (
            "quatf",
            "quatf",
            "(1, 0, 0, 0)",
            V::Quatf([0.0, 0.0, 0.0, 1.0]),
        ),
        (
            "quatd",
            "quatd",
            "(0, 1, 0, 0)",
            V::Quatd([1.0, 0.0, 0.0, 0.0]),
        ),
        // Matrices: inlined as the `int8` diagonal when zero elsewhere.
        (
            "matrix2d",
            "matrix2d",
            "( (2, 0), (0, -3) )",
            V::Matrix2d([[2.0, 0.0], [0.0, -3.0]]),
        ),
        (
            "matrix2dFull",
            "matrix2d",
            "( (1, 2), (3, 4) )",
            V::Matrix2d([[1.0, 2.0], [3.0, 4.0]]),
        ),
        (
            "matrix3d",
            "matrix3d",
            "( (1, 0, 0), (0, 1, 0), (0, 0, 1) )",
            V::Matrix3d(core::array::from_fn(|i| {
                core::array::from_fn(|j| identity(3)[i][j])
            })),
        ),
        (
            "matrix4d",
            "matrix4d",
            "( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1) )",
            V::Matrix4d(identity(4)),
        ),
        (
            "matrix4dMoved",
            "matrix4d",
            "( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (5, 0, 0, 1) )",
            V::Matrix4d(moved),
        ),
    ]
}

/// Differential check of every inlining rule: specs written by the crate
/// writer must print, through `usdcat`, exactly as the USDA of the same
/// values, and decode (through this workspace's reader) to the same fields
/// and values as OpenUSD's own USDC for that USDA. `usdcat` is the oracle:
/// a mistake the reader and writer share (such as packing a `half2` as
/// `int8` components) survives a round trip but not this.
#[test]
fn usdc_inlined_values_match_openusd() {
    if tool("usdcat").is_none() {
        eprintln!("skipped: usdcat is not on PATH");
        return;
    }
    let cases = inline_cases();
    let mut usda = String::from("#usda 1.0\n\ndef \"Root\"\n{\n");
    let mut specs = vec![
        UsdcSpec::new("/", SpecForm::PseudoRoot)
            .with_field("primChildren", UsdcValue::TokenVector(vec!["Root".into()])),
        UsdcSpec::new("/Root", SpecForm::Prim)
            .with_field("specifier", UsdcValue::Specifier(Specifier::Def))
            .with_field(
                "properties",
                UsdcValue::TokenVector(cases.iter().map(|c| c.0.to_string()).collect()),
            ),
    ];
    for (name, ty, literal, value) in cases {
        usda.push_str(&format!("    {ty} {name} = {literal}\n"));
        specs.push(
            UsdcSpec::new(format!("/Root.{name}"), SpecForm::Attribute)
                .with_field("custom", UsdcValue::Bool(false))
                .with_field("typeName", UsdcValue::Token(ty.into()))
                .with_field("variability", UsdcValue::Variability(Variability::Varying))
                .with_field("default", value),
        );
    }
    usda.push_str("}\n");

    let dir = scratch_dir("usdc-inlined");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let text = dir.join("values.usda");
    let ours = dir.join("values.usdc");
    let reference = dir.join("values.openusd.usdc");
    std::fs::write(&text, &usda).unwrap();
    let bytes = write_crate(&specs).unwrap();
    std::fs::write(&ours, &bytes).unwrap();
    usdcat(&[&text, Path::new("-o"), &reference]).expect("usdcat writes the reference");

    let from_ours = usdcat(&[&ours]).expect("usdcat reads our USDC");
    let from_text = usdcat(&[&text]).expect("usdcat reads the USDA");
    let differing: Vec<(&str, &str)> = from_ours
        .lines()
        .zip(from_text.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert!(
        differing.is_empty() && from_ours.lines().count() == from_text.lines().count(),
        "usdcat prints our values differently (ours, from USDA):\n{differing:#?}"
    );
    let ours = crate_structure(&bytes).expect("our USDC decodes");
    let theirs = crate_structure(&std::fs::read(&reference).unwrap()).expect("OpenUSD's decodes");
    assert_eq!(ours.version, theirs.version, "crate version");
    assert_eq!(
        ours.specs.keys().collect::<Vec<_>>(),
        theirs.specs.keys().collect::<Vec<_>>(),
        "spec paths"
    );
    let differing: Vec<&str> = ours
        .specs
        .iter()
        .filter(|(path, spec)| theirs.specs.get(*path) != Some(spec))
        .map(|(path, _)| path.as_str())
        .collect();
    // The one intended difference: OpenUSD inlines (-0, 1, 2) as `int8`
    // components, which drops the sign of zero (`_IsExactlyRepresented`
    // compares -0 equal to 0); this writer stores it out of line instead,
    // and `usdcat` prints it as the USDA does (checked above).
    assert_eq!(
        differing,
        ["/Root.float3NegZero"],
        "our USDC decodes to the same fields and values as OpenUSD's"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The `ARKit`-profile packages hold exactly one USD layer, a USDC root
/// (checked with this workspace's archive reader; the external gate checks
/// the same with Python's `zipfile`).
#[test]
fn arkit_packages_have_a_single_usdc_root() {
    let dir = scratch_dir("arkit-layout");
    let _ = std::fs::remove_dir_all(&dir);
    let fixtures = write_all(&dir);
    let arkit: Vec<_> = fixtures
        .iter()
        .filter(|f| f.expect == Expect::ValidArkit)
        .collect();
    assert!(
        arkit.len() > documents().len(),
        "every document, plus media"
    );
    for fixture in arkit {
        let bytes = std::fs::read(&fixture.path).unwrap();
        let archive =
            layerstack_usdz::zip::ZipArchive::parse(&bytes).expect("stored, aligned archive");
        let entries = archive.entries();
        assert_eq!(&*entries[0].name, "scene.usdc", "root layer");
        assert_eq!(
            &archive.entry_data(&entries[0])[..8],
            b"PXR-USDC",
            "crate root layer"
        );
        let layers = entries
            .iter()
            .filter(|e| {
                let ext = e.name.rsplit('.').next().unwrap_or("");
                matches!(ext, "usd" | "usda" | "usdc" | "usdz")
            })
            .count();
        assert_eq!(layers, 1, "{} has one USD layer", fixture.path.display());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A Python that imports OpenUSD's `pxr`, with the OpenUSD version it
/// reports.
fn usd_python() -> Option<(String, String)> {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python)
        .arg(snapshot_script())
        .arg("version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some((python, String::from_utf8_lossy(&out.stdout).trim().into()))
}

fn snapshot_script() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/layer_snapshot.py")
}

/// Runs `layer_snapshot.py` with `args`, returning its standard output.
fn layer_snapshot(python: &str, args: &[&Path]) -> Result<String, String> {
    let out = Command::new(python)
        .arg(snapshot_script())
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// The preservation corpus through OpenUSD (`save_corpus`): each case is
/// imported from its USDA and from OpenUSD's USDC of it, edited through the
/// `Layer` API and saved as USDA and USDC. OpenUSD must read every saved
/// file exactly as it reads the hand-written expected layer — the same text
/// (every spec, field and type), the same authored child and property
/// order, and the same `ClaimsAPI` records — and our USDC must decode like
/// OpenUSD's own USDC of our USDA, which pins every field's value type.
#[test]
fn saved_layers_round_trip_through_openusd() {
    let Some((python, version)) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    eprintln!("OpenUSD {version} via {python}");
    let release = parse_openusd_release(&version);
    let dir = scratch_dir("layer-save");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for case in cases() {
        let name = case.name;
        if let Some(reason) = too_old(release, case.minimum_openusd) {
            eprintln!("skipped {name} with OpenUSD {version}: {reason}");
            continue;
        }
        let source = dir.join(format!("{name}.source.usda"));
        let source_usdc = dir.join(format!("{name}.source.openusd.usdc"));
        let expected = dir.join(format!("{name}.expected.usda"));
        std::fs::write(&source, case.source).unwrap();
        std::fs::write(&expected, case.expected).unwrap();
        let weaker = case.weaker.map(|text| {
            let path = dir.join(format!("{name}.weaker.usda"));
            std::fs::write(&path, text).unwrap();
            path
        });
        let snapshot_args = |layer: &Path| -> Vec<std::path::PathBuf> {
            let mut args = vec![Path::new("snapshot").to_path_buf(), layer.to_path_buf()];
            args.extend(weaker.clone());
            args
        };
        let snapshot = |layer: &Path| {
            let args = snapshot_args(layer);
            let args: Vec<&Path> = args.iter().map(|a| a.as_path()).collect();
            layer_snapshot(&python, &args)
        };
        layer_snapshot(&python, &[Path::new("convert"), &source, &source_usdc])
            .unwrap_or_else(|e| panic!("{name}: OpenUSD cannot convert the source: {e}"));
        let want = snapshot(&expected)
            .unwrap_or_else(|e| panic!("{name}: OpenUSD cannot read the expected layer: {e}"));
        if name == "explicit_empty_lists" {
            // The oracle itself: explicit-empty lists block the weaker
            // opinions, a bare declaration does not, a delete edits them.
            let want: serde_json::Value = serde_json::from_str(&want).unwrap();
            let a = &want["composed"]["/A"];
            let none: Vec<String> = Vec::new();
            for blocked in [
                "blocked",
                "emptied",
                "blockedInput",
                "emptiedInput",
                "apiSchemas",
            ] {
                assert_eq!(
                    a[blocked],
                    serde_json::json!(none),
                    "{blocked} is blocked by an explicit empty list"
                );
            }
            assert_eq!(a["declared"], serde_json::json!(["/Elsewhere"]), "declared");
            assert_eq!(
                a["pruned"],
                serde_json::json!(["/Elsewhere/Kept"]),
                "pruned"
            );
        }

        let imports = [
            ("usda", Imported::usda(case.source)),
            (
                "usdc",
                Imported::usdc(&std::fs::read(&source_usdc).unwrap()),
            ),
        ];
        for (from, mut layer) in imports {
            (case.edit)(&mut layer);
            let usda = dir.join(format!("{name}.from-{from}.usda"));
            let usdc = dir.join(format!("{name}.from-{from}.usdc"));
            let ours = layer.save_usdc().unwrap();
            std::fs::write(&usda, layer.save_usda().unwrap()).unwrap();
            std::fs::write(&usdc, &ours).unwrap();
            for saved in [&usda, &usdc] {
                match snapshot(saved) {
                    Ok(got) if got == want => {}
                    Ok(got) => failures.push(format!(
                        "{}: OpenUSD reads it differently\n--- saved\n{got}\n--- expected\n{want}",
                        saved.display()
                    )),
                    Err(e) => failures.push(format!("{}: {e}", saved.display())),
                }
            }
            let reference = dir.join(format!("{name}.from-{from}.openusd.usdc"));
            if let Err(e) = layer_snapshot(&python, &[Path::new("convert"), &usda, &reference]) {
                failures.push(format!("{name}: OpenUSD cannot convert our USDA: {e}"));
                continue;
            }
            let theirs = crate_structure(&std::fs::read(&reference).unwrap()).unwrap();
            let ours = crate_structure(&ours).unwrap();
            if ours != theirs {
                failures.push(format!(
                    "{name} (from {from}): USDC differs from OpenUSD's USDC of our USDA\n--- ours\n{ours:#?}\n--- OpenUSD\n{theirs:#?}"
                ));
            }
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
