// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Generate standard shader-node views from composed `shaderDefs.usda`.
//! Composition includes inherited inputs, notably primvar readers' `varname`.

use crate::model::Model;
use layerstack::{
    InMemoryStore, PropertyKind, PropertyPath, PropertyType, Stage, StageOptions, TokenInterner,
    Value, Variability,
};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct Node {
    id: String,
    ports: Vec<NodePort>,
}
#[derive(Debug)]
struct NodePort {
    name: String,
    ty: PropertyType,
    variability: Variability,
    default: Option<Value>,
    doc: String,
}

pub(crate) fn read(
    pxr: &Path,
    store: &mut InMemoryStore,
    files: &mut Vec<String>,
) -> Result<Vec<Node>, String> {
    let relative = "pluginfo/usdShaders/resources/shaders/shaderDefs.usda";
    let path = pxr.join(relative);
    files.push(format!("pxr/{relative}"));
    read_path(&path, store)
}

pub(crate) fn read_path(path: &Path, store: &mut InMemoryStore) -> Result<Vec<Node>, String> {
    let root = crate::library::load(path, store)?;
    read_store(root, store)
}

pub(crate) fn read_store(
    root: layerstack::LayerId,
    store: &mut InMemoryStore,
) -> Result<Vec<Node>, String> {
    let stage = Stage::compose(store, root, StageOptions::default());
    if !stage.composition_errors().is_empty() {
        return Err(format!(
            "shader definitions: {:?}",
            stage.composition_errors()
        ));
    }
    let mut pending = stage.children_of(store.path("/")).unwrap_or(&[]).to_vec();
    let mut prims = Vec::new();
    while let Some(path) = pending.pop() {
        pending.extend_from_slice(stage.children_of(path).unwrap_or(&[]));
        if stage.is_defined(path, store)
            && !stage.is_abstract(path, store)
            && stage
                .resolve_type_name(path, store)
                .is_some_and(|name| store.tokens.resolve(name) == "Shader")
        {
            prims.push(path);
        }
    }
    let id_key = store.tokens.intern("info:id");
    let doc_key = store.tokens.intern("doc");
    let mut nodes = Vec::new();
    for prim in prims {
        let Some(Value::Token(id)) = stage
            .resolve_field_path(PropertyPath::new(prim, id_key))
            .map(|value| value.value)
        else {
            return Err("shader definition has no token info:id".into());
        };
        let id = store.tokens.resolve(id).to_owned();
        let mut ports = Vec::new();
        for property in stage.property_names(prim, store) {
            let name = store.tokens.resolve(property).to_owned();
            if !name.starts_with("inputs:") && !name.starts_with("outputs:") {
                continue;
            }
            let declared = stage
                .resolve_property_declaration(prim, property)
                .ok_or("shader port has no declaration")?;
            if declared.kind != PropertyKind::Attribute {
                return Err(format!("{id}.{name}: shader port is not an attribute"));
            }
            let doc = match stage
                .resolve_property_metadata(prim, property, doc_key)
                .map(|value| value.value)
            {
                Some(layerstack::ResolvedValue::Scalar(Value::String(text))) => text.to_string(),
                _ => String::new(),
            };
            ports.push(NodePort {
                name,
                ty: declared.type_name.ok_or("shader port has no type")?,
                variability: declared.variability,
                default: stage
                    .resolve_field_path(PropertyPath::new(prim, property))
                    .map(|value| value.value),
                doc,
            });
        }
        nodes.push(Node { id, ports });
    }
    if nodes.is_empty() {
        return Err("shader library defines no Shader nodes".into());
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(nodes)
}

fn method(name: &str) -> String {
    // OpenUSD's Transform2d `inputs:in` needs an explicit Rust spelling.
    if name == "in" {
        "in_value".into()
    } else {
        crate::views::snake(name)
    }
}

fn default_literal(
    value: &Value,
    tokens: &TokenInterner,
    external: bool,
) -> Result<String, String> {
    Ok(match value {
        Value::Bool(value) => value.to_string(),
        Value::Float(number) => format!("{number:?}_f32"),
        Value::Double(number) => format!("{number:?}_f64"),
        Value::Int(number) => format!("{number}_i32"),
        Value::Vec2f(vector) => format!("{vector:?}"),
        Value::Vec3f(vector) => format!("{vector:?}"),
        Value::Vec4f(vector) => format!("{vector:?}"),
        Value::Matrix4d(matrix) => format!(
            "[{}]",
            matrix
                .as_chunks::<4>()
                .0
                .iter()
                .map(|row| format!("{row:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(text) | Value::Asset(text) => {
            let arc = if external {
                "Arc"
            } else {
                "::alloc::sync::Arc"
            };
            format!("{arc}::from({text:?})")
        }
        Value::Token(token) => format!("{:?}", tokens.resolve(*token)),
        other => return Err(format!("unsupported shader-node default {other:?}")),
    })
}

pub(crate) fn files(model: &Model) -> Result<Vec<(String, String)>, String> {
    render(model, false)
}

pub(crate) fn render(model: &Model, external: bool) -> Result<Vec<(String, String)>, String> {
    // Select Rust paths before escaping library identifiers, docs or defaults.
    // AOUSD Core §7.6.4.1: node-defined attribute values remain verbatim data.
    let schema_crate = if external {
        "layerstack_schemas"
    } else {
        "crate"
    };
    let library = if external {
        "Library-defined"
    } else {
        "OpenUSD's standard"
    };
    let standard = if external { "" } else { "standard " };
    let alloc_decl = if external {
        "extern crate alloc;\n"
    } else {
        ""
    };
    let uses_box = external
        && model.nodes.iter().flat_map(|node| &node.ports).any(|port| {
            matches!(
                port.ty.default_scalar,
                Value::Matrix2d(_) | Value::Matrix3d(_) | Value::Matrix4d(_)
            )
        });
    let box_import = if uses_box {
        "use alloc::boxed::Box;\n"
    } else {
        ""
    };
    let uses_arc = model.nodes.iter().flat_map(|node| &node.ports).any(|port| {
        matches!(
            port.ty.default_scalar,
            Value::String(_) | Value::Asset(_) | Value::PathExpression(_)
        )
    });
    let arc_import = if uses_arc {
        "use alloc::sync::Arc;\n"
    } else {
        ""
    };
    let uses_vec = external
        && model
            .nodes
            .iter()
            .flat_map(|node| &node.ports)
            .any(|port| port.ty.is_array && port.name.starts_with("inputs:"));
    let vec_import = if uses_vec {
        "use alloc::vec::Vec;\n"
    } else {
        ""
    };
    let mut out = if external {
        String::new()
    } else {
        crate::emit::header(model)
    };
    out.push_str(&format!("\n//! {library} shader nodes, keyed by `info:id`.\n//! Inputs read authored values at the requested time; definition defaults are\n//! explicit associated functions. These views do not evaluate shaders or follow\n//! input connections. Port creation uses ordinary shading transactions.\n//! Spec: AOUSD Core §7.6.4.1 (typed attributes), §12.3 (value resolution),\n//! §12.4 (connections). Node identifiers and defaults are OpenUSD usdShaders definitions.\n#![allow(clippy::doc_markdown, clippy::too_long_first_doc_paragraph, reason = \"documentation is copied verbatim from OpenUSD\")]\n\n{alloc_decl}use core::ops::Deref;\nuse layerstack::{{PathId, PropertyType, Value}};\n{box_import}{arc_import}{vec_import}use {schema_crate}::{{Scene, SchemaEdit}};\nuse {schema_crate}::shading::{{Port, PortEdit, PortError}};\nuse {schema_crate}::usd_shade::{{Shader, ShaderEdit, NodeDefApiImplementationSource}};\n"));
    // Include implementation imports and referenced prelude types in the same
    // namespace as node structs and their authoring handles.
    let mut node_names: std::collections::BTreeSet<String> = [
        "Shader",
        "ShaderEdit",
        "Scene",
        "SchemaEdit",
        "PathId",
        "PropertyType",
        "Value",
        "Port",
        "PortEdit",
        "PortError",
        "NodeDefApiImplementationSource",
        "Deref",
        "Arc",
        "Vec",
        "Box",
        "Option",
        "Result",
        "Some",
        "None",
        "Ok",
        "Err",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for node in &model.nodes {
        let name = crate::views::pascal(&crate::views::snake(
            node.id.strip_prefix("Usd").unwrap_or(&node.id),
        ));
        validate_identifier(&name)?;
        if !node_names.insert(name.clone()) || !node_names.insert(format!("{name}Edit")) {
            return Err(format!("generated shader node type collision `{name}`"));
        }
        let id = &node.id;
        let _ = writeln!(
            out,
            "\n/// The {standard}`{id}` shader node.\n#[derive(Clone, Copy, Debug)]\npub struct {name}<'a> {{ shader: Shader<'a> }}\nimpl<'a> Deref for {name}<'a> {{ type Target = Shader<'a>; fn deref(&self) -> &Self::Target {{ &self.shader }} }}\nimpl<'a> {name}<'a> {{\n    /// The shader's definition identifier.\n    pub const ID: &'static str = {id:?};\n    /// Reads an identifier-based Shader whose composed `info:id` matches.\n    #[must_use]\n    pub fn new(scene: &Scene<'a>, path: PathId) -> Option<Self> {{\n        let shader = Shader::new(scene, path)?;\n        let definition = shader.node_def_api();\n        (definition.id() == Some(Self::ID) && definition.implementation_source() == Some(NodeDefApiImplementationSource::Id)).then_some(Self {{ shader }})\n    }}\n    /// Defines a Shader with this node's `info:id`; inputs remain unauthored.\n    pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> {name}Edit {{\n        let shader = Shader::define(edit, path);\n        shader.node_def_api().set_id(edit, Self::ID);\n        shader.node_def_api().set_implementation_source(edit, NodeDefApiImplementationSource::Id);\n        {name}Edit {{ shader }}\n    }}\n    /// An authoring handle for this validated node.\n    #[must_use]\n    pub fn edit(&self) -> {name}Edit {{ {name}Edit {{ shader: self.shader.edit() }} }}"
        );
        let mut setters = String::new();
        let mut methods: std::collections::BTreeSet<String> = [
            "new",
            "define",
            "edit",
            "path",
            "scene",
            "read_value",
            "read_value_at",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for port in &node.ports {
            let (namespace, base) = port
                .name
                .split_once(':')
                .ok_or("shader port without namespace")?;
            let input = namespace == "inputs";
            let m = method(base);
            let kind = if input { "input" } else { "output" };
            let mut names = vec![format!("{m}_{kind}"), format!("create_{m}_{kind}")];
            if input {
                names.extend([
                    m.clone(),
                    format!("{m}_at"),
                    format!("{m}_default"),
                    format!("set_{m}"),
                    format!("set_{m}_at"),
                ]);
            }
            for symbol in names {
                validate_identifier(&symbol)?;
                if !methods.insert(symbol.clone()) {
                    return Err(format!("{id}: generated method collision `{symbol}`"));
                }
            }
            let mut ty = crate::views::rust_type(&port.ty.default_scalar, port.ty.is_array)?
                .ok_or("opaque shader port")?;
            if external {
                // These fragments contain generator-owned Rust syntax only.
                ty.read = ty
                    .read
                    .replace("::alloc::sync::Arc<", "Arc<")
                    .replace("::alloc::vec::Vec<", "Vec<");
                ty.read_fn = ty.read_fn.replace("crate::", "layerstack_schemas::");
                ty.write_fn = ty.write_fn.replace("crate::", "layerstack_schemas::");
            }
            let full = &port.name;
            let kind = if input { "input" } else { "output" };
            let _ = writeln!(
                out,
                "    #[doc = {:?}]\n    #[must_use]\n    pub fn {m}_{kind}(&self) -> Option<Port<'a>> {{ self.shader.{kind}({base:?}) }}",
                format!("The authored `{full}` port, if it exists. {}", port.doc)
            );
            if input {
                if external {
                    let _ = writeln!(
                        out,
                        "    #[doc = {:?}]\n    #[must_use]\n    pub fn {m}(&self) -> Option<{}> {{ self.read_value({full:?}, {}) }}",
                        format!(
                            "The composed `{full}`, without node defaults or connection evaluation. {}",
                            port.doc
                        ),
                        ty.read,
                        ty.read_fn
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "    uniform_attribute! {{ #[doc = {:?}] {m}, {full:?}, {}, {} }}",
                        format!(
                            "The composed value of `{full}`, without following connections or applying the node default. {}",
                            port.doc
                        ),
                        ty.read,
                        ty.read_fn
                    );
                }
                if port.variability == Variability::Varying {
                    let _ = writeln!(
                        out,
                        "    /// The composed `{full}` at a numeric time.\n    #[must_use]\n    pub fn {m}_at(&self, time: f64, interpolation: layerstack::InterpolationType) -> Option<{}> {{ self.read_value_at({full:?}, time, interpolation, {}) }}",
                        ty.read, ty.read_fn
                    );
                }
                if let Some(value) = &port.default {
                    let literal = default_literal(value, &model.tokens, external)?;
                    let _ = writeln!(
                        out,
                        "    /// The `{full}` default in the node definition, separate from authored values.\n    #[must_use]\n    pub fn {m}_default() -> {} {{ {literal} }}",
                        ty.read.replace("'a", "'static")
                    );
                }
            }
            let mut zero = crate::emit::expr(&port.ty.default_scalar, &model.tokens)?;
            if external {
                // Matrix ports import Box explicitly, including in no_std.
                // Type zeros contain no library literals to rewrite.
                zero = zero.replace("alloc::boxed::Box::", "Box::");
            }
            let interner = if zero.contains("t.intern(") {
                "let t = edit.tokens();"
            } else {
                ""
            };
            let _ = writeln!(
                setters,
                "    /// Creates `{full}` with its node-defined USD type for values or connections.\n    /// Rejects existing ports of a different type before appending edits.\n    pub fn create_{m}_{kind}(&self, edit: &mut SchemaEdit<'_>) -> Result<PortEdit, PortError> {{\n        {interner}\n        let zero = {zero};\n        self.shader.create_{kind}(edit, {base:?}, PropertyType::new({:?}, {}, zero))\n    }}",
                port.ty.type_name, port.ty.is_array
            );
            if input {
                let _ = writeln!(
                    setters,
                    "    /// Authors `{full}` at default time, creating the typed input if needed.\n    pub fn set_{m}(&self, edit: &mut SchemaEdit<'_>, value: {}) -> Result<&Self, PortError> {{\n        let port = self.create_{m}_input(edit)?;\n        let value = ({})(value, edit.tokens());\n        port.set(edit, value)?;\n        Ok(self)\n    }}",
                    ty.write, ty.write_fn
                );
                if port.variability == Variability::Varying {
                    let _ = writeln!(
                        setters,
                        "    /// Authors a `{full}` sample in stage time through the edit target.\n    pub fn set_{m}_at(&self, edit: &mut SchemaEdit<'_>, time: f64, value: {}) -> Result<&Self, PortError> {{\n        let port = self.create_{m}_input(edit)?;\n        let value = ({})(value, edit.tokens());\n        port.set_at(edit, time, value)?;\n        Ok(self)\n    }}",
                        ty.write, ty.write_fn
                    );
                }
            }
        }
        let _ = writeln!(
            out,
            "}}\n\n/// Authors `{id}` inputs and typed ports through an explicit edit target.\n#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub struct {name}Edit {{ shader: ShaderEdit }}\nimpl Deref for {name}Edit {{ type Target = ShaderEdit; fn deref(&self) -> &Self::Target {{ &self.shader }} }}\nimpl {name}Edit {{\n{setters}}}"
        );
    }
    Ok(vec![
        ("shader_nodes.rs".into(), out),
        ("shader_node_test_table".into(), test_table(model)),
    ])
}

/// Exercises every generated port against the composed OpenUSD definition.
fn test_table(model: &Model) -> String {
    let mut out = crate::emit::header(model);
    out.push_str("\n// All standard node types, inherited inputs and outputs.\nuse layerstack::{PathId, Time};\nuse layerstack_schemas::{Scene, SchemaEdit, shading::{PortKind, nodes::*}};\n");
    let ids: Vec<_> = model.nodes.iter().map(|node| node.id.as_str()).collect();
    let _ = writeln!(
        out,
        "pub(crate) const IDS: &[&str] = &{ids:?};\npub(crate) fn author(edit: &mut SchemaEdit<'_>, paths: &[PathId]) {{"
    );
    let mut comparisons = String::new();
    for (index, node) in model.nodes.iter().enumerate() {
        let name = crate::views::pascal(&crate::views::snake(
            node.id.strip_prefix("Usd").unwrap_or(&node.id),
        ));
        let _ = writeln!(out, "    let node = {name}::define(edit, paths[{index}]);");
        let _ = writeln!(
            comparisons,
            "    let node = {name}::new(scene, paths[{index}]).unwrap();\n    assert_eq!(node.ports(PortKind::Input).len() + node.ports(PortKind::Output).len(), expected[{:?}].as_object().unwrap().len(), \"{}: complete port inventory\");",
            node.id, node.id
        );
        for port in &node.ports {
            let (namespace, base) = port.name.split_once(':').expect("validated shader port");
            let m = method(base);
            let kind = if namespace == "inputs" {
                "input"
            } else {
                "output"
            };
            let full = &port.name;
            let id = &node.id;
            if kind == "input" {
                if port.default.is_some() {
                    let default = format!("{name}::{m}_default()");
                    let argument =
                        if matches!(port.ty.default_scalar, Value::String(_) | Value::Asset(_)) {
                            format!("&{default}")
                        } else {
                            default.clone()
                        };
                    let _ = writeln!(out, "    node.set_{m}(edit, {argument}).unwrap();");
                    let resolved =
                        if matches!(port.ty.default_scalar, Value::String(_) | Value::Asset(_)) {
                            format!("node.{m}().map(|value| value.to_string())")
                        } else {
                            format!("node.{m}()")
                        };
                    let default =
                        if matches!(port.ty.default_scalar, Value::String(_) | Value::Asset(_)) {
                            format!("{default}.to_string()")
                        } else {
                            default
                        };
                    let _ = writeln!(
                        comparisons,
                        "    assert_eq!(serde_json::json!({resolved}), expected[{id:?}][{full:?}][\"default\"], \"{id}.{full}: authored value\");\n    assert_eq!(serde_json::json!({default}), expected[{id:?}][{full:?}][\"default\"], \"{id}.{full}: definition default\");"
                    );
                } else {
                    let _ = writeln!(out, "    node.create_{m}_input(edit).unwrap();");
                }
            } else {
                let _ = writeln!(out, "    node.create_{m}_output(edit).unwrap();");
                let _ = writeln!(
                    comparisons,
                    "    assert!(node.{m}_output().unwrap().value(Time::Default).is_none(), \"{id}.{full}: outputs are not evaluated\");"
                );
            }
            let _ = writeln!(
                comparisons,
                "    let port = node.{m}_{kind}().unwrap();\n    assert_eq!(port.property_type().unwrap().type_name.as_ref(), expected[{id:?}][{full:?}][\"type\"].as_str().unwrap(), \"{id}.{full}: port type\");"
            );
        }
    }
    out.push_str(
        "}\npub(crate) fn compare(scene: &Scene<'_>, paths: &[PathId], expected: &serde_json::Value) {\n",
    );
    out.push_str(&comparisons);
    out.push_str("}\n");
    out
}

fn validate_identifier(name: &str) -> Result<(), String> {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
        "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box",
        "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
        "_",
    ];
    if name.is_empty()
        || name.as_bytes()[0].is_ascii_digit()
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || KEYWORDS.contains(&name)
    {
        return Err(format!("invalid generated Rust identifier `{name}`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_emission_preserves_documentation() {
        let doc = "The standard `crate::example` uses alloc::sync::Arc::from";
        let model = Model {
            version: "custom".into(),
            files: Vec::new(),
            domains: Vec::new(),
            tokens: TokenInterner::default(),
            nodes: vec![Node {
                id: "Text".into(),
                ports: vec![NodePort {
                    name: "inputs:label".into(),
                    ty: PropertyType::new("string", false, Value::String("".into())),
                    variability: Variability::Varying,
                    default: None,
                    doc: doc.into(),
                }],
            }],
        };
        assert!(render(&model, true).unwrap()[0].1.contains(doc));
    }
}
