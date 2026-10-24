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
    "from_prim_unchecked",
    "from_instance_unchecked",
    "from_prim",
    "from_instance",
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
    "try_read_value",
    "read_targets",
    "write_value",
    "write_value_at",
    "write_targets",
    "add_target",
    "remove_target",
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
pub(crate) fn pascal(snake: &str) -> String {
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

/// Where generated views live, which decides the paths their code names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// `layerstack_schemas` itself: OpenUSD's domains.
    BuiltIn,
    /// A module of another crate, for a schema library of its own, which
    /// declares `alloc` and imports `Arc` and `Vec` itself
    /// (`compile --rust`).
    Library,
}

impl Target {
    /// The path of `layerstack_schemas`.
    pub(crate) fn schemas(self) -> &'static str {
        match self {
            Self::BuiltIn => "crate",
            Self::Library => "::layerstack_schemas",
        }
    }

    /// The path of `Arc`.
    pub(crate) fn arc(self) -> &'static str {
        match self {
            Self::BuiltIn => "::alloc::sync::Arc",
            Self::Library => "Arc",
        }
    }

    /// The path of `Vec`.
    pub(crate) fn vec(self) -> &'static str {
        match self {
            Self::BuiltIn => "::alloc::vec::Vec",
            Self::Library => "Vec",
        }
    }
}

/// How a USD value type reads and writes in Rust.
pub(crate) struct RustType {
    /// The getter's type.
    pub(crate) read: String,
    /// The setter's type.
    pub(crate) write: String,
    /// The expression reading a `Value`.
    pub(crate) read_fn: String,
    /// The expression writing one.
    pub(crate) write_fn: String,
    /// The call writing one: `write_fn` applied to `VALUE` and `TOKENS`.
    write_call: String,
}

impl RustType {
    /// The expression writing `value` with the interner `tokens`.
    fn write(&self, value: &str, tokens: &str) -> String {
        self.write_call
            .replace("VALUE", value)
            .replace("TOKENS", tokens)
    }
}

/// Element representations that match native typed-array storage exactly.
pub(crate) fn shared_array(zero: &Value) -> bool {
    matches!(
        zero,
        Value::Bool(_)
            | Value::UChar(_)
            | Value::Int(_)
            | Value::UInt(_)
            | Value::Int64(_)
            | Value::UInt64(_)
            | Value::Float(_)
            | Value::Double(_)
            | Value::TimeCode(_)
            | Value::Vec2f(_)
            | Value::Vec3f(_)
            | Value::Vec4f(_)
            | Value::Vec2d(_)
            | Value::Vec3d(_)
            | Value::Vec4d(_)
            | Value::Vec2i(_)
            | Value::Vec3i(_)
            | Value::Vec4i(_)
            | Value::Quatf(_)
            | Value::Quatd(_)
    )
}

/// The Rust type of a property whose element type's zero value is `zero`;
/// `None` for a value-less type (`opaque`).
pub(crate) fn rust_type(zero: &Value, is_array: bool) -> Result<Option<RustType>, String> {
    rust_type_in(zero, is_array, Target::BuiltIn)
}

/// [`rust_type`] for views in `target`.
pub(crate) fn rust_type_in(
    zero: &Value,
    is_array: bool,
    target: Target,
) -> Result<Option<RustType>, String> {
    let (schemas, arc, vec) = (target.schemas(), target.arc(), target.vec());
    let arc_str = format!("{arc}<str>");
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
        Value::String(_) => (arc_str.as_str(), "&str", "string"),
        Value::Asset(_) => (arc_str.as_str(), "&str", "asset"),
        Value::PathExpression(_) => (arc_str.as_str(), "&str", "path_expression"),
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
    let element_wise = matches!(
        zero,
        Value::String(_) | Value::Asset(_) | Value::PathExpression(_) | Value::Token(_)
    );
    let value = format!("{schemas}::value");
    Ok(Some(if is_array && matches!(zero, Value::String(_)) {
        // A `string[]` reads as its shared text buffer, as numeric arrays
        // read as theirs (AOUSD Core §6.2).
        RustType {
            read: format!("{arc}<::layerstack::StringArray>"),
            write: "&[&str]".into(),
            read_fn: format!("{value}::read_string_array_shared"),
            write_fn: format!("{value}::write_string_array"),
            write_call: format!("{value}::write_string_array(VALUE, TOKENS)"),
        }
    } else if is_array && element_wise {
        RustType {
            read: format!("{vec}<{read}>"),
            write: format!("&[{write}]"),
            read_fn: format!("|v, t| {value}::read_array(v, t, {value}::read_{name})"),
            write_fn: format!("|v, t| {value}::write_array(v, t, {value}::write_{name})"),
            write_call: format!("{value}::write_array(VALUE, TOKENS, {value}::write_{name})"),
        }
    } else if is_array {
        RustType {
            read: if shared_array(zero) {
                format!("{arc}<{vec}<{read}>>")
            } else {
                format!("{vec}<{read}>")
            },
            write: format!("&[{write}]"),
            read_fn: if shared_array(zero) {
                format!("{value}::read_{name}_array_shared")
            } else {
                format!("{value}::read_{name}_array")
            },
            write_fn: format!("{value}::write_{name}_array"),
            write_call: format!("{value}::write_{name}_array(VALUE, TOKENS)"),
        }
    } else {
        RustType {
            read: read.into(),
            write: write.into(),
            read_fn: format!("{value}::read_{name}"),
            write_fn: format!("{value}::write_{name}"),
            write_call: format!("{value}::write_{name}(VALUE, TOKENS)"),
        }
    }))
}

/// Text for a `#[doc = ...]` attribute: Markdown-escaped, on one line.
/// Code spans are kept as written: Markdown decodes neither entities nor
/// backslash escapes inside them, so escaping there would show literally.
fn doc_text(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if !in_code && word.contains("://") {
            let _ = write!(out, "`{}`", word.replace('`', ""));
            continue;
        }
        for c in word.chars() {
            match c {
                '`' => {
                    in_code = !in_code;
                    out.push(c);
                }
                c if in_code => out.push(c),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '[' => out.push_str("\\["),
                ']' => out.push_str("\\]"),
                '\\' => out.push_str("\\\\"),
                '*' => out.push_str("\\*"),
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

/// Search aliases only for native classes and declarations found in their headers.
fn cpp_method_alias(schema: &Schema, method: &str, indent: &str) -> String {
    schema
        .cpp
        .as_ref()
        .filter(|(_, header)| header.contains(&format!("{method}(")))
        .map_or_else(String::new, |(class, _)| {
            format!(
                "{indent}#[doc(alias = {:?})]\n{indent}#[doc(alias = {method:?})]\n",
                format!("{class}::{method}")
            )
        })
}

/// Native schema type names are independent of Rust's API/underscore spelling.
fn schema_aliases(schema: &Schema, view: &str) -> String {
    let mut docs = String::new();
    if let Some((class, _)) = &schema.cpp {
        let _ = writeln!(
            docs,
            "#[doc = {:?}]\n#[doc(alias = {class:?})]",
            format!("C++ schema class: `{class}`.")
        );
    }
    if schema.name != view {
        let _ = writeln!(docs, "#[doc(alias = {:?})]", schema.name);
    }
    docs
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
pub(crate) type Index<'m> = BTreeMap<&'m str, (&'m Schema, String)>;

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
        let placeholder = layerstack::schema::INSTANCE_NAME_PLACEHOLDER;
        if property.api_name.is_none()
            && property.name.rsplit(':').next() == Some(placeholder)
            && !RENAMES
                .iter()
                .any(|(s, p, _)| *s == schema.name && *p == property.name)
        {
            return Err(format!(
                "{}: `{}` is named for the instance alone, so only an `apiName` can name its \
                 accessors",
                schema.name, property.name
            ));
        }
        let name = method_name(&schema.name, property);
        out.push((name.clone(), Some(property)));
        out.push((format!("set_{name}"), Some(property)));
        if property.kind == PropertyKind::Relationship {
            out.push((format!("add_to_{name}"), Some(property)));
            out.push((format!("remove_from_{name}"), Some(property)));
        }
        if property
            .value_type
            .as_ref()
            .is_some_and(|(_, array, _)| *array)
        {
            out.push((format!("try_{name}"), Some(property)));
        }
        if property
            .value_type
            .as_ref()
            .is_some_and(|(_, array, zero)| *array && shared_array(zero))
        {
            for suffix in ["owned", "shared"] {
                out.push((format!("set_{name}_{suffix}"), Some(property)));
                if property.variability == Variability::Varying {
                    out.push((format!("set_{name}_{suffix}_at"), Some(property)));
                }
            }
        }
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

/// The path of a schema's view type from the module being generated.
fn type_path(cx: &Context<'_, '_>, schema: &str) -> String {
    let module = cx
        .index
        .get(schema)
        .map(|(_, m)| m.as_str())
        .unwrap_or("usd");
    if module == cx.module {
        type_name(schema)
    } else {
        format!("{}::{module}::{}", cx.target.schemas(), type_name(schema))
    }
}

/// What the views of one module are generated with.
pub(crate) struct Context<'c, 'm> {
    /// The interner fallback tokens are in.
    pub(crate) tokens: &'c TokenInterner,
    /// Every schema the views may name, with its module.
    pub(crate) index: &'c Index<'m>,
    /// The module being generated.
    pub(crate) module: String,
    /// Where the module lives.
    pub(crate) target: Target,
    /// Whose schemas they are, as the views' documentation says
    /// (`OpenUSD's`).
    pub(crate) owner: String,
}

/// The `#[allow]` a library's items carry for documentation as its schema
/// writes it; OpenUSD's domain files allow it once, for the file.
fn item_allow(target: Target) -> &'static str {
    match target {
        Target::BuiltIn => "",
        Target::Library => {
            "#[allow(\n    clippy::doc_markdown,\n    clippy::too_long_first_doc_paragraph,\n    \
             reason = \"documentation as the schema writes it\"\n)]\n"
        }
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
    let cx = Context {
        tokens: &model.tokens,
        index,
        module: module(domain.plugin),
        target: Target::BuiltIn,
        owner: "OpenUSD's".into(),
    };
    let mut out = header(model);
    let _ = write!(
        out,
        "\n//! OpenUSD's `{}` schemas as typed views, edit handles and token enums.{}\n\n\
         #![allow(\n    clippy::doc_markdown,\n    clippy::too_long_first_doc_paragraph,\n    \
         reason = \"documentation as OpenUSD writes it\"\n)]\n\n",
        domain.name,
        origin_note(domain)
    );
    let schemas: Vec<&Schema> = domain.schemas.iter().collect();
    let items = module_items(&cx, &schemas, table)?;
    let _ = write!(
        out,
        "use core::ops::Deref;\n\nuse layerstack::{{{}}};\n\nuse crate::{{{}}};\n{}",
        items.layerstack.join(", "),
        items.schemas.join(", "),
        items.body
    );
    Ok(out)
}

/// The views, edit handles and enums of a module, with the items they
/// import.
pub(crate) struct Items {
    /// What they import from `layerstack`.
    pub(crate) layerstack: Vec<&'static str>,
    /// What they import from `layerstack_schemas`.
    pub(crate) schemas: Vec<&'static str>,
    /// The views, edit handles and enums.
    pub(crate) body: String,
}

/// The views, edit handles and enums of `schemas`, for a module that
/// holds them all, which imports `Deref` and the [`Items`]' imports.
pub(crate) fn module_items(
    cx: &Context<'_, '_>,
    schemas: &[&Schema],
    table: &mut TestTable,
) -> Result<Items, String> {
    let mut body = String::new();
    for schema in schemas {
        let text = match schema.kind {
            SchemaKind::ConcreteTyped | SchemaKind::AbstractTyped => {
                chain_names(schema, cx.index)?;
                typed_view(cx, schema, table)?
            }
            SchemaKind::SingleApplyApi => applied_view(cx, schema, false, table)?,
            SchemaKind::MultipleApplyApi => applied_view(cx, schema, true, table)?,
        };
        body.push_str(&text);
    }
    let used = |names: &[&'static str]| -> Vec<&'static str> {
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
            .collect()
    };
    Ok(Items {
        layerstack: used(&["CannotApply", "PathId"]),
        schemas: used(&[
            "CannotDefine",
            "InstanceEdit",
            "InstanceView",
            "PrimEdit",
            "PrimView",
            "Scene",
            "SchemaEdit",
        ]),
        body,
    })
}

/// A property's getters and setters, and its enum, if any.
struct Accessors {
    getters: String,
    setters: String,
    enums: String,
}

/// A getter reading the property `usd` with `read`, as `name`, and for a
/// varying attribute its `_at` getter.
fn getter(docs: &str, name: &str, usd: &str, ty: &RustType, varying: bool) -> String {
    let mut out = format!(
        "{docs}    #[must_use]\n    pub fn {name}(&self) -> Option<{read}> {{\n        \
         self.read_value({usd:?}, {read_fn})\n    }}\n\n",
        read = ty.read,
        read_fn = ty.read_fn
    );
    if varying {
        let _ = write!(
            out,
            "    #[doc = \"[`Self::{name}`] at `time`, interpolating time samples as it says.\"]\n    \
             #[must_use]\n    \
             pub fn {name}_at(&self, time: ::layerstack::Time) -> Option<{read}> {{\n        \
             self.read_value_at({usd:?}, time, {read_fn})\n    }}\n\n",
            read = ty.read,
            read_fn = ty.read_fn
        );
    }
    out
}

/// A setter `set` authoring the property `usd` from a `ty` written with
/// `write` (`VALUE`, `TOKENS`), and for a varying attribute its `_at`
/// setter.
fn setter(docs: &str, set: &str, usd: &str, ty: &str, write: &str, varying: bool) -> String {
    let value = write
        .replace("VALUE", "value")
        .replace("TOKENS", "edit.tokens()");
    let mut out = format!(
        "{docs}    pub fn {set}(&self, edit: &mut SchemaEdit<'_>, value: {ty}) -> &Self {{\n        \
         let value = {value};\n        self.write_value(edit, {usd:?}, value);\n        self\n    }}\n\n"
    );
    if varying {
        let _ = write!(
            out,
            "    #[doc = \"[`Self::{set}`] as a time sample at the time code `time`.\"]\n    \
             pub fn {set}_at(&self, edit: &mut SchemaEdit<'_>, time: f64, value: {ty}) -> &Self {{\n        \
             let value = {value};\n        self.write_value_at(edit, {usd:?}, time, value);\n        self\n    }}\n\n"
        );
    }
    out
}

fn accessors(
    cx: &Context<'_, '_>,
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
        let _ = write!(facts, "; fallback `{}`", usd_text(fallback, cx.tokens));
    }
    facts.push_str(").");
    brief.push(doc_text(&facts));
    if property
        .value_type
        .as_ref()
        .is_some_and(|(_, array, zero)| *array && shared_array(zero))
    {
        brief.push("Numeric array getters retain shared storage without copying elements. Legacy arrays, sparse composition and interpolation may materialize storage. Use `as_slice()` to borrow or `as_ref().clone()` for an explicit mutable copy. Slice setters copy; the `_owned` and `_shared` setters transfer storage.".into());
    }
    let stem = property.api_name.as_deref().unwrap_or(&property.name);
    let stem = stem
        .split_once(&format!(
            "{}:",
            layerstack::schema::INSTANCE_NAME_PLACEHOLDER
        ))
        .map_or(stem, |(_, tail)| tail);
    let suffix = if property.kind == PropertyKind::Relationship {
        "Rel"
    } else {
        "Attr"
    };
    let get = format!("Get{}{suffix}", pascal(&snake(stem)));
    let create = format!("Create{}{suffix}", pascal(&snake(stem)));
    let mut docs = doc_attrs("    ", &brief);
    let mut setter_docs = docs.clone();
    let alias = cpp_method_alias(schema, &get, "    ");
    if !alias.is_empty() {
        let class = &schema.cpp.as_ref().expect("verified native method").0;
        docs.push_str(&doc_attrs("    ", &[format!("C++ read: `{class}::{get}().{}`; this getter returns the resolved value rather than an attribute/relationship handle.", if property.kind == PropertyKind::Relationship { "GetTargets()" } else { "Get()" })]));
        docs.push_str(&alias);
    }
    let alias = cpp_method_alias(schema, &create, "    ");
    // How C++ makes a relationship call: through the schema's `Create*Rel`
    // when it declares one, else on the relationship itself.
    let rel_call = |call: &str| match &schema.cpp {
        Some((class, _)) if !alias.is_empty() => {
            format!(
                "C++ authoring: `{class}::{create}().{call}()`; changes are queued in `SchemaEdit` until its transaction is applied."
            )
        }
        _ => format!(
            "C++ authoring: `UsdRelationship::{call}()`; changes are queued in `SchemaEdit` until its transaction is applied."
        ),
    };
    if !alias.is_empty() {
        let class = &schema.cpp.as_ref().expect("verified native method").0;
        setter_docs.push_str(&doc_attrs("    ", &[format!("C++ authoring: `{class}::{create}().{}`; changes are queued in `SchemaEdit` until its transaction is applied.", if property.kind == PropertyKind::Relationship { "SetTargets()" } else { "Set()" })]));
        setter_docs.push_str(&alias);
    }
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
    let usd = &property.name;
    let mut enums = String::new();
    if property.kind == PropertyKind::Relationship {
        table.getter(&name, property, Getter::Targets);
        return Ok(Accessors {
            getters: format!(
                "{constant}{docs}    #[must_use]\n    \
                 pub fn {name}(&self) -> {}<::layerstack::TargetPath> {{\n        \
                 self.read_targets({usd:?})\n    }}\n\n",
                cx.target.vec()
            ),
            setters: format!(
                "{setter_docs}    pub fn set_{name}(\n        &self,\n        \
                 edit: &mut SchemaEdit<'_>,\n        targets: &[::layerstack::TargetPath],\n    \
                 ) -> &Self {{\n        self.write_targets(edit, {usd:?}, targets);\n        self\n    }}\n\n\
                 {add_docs}    pub fn add_to_{name}(\n        &self,\n        \
                 edit: &mut SchemaEdit<'_>,\n        target: ::layerstack::TargetPath,\n        \
                 position: ::layerstack::ListPosition,\n    \
                 ) -> &Self {{\n        self.add_target(edit, {usd:?}, target, position);\n        self\n    }}\n\n\
                 {remove_docs}    pub fn remove_from_{name}(\n        &self,\n        \
                 edit: &mut SchemaEdit<'_>,\n        target: ::layerstack::TargetPath,\n    \
                 ) -> &Self {{\n        self.remove_target(edit, {usd:?}, target);\n        self\n    }}\n\n",
                add_docs = doc_attrs(
                    "    ",
                    &[
                        format!(
                            "Adds `target` to [`Self::set_{name}`]'s targets at `position`, editing the edit target's list op instead of replacing it: weaker layers' targets stay, and a target already in the chosen list moves. An explicit list takes it whatever the position."
                        ),
                        rel_call("AddTarget"),
                    ],
                ),
                remove_docs = doc_attrs(
                    "    ",
                    &[
                        format!(
                            "Removes `target` from [`Self::set_{name}`]'s targets: it is deleted in the edit target's list op, which removes it from weaker layers' targets too, or taken out of an explicit list."
                        ),
                        rel_call("RemoveTarget"),
                    ],
                ),
            ),
            enums,
        });
    }
    let Some((_, is_array, zero)) = &property.value_type else {
        return Err(format!("{}.{}: no value type", schema.name, property.name));
    };
    let Some(mut ty) = rust_type_in(zero, *is_array, cx.target)
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
        enums = token_enum(cx, &enum_name, property)?;
        ty = RustType {
            read: enum_name.clone(),
            write: enum_name.clone(),
            read_fn: format!("{enum_name}::read"),
            write_fn: format!("{enum_name}::write"),
            write_call: "VALUE.write(TOKENS)".into(),
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
    let mut getters = constant;
    getters.push_str(&getter(&docs, &name, usd, &ty, varying));
    if *is_array {
        let _ = writeln!(
            getters,
            "    /// Checked array read at `time`; deferred decode errors remain distinct from missing values.\n    pub fn try_{name}(&self, time: {}::Time) -> Result<Option<{}>, ::layerstack::ArrayReadError> {{ self.try_read_value({usd:?}, time, {}) }}\n",
            cx.target.schemas(),
            ty.read,
            ty.read_fn
        );
    }
    let mut setters = setter(
        &setter_docs,
        &format!("set_{name}"),
        usd,
        &ty.write,
        &ty.write("VALUE", "TOKENS"),
        varying,
    );
    if *is_array && shared_array(zero) {
        let element = rust_type_in(zero, false, cx.target)?
            .expect("an array element")
            .read;
        let (arc, vec) = (cx.target.arc(), cx.target.vec());
        for (suffix, input) in [
            ("owned", format!("{vec}<{element}>")),
            ("shared", format!("{arc}<{vec}<{element}>>")),
        ] {
            let note = if suffix == "owned" {
                "Transfers the vector allocation and capacity; allocates only its shared owner."
            } else {
                "Transfers a shared owner without allocating or copying elements."
            };
            let docs = doc_attrs(
                "    ",
                &[format!("{note} Slice callers can use `set_{name}`.")],
            );
            setters.push_str(&setter(
                &docs,
                &format!("set_{name}_{suffix}"),
                usd,
                &input,
                &format!("{}_{suffix}(VALUE, TOKENS)", ty.write_fn),
                varying,
            ));
        }
    }
    Ok(Accessors {
        getters,
        setters,
        enums,
    })
}

/// The enum variant naming the allowed token `token`: its words in
/// upper camel case (`catmullClark` → `CatmullClark`). A token that does
/// not split into such words is named as usdGenSchema names its token
/// identifiers (`_MakeValidToken`: a leading digit gets a `_`, the rest is
/// camel-cased with non-word characters dropped, `TfMakeValidIdentifier`
/// last), in upper camel case: `choice/mandatory` → `ChoiceMandatory`,
/// `3d` → `_3d`.
fn token_variant(token: &str) -> String {
    let direct = pascal(&snake(token));
    if !direct.is_empty()
        && direct.starts_with(|c: char| c.is_ascii_alphabetic())
        && direct.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return direct;
    }
    use crate::compile::names;
    let digit = token.starts_with(|c: char| c.is_ascii_digit());
    let mut id = names::camel_case(&if digit {
        format!("_{token}")
    } else {
        token.to_string()
    });
    if !names::is_valid_identifier(&id) {
        id = names::make_valid_identifier(&id);
    }
    let words = pascal(&snake(&id));
    if words.starts_with(|c: char| c.is_ascii_alphabetic()) {
        words
    } else {
        format!("_{words}")
    }
}

/// The enum of a property's `allowedTokens`, with `Other` for any token
/// the schema does not list.
fn token_enum(cx: &Context<'_, '_>, name: &str, property: &Property) -> Result<String, String> {
    let mut variants = BTreeSet::new();
    let mut declared = String::new();
    let mut from = String::new();
    let mut to = String::new();
    for token in &property.allowed_tokens {
        let variant = token_variant(token);
        if variant == "Other" {
            return Err(format!(
                "{}: the allowed token {token:?} makes the variant `Other`, which stands \
                 for unlisted tokens",
                property.name
            ));
        }
        if !variants.insert(variant.clone()) {
            return Err(format!(
                "{}: two allowed tokens make the variant `{variant}`; usdGenSchema \
                 requires token identifiers to map to exactly one token value",
                property.name
            ));
        }
        // A token starting with a digit keeps usdGenSchema's leading `_`.
        let allow = if variant.starts_with('_') {
            "    #[allow(non_camel_case_types, reason = \"usdGenSchema's identifier\")]\n"
        } else {
            ""
        };
        let _ = writeln!(
            declared,
            "    #[doc = {:?}]\n{allow}    {variant},",
            format!("`{token}`")
        );
        let _ = writeln!(from, "            {token:?} => Self::{variant},");
        let _ = writeln!(to, "            Self::{variant} => {token:?},");
    }
    let tokens: Vec<String> = property
        .allowed_tokens
        .iter()
        .map(|token| format!("{token:?}"))
        .collect();
    let (schemas, arc) = (cx.target.schemas(), cx.target.arc());
    Ok(format!(
        "#[doc = {doc:?}]\n#[derive(Clone, Debug, PartialEq, Eq, Hash)]\npub enum {name} {{\n{declared}    \
         /// A token the schema does not list: OpenUSD does not enforce\n    \
         /// `allowedTokens`, so an authored value may be anything. The\n    \
         /// `AllowedTokens` validation rule reports it.\n    Other({arc}<str>),\n}}\n\n\
         impl {name} {{\n    /// The tokens the schema allows, in its order.\n    \
         pub const TOKENS: &'static [&'static str] = &[{tokens}];\n\n    \
         /// The value `token` names, `Other` when the schema does not\n    /// list it.\n    \
         #[must_use]\n    pub fn from_token(token: &str) -> Self {{\n        match token {{\n{from}            \
         other => Self::Other({arc}::from(other)),\n        }}\n    }}\n\n    \
         /// The token this value is.\n    #[must_use]\n    pub fn as_str(&self) -> &str {{\n        \
         match self {{\n{to}            Self::Other(token) => token,\n        }}\n    }}\n\n    \
         pub(crate) fn read(\n        value: &::layerstack::Value,\n        \
         tokens: &::layerstack::TokenInterner,\n    ) -> Option<Self> {{\n        \
         {schemas}::value::read_token(value, tokens).map(Self::from_token)\n    }}\n\n    \
         pub(crate) fn write(self, tokens: &mut ::layerstack::TokenInterner) -> ::layerstack::Value {{\n        \
         {schemas}::value::write_token(self.as_str(), tokens)\n    }}\n}}\n\n",
        doc = format!(
            "The tokens `{}` allows.",
            property
                .name
                .replace(layerstack::schema::INSTANCE_NAME_PLACEHOLDER, "<instance>")
        ),
        tokens = tokens.join(", "),
    ))
}

/// The view, edit handle and enums of a typed schema.
fn typed_view(
    cx: &Context<'_, '_>,
    schema: &Schema,
    table: &mut TestTable,
) -> Result<String, String> {
    let index = cx.index;
    let view = type_name(&schema.name);
    let edit = format!("{view}Edit");
    let concrete = schema.kind == SchemaKind::ConcreteTyped;
    table.begin(schema, &view);
    let (base, base_edit, wrap_base, edit_base) = match &schema.parent {
        Some(parent) => {
            let path = type_path(cx, parent);
            (
                format!("{path}<'a>"),
                format!("{path}Edit"),
                format!("{path}::from_prim_unchecked(prim)"),
                format!("{path}Edit::from_prim(prim)"),
            )
        }
        None => (
            "PrimView<'a>".to_string(),
            "PrimEdit".to_string(),
            "prim".to_string(),
            "prim".to_string(),
        ),
    };
    let mut getters = String::new();
    let mut setters = String::new();
    let mut enums = String::new();
    for property in local_properties(schema, index) {
        let accessors = accessors(cx, schema, property, &view, table)?;
        getters.push_str(&accessors.getters);
        setters.push_str(&accessors.setters);
        enums.push_str(&accessors.enums);
    }
    for built_in in local_built_ins(schema, index) {
        let api = type_path(cx, built_in);
        let method = snake(&type_name(built_in));
        table.built_in(&method);
        let _ = write!(
            getters,
            "    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> {api}<'a> {{\n        \
             {api}::from_prim_unchecked(PrimView::new(self.scene(), self.path()))\n    }}\n\n",
            format!(
                "The prim's built-in `{built_in}`, which every `{}` has.",
                schema.name
            )
        );
        let _ = write!(
            setters,
            "    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> {api}Edit {{\n        \
             {api}Edit::from_prim(PrimEdit::from_path_unchecked(self.path()))\n    }}\n\n",
            format!("An edit handle for the prim's built-in `{built_in}`.")
        );
    }
    let kind_text = if concrete { "concrete" } else { "abstract" };
    let inherits = schema
        .parent
        .as_deref()
        .map(|parent| format!(", inheriting [`{}`]", type_path(cx, parent)))
        .unwrap_or_default();
    let docs = doc_attrs(
        "",
        &[
            doc_text(&schema.doc),
            format!(
                "The view of {owner} {kind_text} typed schema `{}`{inherits}. Construct it \
                 with [`{view}::new`], which checks the prim is of the schema (`IsA`); it \
                 derefs to the view of the schema it inherits from. For anything it does not \
                 offer, read the property by its USD name through the generic handle \
                 ([`PrimView::prim`]; `Attribute::get` returns the raw resolved value).",
                schema.name,
                owner = cx.owner,
            ),
        ],
    );
    let mut define = if concrete {
        format!(
            "    /// Defines the prim at `path` as a `{name}`, and its undefined\n    \
             /// ancestors, through the edit's target, as OpenUSD's\n    \
             /// `UsdStage::DefinePrim` does ([`SchemaEdit::define_prim`]): a prim\n    \
             /// already defined as a `{name}` is left as it is.\n    \
             ///\n    /// # Errors\n    ///\n    \
             /// [`CannotDefine`] for a path beneath an instance or in a\n    \
             /// prototype, which OpenUSD refuses; nothing is authored.\n    \
             pub fn define(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<{edit}, CannotDefine> {{\n        \
             edit.define_prim(path, Self::SCHEMA).map({edit}::from_prim)\n    }}\n\n",
            name = schema.name
        )
    } else {
        String::new()
    };
    define = define.replace(
        "    pub fn define(",
        &format!(
            "{}    pub fn define(",
            cpp_method_alias(schema, "Define", "    ")
        ),
    );
    let allow = item_allow(cx.target);
    Ok(format!(
        "{enums}{docs}{aliases}{allow}#[derive(Clone, Copy, Debug)]\npub struct {view}<'a> {{\n    base: {base},\n}}\n\n\
         impl<'a> Deref for {view}<'a> {{\n    type Target = {base};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         {allow}impl<'a> {view}<'a> {{\n    /// The schema's name.\n    pub const SCHEMA: &'static str = {name:?};\n\n    \
         #[doc = {new_doc:?}]\n{get_alias}    #[must_use]\n    \
         pub fn new(scene: &Scene<'a>, path: impl ::layerstack::PrimPathArg) -> Option<Self> {{\n        \
         let path = scene.prim_path(path)?;\n        \
         scene\n            .is_a(path, Self::SCHEMA)\n            .then(|| Self::from_prim_unchecked(PrimView::new(*scene, path)))\n    }}\n\n    \
         /// A view of `prim` as of the schema, without checking that it is\n    \
         /// (`IsA`), as [`Self::new`] does: a getter reads what the prim\n    \
         /// authors, and the schema's fallbacks only for a prim of the schema.\n    \
         /// For the view of a schema built on this one.\n    #[must_use]\n    \
         pub fn from_prim_unchecked(prim: PrimView<'a>) -> Self {{\n        Self {{\n            base: {wrap_base},\n        }}\n    }}\n\n    \
         /// An edit handle for this prim.\n    #[must_use]\n    \
         pub fn edit(&self) -> {edit} {{\n        {edit}::from_prim(PrimEdit::from_path_unchecked(self.path()))\n    }}\n\n\
         {define}{getters}}}\n\n\
         #[doc = {edit_doc:?}]\n#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub struct {edit} {{\n    base: {base_edit},\n}}\n\n\
         impl Deref for {edit} {{\n    type Target = {base_edit};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         {allow}impl {edit} {{\n    #[doc = {new_edit_doc:?}]\n    #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: impl ::layerstack::PrimPathArg) -> Option<Self> {{\n        \
         let path = edit.prim_path(path)?;\n        \
         edit.is_a(path, {view}::SCHEMA)\n            \
         .then(|| Self::from_prim(PrimEdit::from_path_unchecked(path)))\n    }}\n\n    \
         /// A handle authoring the schema's properties on the prim `prim`\n    \
         /// authors, whatever its type.\n    #[must_use]\n    \
         pub fn from_prim(prim: PrimEdit) -> Self {{\n        Self {{\n            base: {edit_base},\n        }}\n    }}\n\n\
         {setters}}}\n\n",
        aliases = schema_aliases(schema, &view),
        get_alias = cpp_method_alias(schema, "Get", "    "),
        name = schema.name,
        new_doc = format!(
            "A view of the prim at `path` (text, `\"/World/Prim\"`, or an id), if it is a `{}` or \
             of a schema derived from it.",
            schema.name
        ),
        new_edit_doc = format!(
            "A handle authoring the prim at `path` through `edit`, if it is a `{name}` or of a \
             schema derived from it (`IsA`) as the edit leaves it: defined so earlier in \
             `edit`, or so on the edit's stage ([`SchemaEdit::is_a`]).\n\n\
             `None` for a prim that is not there, or not typed so yet, such as an untyped \
             `def` or an `over`: {define_ref} types it, and \
             `{edit}::from_prim(PrimEdit::new(edit, path)?)` authors the schema's properties \
             on any prim that exists. Setters never create a prim.",
            name = schema.name,
            define_ref = if concrete {
                format!("[`{view}::define`]")
            } else {
                "[`SchemaEdit::define_prim`] with a concrete schema derived from it".to_owned()
            },
        ),
        edit_doc = format!(
            "Authors the properties of {} `{}` through a [`SchemaEdit`]; it derefs to \
             the handle of the schema it inherits from.",
            cx.owner, schema.name
        ),
    ))
}

/// The view, edit handle and enums of an applied schema.
fn applied_view(
    cx: &Context<'_, '_>,
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
        let accessors = accessors(cx, schema, property, &view, table)?;
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
                "The view of {owner} {kind_text} API schema `{}`. Get it with [`{view}::get`], \
                 which checks the prim has the schema applied (`HasAPI`). For anything it does \
                 not offer, read the property by its USD name through the generic handle \
                 ([`PrimView::prim`]; `Attribute::get` returns the raw resolved value).",
                schema.name,
                owner = cx.owner,
            ),
        ],
    );
    let vec = cx.target.vec();
    let mut constructors = if multiple {
        format!(
            "    /// A view of the instance `instance` of the schema on the prim at `path`,\n    \
             /// if the prim has it applied.\n    #[must_use]\n    \
             pub fn get(scene: &Scene<'a>, path: impl ::layerstack::PrimPathArg, instance: &str) -> Option<Self> {{\n        \
             let path = scene.prim_path(path)?;\n        \
             if !scene.has_api(path, Self::SCHEMA, Some(instance)) {{\n            return None;\n        }}\n        \
             let instance = scene\n            .instances(path, Self::SCHEMA)\n            .into_iter()\n            \
             .find(|applied| *applied == instance)?;\n        \
             Some(Self::from_instance_unchecked(InstanceView::new(\n            PrimView::new(*scene, path),\n            instance,\n        )))\n    }}\n\n    \
             /// Every instance of the schema the prim at `path` has applied, in the\n    \
             /// order its definition applies them.\n    #[must_use]\n    \
             pub fn instances(scene: &Scene<'a>, path: impl ::layerstack::PrimPathArg) -> {vec}<Self> {{\n        \
             let Some(path) = scene.prim_path(path) else {{\n            return {vec}::new();\n        }};\n        \
             scene\n            .instances(path, Self::SCHEMA)\n            .into_iter()\n            \
             .map(|instance| {{\n                Self::from_instance_unchecked(InstanceView::new(PrimView::new(*scene, path), instance))\n            }})\n            .collect()\n    }}\n\n    \
             /// A view of `instance` as an instance of the schema, without\n    \
             /// checking that the prim has it applied (`HasAPI`), as [`Self::get`]\n    \
             /// does: a getter reads what the prim authors, and the schema's\n    \
             /// fallbacks only for an applied instance.\n    #[must_use]\n    \
             pub fn from_instance_unchecked(instance: InstanceView<'a>) -> Self {{\n        \
             Self {{ base: instance }}\n    }}\n\n    \
             /// An edit handle for this instance.\n    #[must_use]\n    \
             pub fn edit(&self) -> {edit} {{\n        {edit}::from_instance(InstanceEdit::new(\n            \
             PrimEdit::from_path_unchecked(self.path()),\n            self.instance(),\n        ))\n    }}\n\n    \
             /// Applies the instance `instance` of the schema to `path` (a `prepend\n    \
             /// apiSchemas` entry in the edit's target) if the prim may have it.\n    \
             ///\n    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`.\n    \
             ///\n    /// # Errors\n    ///\n    /// Why the schema cannot be applied there.\n    \
             pub fn apply(\n        edit: &mut SchemaEdit<'_>,\n        path: PathId,\n        instance: &str,\n    ) -> Result<{edit}, CannotApply> {{\n        \
             let prim = edit.apply_api(path, Self::SCHEMA, Some(instance))?;\n        \
             Ok({edit}::from_instance(InstanceEdit::new(prim, instance)))\n    }}\n\n"
        )
    } else {
        format!(
            "    /// A view of the schema on the prim at `path`, if the prim has it\n    \
             /// applied.\n    #[must_use]\n    \
             pub fn get(scene: &Scene<'a>, path: impl ::layerstack::PrimPathArg) -> Option<Self> {{\n        \
             let path = scene.prim_path(path)?;\n        \
             scene\n            .has_api(path, Self::SCHEMA, None)\n            .then(|| Self::from_prim_unchecked(PrimView::new(*scene, path)))\n    }}\n\n    \
             /// A view of the schema on `prim`, without checking that the prim\n    \
             /// has it applied (`HasAPI`), as [`Self::get`] does: a getter reads\n    \
             /// what the prim authors, and the schema's fallbacks only for a\n    \
             /// prim with the schema applied. For a built-in's view.\n    #[must_use]\n    \
             pub fn from_prim_unchecked(prim: PrimView<'a>) -> Self {{\n        Self {{ base: prim }}\n    }}\n\n    \
             /// An edit handle for this prim.\n    #[must_use]\n    \
             pub fn edit(&self) -> {edit} {{\n        {edit}::from_prim(PrimEdit::from_path_unchecked(self.path()))\n    }}\n\n    \
             /// Applies the schema to `path` (a `prepend apiSchemas` entry in the\n    \
             /// edit's target) if the prim may have it.\n    \
             ///\n    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`.\n    \
             ///\n    /// # Errors\n    ///\n    /// Why the schema cannot be applied there.\n    \
             pub fn apply(edit: &mut SchemaEdit<'_>, path: PathId) -> Result<{edit}, CannotApply> {{\n        \
             edit.apply_api(path, Self::SCHEMA, None).map({edit}::from_prim)\n    }}\n\n"
        )
    };
    for (rust, native) in [("get", "Get"), ("instances", "GetAll"), ("apply", "Apply")] {
        constructors = constructors.replace(
            &format!("    pub fn {rust}("),
            &format!(
                "{}    pub fn {rust}(",
                cpp_method_alias(schema, native, "    ")
            ),
        );
    }
    let new_edit = if multiple {
        "    /// A handle authoring the instance `instance` on the prim at `path`\n    \
         /// through `edit`, if the prim exists: on the edit's stage, or defined\n    \
         /// earlier in `edit`. Setters never create a prim that does not.\n    \
         #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: impl ::layerstack::PrimPathArg, instance: &str) -> Option<Self> {\n        \
         PrimEdit::new(edit, path).map(|prim| Self::from_instance(InstanceEdit::new(prim, instance)))\n    }\n\n    \
         /// A handle authoring the schema's properties for `instance`, whether\n    \
         /// or not the prim has it applied.\n    #[must_use]\n    \
         pub fn from_instance(instance: InstanceEdit) -> Self {\n        \
         Self { base: instance }\n    }\n\n"
            .to_string()
    } else {
        "    /// A handle authoring the prim at `path` through `edit`, if the prim\n    /// exists: on the edit's stage, or defined earlier in `edit`. Setters\n    /// never create a prim that does not.\n    #[must_use]\n    \
         pub fn new(edit: &SchemaEdit<'_>, path: impl ::layerstack::PrimPathArg) -> Option<Self> {\n        \
         PrimEdit::new(edit, path).map(Self::from_prim)\n    }\n\n    \
         /// A handle authoring the schema's properties on the prim `prim`\n    \
         /// authors, whether or not it has the schema applied.\n    #[must_use]\n    \
         pub fn from_prim(prim: PrimEdit) -> Self {\n        Self { base: prim }\n    }\n\n"
            .to_string()
    };
    let edit_derive = if multiple {
        "#[derive(Clone, Debug, PartialEq, Eq, Hash)]"
    } else {
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]"
    };
    let allow = item_allow(cx.target);
    Ok(format!(
        "{enums}{docs}{aliases}{allow}#[derive(Clone, Copy, Debug)]\npub struct {view}<'a> {{\n    base: {base},\n}}\n\n\
         impl<'a> Deref for {view}<'a> {{\n    type Target = {base};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         {allow}impl<'a> {view}<'a> {{\n    /// The schema's name.\n    pub const SCHEMA: &'static str = {name:?};\n\n\
         {constructors}{getters}}}\n\n\
         #[doc = {edit_doc:?}]\n{edit_derive}\npub struct {edit} {{\n    base: {base_edit},\n}}\n\n\
         impl Deref for {edit} {{\n    type Target = {base_edit};\n\n    \
         fn deref(&self) -> &Self::Target {{\n        &self.base\n    }}\n}}\n\n\
         {allow}impl {edit} {{\n{new_edit}{setters}}}\n\n",
        aliases = schema_aliases(schema, &view),
        name = schema.name,
        edit_doc = format!(
            "Authors the properties of {} `{}` through a [`SchemaEdit`].",
            cx.owner, schema.name
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
pub(crate) struct TestTable {
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
                            "    {}::define(edit, h.path({prim:?})).expect(\"outside any instance\");",
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
                        "    {view_path}::define(edit, h.path({prim:?})).expect(\"outside any instance\");"
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
                                "        r.value(prim, {usd:?}, When::Sample, v.{method}_at(Time::held({SAMPLE_TIME})));"
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
                                "        r.value(prim, {usd:?}, When::Sample, v.{method}_at(Time::held({SAMPLE_TIME})).as_ref().map({path}::as_str));"
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
             use layerstack::Time;\nuse layerstack_schemas::*;\n\n\
             use super::{Harness, Readings, When};\n\n\
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Identifier tokens keep the names the built-in views have; others are
    /// named as usdGenSchema names their token identifiers.
    #[test]
    fn token_variants_follow_usd_gen_schema() {
        let names = |tokens: &[&str]| tokens.iter().map(|t| token_variant(t)).collect::<Vec<_>>();
        assert_eq!(
            names(&["catmullClark", "none", "half-to-even", "fromTexture"]),
            ["CatmullClark", "None", "HalfToEven", "FromTexture"]
        );
        assert_eq!(
            names(&["choice/mandatory", "excludes-rows", "3d", "x-ray"]),
            ["ChoiceMandatory", "ExcludesRows", "_3d", "XRay"]
        );
    }

    /// `doc_text` escapes Markdown outside code spans only: inside them
    /// neither entities nor backslash escapes are decoded.
    #[test]
    fn code_spans_are_written_literally() {
        assert_eq!(
            doc_text("USD attribute `price:<instance>:base` [*] <b>"),
            "USD attribute `price:<instance>:base` \\[\\*\\] &lt;b&gt;"
        );
    }
}
