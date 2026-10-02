// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reads `SdfMetadata` and emits typed metadata accessors.

use crate::model::Model;
use layerstack::{MetadataDefinition, MetadataTarget, TokenInterner, Value};
use serde_json::Value as Json;
use std::fmt::Write as _;

pub(crate) fn read(
    json: &Json,
    tokens: &mut TokenInterner,
) -> Result<Vec<MetadataDefinition>, String> {
    if json.is_null() {
        return Ok(Vec::new());
    }
    let fields = json.as_object().ok_or("SdfMetadata must be an object")?;
    fields
        .iter()
        .map(|(name, entry)| {
            let type_name = entry["type"]
                .as_str()
                .ok_or_else(|| format!("{name}: missing metadata type"))?;
            // Fail generation when a newly introduced type needs a conversion.
            conversion(type_name)?;
            let applies = &entry["appliesTo"];
            let names = if let Some(name) = applies.as_str() {
                vec![name]
            } else {
                applies
                    .as_array()
                    .ok_or_else(|| format!("{name}: appliesTo must be a string or array"))?
                    .iter()
                    .map(|value| value.as_str().ok_or("appliesTo entries must be strings"))
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut targets = Vec::new();
            for target in names {
                match target {
                    "layers" => targets.push(MetadataTarget::Layer),
                    "prims" => targets.push(MetadataTarget::Prim),
                    "attributes" => targets.push(MetadataTarget::Attribute),
                    "relationships" => targets.push(MetadataTarget::Relationship),
                    "properties" => {
                        targets.extend([MetadataTarget::Attribute, MetadataTarget::Relationship]);
                    }
                    other => return Err(format!("{name}: unknown metadata target {other}")),
                }
            }
            if targets.is_empty() {
                return Err(format!("{name}: no metadata targets"));
            }
            targets.sort_unstable();
            targets.dedup();
            let default = entry
                .get("default")
                .map(|value| default_value(type_name, value, tokens))
                .transpose()
                .map_err(|why| format!("{name}: {why}"))?;
            Ok(MetadataDefinition {
                name: tokens.intern(name),
                type_name: type_name.into(),
                targets,
                default,
                documentation: entry["documentation"].as_str().unwrap_or_default().into(),
            })
        })
        .collect()
}

fn default_value(ty: &str, value: &Json, tokens: &mut TokenInterner) -> Result<Value, String> {
    Ok(match ty {
        "token" => {
            Value::Token(tokens.intern(value.as_str().ok_or("token default must be a string")?))
        }
        "string" => Value::string(value.as_str().ok_or("string default must be a string")?),
        "bool" => Value::Bool(value.as_bool().ok_or("bool default must be a boolean")?),
        "int" => Value::Int(
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or("int default must fit i32")?,
        ),
        "float" => {
            let number = serde_json::from_value::<f32>(value.clone())
                .map_err(|_| "float default must be numeric")?;
            if !number.is_finite() {
                return Err("float default must fit f32".into());
            }
            Value::Float(number)
        }
        "double" => Value::Double(value.as_f64().ok_or("double default must be numeric")?),
        _ => return Err(format!("cannot emit a {ty} metadata default")),
    })
}

fn conversion(ty: &str) -> Result<(&'static str, &'static str), String> {
    Ok(match ty {
        "token" => ("&'a str", "crate::value::read_token"),
        "string" => ("::alloc::sync::Arc<str>", "crate::value::read_string"),
        "bool" => ("bool", "crate::value::read_bool"),
        "int" => ("i32", "crate::value::read_int"),
        "float" => ("f32", "crate::value::read_float"),
        "double" => ("f64", "crate::value::read_double"),
        "tokenlistop" => (
            "::alloc::vec::Vec<&'a str>",
            "|v, t| crate::value::read_array(v, t, crate::value::read_token)",
        ),
        "int64listop" => ("::alloc::vec::Vec<i64>", "crate::value::read_int64_array"),
        "dictionary" => (
            "::alloc::vec::Vec<(::alloc::sync::Arc<str>, ::layerstack::Value)>",
            "|v, _| match v { ::layerstack::Value::Dictionary(entries) => Some(entries.clone()), _ => None }",
        ),
        _ => return Err(format!("no typed metadata conversion for {ty}")),
    })
}

pub(crate) fn files(model: &Model) -> Result<Vec<(String, String)>, String> {
    let mut out = crate::emit::header(model);
    out.push_str("\n//! Typed readers of plugin-defined metadata.\n#![allow(clippy::doc_markdown, clippy::too_long_first_doc_paragraph, reason = \"documentation is copied verbatim from OpenUSD\")]\n");
    let mut seen = std::collections::BTreeSet::new();
    for domain in &model.domains {
        for field in &domain.metadata {
            let name = model.tokens.resolve(field.name);
            let method = crate::views::snake(name);
            let (ty, read) = conversion(&field.type_name)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_targets_expand_and_defaults_keep_the_declared_type() {
        let mut tokens = TokenInterner::default();
        let fields = read(&serde_json::json!({"renderType": {"type": "token", "appliesTo": "properties", "default": "color"}}), &mut tokens).unwrap();
        assert_eq!(
            fields[0].targets,
            [MetadataTarget::Attribute, MetadataTarget::Relationship]
        );
        assert_eq!(
            fields[0].default,
            Some(Value::Token(tokens.intern("color")))
        );
        assert!(read(&serde_json::json!({"bad": {"type": "int", "appliesTo": ["prims"], "default": 2147483648_i64}}), &mut tokens).is_err());
        assert!(
            read(
                &serde_json::json!({"bad": {"type": "token", "appliesTo": ["unknown"]}}),
                &mut tokens
            )
            .is_err()
        );
    }
}
