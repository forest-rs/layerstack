// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Store-independent text dumps of a layer's authored content.
//!
//! Two layers loaded into different stores (for example the same asset read
//! from USDA and from USDC, or before and after an OpenUSD round trip) intern
//! tokens and paths differently. [`dump_layer`] renders everything a layer
//! authors with names resolved, so such layers compare line by line.
//!
//! Properties are listed by name, not in authored order: OpenUSD's writers
//! emit properties sorted by name, so authored order does not survive an
//! OpenUSD round trip. Authored `reorder properties` statements are dumped
//! as `propertyOrder`. Dictionary entries are sorted by key, as in
//! `VtDictionary`.

use std::fmt::Write as _;

use layerstack::doc::{FieldEntry, FieldValue, Layer, PrimSpec, Reference, VariantSpec};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{PathId, PathInterner, TargetPath};
use layerstack::property::{PropertyEntry, PropertyKind, Variability};
use layerstack::{ReferenceTarget, Value};

/// Name resolution for a dump.
#[derive(Clone, Copy, Debug)]
pub struct Names<'a> {
    /// Token interner of the layer's store.
    pub tokens: &'a TokenInterner,
    /// Path interner of the layer's store.
    pub paths: &'a PathInterner,
}

impl Names<'_> {
    fn token(&self, token: TokenId) -> &str {
        self.tokens.resolve(token)
    }

    fn path(&self, path: PathId) -> String {
        self.paths.display(path, self.tokens)
    }

    fn target(&self, target: &TargetPath) -> String {
        target.display(self.paths, self.tokens)
    }
}

/// Renders a value with token names resolved and dictionaries sorted by key.
pub fn render_value(value: &Value, names: Names<'_>) -> String {
    match value {
        Value::Token(token) => format!("Token({:?})", names.token(*token)),
        Value::Array(items) => {
            let items: Vec<_> = items.iter().map(|v| render_value(v, names)).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Dictionary(entries) => {
            let mut entries: Vec<_> = entries
                .iter()
                .map(|(key, v)| format!("{key:?}: {}", render_value(v, names)))
                .collect();
            entries.sort();
            format!("{{{}}}", entries.join(", "))
        }
        Value::Opaque { type_name, bytes } => {
            format!("Opaque({}, {} bytes)", names.token(*type_name), bytes.len())
        }
        other => format!("{other:?}"),
    }
}

fn render_list<T>(list: &ListOp<T>, item: impl Fn(&T) -> String) -> String {
    let render = |items: &[T]| items.iter().map(&item).collect::<Vec<_>>().join(", ");
    let mut out = String::from("listop(");
    if let Some(explicit) = &list.explicit {
        let _ = write!(out, "explicit=[{}] ", render(explicit));
    }
    for (label, items) in [
        ("prepend", &list.prepend),
        ("append", &list.append),
        ("delete", &list.delete),
    ] {
        if !items.is_empty() {
            let _ = write!(out, "{label}=[{}] ", render(items));
        }
    }
    format!("{})", out.trim_end())
}

/// Renders a metadata field value.
pub fn render_field(value: &FieldValue, names: Names<'_>) -> String {
    match value {
        FieldValue::Value(value) => render_value(value, names),
        FieldValue::TokenListOp(list) => render_list(list, |t| format!("{:?}", names.token(*t))),
        FieldValue::PathListOp(list) => render_list(list, |t| names.target(t)),
        FieldValue::StringListOp(list) => render_list(list, |s| format!("String({s:?})")),
        FieldValue::IntListOp(list) => render_list(list, |v| format!("Int({v})")),
        FieldValue::UIntListOp(list) => render_list(list, |v| format!("UInt({v})")),
        FieldValue::Int64ListOp(list) => render_list(list, |v| format!("Int64({v})")),
        FieldValue::UInt64ListOp(list) => render_list(list, |v| format!("UInt64({v})")),
    }
}

fn render_fields(out: &mut Vec<String>, indent: &str, fields: &[FieldEntry], names: Names<'_>) {
    let mut lines: Vec<_> = fields
        .iter()
        .map(|entry| {
            format!(
                "{indent}{} = {}",
                names.token(entry.name),
                render_field(&entry.value, names)
            )
        })
        .collect();
    lines.sort();
    out.extend(lines);
}

fn render_properties(
    out: &mut Vec<String>,
    indent: &str,
    properties: &[PropertyEntry],
    names: Names<'_>,
) {
    let mut sorted: Vec<_> = properties.iter().collect();
    sorted.sort_by_key(|entry| names.token(entry.name));
    for entry in sorted {
        let spec = &entry.spec;
        let kind = match spec.kind {
            PropertyKind::Attribute => "attribute",
            PropertyKind::Relationship => "relationship",
        };
        let mut header = format!("{indent}{kind} {}", names.token(entry.name));
        if spec.custom {
            header.push_str(" custom");
        }
        if spec.variability == Variability::Uniform {
            header.push_str(" uniform");
        }
        if let Some(ty) = &spec.type_name {
            let _ = write!(header, " type={}", ty.type_name);
            if ty.is_array && !ty.type_name.ends_with("[]") {
                header.push_str("[]");
            }
        }
        out.push(header);
        let inner = format!("{indent}    ");
        if let Some(value) = &spec.default {
            out.push(format!("{inner}default = {}", render_value(value, names)));
        }
        if let Some(samples) = &spec.time_samples {
            let samples: Vec<_> = samples
                .iter()
                .map(|(t, v)| format!("{t}: {}", render_value(v, names)))
                .collect();
            out.push(format!("{inner}timeSamples = [{}]", samples.join(", ")));
        }
        if let Some(spline) = &spec.spline {
            out.push(format!("{inner}spline = {spline:?}"));
        }
        if let Some(targets) = &spec.targets {
            out.push(format!(
                "{inner}targets = {}",
                render_list(targets, |t| names.target(t))
            ));
        }
        render_fields(out, &format!("{inner}meta "), &spec.metadata, names);
    }
}

fn render_reference(reference: &Reference, names: Names<'_>) -> String {
    let target = match reference.target {
        ReferenceTarget::Prim(path) => names.path(path),
        ReferenceTarget::DefaultPrim => String::from("<defaultPrim>"),
    };
    format!(
        "@{}@{target} offset={} scale={}",
        reference.asset.as_deref().unwrap_or(""),
        reference.layer_offset.offset,
        reference.layer_offset.scale
    )
}

fn is_empty_list<T>(list: &ListOp<T>) -> bool {
    list.explicit.is_none()
        && list.prepend.is_empty()
        && list.append.is_empty()
        && list.delete.is_empty()
}

fn render_arcs(
    out: &mut Vec<String>,
    indent: &str,
    [references, payloads]: [&ListOp<Reference>; 2],
    [inherits, specializes]: [&ListOp<PathId>; 2],
    names: Names<'_>,
) {
    for (label, list) in [("references", references), ("payload", payloads)] {
        if !is_empty_list(list) {
            out.push(format!(
                "{indent}{label} = {}",
                render_list(list, |r| render_reference(r, names))
            ));
        }
    }
    for (label, list) in [("inherits", inherits), ("specializes", specializes)] {
        if !is_empty_list(list) {
            out.push(format!(
                "{indent}{label} = {}",
                render_list(list, |p| names.path(*p))
            ));
        }
    }
}

fn render_names(tokens: &[TokenId], names: Names<'_>) -> String {
    let tokens: Vec<_> = tokens.iter().map(|t| names.token(*t)).collect();
    format!("[{}]", tokens.join(", "))
}

fn render_prim_like(
    out: &mut Vec<String>,
    indent: &str,
    fields: &[FieldEntry],
    properties: &[PropertyEntry],
    property_order: Option<&[TokenId]>,
    names: Names<'_>,
) {
    render_fields(out, &format!("{indent}meta "), fields, names);
    if let Some(order) = property_order {
        out.push(format!(
            "{indent}propertyOrder = {}",
            render_names(order, names)
        ));
    }
    render_properties(out, indent, properties, names);
}

fn render_variant(out: &mut Vec<String>, indent: &str, variant: &VariantSpec, names: Names<'_>) {
    render_prim_like(
        out,
        indent,
        &variant.fields,
        &variant.properties,
        variant.property_order.as_deref(),
        names,
    );
    render_arcs(
        out,
        indent,
        [&variant.references, &variant.payloads],
        [&variant.inherits, &variant.specializes],
        names,
    );
}

fn render_prim(out: &mut Vec<String>, path: PathId, spec: &PrimSpec, names: Names<'_>) {
    let specifier = spec
        .specifier
        .map_or(String::from("-"), |s| format!("{s:?}").to_lowercase());
    let type_name = spec.type_name.map_or("", |t| names.token(t));
    let branch: String = spec
        .outer_variant_sites
        .iter()
        .map(|site| {
            format!(
                "{}{{{}={}}}",
                names.path(site.host_path),
                names.token(site.set),
                names.token(site.variant)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let branch = if branch.is_empty() {
        branch
    } else {
        format!(" in {branch}")
    };
    out.push(
        format!("prim {}{branch} {specifier} {type_name}", names.path(path))
            .trim_end()
            .to_owned(),
    );
    let indent = "    ";
    if !spec.authored_children.is_empty() {
        out.push(format!(
            "{indent}children = {}",
            render_names(&spec.authored_children, names)
        ));
    }
    if let Some(order) = &spec.prim_order {
        out.push(format!(
            "{indent}primOrder = {}",
            render_names(order, names)
        ));
    }
    if let Some(active) = spec.active {
        out.push(format!("{indent}active = {active}"));
    }
    if let Some(instanceable) = spec.instanceable {
        out.push(format!("{indent}instanceable = {instanceable}"));
    }
    render_arcs(
        out,
        indent,
        [&spec.references, &spec.payloads],
        [&spec.inherits, &spec.specializes],
        names,
    );
    render_prim_like(
        out,
        indent,
        &spec.fields,
        &spec.properties,
        spec.property_order.as_deref(),
        names,
    );
    // Every variant spec, nested ones included, by its path on the prim
    // spec (`{a=x}{b=y}`).
    let mut variants: Vec<(String, &VariantSpec)> = spec
        .variant_branches()
        .map(|branch| {
            let path: String = branch
                .chain()
                .map(|(set, variant)| format!("{{{}={}}}", names.token(set), names.token(variant)))
                .collect();
            (path, branch.spec)
        })
        .collect();
    variants.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, variant) in variants {
        out.push(format!("{indent}variant {path}"));
        render_variant(out, &format!("{indent}    "), variant, names);
    }
}

/// Dumps everything `layer` authors, one line per item, with names resolved.
pub fn dump_layer(layer: &Layer, names: Names<'_>) -> Vec<String> {
    let mut out = vec![String::from("layer")];
    if let Some(default_prim) = layer.default_prim {
        out.push(format!("    defaultPrim = {}", names.token(default_prim)));
    }
    render_fields(&mut out, "    meta ", &layer.metadata, names);
    // Every spec, including those of other variant branches at the same
    // path; each prim header names its branch context.
    let mut prims: Vec<(PathId, Vec<String>)> = layer
        .prims
        .keys()
        .map(|path| {
            let mut lines = Vec::new();
            for spec in layer.prim_specs(*path) {
                let mut spec_lines = Vec::new();
                render_prim(&mut spec_lines, *path, spec, names);
                lines.push(spec_lines);
            }
            // Branch order at one path depends on ingestion order.
            lines.sort();
            (*path, lines.concat())
        })
        .collect();
    prims.sort_by_key(|(path, _)| names.path(*path));
    for (_, lines) in prims {
        out.extend(lines);
    }
    out
}
