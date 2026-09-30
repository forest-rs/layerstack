// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writes the typed views, edit handles and token enums of each domain, and
//! the table of getters the conformance tests exercise.
//!
//! Names come from each property's `apiName`, or else its USD name, with a
//! multiple-apply schema's instance prefix dropped, snake-cased. A name
//! that is a Rust keyword, or that collides with another method of the
//! view or of a view it derefs to, fails the generation: nothing is renamed
//! silently.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use layerstack::{PropertyKind, SchemaKind, TokenInterner, Value, Variability};

use crate::emit::{header, module, origin_note};
use crate::model::{Domain, Model, Property, Schema};

/// Rust keywords, which no generated name may be.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield",
];

/// Methods every view or edit handle has, which no generated name may be.
const RESERVED: &[&str] = &[
    "new",
    "from_view",
    "from_path",
    "get",
    "edit",
    "define",
    "apply",
    "instances",
    "instance",
    "scene",
    "path",
    "schema",
    "property_path",
    "read_value",
    "read_value_at",
    "read_targets",
    "write_value",
    "write_targets",
];

/// The Rust type of a schema (`CollectionAPI` → `CollectionApi`,
/// `Cylinder_1` → `Cylinder1`).
pub(crate) fn type_name(schema: &str) -> String {
    let base = schema
        .strip_suffix("API")
        .map_or_else(|| schema.to_string(), |stem| format!("{stem}Api"));
    base.replace('_', "")
}

/// `name` in snake case: `:` becomes `_`, and each segment's words are
/// split at case changes (`displayColor` → `display_color`,
/// `inputs:shaping:cone:angle` → `inputs_shaping_cone_angle`).
pub(crate) fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, segment) in name.split(':').enumerate() {
        if i > 0 {
            out.push('_');
        }
        let chars: Vec<char> = segment.chars().collect();
        for (j, &c) in chars.iter().enumerate() {
            if c.is_ascii_uppercase() && j > 0 {
                let previous = chars[j - 1];
                let next_lower = chars.get(j + 1).is_some_and(char::is_ascii_lowercase);
                if previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_ascii_uppercase() && next_lower)
                {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// `snake` in upper camel case (`subdivision_scheme` →
/// `SubdivisionScheme`).
fn pascal(snake: &str) -> String {
    snake
        .split(['_', ':', '-'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

/// Explicit method names for properties whose derived name is a Rust
/// keyword or collides: `(schema, USD property name, method name)`. Every
/// entry is a deliberate choice; the generator never renames on its own.
const RENAMES: &[(&str, &str, &str)] = &[
    // `type` is a keyword; OpenUSD's `GetTypeAttr` reads the curve type.
    ("BasisCurves", "type", "curve_type"),
    // apiName `type`, a keyword; OpenUSD's `GetTypeAttr`: force or acceleration.
    (
        "PhysicsDriveAPI",
        "drive:__INSTANCE_NAME__:physics:type",
        "drive_type",
    ),
];

/// A property's Rust method name: an explicit [`RENAMES`] entry, its
/// `apiName`, or else its USD name with a multiple-apply instance prefix
/// dropped, snake-cased.
fn method_name(schema: &str, property: &Property) -> String {
    if let Some((_, _, name)) = RENAMES
        .iter()
        .find(|(s, p, _)| *s == schema && *p == property.name)
    {
        return (*name).to_string();
    }
    if let Some(api_name) = &property.api_name {
        return snake(api_name);
    }
    let placeholder = layerstack::schema::INSTANCE_NAME_PLACEHOLDER;
    let name = match property.name.split_once(&format!("{placeholder}:")) {
        Some((_, rest)) => rest,
        None => &property.name,
    };
    snake(name)
}

/// How a USD value type reads and writes in Rust.
struct RustType {
    /// The getter's type.
    read: String,
    /// The setter's type.
    write: String,
    /// The expression reading a `Value`.
    read_fn: String,
    /// The expression writing one.
    write_fn: String,
}

/// The Rust type of a property whose element type's zero value is `zero`;
/// `None` for a value-less type (`opaque`).
fn rust_type(zero: &Value, is_array: bool) -> Result<Option<RustType>, String> {
    let (read, write, name) = match zero {
        Value::Null => return Ok(None),
        Value::Bool(_) => ("bool", "bool", "bool"),
        Value::UChar(_) => ("u8", "u8", "uchar"),
        Value::Int(_) => ("i32", "i32", "int"),
        Value::UInt(_) => ("u32", "u32", "uint"),
        Value::Int64(_) => ("i64", "i64", "int64"),
        Value::UInt64(_) => ("u64", "u64", "uint64"),
        Value::Half(_) => ("f32", "f32", "half"),
        Value::Float(_) => ("f32", "f32", "float"),
        Value::Double(_) => ("f64", "f64", "double"),
        Value::TimeCode(_) => ("f64", "f64", "timecode"),
        Value::String(_) => ("::alloc::sync::Arc<str>", "&str", "string"),
        Value::Asset(_) => ("::alloc::sync::Arc<str>", "&str", "asset"),
        Value::PathExpression(_) => ("::alloc::sync::Arc<str>", "&str", "path_expression"),
        Value::Token(_) => ("&'a str", "&str", "token"),
        Value::Vec2f(_) => ("[f32; 2]", "[f32; 2]", "float2"),
        Value::Vec3f(_) => ("[f32; 3]", "[f32; 3]", "float3"),
        Value::Vec4f(_) => ("[f32; 4]", "[f32; 4]", "float4"),
        Value::Vec2d(_) => ("[f64; 2]", "[f64; 2]", "double2"),
        Value::Vec3d(_) => ("[f64; 3]", "[f64; 3]", "double3"),
        Value::Vec4d(_) => ("[f64; 4]", "[f64; 4]", "double4"),
        Value::Vec2h(_) => ("[f32; 2]", "[f32; 2]", "half2"),
        Value::Vec3h(_) => ("[f32; 3]", "[f32; 3]", "half3"),
        Value::Vec4h(_) => ("[f32; 4]", "[f32; 4]", "half4"),
        Value::Vec2i(_) => ("[i32; 2]", "[i32; 2]", "int2"),
        Value::Vec3i(_) => ("[i32; 3]", "[i32; 3]", "int3"),
        Value::Vec4i(_) => ("[i32; 4]", "[i32; 4]", "int4"),
        Value::Matrix2d(_) => ("[[f64; 2]; 2]", "[[f64; 2]; 2]", "matrix2d"),
        Value::Matrix3d(_) => ("[[f64; 3]; 3]", "[[f64; 3]; 3]", "matrix3d"),
        Value::Matrix4d(_) => ("[[f64; 4]; 4]", "[[f64; 4]; 4]", "matrix4d"),
        Value::Quatf(_) => ("[f32; 4]", "[f32; 4]", "quatf"),
        Value::Quatd(_) => ("[f64; 4]", "[f64; 4]", "quatd"),
        Value::Quath(_) => ("[f32; 4]", "[f32; 4]", "quath"),
        other => return Err(format!("no Rust type for values like {other:?}")),
    };
    Ok(Some(if is_array {
        RustType {
            read: format!("::alloc::vec::Vec<{read}>"),
            write: format!("&[{write}]"),
            read_fn: format!("|v, t| crate::value::read_array(v, t, crate::value::read_{name})"),
            write_fn: format!("|v, t| crate::value::write_array(v, t, crate::value::write_{name})"),
        }
    } else {
        RustType {
            read: read.into(),
            write: write.into(),
            read_fn: format!("crate::value::read_{name}"),
            write_fn: format!("crate::value::write_{name}"),
        }
    }))
}

/// Text for a `#[doc = ...]` attribute: Markdown-escaped, on one line.
fn doc_text(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if word.contains("://") {
            let _ = write!(out, "`{}`", word.replace('`', ""));
            continue;
        }
        for c in word.chars() {
            match c {
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '[' => out.push_str("\\["),
                ']' => out.push_str("\\]"),
                '\\' => out.push_str("\\\\"),
                '*' => out.push_str("\\*"),
                '_' if false => {}
                c => out.push(c),
            }
        }
    }
    shorten(&out)
}

/// `text` cut after its second sentence once it is long.
fn shorten(text: &str) -> String {
    if text.len() <= 300 {
        return text.to_string();
    }
    let mut end = 0;
    let mut sentences = 0;
    for (i, c) in text.char_indices() {
        if c == '.' && text[i + 1..].starts_with(' ') {
            sentences += 1;
            end = i + 1;
            if sentences == 2 || end > 300 {
                break;
            }
        }
    }
    if end == 0 {
        text.to_string()
    } else {
        text[..end].to_string()
    }
}

/// `#[doc = "..."]` lines for `paragraphs`, joined by blank doc lines.
fn doc_attrs(indent: &str, paragraphs: &[String]) -> String {
    let mut out = String::new();
    let mut first = true;
    for paragraph in paragraphs.iter().filter(|p| !p.is_empty()) {
        if !first {
            let _ = writeln!(out, "{indent}#[doc = \"\"]");
        }
        first = false;
        let _ = writeln!(out, "{indent}#[doc = {paragraph:?}]");
    }
    out
}

/// A fallback value as USD text.
fn usd_text(value: &Value, tokens: &TokenInterner) -> String {
    if let Some(array) = value.array_ref() {
        return format!(
            "[{}]",
            array
                .iter()
                .map(|item| usd_text(&item, tokens))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let list = |items: &mut dyn Iterator<Item = String>| items.collect::<Vec<_>>().join(", ");
    match value {
        Value::Bool(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::Int64(v) => v.to_string(),
        Value::UInt64(v) => v.to_string(),
        Value::UChar(v) => v.to_string(),
        Value::Float(v) => format!("{v:?}"),
        Value::Double(v) | Value::TimeCode(v) => format!("{v:?}"),
        Value::Half(bits) => format!("{:?}", layerstack::half::to_f32(*bits)),
        Value::String(v) | Value::Asset(v) | Value::PathExpression(v) => format!("{:?}", &**v),
        Value::Token(v) => format!("{:?}", tokens.resolve(*v)),
        Value::Vec2f(v) => format!("({})", list(&mut v.iter().map(|x| format!("{x:?}")))),
        Value::Vec3f(v) => format!("({})", list(&mut v.iter().map(|x| format!("{x:?}")))),
        Value::Vec4f(v) | Value::Quatf(v) => {
            format!("({})", list(&mut v.iter().map(|x| format!("{x:?}"))))
        }
        Value::Vec2d(v) => format!("({})", list(&mut v.iter().map(|x| format!("{x:?}")))),
        Value::Vec3d(v) => format!("({})", list(&mut v.iter().map(|x| format!("{x:?}")))),
        Value::Vec4d(v) | Value::Quatd(v) => {
            format!("({})", list(&mut v.iter().map(|x| format!("{x:?}"))))
        }
        Value::Vec2i(v) => format!("({})", list(&mut v.iter().map(ToString::to_string))),
        Value::Vec3i(v) => format!("({})", list(&mut v.iter().map(ToString::to_string))),
        Value::Vec4i(v) => format!("({})", list(&mut v.iter().map(ToString::to_string))),
        Value::Array(items) => format!(
            "[{}]",
            list(&mut items.iter().map(|item| usd_text(item, tokens)))
        ),
        other => format!("{other:?}"),
    }
}

/// The property's USD type name as a schema writes it (`point3f[]`).
fn usd_type(property: &Property) -> String {
    match (&property.kind, &property.value_type) {
        (PropertyKind::Relationship, _) => "rel".into(),
        (_, Some((name, is_array, _))) if *is_array && !name.ends_with("[]") => {
            format!("{name}[]")
        }
        (_, Some((name, _, _))) => name.clone(),
        (_, None) => "untyped".into(),
    }
}

/// Every schema of every domain, by name, with its domain's module.
type Index<'m> = BTreeMap<&'m str, (&'m Schema, String)>;

/// The generated views: one file per domain, `mod.rs`, and the test
/// table.
pub(crate) fn files(model: &Model) -> Result<Vec<(String, String)>, String> {
    let mut index: Index<'_> = BTreeMap::new();
    for domain in &model.domains {
        for schema in &domain.schemas {
            index.insert(&schema.name, (schema, module(domain.plugin)));
        }
    }
    let mut files = Vec::new();
    let mut table = TestTable::default();
    let mut mod_rs = header(model);
    mod_rs.push_str("\n//! The generated views, one module per domain.\n\n");
    for domain in &model.domains {
        let module = module(domain.plugin);
        let _ = writeln!(
            mod_rs,
            "#[cfg(feature = {:?})]\n#[doc = {:?}]\npub mod {module};",
            feature(domain.plugin),
            format!(
                "OpenUSD's `{}` schemas as typed views, edit handles and token enums.{}",
                domain.name,
                origin_note(domain)
            )
        );
        files.push((
            format!("views/{module}.rs"),
            domain_file(model, domain, &index, &mut table)?,
        ));
    }
    files.push(("views/mod.rs".into(), mod_rs));
    files.push(("test_table".into(), table.finish(model)?));
    Ok(files)
}

/// The Cargo feature of a plugin (`usdGeom` → `usd-geom`).
pub(crate) fn feature(plugin: &str) -> String {
    module(plugin).replace('_', "-")
}

/// The local properties of a typed schema: those its parent does not
/// define (a generated schema bakes inherited properties into each
/// schema).
fn local_properties<'m>(schema: &'m Schema, index: &Index<'m>) -> Vec<&'m Property> {
    let inherited: BTreeSet<&str> = schema
        .parent
        .as_deref()
        .and_then(|parent| index.get(parent))
        .map(|(parent, _)| parent.properties.iter().map(|p| p.name.as_str()).collect())
        .unwrap_or_default();
    schema
        .properties
        .iter()
        .filter(|p| !inherited.contains(p.name.as_str()))
        .collect()
}

/// The single-apply built-ins a typed schema adds to its parent's.
fn local_built_ins<'m>(schema: &'m Schema, index: &Index<'m>) -> Vec<&'m str> {
    let inherited: BTreeSet<&str> = schema
        .parent
        .as_deref()
        .and_then(|parent| index.get(parent))
        .map(|(parent, _)| parent.built_ins.iter().map(String::as_str).collect())
        .unwrap_or_default();
    schema
        .built_ins
        .iter()
        .map(String::as_str)
        .filter(|built_in| !built_in.contains(':') && !inherited.contains(built_in))
        .filter(|built_in| {
            index
                .get(built_in)
                .is_some_and(|(s, _)| s.kind == SchemaKind::SingleApplyApi)
        })
        .collect()
}

/// The method names a typed schema's view and its ancestors' views have.
fn chain_names(schema: &Schema, index: &Index<'_>) -> Result<BTreeMap<String, String>, String> {
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut current = Some(schema);
    while let Some(schema) = current {
        for (name, _) in method_names(schema, index)? {
            if let Some(owner) = names.insert(name.clone(), schema.name.clone()) {
                return Err(format!(
                    "{owner} and {}: both have a method `{name}`, and one would hide the \
                     other through `Deref`",
                    schema.name
                ));
            }
        }
        current = schema
            .parent
            .as_deref()
            .and_then(|parent| index.get(parent))
            .map(|(parent, _)| *parent);
    }
    Ok(names)
}

/// The methods one schema's view declares itself, getters and setters,
/// with the property each reads (none for a built-in accessor).
fn method_names<'m>(
    schema: &'m Schema,
    index: &Index<'m>,
) -> Result<Vec<(String, Option<&'m Property>)>, String> {
    let properties = if schema.kind.is_typed() {
        local_properties(schema, index)
    } else {
        schema.properties.iter().collect()
    };
    let mut out = Vec::new();
    for property in properties {
        if property.kind == PropertyKind::Attribute
            && property
                .value_type
                .as_ref()
                .is_some_and(|(_, _, zero)| matches!(zero, Value::Null))
        {
            continue;
        }
        let name = method_name(&schema.name, property);
        out.push((name.clone(), Some(property)));
        out.push((format!("set_{name}"), Some(property)));
        if property.kind == PropertyKind::Attribute && property.variability == Variability::Varying
        {
            out.push((format!("{name}_at"), Some(property)));
            out.push((format!("set_{name}_at"), Some(property)));
        }
    }
    if schema.kind.is_typed() {
        for built_in in local_built_ins(schema, index) {
            out.push((snake(&type_name(built_in)), None));
        }
    }
    let mut seen = BTreeSet::new();
    for (name, _) in &out {
        if KEYWORDS.contains(&name.as_str()) || RESERVED.contains(&name.as_str()) {
            return Err(format!(
                "{}: the method name `{name}` is a Rust keyword or a reserved view method",
                schema.name
            ));
        }
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(format!("{}: `{name}` is not a method name", schema.name));
        }
        if !seen.insert(name.clone()) {
            return Err(format!(
                "{}: two properties are named `{name}`",
                schema.name
            ));
        }
    }
    Ok(out)
}

/// The path of a schema's view type from the module `current`.
fn type_path(schema: &str, index: &Index<'_>, current: &str) -> String {
    let module = index.get(schema).map(|(_, m)| m.as_str()).unwrap_or("usd");
    if module == current {
        type_name(schema)
    } else {
        format!("crate::{module}::{}", type_name(schema))
    }
}

/// The views of one domain.
fn domain_file(
    model: &Model,
    domain: &Domain,
    index: &Index<'_>,
    table: &mut TestTable,
) -> Result<String, String> {
    table.module = module(domain.plugin);
    let mut out = header(model);
    let _ = write!(
        out,
        "\n//! OpenUSD's `{}` schemas as typed views, edit handles and token enums.{}\n\n\
         #![allow(\n    clippy::doc_markdown,\n    clippy::too_long_first_doc_paragraph,\n    \
         reason = \"documentation as OpenUSD writes it\"\n)]\n\n\
         use core::ops::Deref;\n\n\
         use layerstack::{{@LAYERSTACK@}};\n\n\
         use crate::{{@CRATE@}};\n",
        domain.name,
        origin_note(domain)
    );
    let mut uses_instances = false;
    let mut body = String::new();
    for schema in &domain.schemas {
        let text = match schema.kind {
            SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped => {
                chain_names(schema, index)?;
                typed_view(model, schema, index, table)?
            }
            SchemaKind::SingleApplyApi => applied_view(model, schema, false, table)?,
            SchemaKind::MultipleApplyApi => {
                uses_instances = true;
                applied_view(model, schema, true, table)?
            }
        };
        body.push_str(&text);
    }
    if uses_instances {
        out.push_str("use crate::view::{InstanceEdit, InstanceView};\n");
    }
    out.push_str(&body);
    let used = |names: &[&str]| -> String {
        names
            .iter()
            .filter(|name| {
                body.contains(&format!("{name}<"))
                    || body.contains(&format!("{name}::"))
                    || body.contains(&format!("{name},"))
                    || body.contains(&format!("{name})"))
                    || body.contains(&format!("{name} "))
                    || body.contains(&format!("{name}>"))
            })
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let layerstack = used(&["CannotApply", "PathId"]);
    let crate_items = used(&["PrimEdit", "PrimView", "Scene", "SchemaEdit"]);
    Ok(out
        .replace("@LAYERSTACK@", &layerstack)
        .replace("@CRATE@", &crate_items))
}

/// A property's getter and setter macro invocations, and its enum, if any.
struct Accessors {
    getters: String,
    setters: String,
    enums: String,
}

fn accessors(
    model: &Model,
    schema: &Schema,
    property: &Property,
    view: &str,
    table: &mut TestTable,
) -> Result<Accessors, String> {
    let name = method_name(&schema.name, property);
    let mut brief = vec![doc_text(&property.doc)];
    let mut facts = format!(
        "USD {} `{}` (`{}`",
        if property.kind == PropertyKind::Relationship {
            "relationship"
        } else {
            "attribute"
        },
        property
            .name
            .replace(layerstack::schema::INSTANCE_NAME_PLACEHOLDER, "<instance>"),
        usd_type(property)
    );
    if property.variability == Variability::Uniform && property.kind == PropertyKind::Attribute {
        facts.push_str(", uniform");
    }
    if let Some(fallback) = &property.fallback {
        let _ = write!(facts, "; fallback `{}`", usd_text(fallback, &model.tokens));
    }
    facts.push_str(").");
    brief.push(doc_text(&facts));
    let docs = doc_attrs("        ", &brief);
    // The USD name, for authoring and reading by name (OpenUSD's schema
    // tokens); a multiple-apply schema's names depend on the instance.
    let constant = if property
        .name
        .contains(layerstack::schema::INSTANCE_NAME_PLACEHOLDER)
    {
        String::new()
    } else {
        format!(
            "    /// The USD name of [`Self::{name}`].\n    pub const {}: &'static str = {:?};\n\n",
            name.to_ascii_uppercase(),
            property.name
        )
    };
    let mut enums = String::new();
    if property.kind == PropertyKind::Relationship {
        table.getter(&name, property, Getter::Targets);
        return Ok(Accessors {
            getters: format!(
                "{constant}    relationship! {{\n{docs}        {name}, {:?}\n    }}\n",
                property.name
            ),
            setters: format!(
                "    set_relationship! {{\n{docs}        set_{name}, {:?}\n    }}\n",
                property.name
            ),
            enums,
        });
    }
    let Some((_, is_array, zero)) = &property.value_type else {
        return Err(format!("{}.{}: no value type", schema.name, property.name));
    };
    let Some(mut ty) = rust_type(zero, *is_array)
        .map_err(|e| format!("{}.{}: {e}", schema.name, property.name))?
    else {
        return Ok(Accessors {
            getters: String::new(),
            setters: String::new(),
            enums,
        });
    };
    let mut enum_path = None;
    if matches!(zero, Value::Token(_)) && !*is_array && !property.allowed_tokens.is_empty() {
        let enum_name = format!("{view}{}", pascal(&name));
        enums = token_enum(&enum_name, property)?;
        ty = RustType {
            read: enum_name.clone(),
            write: enum_name.clone(),
            read_fn: format!("{enum_name}::read"),
            write_fn: format!("{enum_name}::write"),
        };
        enum_path = Some(enum_name);
    }
    let varying = property.variability == Variability::Varying;
    table.getter(
        &name,
        property,
        match enum_path {
            Some(path) => Getter::Enum {
                path,
                tokens: property.allowed_tokens.clone(),
                varying,
            },
            None => Getter::Value {
                zero: zero.clone(),
                is_array: *is_array,
                varying,
            },
        },
    );
    let getters = if varying {
        format!(
            "{constant}    attribute! {{\n{docs}        {name}, {name}_at, {:?}, {}, {}\n    }}\n",
            property.name, ty.read, ty.read_fn
        )
    } else {
        format!(
            "{constant}    uniform_attribute! {{\n{docs}        {name}, {:?}, {}, {}\n    }}\n",
            property.name, ty.read, ty.read_fn
        )
    };
    let setters = if varying {
        format!(
            "    set_attribute! {{\n{docs}        set_{name}, set_{name}_at, {:?}, {}, {}\n    }}\n",
            property.name, ty.write, ty.write_fn
        )
    } else {
        format!(
            "    set_uniform_attribute! {{\n{docs}        set_{name}, {:?}, {}, {}\n    }}\n",
            property.name, ty.write, ty.write_fn
        )
    };
    Ok(Accessors {
        getters,
        setters,
        enums,
    })
}

/// A `token_enum!` of a property's `allowedTokens`.
fn token_enum(name: &str, property: &Property) -> Result<String, String> {
    let mut out = format!(
        "token_enum! {{\n    #[doc = {:?}]\n    {name} {{\n",
        format!(
            "The tokens `{}` allows.",
            property
                .name
                .replace(layerstack::schema::INSTANCE_NAME_PLACEHOLDER, "<instance>")
        )
    );
    let mut variants = BTreeSet::new();
    for token in &property.allowed_tokens {
        let variant = pascal(&snake(token));
        if variant.is_empty()
            || variant.starts_with(|c: char| c.is_ascii_digit())
            || variant == "Other"
            || !variant.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(format!(
                "{}: the allowed token {token:?} makes no enum variant",
                property.name
            ));
        }
        if !variants.insert(variant.clone()) {
            return Err(format!(
                "{}: two allowed tokens make the variant `{variant}`",
                property.name
            ));
        }
        let _ = writeln!(
            out,
            "        #[doc = {:?}]\n        {variant} = {token:?},",
            format!("`{token}`")
        );
    }
    out.push_str("    }\n}\n\n");
    Ok(out)
}

/// The view, edit handle and enums of a typed schema.
fn typed_view(
    model: &Model,
    schema: &Schema,
    index: &Index<'_>,
    table: &mut TestTable,
) -> Result<String, String> {
    let view = type_name(&schema.name);
    let edit = format!("{view}Edit");
    let concrete = schema.kind == SchemaKind::ConcreteTyped;
    table.begin(schema, &view);
    let (base, base_edit, wrap_base, new_base) = match &schema.parent {
        Some(parent) => {
            let path = type_path(parent, index, &table.module);
            (
                format!("{path}<'a>"),
                format!("{path}Edit"),
                format!("{path}::from_view(prim)"),
                format!("{path}Edit::from_path(path)"),
            )
        }
        None => (
            "PrimView<'a>".to_string(),
            "PrimEdit".to_string(),
            "prim".to_string(),
            "PrimEdit::new(path)".to_string(),
        ),
    };
    let mut getters = String::new();
    let mut setters = String::new();
    let mut enums = String::new();
    for property in local_properties(schema, index) {
        let accessors = accessors(model, schema, property, &view, table)?;
        getters.push_str(&accessors.getters);
        setters.push_str(&accessors.setters);
        enums.push_str(&accessors.enums);
    }
    for built_in in local_built_ins(schema, index) {
        let api = type_path(built_in, index, &table.module);
        let method = snake(&type_name(built_in));
        table.built_in(&method);
        let _ = write!(
            getters,
            "    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> {api}<'a> {{\n        \
             {api}::from_view(PrimView::new(self.scene(), self.path()))\n    }}\n\n",
            format!(
                "The prim's built-in `{built_in}`, which every `{}` has.",
                schema.name
            )
        );
        let _ = write!(
            setters,
            "    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> {api}Edit {{\n        \
             {api}Edit::from_path(self.path())\n    }}\n\n",
            format!("An edit handle for the prim's built-in `{built_in}`.")
        );
    }
    let kind_text = if concrete { "concrete" } else { "abstract" };
    let inherits = schema
        .parent
        .as_deref()
        .map(|parent| {
            format!(
                ", inheriting [`{}`]",
                type_path(parent, index, &table.module)
            )
        })
        .unwrap_or_default();
    let docs = doc_attrs(
        "",
        &[
            doc_text(&schema.doc),
            format!(
                "The view of OpenUSD's {kind_text} typed schema `{}`{inherits}. Construct it \
                 with [`{view}::new`], which checks the prim is of the schema (`IsA`); it \
                 derefs to the view of the schema it inherits from. For anything it does not \
                 offer, resolve the property by its USD name on [`Scene::stage`](crate::Scene::stage) \
                 (`Stage::resolve_value_with_schema` returns the raw resolved value).",
                schema.name
            ),
        ],
    );
    let define = if concrete {
        format!(
            "    /// Defines `path` as a new `def {name}` prim spec in the edit's target.\n    \
             ///\n    /// OpenUSD: `UsdStage::DefinePrim` for a prim the target does not\n    \
             /// author yet; defining a spec that exists is rejected when the\n    \
             /// transaction applies.\n    \
             pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> {edit} {{\n        \
             edit.define(path, Self::SCHEMA);\n        {edit}::from_path(path)\n    }}\n\n",
            name = schema.name
        )
    } else {
        String::new()
    };
    Ok(format!(
        "{enums}{docs}#[derive(Clone, Copy, Debug)]\npub struct {view}<'a> {{\n    base: {base},\n}}\n\n\
         impl<'a> Deref for {view}<'a> {{\n    type Target = {base};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         impl<'a> {view}<'a> {{\n    /// The schema's name.\n    pub const SCHEMA: &'static str = {name:?};\n\n    \
         #[doc = {new_doc:?}]\n    #[must_use]\n    \
         pub fn new(scene: &Scene<'a>, path: PathId) -> Option<Self> {{\n        \
         scene\n            .is_a(path, Self::SCHEMA)\n            .then(|| Self::from_view(PrimView::new(*scene, path)))\n    }}\n\n    \
         pub(crate) fn from_view(prim: PrimView<'a>) -> Self {{\n        Self {{\n            base: {wrap_base},\n        }}\n    }}\n\n    \
         /// An edit handle for this prim.\n    #[must_use]\n    \
         pub fn edit(&self) -> {edit} {{\n        {edit}::from_path(self.path())\n    }}\n\n\
         {define}{getters}}}\n\n\
         #[doc = {edit_doc:?}]\n#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub struct {edit} {{\n    base: {base_edit},\n}}\n\n\
         impl Deref for {edit} {{\n    type Target = {base_edit};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         impl {edit} {{\n    /// A handle authoring the prim at `path` through `edit`, if the prim\n    /// exists: on the edit's stage, or defined earlier in `edit`. Setters\n    /// never create a prim that does not.\n    #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: PathId) -> Option<Self> {{\n        \
         edit.exists(path).then(|| Self::from_path(path))\n    }}\n\n    \
         pub(crate) fn from_path(path: PathId) -> Self {{\n        Self {{\n            base: {new_base},\n        }}\n    }}\n\n\
         {setters}}}\n\n",
        name = schema.name,
        new_doc = format!(
            "A view of the prim at `path`, if it is a `{}` or of a schema derived from it.",
            schema.name
        ),
        edit_doc = format!(
            "Authors the properties of OpenUSD's `{}` through a [`SchemaEdit`]; it derefs to \
             the handle of the schema it inherits from.",
            schema.name
        ),
    ))
}

/// The view, edit handle and enums of an applied schema.
fn applied_view(
    model: &Model,
    schema: &Schema,
    multiple: bool,
    table: &mut TestTable,
) -> Result<String, String> {
    let view = type_name(&schema.name);
    let edit = format!("{view}Edit");
    let mut getters = String::new();
    let mut setters = String::new();
    let mut enums = String::new();
    table.begin(schema, &view);
    method_names(schema, &BTreeMap::new())?;
    for property in &schema.properties {
        let accessors = accessors(model, schema, property, &view, table)?;
        getters.push_str(&accessors.getters);
        setters.push_str(&accessors.setters);
        enums.push_str(&accessors.enums);
    }
    let (base, base_edit) = if multiple {
        ("InstanceView<'a>", "InstanceEdit")
    } else {
        ("PrimView<'a>", "PrimEdit")
    };
    let kind_text = if multiple {
        "multiple-apply"
    } else {
        "single-apply"
    };
    let docs = doc_attrs(
        "",
        &[
            doc_text(&schema.doc),
            format!(
                "The view of OpenUSD's {kind_text} API schema `{}`. Get it with [`{view}::get`], \
                 which checks the prim has the schema applied (`HasAPI`). For anything it does \
                 not offer, resolve the property by its USD name on \
                 [`Scene::stage`](crate::Scene::stage) (`Stage::resolve_value_with_schema` \
                 returns the raw resolved value).",
                schema.name
            ),
        ],
    );
    let constructors = if multiple {
        format!(
            "    /// A view of the instance `instance` of the schema on the prim at `path`,\n    \
             /// if the prim has it applied.\n    #[must_use]\n    \
             pub fn get(scene: &Scene<'a>, path: PathId, instance: &str) -> Option<Self> {{\n        \
             if !scene.has_api(path, Self::SCHEMA, Some(instance)) {{\n            return None;\n        }}\n        \
             let instance = scene.instance_name(instance)?;\n        \
             Some(Self::from_view(PrimView::new(*scene, path), instance))\n    }}\n\n    \
             /// Every instance of the schema the prim at `path` has applied, in the\n    \
             /// order its definition applies them.\n    #[must_use]\n    \
             pub fn instances(scene: &Scene<'a>, path: PathId) -> ::alloc::vec::Vec<Self> {{\n        \
             scene\n            .instances(path, Self::SCHEMA)\n            .into_iter()\n            \
             .map(|instance| Self::from_view(PrimView::new(*scene, path), instance))\n            .collect()\n    }}\n\n    \
             pub(crate) fn from_view(prim: PrimView<'a>, instance: &'a str) -> Self {{\n        \
             Self {{\n            base: InstanceView::new(prim, instance),\n        }}\n    }}\n\n    \
             /// An edit handle for this instance.\n    #[must_use]\n    \
             pub fn edit(&self) -> {edit} {{\n        {edit}::from_path(self.path(), self.instance())\n    }}\n\n    \
             /// Applies the instance `instance` of the schema to `path` (a `prepend\n    \
             /// apiSchemas` entry in the edit's target) if the prim may have it.\n    \
             ///\n    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`.\n    \
             ///\n    /// # Errors\n    ///\n    /// Why the schema cannot be applied there.\n    \
             pub fn apply(\n        edit: &mut SchemaEdit<'_>,\n        path: PathId,\n        instance: &str,\n    ) -> Result<{edit}, CannotApply> {{\n        \
             edit.apply(path, Self::SCHEMA, Some(instance))?;\n        Ok({edit}::from_path(path, instance))\n    }}\n\n"
        )
    } else {
        format!(
            "    /// A view of the schema on the prim at `path`, if the prim has it\n    \
             /// applied.\n    #[must_use]\n    \
             pub fn get(scene: &Scene<'a>, path: PathId) -> Option<Self> {{\n        \
             scene\n            .has_api(path, Self::SCHEMA, None)\n            .then(|| Self::from_view(PrimView::new(*scene, path)))\n    }}\n\n    \
             pub(crate) fn from_view(prim: PrimView<'a>) -> Self {{\n        Self {{ base: prim }}\n    }}\n\n    \
             /// An edit handle for this prim.\n    #[must_use]\n    \
             pub fn edit(&self) -> {edit} {{\n        {edit}::from_path(self.path())\n    }}\n\n    \
             /// Applies the schema to `path` (a `prepend apiSchemas` entry in the\n    \
             /// edit's target) if the prim may have it.\n    \
             ///\n    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`.\n    \
             ///\n    /// # Errors\n    ///\n    /// Why the schema cannot be applied there.\n    \
             pub fn apply(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<{edit}, CannotApply> {{\n        \
             edit.apply(path, Self::SCHEMA, None)?;\n        Ok({edit}::from_path(path))\n    }}\n\n"
        )
    };
    let new_edit = if multiple {
        "    /// A handle authoring the instance `instance` on the prim at `path`\n    \
         /// through `edit`, if the prim exists: on the edit's stage, or defined\n    \
         /// earlier in `edit`. Setters never create a prim that does not.\n    \
         #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: PathId, instance: &str) -> Option<Self> {\n        \
         edit.exists(path).then(|| Self::from_path(path, instance))\n    }\n\n    \
         pub(crate) fn from_path(path: PathId, instance: &str) -> Self {\n        \
         Self {\n            base: InstanceEdit::new(path, instance),\n        }\n    }\n\n"
            .to_string()
    } else {
        "    /// A handle authoring the prim at `path` through `edit`, if the prim\n    /// exists: on the edit's stage, or defined earlier in `edit`. Setters\n    /// never create a prim that does not.\n    #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: PathId) -> Option<Self> {\n        \
         edit.exists(path).then(|| Self::from_path(path))\n    }\n\n    \
         pub(crate) fn from_path(path: PathId) -> Self {\n        Self {\n            base: PrimEdit::new(path),\n        }\n    }\n\n"
            .to_string()
    };
    let edit_derive = if multiple {
        "#[derive(Clone, Debug, PartialEq, Eq, Hash)]"
    } else {
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]"
    };
    Ok(format!(
        "{enums}{docs}#[derive(Clone, Copy, Debug)]\npub struct {view}<'a> {{\n    base: {base},\n}}\n\n\
         impl<'a> Deref for {view}<'a> {{\n    type Target = {base};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         impl<'a> {view}<'a> {{\n    /// The schema's name.\n    pub const SCHEMA: &'static str = {name:?};\n\n\
         {constructors}{getters}}}\n\n\
         #[doc = {edit_doc:?}]\n{edit_derive}\npub struct {edit} {{\n    base: {base_edit},\n}}\n\n\
         impl Deref for {edit} {{\n    type Target = {base_edit};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         impl {edit} {{\n{new_edit}{setters}}}\n\n",
        name = schema.name,
        edit_doc = format!(
            "Authors the properties of OpenUSD's `{}` through a [`SchemaEdit`].",
            schema.name
        ),
    ))
}

/// What a table entry's getter reads.
enum Getter {
    /// A relationship's targets.
    Targets,
    /// An attribute value of a plain Rust type.
    Value {
        /// The type's zero value.
        zero: Value,
        /// Whether it is an array.
        is_array: bool,
        /// Whether it has `_at` accessors.
        varying: bool,
    },
    /// A token attribute read as an enum.
    Enum {
        /// The enum's name in its module.
        path: String,
        /// Its allowed tokens.
        tokens: Vec<String>,
        /// Whether it has `_at` accessors.
        varying: bool,
    },
}

/// One view the table exercises.
struct TableView {
    /// Its domain's module.
    module: String,
    /// The view type's name.
    view: String,
    /// The schema's name.
    schema: String,
    /// The schema's kind.
    kind: SchemaKind,
    /// `(method, USD property name, what it reads)`.
    getters: Vec<(String, String, Getter)>,
    /// The built-in accessors of a typed view.
    built_ins: Vec<String>,
}

/// The table of every generated view, getter, setter and enum, which the
/// conformance test `schema_views` exercises against OpenUSD.
///
/// Every concrete typed and applied schema gets two prims: `/Fallback_<S>`,
/// which authors nothing but the schema, and `/Authored_<S>`, where every
/// setter authors a value (a default and, for a varying attribute, a time
/// sample at time 2). An abstract typed schema is read and authored on the
/// prims of the first concrete schema derived from it; an applied schema is
/// applied to a prim of the first concrete schema it may be applied to
/// (`Scope` when unrestricted), a multiple-apply one with its first allowed
/// instance name, else `a`.
#[derive(Default)]
struct TestTable {
    /// The views, in generation order.
    views: Vec<TableView>,
    /// The module of the schema being generated.
    module: String,
}

/// The time of the time samples the table authors and reads.
const SAMPLE_TIME: &str = "2.0";

impl TestTable {
    fn begin(&mut self, schema: &Schema, view: &str) {
        self.views.push(TableView {
            module: self.module.clone(),
            view: view.into(),
            schema: schema.name.clone(),
            kind: schema.kind,
            getters: Vec::new(),
            built_ins: Vec::new(),
        });
    }

    fn getter(&mut self, method: &str, property: &Property, getter: Getter) {
        if let Some(view) = self.views.last_mut() {
            view.getters
                .push((method.into(), property.name.clone(), getter));
        }
    }

    fn built_in(&mut self, method: &str) {
        if let Some(view) = self.views.last_mut() {
            view.built_ins.push(method.into());
        }
    }

    fn finish(&self, model: &Model) -> Result<String, String> {
        let schemas: BTreeMap<&str, &Schema> = model
            .domains
            .iter()
            .flat_map(|d| &d.schemas)
            .map(|s| (s.name.as_str(), s))
            .collect();
        let views: BTreeMap<&str, &TableView> =
            self.views.iter().map(|v| (v.schema.as_str(), v)).collect();
        let is_a = |schema: &str, ancestor: &str| {
            let mut current = Some(schema);
            while let Some(name) = current {
                if name == ancestor {
                    return true;
                }
                current = schemas.get(name).and_then(|s| s.parent.as_deref());
            }
            false
        };
        // `ancestor` if it is concrete, else the first concrete schema
        // derived from it.
        let concrete = |ancestor: &str| {
            views
                .get(ancestor)
                .copied()
                .filter(|v| v.kind == SchemaKind::ConcreteTyped)
                .or_else(|| {
                    self.views
                        .iter()
                        .find(|v| v.kind == SchemaKind::ConcreteTyped && is_a(&v.schema, ancestor))
                })
        };
        let path_of = |view: &TableView| format!("{}::{}", view.module, view.view);

        let mut prims = Vec::new();
        let mut structure = String::new();
        let mut values = String::new();
        let mut reads = String::new();
        let mut enums = String::new();
        for view in &self.views {
            let schema = schemas[view.schema.as_str()];
            let view_path = path_of(view);
            // The prims to read and author on, and the instance name.
            let (host, instance) = match view.kind {
                SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped => {
                    let Some(host) = concrete(&view.schema) else {
                        if view.getters.is_empty() {
                            continue;
                        }
                        return Err(format!(
                            "{}: no concrete schema derives from it",
                            view.schema
                        ));
                    };
                    (host.schema.clone(), None)
                }
                SchemaKind::SingleApplyApi | SchemaKind::MultipleApplyApi => {
                    let instance = (view.kind == SchemaKind::MultipleApplyApi).then(|| {
                        schema
                            .allowed_instance_names
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "a".into())
                    });
                    let restriction = instance
                        .as_ref()
                        .and_then(|i| {
                            schema
                                .instance_can_only_apply_to
                                .iter()
                                .find(|(name, _)| name == i)
                                .map(|(_, types)| types)
                        })
                        .unwrap_or(&schema.can_only_apply_to);
                    let host_type = match restriction.first() {
                        None => views.get("Scope").copied(),
                        Some(ty) => concrete(ty),
                    }
                    .ok_or_else(|| format!("{}: no prim type to apply it to", view.schema))?;
                    let fallback = format!("/Fallback_{}", view.schema);
                    let authored = format!("/Authored_{}", view.schema);
                    for prim in [&fallback, &authored] {
                        let _ = writeln!(
                            structure,
                            "    {}::define(edit, h.path({prim:?}));",
                            path_of(host_type)
                        );
                        let instance_arg = instance
                            .as_ref()
                            .map(|i| format!(", {i:?}"))
                            .unwrap_or_default();
                        let _ = writeln!(
                            structure,
                            "    {view_path}::apply(edit, h.path({prim:?}){instance_arg})\n        \
                             .expect(\"{} applies to a {}\");",
                            view.schema, host_type.schema
                        );
                    }
                    (view.schema.clone(), instance)
                }
            };
            let fallback = format!("/Fallback_{host}");
            let authored = format!("/Authored_{host}");
            if view.kind == SchemaKind::ConcreteTyped {
                for prim in [&fallback, &authored] {
                    prims.push(prim.clone());
                    let _ = writeln!(
                        structure,
                        "    {view_path}::define(edit, h.path({prim:?}));"
                    );
                }
            } else if view.kind != SchemaKind::AbstractTyped {
                prims.push(fallback.clone());
                prims.push(authored.clone());
            }
            let instance_arg = instance
                .as_ref()
                .map(|i| format!(", {i:?}"))
                .unwrap_or_default();
            // The getters whose values the fixture layer can hold.
            let getters: Vec<&(String, String, Getter)> =
                view.getters.iter().filter(|(_, _, g)| saved(g)).collect();
            // Setters.
            if !getters.is_empty() {
                let _ = writeln!(
                    values,
                    "    {{\n        let e = {view_path}Edit::new(edit, h.path({authored:?}){instance_arg})\n            .expect(\"the prim is defined\");"
                );
            }
            for (method, _, getter) in &getters {
                match getter {
                    Getter::Targets => {
                        let _ = writeln!(values, "        e.set_{method}(edit, h.targets());");
                    }
                    Getter::Value {
                        zero,
                        is_array,
                        varying,
                    } => {
                        let first = literal(zero, *is_array, 1)?;
                        let _ = writeln!(values, "        e.set_{method}(edit, {first});");
                        if *varying {
                            let second = literal(zero, *is_array, 2)?;
                            let _ = writeln!(
                                values,
                                "        e.set_{method}_at(edit, {SAMPLE_TIME}, {second});"
                            );
                        }
                    }
                    Getter::Enum {
                        path,
                        tokens,
                        varying,
                    } => {
                        let token = |k: usize| {
                            format!(
                                "{}::{path}::from_token({:?})",
                                view.module,
                                tokens[k % tokens.len()]
                            )
                        };
                        let _ = writeln!(values, "        e.set_{method}(edit, {});", token(1));
                        if *varying {
                            let _ = writeln!(
                                values,
                                "        e.set_{method}_at(edit, {SAMPLE_TIME}, {});",
                                token(2)
                            );
                        }
                    }
                }
            }
            if !getters.is_empty() {
                values.push_str("    }\n");
            }
            // Getters.
            let get = match &instance {
                None if matches!(
                    view.kind,
                    SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped
                ) =>
                {
                    format!("{view_path}::new(scene, h.path(prim))")
                }
                None => format!("{view_path}::get(scene, h.path(prim))"),
                Some(i) => format!("{view_path}::get(scene, h.path(prim), {i:?})"),
            };
            let _ = writeln!(
                reads,
                "    for prim in [{fallback:?}, {authored:?}] {{\n        \
                 let v = {get}.expect(\"a {} at the prim\");\n        let _ = v.edit();",
                view.schema
            );
            if let Some(i) = &instance {
                let _ = writeln!(
                    reads,
                    "        assert!(\n            {view_path}::instances(scene, h.path(prim))\n                \
                     .iter()\n                .any(|v| v.instance() == {i:?}),\n            \"{i} is among the instances\"\n        );"
                );
            }
            for built_in in &view.built_ins {
                let _ = writeln!(reads, "        let _ = v.{built_in}();");
            }
            for (method, usd, getter) in &getters {
                let usd = match &instance {
                    Some(i) => instance_property(usd, i),
                    None => usd.clone(),
                };
                match getter {
                    Getter::Targets => {
                        let _ = writeln!(reads, "        r.targets(prim, {usd:?}, &v.{method}());");
                    }
                    Getter::Value { varying, .. } => {
                        let _ = writeln!(
                            reads,
                            "        r.value(prim, {usd:?}, When::Default, v.{method}());"
                        );
                        if *varying {
                            let _ = writeln!(
                                reads,
                                "        r.value(prim, {usd:?}, When::Sample, v.{method}_at({SAMPLE_TIME}, HELD));"
                            );
                        }
                    }
                    Getter::Enum { path, varying, .. } => {
                        let path = format!("{}::{path}", view.module);
                        let _ = writeln!(
                            reads,
                            "        r.value(prim, {usd:?}, When::Default, v.{method}().as_ref().map({path}::as_str));\n        \
                             r.allowed(prim, {usd:?}, {path}::TOKENS);"
                        );
                        if *varying {
                            let _ = writeln!(
                                reads,
                                "        r.value(prim, {usd:?}, When::Sample, v.{method}_at({SAMPLE_TIME}, HELD).as_ref().map({path}::as_str));"
                            );
                        }
                        let _ = writeln!(
                            enums,
                            "    r.round_trip({path}::TOKENS, {path}::from_token, {path}::as_str);"
                        );
                    }
                }
            }
            reads.push_str("    }\n");
        }

        let header = header(model).replace(
            "`LICENSE-TOST-1.0` and\n// `NOTICE`",
            "the `LICENSE-TOST-1.0`\n// and `NOTICE` of `layerstack_schemas`",
        );
        let mut out = header;
        out.push_str(
            "\n//! Every generated view, getter, setter and enum of `layerstack_schemas`,\n\
             //! which `tests/schema_views.rs` checks against OpenUSD.\n//!\n\
             //! Every concrete typed and applied schema gets two prims: `/Fallback_<S>`,\n\
             //! which authors nothing but the schema, and `/Authored_<S>`, where every\n\
             //! setter authors a value (a default and, for a varying attribute, a time\n\
             //! sample). An abstract typed schema is read and authored on the prims of\n\
             //! the first concrete schema derived from it; an applied schema is applied\n\
             //! to a prim of the first concrete schema it may be applied to (`Scope`\n\
             //! when unrestricted), a multiple-apply one with its first allowed instance\n\
             //! name, else `a`. Path expressions are left out: USDA save does not\n\
             //! write them yet.\n\n\
             use layerstack::InterpolationType;\nuse layerstack_schemas::*;\n\n\
             use super::{Harness, Readings, When};\n\n\
             /// How the table reads time samples.\nconst HELD: InterpolationType = InterpolationType::Held;\n\n\
             /// Every prim the table authors.\npub(crate) const PRIMS: &[&str] = &[\n",
        );
        for prim in &prims {
            let _ = writeln!(out, "    {prim:?},");
        }
        let _ = write!(
            out,
            "];\n\n/// Defines every prim and applies every applied schema, then authors\n\
             /// through every setter.\npub(crate) fn author(edit: &mut SchemaEdit<'_>, h: &Harness) {{\n\
             {structure}{values}}}\n\n\
             /// Reads every getter of every view.\npub(crate) fn read(scene: &Scene<'_>, h: &Harness, r: &mut Readings<'_>) {{\n\
             {reads}}}\n\n\
             /// Round-trips every enum's tokens.\npub(crate) fn enums(r: &mut Readings<'_>) {{\n{enums}}}\n"
        );
        Ok(out)
    }
}

/// Whether the table authors and reads a getter's property: USDA save
/// writes no `pathExpression` value yet, so the fixture layer cannot hold
/// one (`tests/schema_views.rs` checks those setters on the live stage).
fn saved(getter: &Getter) -> bool {
    !matches!(
        getter,
        Getter::Value {
            zero: Value::PathExpression(_),
            ..
        }
    )
}

/// A multiple-apply property's name for `instance`, as
/// `layerstack_schemas` names it.
fn instance_property(template: &str, instance: &str) -> String {
    let placeholder = layerstack::schema::INSTANCE_NAME_PLACEHOLDER;
    let mut replaced = false;
    template
        .split(':')
        .map(|segment| {
            if !replaced && segment == placeholder {
                replaced = true;
                instance
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// A Rust literal for a setter of a value like `zero`, distinct for each
/// `k`, exactly representable in every floating-point type (halves
/// included).
fn literal(zero: &Value, is_array: bool, k: u32) -> Result<String, String> {
    if is_array {
        return Ok(format!(
            "&[{}, {}]",
            literal(zero, false, k)?,
            literal(zero, false, k + 1)?
        ));
    }
    let float = |i: u32| format!("{i}.5");
    let floats = |n: u32| {
        let items: Vec<String> = (0..n).map(|i| float(k + i)).collect();
        format!("[{}]", items.join(", "))
    };
    let ints = |n: u32| {
        let items: Vec<String> = (0..n).map(|i| (k + i + 2).to_string()).collect();
        format!("[{}]", items.join(", "))
    };
    let matrix = |n: u32| {
        let rows: Vec<String> = (0..n)
            .map(|row| {
                let items: Vec<String> = (0..n).map(|col| float(k + row * n + col)).collect();
                format!("[{}]", items.join(", "))
            })
            .collect();
        format!("[{}]", rows.join(", "))
    };
    Ok(match zero {
        Value::Bool(_) => (k % 2 == 1).to_string(),
        Value::UChar(_) | Value::Int(_) | Value::UInt(_) | Value::Int64(_) | Value::UInt64(_) => {
            (k + 2).to_string()
        }
        Value::Half(_) | Value::Float(_) | Value::Double(_) | Value::TimeCode(_) => float(k),
        Value::String(_) => format!("\"text{k}\""),
        Value::Token(_) => format!("\"token{k}\""),
        Value::Asset(_) => format!("\"asset{k}.usda\""),
        Value::PathExpression(_) => format!("\"/Expression{k}\""),
        Value::Vec2f(_) | Value::Vec2d(_) | Value::Vec2h(_) => floats(2),
        Value::Vec3f(_) | Value::Vec3d(_) | Value::Vec3h(_) => floats(3),
        Value::Vec4f(_) | Value::Vec4d(_) | Value::Vec4h(_) => floats(4),
        Value::Quatf(_) | Value::Quatd(_) | Value::Quath(_) => floats(4),
        Value::Vec2i(_) => ints(2),
        Value::Vec3i(_) => ints(3),
        Value::Vec4i(_) => ints(4),
        Value::Matrix2d(_) => matrix(2),
        Value::Matrix3d(_) => matrix(3),
        Value::Matrix4d(_) => matrix(4),
        other => return Err(format!("no literal for values like {other:?}")),
    })
}
