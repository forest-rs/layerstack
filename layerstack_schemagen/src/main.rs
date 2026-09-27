// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generates `layerstack_schemas`' tables and typed views from OpenUSD's own
//! schema definitions.
//!
//! ```text
//! cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr --source <OpenUSD> [--check]
//! ```
//!
//! `--pxr` names the `pxr` package of a usd-core wheel, whose
//! `pluginfo/<plugin>/resources` directories hold each schema plugin's
//! `generatedSchema.usda` and `plugInfo.json`. The generator reads the
//! domains [`model::DOMAINS`] lists, parses each `generatedSchema.usda` with
//! `layerstack_usda`, reads its schemas with
//! `layerstack::schema::read_generated_schema`, and takes each schema's
//! kind, base and auto-applies from `plugInfo.json`.
//!
//! `--source` names a checkout of OpenUSD's sources at the release the wheel
//! is (the generator refuses any other version, from
//! `cmake/defaults/Version.cmake`). Only each property's `apiName`, which
//! `generatedSchema.usda` does not keep, is read from its
//! `pxr/usd/<plugin>/schema.usda`; it names the views' accessors.
//!
//! It writes `layerstack_schemas/src/generated` (the tables, and the views in
//! `views/`), the domain features of `layerstack_schemas/Cargo.toml`, and
//! the conformance test table `layerstack_conformance/tests/generated`.
//! Run it by hand when OpenUSD is upgraded.
//!
//! With `--check` it writes nothing, and fails when the checked-in files
//! differ from what it would write.

mod emit;
mod model;
mod views;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

fn main() -> ExitCode {
    match run() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("layerstack_schemagen: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The directories whose every file the generator writes.
const GENERATED_DIRS: &[&str] = &[
    "layerstack_schemas/src/generated",
    "layerstack_schemas/src/generated/views",
];

/// The features block of `layerstack_schemas/Cargo.toml` the generator
/// writes, between these markers.
const FEATURES_BEGIN: &str = "# BEGIN GENERATED FEATURES (layerstack_schemagen)";
const FEATURES_END: &str = "# END GENERATED FEATURES";

fn run() -> Result<String, String> {
    let mut pxr = None;
    let mut source = None;
    let mut check = false;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pxr" => pxr = args.next().map(PathBuf::from),
            "--source" => source = args.next().map(PathBuf::from),
            "--check" => check = true,
            other => return Err(format!("unknown argument {other}; see the crate docs")),
        }
    }
    let pxr = pxr.ok_or("--pxr <site-packages>/pxr is required")?;
    let source = source.ok_or("--source <OpenUSD checkout at the wheel's tag> is required")?;
    let model = model::read(&pxr, &source)?;

    let mut files: Vec<(String, String)> = Vec::new();
    for (name, text) in emit::files(&model)?
        .into_iter()
        .chain(views::files(&model)?)
    {
        let path = if name == "test_table" {
            "layerstack_conformance/tests/generated/schema_views.rs".to_string()
        } else {
            format!("layerstack_schemas/src/generated/{name}")
        };
        files.push((path, rustfmt(&text)?));
    }
    let manifest = "layerstack_schemas/Cargo.toml";
    let current =
        fs::read_to_string(root.join(manifest)).map_err(|e| format!("{manifest}: {e}"))?;
    files.push((manifest.into(), with_features(&current, &features(&model))?));

    if check {
        let mut stale = Vec::new();
        for (name, text) in &files {
            if fs::read_to_string(root.join(name)).ok().as_deref() != Some(text.as_str()) {
                stale.push(name.clone());
            }
        }
        for dir in GENERATED_DIRS {
            for entry in fs::read_dir(root.join(dir)).map_err(|e| format!("{dir}: {e}"))? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.path().is_dir() {
                    continue;
                }
                let name = format!("{dir}/{}", entry.file_name().to_string_lossy());
                if !files.iter().any(|(generated, _)| *generated == name) {
                    stale.push(format!("{name} (not generated)"));
                }
            }
        }
        if !stale.is_empty() {
            return Err(format!(
                "stale against OpenUSD {}: {}; regenerate without --check",
                model.version,
                stale.join(", ")
            ));
        }
        return Ok(format!(
            "the generated files match OpenUSD {}",
            model.version
        ));
    }

    for (name, text) in &files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(format!(
        "wrote {} files for OpenUSD {}",
        files.len(),
        model.version
    ))
}

/// The Cargo features of `layerstack_schemas`: one per domain, enabling
/// the domains it depends on, and `all`, the default.
fn features(model: &model::Model) -> String {
    let quoted = |names: &mut dyn Iterator<Item = String>| {
        names
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = format!(
        "{FEATURES_BEGIN}\n[features]\ndefault = [\"all\"]\nstd = []\n# Every domain.\nall = [{}]\n",
        quoted(&mut model.domains.iter().map(|d| views::feature(d.plugin)))
    );
    for domain in &model.domains {
        out.push_str(&format!(
            "{} = [{}]\n",
            views::feature(domain.plugin),
            quoted(&mut domain.dependencies.iter().map(|p| views::feature(p)))
        ));
    }
    out.push_str(FEATURES_END);
    out
}

/// `manifest` with its generated features block replaced by `features`.
fn with_features(manifest: &str, features: &str) -> Result<String, String> {
    let begin = manifest
        .find(FEATURES_BEGIN)
        .ok_or("layerstack_schemas/Cargo.toml has no generated features block")?;
    let end = manifest[begin..]
        .find(FEATURES_END)
        .map(|end| begin + end + FEATURES_END.len())
        .ok_or("layerstack_schemas/Cargo.toml's generated features block has no end")?;
    Ok(format!(
        "{}{features}{}",
        &manifest[..begin],
        &manifest[end..]
    ))
}

/// `source` as `rustfmt` formats it, so the files pass `cargo fmt --check`.
fn rustfmt(source: &str) -> Result<String, String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running rustfmt: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(source.as_bytes())
        .map_err(|e| format!("writing to rustfmt: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("rustfmt: {e}"))?;
    if !output.status.success() {
        return Err("rustfmt rejected the generated code".into());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}
