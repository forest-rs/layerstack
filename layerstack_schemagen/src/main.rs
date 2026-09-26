// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generates `layerstack_schemas`' tables from OpenUSD's own schema
//! definitions.
//!
//! ```text
//! cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr [--check]
//! ```
//!
//! `--pxr` names the `pxr` package of a usd-core wheel, whose
//! `pluginfo/<plugin>/resources` directories hold each schema plugin's
//! `generatedSchema.usda` and `plugInfo.json`. The generator reads the
//! domains [`model::DOMAINS`] lists, parses each `generatedSchema.usda` with
//! `layerstack_usda`, reads its schemas with
//! `layerstack::schema::read_generated_schema`, takes each schema's kind,
//! base and auto-applies from `plugInfo.json`, and writes
//! `layerstack_schemas/src/generated`. Run it by hand when OpenUSD is
//! upgraded.
//!
//! With `--check` it writes nothing, and fails when the checked-in files
//! differ from what it would write.

mod emit;
mod model;

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

fn run() -> Result<String, String> {
    let mut pxr = None;
    let mut check = false;
    let mut out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../layerstack_schemas/src/generated");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pxr" => pxr = args.next().map(PathBuf::from),
            "--out" => {
                out = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--out needs a directory")?;
            }
            "--check" => check = true,
            other => return Err(format!("unknown argument {other}; see the crate docs")),
        }
    }
    let pxr = pxr.ok_or("--pxr <site-packages>/pxr is required")?;
    let model = model::read(&pxr)?;
    let files = emit::files(&model)?
        .into_iter()
        .map(|(name, text)| Ok((name, rustfmt(&text)?)))
        .collect::<Result<Vec<_>, String>>()?;

    if check {
        let mut stale = Vec::new();
        for (name, text) in &files {
            if fs::read_to_string(out.join(name)).ok().as_deref() != Some(text.as_str()) {
                stale.push(name.clone());
            }
        }
        for entry in fs::read_dir(&out).map_err(|e| format!("{}: {e}", out.display()))? {
            let name = entry
                .map_err(|e| e.to_string())?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if !files.iter().any(|(generated, _)| *generated == name) {
                stale.push(format!("{name} (not generated)"));
            }
        }
        if !stale.is_empty() {
            return Err(format!(
                "{} is stale against OpenUSD {}: {}; regenerate without --check",
                out.display(),
                model.version,
                stale.join(", ")
            ));
        }
        return Ok(format!(
            "{} matches OpenUSD {}",
            out.display(),
            model.version
        ));
    }

    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (name, text) in &files {
        let path = out.join(name);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(format!(
        "wrote {} files for OpenUSD {} to {}",
        files.len(),
        model.version,
        out.display()
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
