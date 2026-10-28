// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plugin metadata declarations: the `SdfMetadata` of each plugin's `Info`
//! in a `plugInfo.json`, read as OpenUSD's `SdfSchema` registers them, and
//! the typed accessors generated for them.
//!
//! ```json
//! { "Plugins": [{ "Name": "lights", "Info": { "SdfMetadata": {
//!     "lumenScale": { "type": "float", "appliesTo": ["prims", "attributes"],
//!                     "default": 1, "displayGroup": "Lighting" }
//! }}}]}
//! ```
//!
//! [`read`] takes the declarations of a set of `plugInfo.json` files apart
//! into the fields OpenUSD registers ([`MetadataDeclaration`]) and the
//! declarations it refuses ([`MetadataDeclarationError`], naming the
//! plugin, the field, the offending key and value and the
//! [`MetadataRejection`]). OpenUSD's rules, from
//! `SdfSchemaBase::_UpdateMetadataFromPlugins` and `_GetDefaultMetadataValue`
//! (`pxr/usd/sdf/schema.cpp`):
//!
//! - a field is an object with a string `type`: a value type or its array
//!   form, `dictionary`, or a list-op type (`tokenlistop`, …);
//! - a field Sdf or one of usd-core's own plugins registers, or an earlier
//!   plugin of the set declares, is refused (`"…" is already a registered
//!   field`);
//! - `default` is a string, an integer, a real, or an array of one of
//!   those (a JSON `true` is none of them), read as USDA's text of a value
//!   of the type with the values in order (a `float3[]` default is a flat
//!   list of floats): `irIsInvertible`'s `1` is `true`, `1.5` an `int`'s
//!   `1`, a single value an array's one element. An integer is taken as
//!   OpenUSD's plugin reader takes it, as a 32-bit `int`. A dictionary or
//!   list-op field takes no default. Without one the field falls back to
//!   its type's ([`layerstack::MetadataType::fallback`]);
//! - `appliesTo` is a kind of spec or a list of them (`layers`, `prims`,
//!   `attributes`, `relationships`, `properties`, `variants`); without one,
//!   or with one that is neither, the field is metadata on every kind, and
//!   names that are no kind are ignored, so `["nodes"]` registers the
//!   field for none. Prims imply variants.
//!
//! A schema library's Rust module
//! ([`CompiledSchema::rust_module`](crate::compile::CompiledSchema::rust_module))
//! registers the accepted fields and refuses to generate while any
//! declaration is refused.
//!
//! Spec: AOUSD Core §7.4 (metadata).

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use crate::kinds::PlugInfoError;
use crate::model::Model;
use layerstack::{
    MetadataDefinition, MetadataField, MetadataTarget, MetadataType, TokenInterner, Value,
};
use layerstack_usda::metadata::MetadataValueError;
use serde_json::Value as Json;

/// A metadata field a plugin declares and OpenUSD registers.
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataDeclaration {
    /// The declaring plugin ([`MetadataDeclarationError::plugin`]).
    pub plugin: String,
    /// The field, its names interned in the interner [`read`] is given.
    pub definition: MetadataDefinition,
}

/// A metadata declaration OpenUSD refuses, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataDeclarationError {
    /// The plugin: its `Name`, or `Plugins[<index>]` without one, after
    /// the file's label when [`read`] is given several.
    pub plugin: String,
    /// The declared field, or `None` when the plugin's `SdfMetadata` as a
    /// whole is malformed.
    pub field: Option<String>,
    /// The offending key of the declaration (`type`, `default`), if one.
    pub key: Option<&'static str>,
    /// The offending value, as JSON text, if one.
    pub value: Option<String>,
    /// Why it is refused.
    pub reason: MetadataRejection,
}

/// Why OpenUSD refuses a metadata declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataRejection {
    /// The plugin's `SdfMetadata` is not an object; OpenUSD ignores it.
    MetadataNotAnObject,
    /// The declaration is not an object (OpenUSD: "Value must be a
    /// dictionary").
    NotAnObject,
    /// `type` is missing or not a string (OpenUSD: "Could not read a
    /// string for \"type\"").
    TypeNotAString,
    /// `type` names no metadata type (OpenUSD: "\"…\" is not a registered
    /// value type").
    UnknownType,
    /// The field is registered already (OpenUSD: "\"…\" is already a
    /// registered field"), by `by`: Sdf, one of usd-core's own plugins, or
    /// an earlier plugin of the set.
    AlreadyRegistered {
        /// Who registers it.
        by: String,
    },
    /// A `default` for a dictionary or list-op field (OpenUSD: "Default
    /// values are not allowed on fields of type …").
    DefaultNotAllowed,
    /// A `default` that is not a string, an integer, a real or an array of
    /// one of those (OpenUSD: "Value was not a string, an int, a double,
    /// or a vector of those types").
    DefaultNotAValue,
    /// A `default` that is no value of the type (OpenUSD: "Could not parse
    /// default value").
    DefaultNotOfType(MetadataValueError),
}

impl MetadataDeclarationError {
    /// OpenUSD's message for the refusal, as `SdfSchemaBase` raises it,
    /// without the plugin's path; `None` for the refusal OpenUSD ignores
    /// silently.
    #[must_use]
    pub fn openusd_message(&self) -> Option<String> {
        let field = self.field.as_deref().unwrap_or_default();
        Some(match &self.reason {
            MetadataRejection::MetadataNotAnObject => return None,
            MetadataRejection::NotAnObject => format!("Value must be a dictionary (at \"{field}\""),
            MetadataRejection::TypeNotAString => {
                format!("Could not read a string for \"type\" (at \"{field}\"")
            }
            MetadataRejection::AlreadyRegistered { .. } => {
                format!("\"{field}\" is already a registered field")
            }
            MetadataRejection::UnknownType
            | MetadataRejection::DefaultNotAllowed
            | MetadataRejection::DefaultNotAValue
            | MetadataRejection::DefaultNotOfType(_) => {
                format!("Error parsing default value for metadata (at \"{field}\"")
            }
        })
    }
}

impl fmt::Display for MetadataDeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plugin `{}`", self.plugin)?;
        if let Some(field) = &self.field {
            write!(f, ", field `{field}`")?;
        }
        if let Some(key) = self.key {
            write!(f, ", `{key}`")?;
        }
        if let Some(value) = &self.value {
            write!(f, " {value}")?;
        }
        f.write_str(": ")?;
        match &self.reason {
            MetadataRejection::MetadataNotAnObject => f.write_str("`SdfMetadata` is not an object"),
            MetadataRejection::NotAnObject => f.write_str("the declaration is not an object"),
            MetadataRejection::TypeNotAString => f.write_str("no string `type`"),
            MetadataRejection::UnknownType => f.write_str("not a registered value type"),
            MetadataRejection::AlreadyRegistered { by } => {
                write!(f, "registered already, by {by}")
            }
            MetadataRejection::DefaultNotAllowed => {
                f.write_str("a dictionary or list-op field takes no default")
            }
            MetadataRejection::DefaultNotAValue => f.write_str(
                "a default is a string, an integer, a real, or an array of one of those",
            ),
            MetadataRejection::DefaultNotOfType(error) => {
                write!(f, "not a value of the type ({error:?})")
            }
        }
    }
}

impl std::error::Error for MetadataDeclarationError {}

/// The metadata declarations of a set of `plugInfo.json` files, as OpenUSD
/// takes them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginMetadata {
    /// The fields OpenUSD registers, in file and plugin order, each
    /// plugin's in name order (OpenUSD reads `SdfMetadata` as an ordered
    /// map).
    pub accepted: Vec<MetadataDeclaration>,
    /// The declarations it refuses, in the same order.
    pub rejected: Vec<MetadataDeclarationError>,
}

/// Reads the `SdfMetadata` declarations of `plug_infos`, `(label, text)`
/// pairs of `plugInfo.json` files (whole-line `#` comments allowed), as
/// OpenUSD registers them, interning names and tokens in `tokens`. A
/// single file's plugins are labelled by name alone.
///
/// # Errors
///
/// [`PlugInfoError`] for a file that is not JSON or has no `Plugins` list.
pub fn read<'a>(
    plug_infos: impl IntoIterator<Item = (&'a str, &'a str)>,
    tokens: &TokenInterner,
) -> Result<PluginMetadata, PlugInfoError> {
    let files: Vec<(&str, &str)> = plug_infos.into_iter().collect();
    let mut out = PluginMetadata::default();
    let mut declared = BTreeMap::new();
    for &(file, text) in &files {
        let error = |message: String| PlugInfoError {
            file: file.into(),
            message,
        };
        let json = crate::model::plug_info_text(text).map_err(error)?;
        let plugins = json["Plugins"]
            .as_array()
            .ok_or_else(|| error("no `Plugins` list".into()))?;
        for (index, plugin) in plugins.iter().enumerate() {
            let name = plugin["Name"]
                .as_str()
                .map_or_else(|| format!("Plugins[{index}]"), String::from);
            let label = if files.len() == 1 {
                name
            } else {
                format!("{file}: {name}")
            };
            read_plugin(
                &label,
                &plugin["Info"],
                tokens,
                Registration::Plugin(&mut declared),
                &mut out,
            );
        }
    }
    Ok(out)
}

/// Which fields a plugin's declarations are checked against.
pub(crate) enum Registration<'a> {
    /// A plugin of one's own: refused for a built-in field or one an
    /// earlier plugin declares (by name, with the plugin declaring it).
    Plugin(&'a mut BTreeMap<String, String>),
    /// One of usd-core's own, whose fields are the built-in ones.
    Bundled,
}

/// One plugin's `Info.SdfMetadata`, as `_UpdateMetadataFromPlugins` reads
/// it.
pub(crate) fn read_plugin(
    plugin: &str,
    info: &Json,
    tokens: &TokenInterner,
    mut registration: Registration<'_>,
    out: &mut PluginMetadata,
) {
    let Some(fields) = info.get("SdfMetadata") else {
        return;
    };
    let reject =
        |out: &mut PluginMetadata, field: Option<&str>, key, value: Option<&Json>, reason| {
            out.rejected.push(MetadataDeclarationError {
                plugin: plugin.into(),
                field: field.map(String::from),
                key,
                value: value.map(Json::to_string),
                reason,
            });
        };
    let Some(fields) = fields.as_object() else {
        reject(
            out,
            None,
            None,
            Some(fields),
            MetadataRejection::MetadataNotAnObject,
        );
        return;
    };
    // OpenUSD's `JsObject` is a `std::map`: declarations in name order.
    let fields: BTreeMap<&String, &Json> = fields.iter().collect();
    for (name, entry) in fields {
        let Some(entry) = entry.as_object() else {
            reject(
                out,
                Some(name),
                None,
                Some(entry),
                MetadataRejection::NotAnObject,
            );
            continue;
        };
        let Some(type_name) = entry.get("type").and_then(Json::as_str) else {
            reject(
                out,
                Some(name),
                Some("type"),
                entry.get("type"),
                MetadataRejection::TypeNotAString,
            );
            continue;
        };
        if let Registration::Plugin(declared) = &registration {
            let by = MetadataField::builtin(name)
                .map(|field| field.origin().to_string())
                .or_else(|| {
                    declared
                        .get(name.as_str())
                        .map(|by| format!("plugin `{by}`"))
                });
            if let Some(by) = by {
                reject(
                    out,
                    Some(name),
                    None,
                    None,
                    MetadataRejection::AlreadyRegistered { by },
                );
                continue;
            }
        }
        let Some(value_type) = MetadataType::parse(type_name) else {
            reject(
                out,
                Some(name),
                Some("type"),
                entry.get("type"),
                MetadataRejection::UnknownType,
            );
            continue;
        };
        let default = match entry.get("default").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => match default_value(value_type, value, tokens) {
                Ok(default) => Some(default),
                Err(reason) => {
                    reject(out, Some(name), Some("default"), Some(value), reason);
                    continue;
                }
            },
        };
        let mut definition = MetadataDefinition::new(
            tokens.intern(name),
            value_type,
            targets(entry.get("appliesTo")),
        )
        .with_display_group(
            entry
                .get("displayGroup")
                .and_then(Json::as_str)
                .unwrap_or_default(),
        )
        .with_documentation(
            entry
                .get("documentation")
                .and_then(Json::as_str)
                .unwrap_or_default(),
        );
        definition.default = default;
        if let Registration::Plugin(declared) = &mut registration {
            declared.insert(name.clone(), plugin.into());
        }
        out.accepted.push(MetadataDeclaration {
            plugin: plugin.into(),
            definition,
        });
    }
}

/// The fields one of usd-core's own plugins declares, which are the
/// built-in fields; an error naming any declaration OpenUSD would refuse.
pub(crate) fn read_bundled(
    plugin: &str,
    info: &Json,
    tokens: &TokenInterner,
) -> Result<Vec<MetadataDefinition>, String> {
    let mut out = PluginMetadata::default();
    read_plugin(plugin, info, tokens, Registration::Bundled, &mut out);
    if let Some(error) = out.rejected.first() {
        return Err(error.to_string());
    }
    Ok(out.accepted.into_iter().map(|d| d.definition).collect())
}

/// The kinds of spec `appliesTo` registers a field for.
fn targets(applies_to: Option<&Json>) -> Vec<MetadataTarget> {
    let names: Vec<&str> = match applies_to {
        Some(Json::String(name)) => vec![name.as_str()],
        Some(Json::Array(items)) if items.iter().all(Json::is_string) => {
            items.iter().filter_map(Json::as_str).collect()
        }
        // Absent, or neither a string nor a list of strings: every kind.
        _ => Vec::new(),
    };
    if names.is_empty() {
        return vec![
            MetadataTarget::Layer,
            MetadataTarget::Prim,
            MetadataTarget::Attribute,
            MetadataTarget::Relationship,
        ];
    }
    let mut targets = Vec::new();
    for name in names {
        match name {
            "layers" => targets.push(MetadataTarget::Layer),
            "prims" => targets.push(MetadataTarget::Prim),
            "variants" => targets.push(MetadataTarget::Variant),
            "attributes" => targets.push(MetadataTarget::Attribute),
            "relationships" => targets.push(MetadataTarget::Relationship),
            "properties" => {
                targets.extend([MetadataTarget::Attribute, MetadataTarget::Relationship]);
            }
            _ => {}
        }
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.contains(&MetadataTarget::Prim) {
        targets.retain(|target| *target != MetadataTarget::Variant);
    }
    targets
}

/// A plugin's `default` of a field of `value_type`, as `_ParseValue` reads
/// it: its values, in order, fed to the text parser's value context of the
/// type.
fn default_value(
    value_type: MetadataType,
    value: &Json,
    tokens: &TokenInterner,
) -> Result<Value, MetadataRejection> {
    if matches!(
        value_type,
        MetadataType::Dictionary | MetadataType::ListOp(_)
    ) {
        return Err(MetadataRejection::DefaultNotAllowed);
    }
    // A string, an int or a double, or a list of one of them alone.
    let scalar = |value: &Json| -> Option<(u8, String)> {
        Some(match value {
            Json::String(text) => (0, usda_string(text)),
            // OpenUSD takes an integer as an `int`, wrapping it
            // (`JsValue::Get<int>`, a `static_cast` of its 64 bits).
            Json::Number(number) if number.is_i64() || number.is_u64() => {
                let bits = number.as_i64().map_or_else(
                    || number.as_u64().map_or([0; 8], u64::to_le_bytes),
                    i64::to_le_bytes,
                );
                let low = i32::from_le_bytes([bits[0], bits[1], bits[2], bits[3]]);
                (1, low.to_string())
            }
            Json::Number(number) => (2, format!("{:?}", number.as_f64()?)),
            _ => return None,
        })
    };
    let values: Vec<String> = match value {
        Json::Array(items) => {
            let items = items.iter().map(scalar).collect::<Option<Vec<_>>>();
            let items = items.ok_or(MetadataRejection::DefaultNotAValue)?;
            if items.windows(2).any(|pair| pair[0].0 != pair[1].0) {
                return Err(MetadataRejection::DefaultNotAValue);
            }
            items.into_iter().map(|(_, text)| text).collect()
        }
        value => vec![scalar(value).ok_or(MetadataRejection::DefaultNotAValue)?.1],
    };
    let literal = shaped(value_type, &values);
    match layerstack_usda::metadata::metadata_value(&literal, value_type, tokens) {
        Ok(layerstack::FieldValue::Value(value)) => Ok(value),
        Ok(_) => Err(MetadataRejection::DefaultNotAllowed),
        Err(error) => Err(MetadataRejection::DefaultNotOfType(error)),
    }
}

/// USDA's text of `values` as a value of `value_type`, shaped as OpenUSD's
/// `_AddValuesToValueContext` shapes them: tuples of the type's
/// components, in an array for an array type.
fn shaped(value_type: MetadataType, values: &[String]) -> String {
    let (element, array) = match value_type.storage() {
        MetadataType::Value(ty) => (ty, false),
        MetadataType::Array(ty) => (ty, true),
        _ => unreachable!("dictionaries and list ops take no default"),
    };
    let (size, rows) = components(element);
    let one = |values: &[String]| -> String {
        if size == 0 {
            return values.join(", ");
        }
        let tuple = |values: &[String]| format!("({})", values.join(", "));
        if rows {
            let rows: Vec<String> = values.chunks(size).map(tuple).collect();
            format!("({})", rows.join(", "))
        } else {
            tuple(values)
        }
    };
    let per = if rows { size * size } else { size.max(1) };
    if array {
        let items: Vec<String> = values.chunks(per).map(one).collect();
        format!("[{}]", items.join(", "))
    } else if values.len() > per {
        format!("[{}]", values.join(", "))
    } else {
        one(values)
    }
}

/// The components of a value of `ty` (a storage type), and whether they
/// are rows of that many; `0` for a type without components.
fn components(ty: layerstack::ValueType) -> (usize, bool) {
    use layerstack::ValueType as T;
    match ty {
        T::Int2 | T::Half2 | T::Float2 | T::Double2 => (2, false),
        T::Int3 | T::Half3 | T::Float3 | T::Double3 => (3, false),
        T::Int4 | T::Half4 | T::Float4 | T::Double4 | T::Quath | T::Quatf | T::Quatd => (4, false),
        T::Matrix2d => (2, true),
        T::Matrix3d => (3, true),
        T::Matrix4d => (4, true),
        _ => (0, false),
    }
}

/// `text` as a quoted USDA string.
fn usda_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn element_type(ty: &str) -> Option<&str> {
    ty.strip_suffix("[]").or(match ty {
        "tokenlistop" => Some("token"),
        "stringlistop" => Some("string"),
        "intlistop" => Some("int"),
        "uintlistop" => Some("uint"),
        "int64listop" => Some("int64"),
        "uint64listop" => Some("uint64"),
        _ => None,
    })
}

fn primitive(ty: &str, tokens: &TokenInterner) -> Result<Value, String> {
    Ok(match ty {
        "bool" => Value::Bool(false),
        "uchar" => Value::UChar(0),
        "int" => Value::Int(0),
        "uint" => Value::UInt(0),
        "int64" => Value::Int64(0),
        "uint64" => Value::UInt64(0),
        "half" => Value::Half(0),
        "float" => Value::Float(0.),
        "double" => Value::Double(0.),
        "timecode" => Value::TimeCode(0.),
        "duration" => Value::Duration(0.),
        "string" => Value::string(""),
        "token" => Value::Token(tokens.intern("")),
        "asset" => Value::Asset("".into()),
        "pathExpression" => Value::PathExpression("".into()),
        _ => return Err(format!("no typed metadata conversion for {ty}")),
    })
}

fn conversion(ty: &str) -> Result<(String, String), String> {
    // Preserve existing generated getter signatures and spellings.
    match ty {
        "tokenlistop" => return Ok(("::alloc::vec::Vec<&'a str>".into(), "|v, t| crate::value::read_array(v, t, crate::value::read_token)".into())),
        "int64listop" => return Ok(("::alloc::vec::Vec<i64>".into(), "crate::value::read_int64_array".into())),
        "dictionary" => return Ok(("::alloc::vec::Vec<(::alloc::sync::Arc<str>, ::layerstack::Value)>".into(), "|v, _| match v { ::layerstack::Value::Dictionary(entries) => Some(entries.clone()), _ => None }".into())),
        _ => {}
    }
    let tokens = TokenInterner::default();
    if ty == "string[]" {
        // A `string[]` reads as its shared text buffer, as attributes do.
        return Ok((
            "::alloc::sync::Arc<::layerstack::StringArray>".into(),
            "crate::value::read_string_array_shared".into(),
        ));
    }
    if let Some(element) = element_type(ty) {
        primitive(element, &tokens)?;
        let (ty, read) = conversion(element)?;
        return Ok((
            format!("::alloc::vec::Vec<{ty}>"),
            format!("|v, t| crate::value::read_array(v, t, {read})"),
        ));
    }
    let zero = primitive(ty, &tokens)?;
    let conversion =
        crate::views::rust_type(&zero, false)?.expect("metadata primitive has a Rust type");
    Ok((conversion.read, conversion.read_fn))
}

pub(crate) fn files(model: &Model) -> Result<Vec<(String, String)>, String> {
    let mut out = crate::emit::header(model);
    out.push_str("\n//! Typed readers of plugin-defined metadata.\n#![allow(clippy::doc_markdown, clippy::too_long_first_doc_paragraph, reason = \"documentation is copied verbatim from OpenUSD\")]\n");
    let mut seen = std::collections::BTreeSet::new();
    for domain in &model.domains {
        for field in &domain.metadata {
            let name = model.tokens().resolve(field.name);
            let method = crate::views::snake(name);
            let (ty, read) = conversion(field.value_type.name())?;
            for (target, view) in [
                (MetadataTarget::Prim, "PrimView"),
                (MetadataTarget::Attribute, "PropertyMetadata"),
                (MetadataTarget::Layer, "StageMetadata"),
            ] {
                let applies = field.applies_to(target)
                    || (target == MetadataTarget::Attribute
                        && field.applies_to(MetadataTarget::Relationship));
                if !applies {
                    continue;
                }
                if !seen.insert((view, method.clone())) {
                    return Err(format!("duplicate metadata reader {view}::{method}"));
                }
                let _ = writeln!(
                    out,
                    "\n#[cfg(feature = {:?})]\nimpl<'a> crate::{view}<'a> {{\n    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> Option<{ty}> {{\n        self.read_metadata({name:?}, {read})\n    }}\n}}",
                    crate::views::feature(domain.plugin),
                    format!("Reads `{name}`. {}", field.documentation)
                );
            }
        }
    }
    Ok(vec![("metadata.rs".into(), out)])
}

/// How a library's metadata accessor reads and a setter writes a field's
/// value.
struct Accessor {
    /// The getter's type.
    read: String,
    /// The expression reading a `Value` (`fn(&Value, &TokenInterner)`).
    read_fn: String,
    /// The setter's parameter type.
    write: String,
    /// The expression writing `value` with `TOKENS`, a `FieldValue`.
    write_call: String,
}

/// The accessor of a field of `value_type` in a library's module; `None`
/// for a field that holds no value (`opaque`, `group`).
fn accessor(value_type: MetadataType) -> Result<Option<Accessor>, String> {
    use crate::views::{Target, rust_type_in};
    let schemas = "::layerstack_schemas::value";
    let list = |item: &str, read: &str| Accessor {
        read: format!("Vec<{read}>"),
        read_fn: format!("|v, t| {schemas}::read_array(v, t, {schemas}::read_{item})"),
        write: format!(
            "::layerstack::ListOp<{}>",
            if read == "&'a str" || read == "Arc<str>" {
                "&str"
            } else {
                read
            }
        ),
        write_call: match item {
            "token" => format!("{schemas}::write_token_list_op(&value, TOKENS)"),
            "string" => format!("{schemas}::write_string_list_op(&value, TOKENS)"),
            _ => "::layerstack::FieldValue::from(value)".into(),
        },
    };
    Ok(Some(match value_type.storage() {
        MetadataType::Dictionary => Accessor {
            read: "Vec<(Arc<str>, ::layerstack::Value)>".into(),
            read_fn: "|v, _| match v { ::layerstack::Value::Dictionary(entries) => Some(entries.clone()), _ => None }".into(),
            write: "Vec<(Arc<str>, ::layerstack::Value)>".into(),
            write_call: "::layerstack::FieldValue::Value(::layerstack::Value::Dictionary(value))".into(),
        },
        MetadataType::ListOp(item) => {
            let (name, read) = match item {
                layerstack::ListOpItem::Token => ("token", "&'a str"),
                layerstack::ListOpItem::String => ("string", "Arc<str>"),
                layerstack::ListOpItem::Int => ("int", "i32"),
                layerstack::ListOpItem::UInt => ("uint", "u32"),
                layerstack::ListOpItem::Int64 => ("int64", "i64"),
                layerstack::ListOpItem::UInt64 => ("uint64", "u64"),
            };
            list(name, read)
        }
        MetadataType::Value(ty) | MetadataType::Array(ty) => {
            let array = matches!(value_type, MetadataType::Array(_));
            let Some(rust) = rust_type_in(&ty.zero(), array, Target::Library)? else {
                return Ok(None);
            };
            Accessor {
                write_call: format!(
                    "::layerstack::FieldValue::Value({})",
                    rust.write("value", "TOKENS")
                ),
                read: rust.read,
                read_fn: rust.read_fn,
                write: rust.write,
            }
        }
    }))
}

/// The typed views and edit handles of a schema library's metadata fields
/// (`SdfMetadata`), for its Rust module: `PrimMetadata` and
/// `PrimMetadataEdit` for prim fields, `PropertyMetadata` and
/// `PropertyMetadataEdit` for attribute and relationship fields, and
/// `StageMetadata` for layer fields; empty without fields. A field's
/// getter is named for it (`metaFloat` reads with `meta_float`), its
/// setter and clearer `set_…` and `clear_…`; two that would share a name
/// are an error, as a field that holds no value is not given accessors.
pub(crate) fn library_items(
    metadata: &[MetadataDefinition],
    tokens: &TokenInterner,
) -> Result<String, String> {
    #[derive(Default)]
    struct Kind {
        reads: String,
        writes: String,
        names: std::collections::BTreeSet<String>,
    }
    let (mut prim, mut property, mut stage) = (Kind::default(), Kind::default(), Kind::default());
    for field in metadata {
        let name = tokens.resolve(field.name);
        let Some(accessor) = accessor(field.value_type)? else {
            continue;
        };
        let method = crate::views::snake(name);
        let doc = format!(
            "`{name}` (`{}`). {}",
            field.value_type,
            field.documentation.trim()
        );
        let read = &accessor.read;
        let read_fn = &accessor.read_fn;
        let write = &accessor.write;
        for (kind, applies, label) in [
            (&mut prim, field.applies_to(MetadataTarget::Prim), "prim"),
            (
                &mut property,
                field.applies_to(MetadataTarget::Attribute)
                    || field.applies_to(MetadataTarget::Relationship),
                "property",
            ),
            (&mut stage, field.applies_to(MetadataTarget::Layer), "stage"),
        ] {
            if !applies {
                continue;
            }
            for accessor_name in [
                method.clone(),
                format!("set_{method}"),
                format!("clear_{method}"),
            ] {
                if !kind.names.insert(accessor_name.clone()) {
                    return Err(format!(
                        "two metadata fields make the {label} accessor `{accessor_name}`; rename one"
                    ));
                }
            }
            let _ = write!(
                kind.reads,
                "\n    #[doc = {:?}]\n    #[must_use]\n    pub fn {method}(&self) -> Option<{read}> {{\n        self.read({name:?}, {read_fn})\n    }}\n",
                format!("Reads {doc}")
            );
            let write_call = match label {
                "stage" => accessor.write_call.replace("TOKENS", "store.tokens()"),
                _ => accessor.write_call.replace("TOKENS", "edit.tokens()"),
            };
            let _ = match label {
                "stage" => write!(
                    kind.writes,
                    "\n    #[doc = {:?}]\n    ///\n    /// # Errors\n    ///\n    /// Why the field cannot be authored ([`::layerstack::LiveStage::set_stage_metadata`]).\n    pub fn set_{method}(\n        live: &mut ::layerstack::LiveStage,\n        store: &mut dyn ::layerstack::LayerStore,\n        layer: ::layerstack::LayerId,\n        value: {write},\n    ) -> Result<::layerstack::Applied, ::layerstack::StageMetadataError> {{\n        let value = {write_call};\n        let key = store.tokens().intern({name:?});\n        live.set_stage_metadata(store, layer, key, value)\n    }}\n\n    #[doc = {:?}]\n    ///\n    /// # Errors\n    ///\n    /// Why the field cannot be cleared.\n    pub fn clear_{method}(\n        live: &mut ::layerstack::LiveStage,\n        store: &mut dyn ::layerstack::LayerStore,\n        layer: ::layerstack::LayerId,\n    ) -> Result<::layerstack::Applied, ::layerstack::StageMetadataError> {{\n        let key = store.tokens().intern({name:?});\n        live.clear_stage_metadata(store, layer, key)\n    }}\n",
                    format!("Authors {doc} on `layer`, the stage's root or session layer."),
                    format!("Clears `{name}` on `layer`."),
                ),
                _ => write!(
                    kind.writes,
                    "\n    #[doc = {:?}]\n    ///\n    /// # Errors\n    ///\n    /// Why the field cannot be authored there.\n    pub fn set_{method}(\n        &self,\n        edit: &mut ::layerstack_schemas::SchemaEdit<'_>,\n        value: {write},\n    ) -> Result<&Self, ::layerstack_schemas::MetadataError> {{\n        let value = {write_call};\n        self.write(edit, {name:?}, Some(value))?;\n        Ok(self)\n    }}\n\n    #[doc = {:?}]\n    ///\n    /// # Errors\n    ///\n    /// Why the field cannot be cleared there.\n    pub fn clear_{method}(\n        &self,\n        edit: &mut ::layerstack_schemas::SchemaEdit<'_>,\n    ) -> Result<&Self, ::layerstack_schemas::MetadataError> {{\n        self.write(edit, {name:?}, None)?;\n        Ok(self)\n    }}\n",
                    format!("Authors {doc}"),
                    format!("Clears `{name}` in the edit target, so weaker opinions show through."),
                ),
            };
        }
    }
    let mut out = String::new();
    if !prim.reads.is_empty() {
        let _ = write!(
            out,
            "\n/// Reads the library's metadata fields (`SdfMetadata`) of a prim: its\n/// composed authored values ([`::layerstack_schemas::PrimView::metadata_value`]).\n#[derive(Clone, Copy, Debug)]\npub struct PrimMetadata<'a> {{\n    prim: ::layerstack_schemas::PrimView<'a>,\n}}\n\n#[allow(clippy::doc_markdown, reason = \"documentation as the plugin writes it\")]\nimpl<'a> PrimMetadata<'a> {{\n    /// The fields of `prim`.\n    #[must_use]\n    pub fn of(prim: ::layerstack_schemas::PrimView<'a>) -> Self {{\n        Self {{ prim }}\n    }}\n\n    fn read<T>(\n        &self,\n        name: &str,\n        read: impl Fn(&::layerstack::Value, &'a ::layerstack::TokenInterner) -> Option<T>,\n    ) -> Option<T> {{\n        read(&self.prim.metadata_value(name)?, self.prim.scene().store().tokens())\n    }}\n{}}}\n\n/// Authors the library's metadata fields of a prim through a\n/// [`::layerstack_schemas::SchemaEdit`]\n/// ([`::layerstack_schemas::PrimEdit::write_metadata`]).\n#[derive(Clone, Copy, Debug)]\npub struct PrimMetadataEdit {{\n    prim: ::layerstack_schemas::PrimEdit,\n}}\n\n#[allow(clippy::doc_markdown, reason = \"documentation as the plugin writes it\")]\nimpl PrimMetadataEdit {{\n    /// A handle authoring the fields of the prim `prim` authors.\n    #[must_use]\n    pub fn of(prim: ::layerstack_schemas::PrimEdit) -> Self {{\n        Self {{ prim }}\n    }}\n\n    fn write(\n        &self,\n        edit: &mut ::layerstack_schemas::SchemaEdit<'_>,\n        name: &str,\n        value: Option<::layerstack::FieldValue>,\n    ) -> Result<(), ::layerstack_schemas::MetadataError> {{\n        match value {{\n            Some(value) => self.prim.write_metadata(edit, name, value),\n            None => self.prim.clear_metadata(edit, name),\n        }}\n    }}\n{}}}\n",
            prim.reads, prim.writes
        );
    }
    if !property.reads.is_empty() {
        let _ = write!(
            out,
            "\n/// Reads the library's metadata fields (`SdfMetadata`) of a property: its\n/// composed authored values ([`::layerstack_schemas::PropertyMetadata::value`]).\n#[derive(Clone, Copy, Debug)]\npub struct PropertyMetadata<'a> {{\n    metadata: ::layerstack_schemas::PropertyMetadata<'a>,\n    tokens: &'a ::layerstack::TokenInterner,\n}}\n\n#[allow(clippy::doc_markdown, reason = \"documentation as the plugin writes it\")]\nimpl<'a> PropertyMetadata<'a> {{\n    /// The fields of the property `name` (its USD name) of `prim`, if it\n    /// has one.\n    #[must_use]\n    pub fn of(prim: ::layerstack_schemas::PrimView<'a>, name: &str) -> Option<Self> {{\n        Some(Self {{\n            metadata: prim.property_metadata(name)?,\n            tokens: prim.scene().store().tokens(),\n        }})\n    }}\n\n    fn read<T>(\n        &self,\n        name: &str,\n        read: impl Fn(&::layerstack::Value, &'a ::layerstack::TokenInterner) -> Option<T>,\n    ) -> Option<T> {{\n        read(&self.metadata.value(name)?, self.tokens)\n    }}\n{}}}\n\n/// Authors the library's metadata fields of a property through a\n/// [`::layerstack_schemas::SchemaEdit`] ([`::layerstack_schemas::PrimEdit::write_property_metadata`]).\n#[derive(Clone, Copy, Debug)]\npub struct PropertyMetadataEdit<'n> {{\n    prim: ::layerstack_schemas::PrimEdit,\n    name: &'n str,\n}}\n\n#[allow(clippy::doc_markdown, reason = \"documentation as the plugin writes it\")]\nimpl<'n> PropertyMetadataEdit<'n> {{\n    /// A handle authoring the fields of the property `name` (its USD name)\n    /// of the prim `prim` authors.\n    #[must_use]\n    pub fn of(prim: ::layerstack_schemas::PrimEdit, name: &'n str) -> Self {{\n        Self {{ prim, name }}\n    }}\n\n    fn write(\n        &self,\n        edit: &mut ::layerstack_schemas::SchemaEdit<'_>,\n        key: &str,\n        value: Option<::layerstack::FieldValue>,\n    ) -> Result<(), ::layerstack_schemas::MetadataError> {{\n        match value {{\n            Some(value) => self.prim.write_property_metadata(edit, self.name, key, value),\n            None => self.prim.clear_property_metadata(edit, self.name, key),\n        }}\n    }}\n{}}}\n",
            property.reads, property.writes
        );
    }
    if !stage.reads.is_empty() {
        let _ = write!(
            out,
            "\n/// Reads and authors the library's metadata fields (`SdfMetadata`) of the\n/// stage: its root layer's, or their fallbacks ([`::layerstack_schemas::StageMetadata::value`]).\n#[derive(Clone, Copy, Debug)]\npub struct StageMetadata<'a> {{\n    scene: ::layerstack_schemas::Scene<'a>,\n}}\n\n#[allow(clippy::doc_markdown, reason = \"documentation as the plugin writes it\")]\nimpl<'a> StageMetadata<'a> {{\n    /// The fields of the stage of `scene`.\n    #[must_use]\n    pub fn of(scene: ::layerstack_schemas::Scene<'a>) -> Self {{\n        Self {{ scene }}\n    }}\n\n    fn read<T>(\n        &self,\n        name: &str,\n        read: impl Fn(&::layerstack::Value, &'a ::layerstack::TokenInterner) -> Option<T>,\n    ) -> Option<T> {{\n        read(&self.scene.metadata().value(name)?, self.scene.store().tokens())\n    }}\n{}{}}}\n",
            stage.reads, stage.writes
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fields one plugin of `fields`' declarations registers, and the
    /// reasons it refuses the others.
    fn declare(fields: Json) -> (PluginMetadata, TokenInterner) {
        let tokens = TokenInterner::default();
        let text =
            serde_json::json!({"Plugins": [{"Name": "host", "Info": {"SdfMetadata": fields}}]})
                .to_string();
        (
            read([("plugInfo.json", text.as_str())], &tokens).unwrap(),
            tokens,
        )
    }

    fn default_of(ty: &str, default: Json) -> Result<Value, MetadataRejection> {
        let (read, _) = declare(serde_json::json!({"hostField": {"type": ty, "default": default}}));
        match (read.accepted.first(), read.rejected.first()) {
            (Some(accepted), None) => Ok(accepted.definition.default.clone().unwrap()),
            (None, Some(rejected)) => Err(rejected.reason.clone()),
            _ => panic!("one declaration"),
        }
    }

    #[test]
    fn plugin_targets_expand_and_defaults_keep_the_declared_type() {
        let (read, tokens) = declare(
            serde_json::json!({"renderStyle": {"type": "token", "appliesTo": "properties", "default": "color"}}),
        );
        let field = &read.accepted[0].definition;
        assert_eq!(
            field.targets,
            [MetadataTarget::Attribute, MetadataTarget::Relationship]
        );
        assert_eq!(field.default, Some(Value::Token(tokens.intern("color"))));
        for (applies_to, targets) in [
            (
                Json::Null,
                vec![
                    MetadataTarget::Layer,
                    MetadataTarget::Prim,
                    MetadataTarget::Attribute,
                    MetadataTarget::Relationship,
                ],
            ),
            (
                serde_json::json!(3),
                vec![
                    MetadataTarget::Layer,
                    MetadataTarget::Prim,
                    MetadataTarget::Attribute,
                    MetadataTarget::Relationship,
                ],
            ),
            (serde_json::json!(["nodes"]), vec![]),
            (
                serde_json::json!(["variants"]),
                vec![MetadataTarget::Variant],
            ),
            (
                serde_json::json!(["variants", "prims"]),
                vec![MetadataTarget::Prim],
            ),
        ] {
            assert_eq!(targets_of(applies_to.clone()), targets, "{applies_to}");
        }
    }

    fn targets_of(applies_to: Json) -> Vec<MetadataTarget> {
        let mut entry = serde_json::json!({"type": "int"});
        if !applies_to.is_null() {
            entry["appliesTo"] = applies_to;
        }
        let (read, _) = declare(serde_json::json!({ "hostField": entry }));
        read.accepted[0].definition.targets.clone()
    }

    #[test]
    fn defaults_read_as_openusd_feeds_them_to_the_text_parser() {
        for (ty, default, expected) in [
            ("bool", serde_json::json!(1), Value::Bool(true)),
            ("int", serde_json::json!(1.5), Value::Int(1)),
            ("double", serde_json::json!(2), Value::Double(2.0)),
            ("uchar", serde_json::json!(200), Value::UChar(200)),
            // An integer is a 32-bit `int` first, as OpenUSD reads it.
            (
                "int64",
                serde_json::json!(4_294_967_297_i64),
                Value::Int64(1),
            ),
            (
                "asset",
                serde_json::json!("a.usda"),
                Value::Asset("a.usda".into()),
            ),
            (
                "float3",
                serde_json::json!([1, 2, 3]),
                Value::Vec3f([1.0, 2.0, 3.0]),
            ),
            ("int[]", serde_json::json!(4), Value::from(vec![4_i32])),
            (
                "float3[]",
                serde_json::json!([1, 2, 3, 4, 5, 6]),
                Value::from(vec![[1.0_f32, 2.0, 3.0], [4.0, 5.0, 6.0]]),
            ),
        ] {
            assert_eq!(
                default_of(ty, default.clone()),
                Ok(expected),
                "{ty} {default}"
            );
        }
    }

    #[test]
    fn malformed_defaults_are_refused_as_openusd_refuses_them() {
        for (ty, default, reason) in [
            (
                "bool",
                serde_json::json!(true),
                MetadataRejection::DefaultNotAValue,
            ),
            (
                "float[]",
                serde_json::json!([1, 2.5]),
                MetadataRejection::DefaultNotAValue,
            ),
            (
                "int",
                serde_json::json!({"a": 1}),
                MetadataRejection::DefaultNotAValue,
            ),
            (
                "dictionary",
                serde_json::json!({}),
                MetadataRejection::DefaultNotAllowed,
            ),
            (
                "tokenlistop",
                serde_json::json!(["a"]),
                MetadataRejection::DefaultNotAllowed,
            ),
            (
                "uchar",
                serde_json::json!(300),
                MetadataRejection::DefaultNotOfType(MetadataValueError::Unconvertible {
                    element: None,
                }),
            ),
            (
                "float3",
                serde_json::json!([1, 2]),
                MetadataRejection::DefaultNotOfType(MetadataValueError::TupleDimensions),
            ),
            (
                "token",
                serde_json::json!(3),
                MetadataRejection::DefaultNotOfType(MetadataValueError::Unconvertible {
                    element: None,
                }),
            ),
        ] {
            assert_eq!(
                default_of(ty, default.clone()),
                Err(reason),
                "{ty} {default}"
            );
        }
    }

    #[test]
    fn declarations_of_registered_fields_and_unknown_types_are_refused() {
        let (read, _) = declare(serde_json::json!({
            "documentation": {"type": "string"},
            "interpolation": {"type": "token", "appliesTo": "attributes"},
            "hostPath": {"type": "path"},
            "hostCase": {"type": "Float"},
            "hostNumber": {"type": 3},
            "hostText": "token",
        }));
        let reasons: Vec<(&str, &MetadataRejection)> = read
            .rejected
            .iter()
            .map(|error| (error.field.as_deref().unwrap(), &error.reason))
            .collect();
        assert_eq!(
            reasons,
            [
                (
                    "documentation",
                    &MetadataRejection::AlreadyRegistered { by: "Sdf".into() }
                ),
                ("hostCase", &MetadataRejection::UnknownType),
                ("hostNumber", &MetadataRejection::TypeNotAString),
                ("hostPath", &MetadataRejection::UnknownType),
                ("hostText", &MetadataRejection::NotAnObject),
                (
                    "interpolation",
                    &MetadataRejection::AlreadyRegistered {
                        by: "the `usdGeom` plugin".into()
                    }
                ),
            ]
        );
        assert!(read.accepted.is_empty());
        let message = read.rejected[0].openusd_message().unwrap();
        assert_eq!(message, "\"documentation\" is already a registered field");
    }

    #[test]
    fn a_later_plugin_cannot_declare_a_field_again() {
        let tokens = TokenInterner::default();
        let plugin = |name: &str| {
            serde_json::json!({"Plugins": [{"Name": name, "Info": {"SdfMetadata": {
                "hostWeight": {"type": "float"}
            }}}]})
            .to_string()
        };
        let (first, second) = (plugin("first"), plugin("second"));
        let read = read([("a", first.as_str()), ("b", second.as_str())], &tokens).unwrap();
        assert_eq!(read.accepted.len(), 1);
        assert_eq!(
            read.rejected[0].reason,
            MetadataRejection::AlreadyRegistered {
                by: "plugin `a: first`".into()
            }
        );
    }

    #[test]
    fn library_accessors_that_would_share_a_name_are_an_error() {
        let (read, tokens) = declare(serde_json::json!({
            "hostScale": {"type": "float", "appliesTo": "prims"},
            "host_scale": {"type": "double", "appliesTo": "prims"},
        }));
        let fields: Vec<_> = read.accepted.into_iter().map(|d| d.definition).collect();
        let error = library_items(&fields, &tokens).unwrap_err();
        assert!(error.contains("`host_scale`"), "{error}");
        // On different kinds of spec they do not meet.
        let (read, tokens) = declare(serde_json::json!({
            "hostScale": {"type": "float", "appliesTo": "prims"},
            "host_scale": {"type": "double", "appliesTo": "layers"},
            "hostBlob": {"type": "opaque", "appliesTo": "prims"},
        }));
        let fields: Vec<_> = read.accepted.into_iter().map(|d| d.definition).collect();
        let items = library_items(&fields, &tokens).unwrap();
        assert!(items.contains("pub fn host_scale(&self) -> Option<f32>"));
        assert!(items.contains("pub fn host_scale(&self) -> Option<f64>"));
        assert!(
            !items.contains("host_blob"),
            "an opaque field holds no value"
        );
    }
}
