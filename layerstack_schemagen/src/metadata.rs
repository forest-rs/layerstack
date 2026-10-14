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

fn primitive(ty: &str, tokens: &mut TokenInterner) -> Result<Value, String> {
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
        "string" => Value::string(""),
        "token" => Value::Token(tokens.intern("")),
        "asset" => Value::Asset("".into()),
        "pathExpression" => Value::PathExpression("".into()),
        _ => return Err(format!("no typed metadata conversion for {ty}")),
    })
}

fn default_value(ty: &str, value: &Json, tokens: &mut TokenInterner) -> Result<Value, String> {
    if let Some(element) = element_type(ty) {
        let items = value
            .as_array()
            .ok_or_else(|| format!("{ty} default must be an array"))?
            .iter()
            .map(|value| default_value(element, value, tokens))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::array_with_element(
            items,
            Some(&primitive(element, tokens)?),
        ));
    }
    let signed = || {
        value
            .as_i64()
            .ok_or_else(|| format!("{ty} default must be a signed integer"))
    };
    let unsigned = || {
        value
            .as_u64()
            .ok_or_else(|| format!("{ty} default must be an unsigned integer"))
    };
    let float = || {
        let number = serde_json::from_value::<f32>(value.clone())
            .map_err(|_| format!("{ty} default must be numeric"))?;
        if !number.is_finite() {
            return Err(format!("{ty} default must fit f32"));
        }
        Ok(number)
    };
    let text = || {
        value
            .as_str()
            .ok_or_else(|| format!("{ty} default must be a string"))
    };
    Ok(match ty {
        "token" => Value::Token(tokens.intern(text()?)),
        "string" => Value::string(text()?),
        "asset" => Value::Asset(text()?.into()),
        "pathExpression" => Value::PathExpression(text()?.into()),
        "bool" => Value::Bool(value.as_bool().ok_or("bool default must be a boolean")?),
        "uchar" => {
            Value::UChar(u8::try_from(unsigned()?).map_err(|_| "uchar default must fit u8")?)
        }
        "int" => Value::Int(i32::try_from(signed()?).map_err(|_| "int default must fit i32")?),
        "uint" => Value::UInt(u32::try_from(unsigned()?).map_err(|_| "uint default must fit u32")?),
        "int64" => Value::Int64(signed()?),
        "uint64" => Value::UInt64(unsigned()?),
        "half" => {
            let bits = layerstack::half::from_f32(float()?);
            if !layerstack::half::to_f32(bits).is_finite() {
                return Err("half default must fit f16".into());
            }
            Value::Half(bits)
        }
        "float" => Value::Float(float()?),
        "double" => Value::Double(value.as_f64().ok_or("double default must be numeric")?),
        "timecode" => Value::TimeCode(value.as_f64().ok_or("timecode default must be numeric")?),
        "dictionary" => dictionary_default(value)?,
        _ => return Err(format!("cannot emit a {ty} metadata default")),
    })
}

fn dictionary_default(value: &Json) -> Result<Value, String> {
    fn entry(value: &Json) -> Result<Value, String> {
        match value {
            Json::Bool(value) => Ok(Value::Bool(*value)),
            Json::String(value) => Ok(Value::string(value.as_str())),
            Json::Number(value) if value.is_i64() => {
                Ok(Value::Int64(value.as_i64().expect("i64 number")))
            }
            Json::Number(value) if value.is_u64() => {
                Ok(Value::UInt64(value.as_u64().expect("u64 number")))
            }
            Json::Number(value) => Ok(Value::Double(
                value.as_f64().ok_or("dictionary number must fit f64")?,
            )),
            Json::Array(items) => Ok(Value::Array(
                items.iter().map(entry).collect::<Result<_, _>>()?,
            )),
            Json::Object(_) => dictionary_default(value),
            Json::Null => Err("dictionary defaults cannot contain null".into()),
        }
    }
    let entries = value
        .as_object()
        .ok_or("dictionary default must be an object")?;
    Ok(Value::Dictionary(
        entries
            .iter()
            .map(|(key, value)| Ok((key.as_str().into(), entry(value)?)))
            .collect::<Result<_, String>>()?,
    ))
}

fn conversion(ty: &str) -> Result<(String, String), String> {
    // Preserve existing generated getter signatures and spellings.
    match ty {
        "tokenlistop" => return Ok(("::alloc::vec::Vec<&'a str>".into(), "|v, t| crate::value::read_array(v, t, crate::value::read_token)".into())),
        "int64listop" => return Ok(("::alloc::vec::Vec<i64>".into(), "crate::value::read_int64_array".into())),
        "dictionary" => return Ok(("::alloc::vec::Vec<(::alloc::sync::Arc<str>, ::layerstack::Value)>".into(), "|v, _| match v { ::layerstack::Value::Dictionary(entries) => Some(entries.clone()), _ => None }".into())),
        _ => {}
    }
    let mut tokens = TokenInterner::default();
    if let Some(element) = element_type(ty) {
        primitive(element, &mut tokens)?;
        let (ty, read) = conversion(element)?;
        return Ok((
            format!("::alloc::vec::Vec<{ty}>"),
            format!("|v, t| crate::value::read_array(v, t, {read})"),
        ));
    }
    let zero = primitive(ty, &mut tokens)?;
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

    #[test]
    fn primitive_arrays_and_list_defaults_keep_element_types_and_ranges() {
        let mut tokens = TokenInterner::default();
        for (ty, literal, expected) in [
            ("uchar", serde_json::json!(255), Value::UChar(u8::MAX)),
            (
                "uint",
                serde_json::json!(4294967295_u64),
                Value::UInt(u32::MAX),
            ),
            ("int64", serde_json::json!(i64::MIN), Value::Int64(i64::MIN)),
            (
                "uint64",
                serde_json::json!(u64::MAX),
                Value::UInt64(u64::MAX),
            ),
            (
                "half",
                serde_json::json!(0.5),
                Value::Half(layerstack::half::from_f32(0.5)),
            ),
            ("timecode", serde_json::json!(3), Value::TimeCode(3.)),
            (
                "asset",
                serde_json::json!("texture.png"),
                Value::Asset("texture.png".into()),
            ),
            (
                "pathExpression",
                serde_json::json!("/World/**"),
                Value::PathExpression("/World/**".into()),
            ),
        ] {
            assert_eq!(
                default_value(ty, &literal, &mut tokens).unwrap(),
                expected,
                "{ty}"
            );
            assert_eq!(
                default_value(
                    &format!("{ty}[]"),
                    &serde_json::json!([literal]),
                    &mut tokens
                )
                .unwrap()
                .array_ref()
                .unwrap()
                .get(0)
                .unwrap()
                .as_ref(),
                &expected,
                "{ty}[]"
            );
        }
        for (ty, literal) in [
            ("tokenlistop", serde_json::json!(["red", "green"])),
            ("stringlistop", serde_json::json!(["one", "two"])),
            ("intlistop", serde_json::json!([i32::MIN, i32::MAX])),
            ("uintlistop", serde_json::json!([0, u32::MAX])),
            ("int64listop", serde_json::json!([i64::MIN, i64::MAX])),
            ("uint64listop", serde_json::json!([0, u64::MAX])),
        ] {
            let declarations = read(&serde_json::json!({"hostField": {"type": ty, "appliesTo": "prims", "default": literal}}), &mut tokens).unwrap();
            assert_eq!(
                declarations[0]
                    .default
                    .as_ref()
                    .unwrap()
                    .array_ref()
                    .unwrap()
                    .len(),
                2,
                "{ty}"
            );
            assert_eq!(
                default_value(ty, &serde_json::json!([]), &mut tokens)
                    .unwrap()
                    .array_ref()
                    .unwrap()
                    .len(),
                0
            );
        }
    }

    #[test]
    fn malformed_defaults_fail_generation_without_silent_coercion() {
        let mut tokens = TokenInterner::default();
        for (ty, literal) in [
            ("uchar", serde_json::json!(256)),
            ("uint", serde_json::json!(-1)),
            ("int", serde_json::json!(2147483648_i64)),
            ("int64", serde_json::json!(u64::MAX)),
            ("half", serde_json::json!(70000)),
            ("float", serde_json::json!(1e100)),
            ("float[]", serde_json::json!([1, "bad"])),
            ("token[]", serde_json::json!("not an array")),
            ("uintlistop", serde_json::json!([-1])),
            ("dictionary", serde_json::json!([])),
            ("dictionary", serde_json::json!({"nested": null})),
        ] {
            assert!(read(&serde_json::json!({"hostField": {"type": ty, "appliesTo": "prims", "default": literal}}), &mut tokens).is_err(), "{ty}");
        }
        for ty in ["float3", "dictionary[]", "intlistop[]", "hostOpaque"] {
            assert!(
                read(
                    &serde_json::json!({"hostField": {"type": ty, "appliesTo": "prims"}}),
                    &mut tokens
                )
                .is_err(),
                "unsupported {ty} must not generate a foreign-kind reader"
            );
        }
    }

    #[test]
    fn dictionary_defaults_preserve_nested_json_types() {
        let mut tokens = TokenInterner::default();
        let value = default_value(
            "dictionary",
            &serde_json::json!({"nested": {"enabled": true, "count": 3, "items": ["red", 0.5]}}),
            &mut tokens,
        )
        .unwrap();
        assert_eq!(
            value,
            Value::Dictionary(vec![(
                "nested".into(),
                Value::Dictionary(vec![
                    ("count".into(), Value::Int64(3)),
                    ("enabled".into(), Value::Bool(true)),
                    (
                        "items".into(),
                        Value::Array(vec![Value::string("red"), Value::Double(0.5)])
                    ),
                ])
            )])
        );
    }
}
