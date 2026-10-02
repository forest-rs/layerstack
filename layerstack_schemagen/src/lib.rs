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
//! domains `model::DOMAINS` lists, parses each `generatedSchema.usda` with
//! `layerstack_usda`, reads its schemas with
//! `layerstack::schema::read_generated_schema`, and takes each schema's
//! kind, base and auto-applies from `plugInfo.json`. Its `SdfMetadata`
//! declarations generate registry fields and typed metadata readers. Standard
//! shader nodes come from the wheel's `usdShaders/shaders/shaderDefs.usda`;
//! the generator composes it before reading ports and defaults so inherited
//! inputs are included.
//!
//! `--source` names a checkout of OpenUSD's sources at the release the wheel
//! is (the generator refuses any other version, from
//! `cmake/defaults/Version.cmake`). Each property's `apiName`, which
//! `generatedSchema.usda` does not keep, is read from its
//! `pxr/usd/<plugin>/schema.usda`; it names the views' accessors. A domain
//! the wheel is built without (`usdMtlx`, which needs `MaterialX`) is read
//! entirely from the source, `pxr/usd/<plugin>/generatedSchema.usda` and
//! `plugInfo.json` (`model::Origin::Source`), and its generated docs say
//! so.
//!
//! It writes `layerstack_schemas/src/generated` (the tables, and the views in
//! `views/`), the domain features of `layerstack_schemas/Cargo.toml`, and
//! the conformance test table `layerstack_conformance/tests/generated`.
//! Run it by hand when OpenUSD is upgraded.
//!
//! With `--check` it writes nothing, and fails when the checked-in files
//! differ from what it would write.

mod emit;
mod library;
mod metadata;
mod model;
mod shader_nodes;
mod views;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The directories whose every file the generator writes.
const GENERATED_DIRS: &[&str] = &[
    "layerstack_schemas/src/generated",
    "layerstack_schemas/src/generated/views",
];

/// The features block of `layerstack_schemas/Cargo.toml` the generator
/// writes, between these markers.
const FEATURES_BEGIN: &str = "# BEGIN GENERATED FEATURES (layerstack_schemagen)";
const FEATURES_END: &str = "# END GENERATED FEATURES";

/// Runs the generation CLI with the process arguments.
pub fn run() -> Result<String, String> {
    let mut pxr = None;
    let mut source = None;
    let mut check = false;
    let mut definitions = None;
    let mut output = None;
    let mut materialx = None;
    let mut selected = Vec::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pxr" => pxr = args.next().map(PathBuf::from),
            "--source" => source = args.next().map(PathBuf::from),
            "--check" => check = true,
            "--shader-defs" => definitions = args.next().map(PathBuf::from),
            "--out" => output = args.next().map(PathBuf::from),
            "--materialx" => materialx = args.next().map(PathBuf::from),
            "--node" => selected.push(args.next().ok_or("--node requires a NodeDef name")?),
            other => return Err(format!("unknown argument {other}; see the crate docs")),
        }
    }
    if definitions.is_some() && materialx.is_some() {
        return Err("select either --shader-defs or --materialx".into());
    }
    if !selected.is_empty() && materialx.is_none() {
        return Err("--node requires --materialx".into());
    }
    if let Some(path) = definitions.as_ref().or(materialx.as_ref()) {
        if pxr.is_some() || source.is_some() {
            return Err("--shader-defs cannot be combined with --pxr or --source".into());
        }
        let output = output.ok_or("--out <Rust file> is required for shader libraries")?;
        let text = if materialx.is_some() {
            generate_materialx_library(
                path,
                &selected.iter().map(String::as_str).collect::<Vec<_>>(),
            )?
        } else {
            generate_shader_library(path)?
        };
        if check {
            if fs::read_to_string(&output).ok().as_deref() != Some(text.as_str()) {
                return Err(format!("{} is stale", output.display()));
            }
        } else {
            fs::write(&output, text).map_err(|e| format!("{}: {e}", output.display()))?;
        }
        return Ok(format!(
            "{} shader library {}",
            if check { "checked" } else { "wrote" },
            output.display()
        ));
    }
    if output.is_some() {
        return Err("--out requires --shader-defs or --materialx".into());
    }
    let pxr = pxr.ok_or("--pxr <site-packages>/pxr is required")?;
    let source = source.ok_or("--source <OpenUSD checkout at the wheel's tag> is required")?;
    let model = model::read(&pxr, &source)?;

    let mut files: Vec<(String, String)> = Vec::new();
    for (name, text) in emit::files(&model)?
        .into_iter()
        .chain(views::files(&model)?)
        .chain(metadata::files(&model)?)
        .chain(shader_nodes::files(&model)?)
    {
        let path = if name == "shader_node_test_table" {
            "layerstack_conformance/tests/generated/shader_nodes.rs".to_string()
        } else if name == "test_table" {
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
/// the domains it depends on, and `all`, the default. Runtime features such as
/// `std` and `simd` remain owned by the handwritten manifest.
fn features(model: &model::Model) -> String {
    let quoted = |names: &mut dyn Iterator<Item = String>| {
        names
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = format!(
        "{FEATURES_BEGIN}\n[features]\ndefault = [\"all\"]\n# Every domain.\nall = [{}]\n",
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

/// Generates a standalone Rust module for a local `shaderDefs.usda` library.
///
/// Composes relative sublayers, references and inherited ports. The generated
/// module uses `layerstack` and `layerstack_schemas` (with `usd-shade`) and
/// works with `no_std` plus `alloc`. Inputs preserve authored absence; node
/// defaults are separate functions. Unknown types, invalid symbols, missing
/// assets and composition failures return an error instead of partial output.
/// File loading happens only during generation. Returns unformatted Rust source;
/// build-script consumers do not need an installed `rustfmt` component.
pub fn generate_shader_library(path: impl AsRef<Path>) -> Result<String, String> {
    let mut store = layerstack::InMemoryStore::default();
    let nodes = shader_nodes::read_path(path.as_ref(), &mut store)?;
    render_library(nodes, store.tokens)
}

fn generate_shader_text(text: &str) -> Result<String, String> {
    let mut store = layerstack::InMemoryStore::default();
    let parsed = layerstack_usda::parser::parse(text);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        layerstack::LayerId(0),
        &mut store.tokens,
        &mut store.paths,
        &mut model::NoAssets,
    );
    if !parsed.diagnostics.is_empty() || !emitted.diagnostics.is_empty() {
        return Err(format!(
            "shader definitions: {:?} {:?}",
            parsed.diagnostics, emitted.diagnostics
        ));
    }
    store.insert_layer(emitted.layer);
    let nodes = shader_nodes::read_store(layerstack::LayerId(0), &mut store)?;
    render_library(nodes, store.tokens)
}

fn render_library(
    nodes: Vec<shader_nodes::Node>,
    tokens: layerstack::TokenInterner,
) -> Result<String, String> {
    let model = model::Model {
        nodes,
        version: "custom".into(),
        files: Vec::new(),
        domains: Vec::new(),
        tokens,
    };
    Ok(shader_nodes::render(&model, true)?.remove(0).1)
}

/// Generates typed USD APIs for explicitly selected `MaterialX` `NodeDefs`.
///
/// Uses `python3` and its standard XML reader during generation only. Local
/// whole-file `XIncludes` and `NodeDef` inheritance are resolved. Numeric, boolean,
/// string and filename ports with literal defaults are supported. Unsupported
/// types and nonliteral defaults fail explicitly. This does not import material
/// graphs, select or compile implementations, or evaluate shaders. `NodeDef` names
/// become USD identifiers; execution requires a compatible renderer registry.
pub fn generate_materialx_library(
    path: impl AsRef<Path>,
    definitions: &[&str],
) -> Result<String, String> {
    let output = Command::new("python3")
        .arg("-c")
        .arg(include_str!("materialx.py"))
        .arg(path.as_ref())
        .args(definitions)
        .output()
        .map_err(|e| format!("`MaterialX` generation needs python3: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`MaterialX`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    generate_shader_text(&String::from_utf8(output.stdout).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_colliding_and_invalid_generated_names() {
        for ports in [
            "float inputs:new",
            "float inputs:foo\nfloat inputs:foo_input",
            "float inputs:type",
        ] {
            let text = format!(
                "#usda 1.0\ndef Shader \"Foo\" {{\nuniform token info:id = \"Foo\"\n{ports}\n}}"
            );
            assert!(generate_shader_text(&text).is_err(), "{ports}");
        }
        assert!(generate_shader_text("#usda 1.0\ndef Shader \"Foo\" {}").is_err());
    }
    #[test]
    fn rejects_names_that_shadow_generated_imports_or_prelude_types() {
        for id in [
            "Shader",
            "ShaderEdit",
            "Scene",
            "SchemaEdit",
            "Value",
            "PathId",
            "PropertyType",
            "Port",
            "PortEdit",
            "PortError",
            "Deref",
            "Arc",
            "NodeDefApiImplementationSource",
            "Option",
            "Result",
            "Some",
            "None",
            "Ok",
            "Err",
            "Vec",
            "Box",
        ] {
            let text =
                format!("#usda 1.0\ndef Shader \"Node\" {{\nuniform token info:id = {id:?}\n}}");
            assert!(generate_shader_text(&text).is_err(), "{id}");
        }
    }
    #[test]
    fn library_identifiers_are_not_rust_path_fragments() {
        let text = r#"#usda 1.0
        def Shader "Node" {
            uniform token info:id = "crate::literal"
            string inputs:label = "crate::example" (
                doc = "The standard `crate::example` uses alloc::sync::Arc::from"
            )
        }"#;
        let generated = generate_shader_text(text).unwrap();
        assert!(generated.contains(r#"ID: &'static str = "crate::literal""#));
    }
    #[test]
    fn materialx_selection_bounds_the_supported_interface() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../layerstack_conformance/fixtures/custom_nodes/nodes.mtlx");
        assert!(
            generate_materialx_library(&path, &[])
                .unwrap_err()
                .contains("select at least")
        );
        assert!(
            generate_materialx_library(&path, &["missing"])
                .unwrap_err()
                .contains("unknown NodeDef")
        );
        assert!(
            generate_materialx_library(&path, &["ND_unsupported"])
                .unwrap_err()
                .contains("unsupported MaterialX type")
        );
        assert!(
            generate_materialx_library(&path, &["ND_paint", "ND_paint"])
                .unwrap_err()
                .contains("duplicate")
        );
    }
    #[test]
    fn missing_assets_fail_instead_of_dropping_ports() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../layerstack_conformance/fixtures/custom_nodes/missing.usda");
        assert!(generate_shader_library(path).is_err());
    }
}
