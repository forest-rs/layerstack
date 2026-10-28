// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Metadata statements read as their field's registered type.
//!
//! OpenUSD's text parser looks a metadata statement's key up among the
//! fields of the kind of spec it is on (`_KeyValueMetadataStart`,
//! `pxr/usd/sdf/textParserHelpers.cpp`). A field registered there — built
//! in or a plugin's, alike — reads its literal as its type, with the value
//! conversions of attribute values (`irIsInvertible = 0` is `false` for a
//! `bool` field, `metaFloat = 1` is `1.0` for a `float` one), and a literal
//! the type cannot hold fails the layer ([`MetadataRejection`]). Any other
//! key is a field no registration types there, whose text OpenUSD keeps as
//! it reads it; here its literal keeps the type it is written as.
//!
//! The metadata of a variant is read as a prim's: OpenUSD's parser takes
//! the prim spec's fields for it (`_GetSpecTypeFromContext`), so a field
//! registered for variants alone is no field there.
//!
//! Spec: AOUSD Core §7.4 (metadata), §16.2 (USDA).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use layerstack::doc::{FieldValue, Value};
use layerstack::interner::TokenInterner;
use layerstack::listop::ListOp;
use layerstack::{ListOpItem, MetadataTarget, MetadataType, ValueType};

use crate::ast;
use crate::numeric::{convert_scalar, dimensioned_value};

/// Why a metadata statement's literal is not a value of its field's type.
///
/// Each names OpenUSD's text parser's error for it
/// ([`MetadataRejection::openusd_message`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataValueError {
    /// The key is a field of the spec that is not metadata (`specifier`,
    /// `variability`, `primOrder`): `"…" is registered as a non-metadata
    /// field`.
    NotMetadata,
    /// `None` for a field that is not a list op: `invalid value for field
    /// "…"`.
    NoValue,
    /// An array literal for a field that is not an array: `Type name
    /// missing [] for shaped value.`
    ArrayForScalar,
    /// A literal that is not an array for an array or list-op field: `Type
    /// name has [] for non-shaped value!`
    ScalarForArray,
    /// A tuple for a field of a type without components: `Tuple nesting
    /// too deep!`
    TupleForScalar,
    /// A tuple with the wrong number of components or rows: `Tuple
    /// dimensions error`.
    TupleDimensions,
    /// A single value for a field of a type with components: `Not enough
    /// values to parse value`.
    NotEnoughValues,
    /// A value that does not convert to the type, such as a string for a
    /// `float`, a number out of an integer type's range, or a number for a
    /// `token`; `element` is its position in an array or list op.
    Unconvertible {
        /// The array element or list-op item, if one.
        element: Option<usize>,
    },
    /// A value that is not a dictionary for a `dictionary` field.
    NotADictionary,
}

/// A metadata statement OpenUSD's text parser refuses, which fails the
/// layer: the field, the kind of spec it is on, the type the field is
/// registered with, and why the literal is not a value of it.
///
/// Spec: AOUSD Core §7.4 (metadata), §16.2 (USDA).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetadataRejection {
    /// The field's name.
    pub field: String,
    /// The kind of spec the statement is on (a variant's metadata as a
    /// prim's).
    pub target: MetadataTarget,
    /// The field's registered type; `None` for a field that is not
    /// metadata.
    pub value_type: Option<MetadataType>,
    /// Why.
    pub error: MetadataValueError,
}

impl MetadataRejection {
    /// OpenUSD's message for the rejection, as its text parser raises it
    /// (`pxr/usd/sdf/textParserHelpers.cpp`, `parserValueContext.cpp`,
    /// `parserHelpers.cpp`), without the position it adds.
    #[must_use]
    pub fn openusd_message(&self) -> String {
        let type_name = self
            .value_type
            .map_or("", |value_type| value_type.storage().name());
        match self.error {
            MetadataValueError::NotMetadata => {
                format!("\"{}\" is registered as a non-metadata field", self.field)
            }
            MetadataValueError::NoValue => format!("invalid value for field \"{}\"", self.field),
            MetadataValueError::ArrayForScalar => "Type name missing [] for shaped value.".into(),
            MetadataValueError::ScalarForArray => "Type name has [] for non-shaped value!".into(),
            MetadataValueError::TupleForScalar => format!(
                "Tuple nesting too deep! Should not be deeper than 0 for attribute of type {type_name}."
            ),
            MetadataValueError::TupleDimensions => {
                format!("Tuple dimensions error for attribute of type {type_name}.")
            }
            MetadataValueError::NotEnoughValues => format!(
                "Not enough values to parse value of type {}",
                self.value_type.map_or("", gf_name)
            ),
            MetadataValueError::Unconvertible { element: None } => {
                "Error parsing simple value: Failed to parse value (at sub-part 0 if there are multiple parts)".into()
            }
            MetadataValueError::Unconvertible {
                element: Some(element),
            } => format!(
                "Error parsing shaped value: Failed to parse at element {element} (at sub-part 0 if there are multiple parts)"
            ),
            MetadataValueError::NotADictionary => "Unrecognized type name ''".into(),
        }
    }
}

impl fmt::Display for MetadataRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (`{}`", self.openusd_message(), self.field)?;
        if let Some(value_type) = self.value_type {
            write!(f, ", `{value_type}` metadata")?;
        }
        let article = if self.target == MetadataTarget::Attribute {
            "an"
        } else {
            "a"
        };
        write!(f, " on {article} {} spec)", self.target)
    }
}

impl core::error::Error for MetadataRejection {}

/// The Gf class of a type with components, as OpenUSD names it.
fn gf_name(value_type: MetadataType) -> &'static str {
    let (MetadataType::Value(ty) | MetadataType::Array(ty)) = value_type.storage() else {
        return "";
    };
    match ty {
        ValueType::Int2 => "Vec2i",
        ValueType::Int3 => "Vec3i",
        ValueType::Int4 => "Vec4i",
        ValueType::Half2 => "Vec2h",
        ValueType::Half3 => "Vec3h",
        ValueType::Half4 => "Vec4h",
        ValueType::Float2 => "Vec2f",
        ValueType::Float3 => "Vec3f",
        ValueType::Float4 => "Vec4f",
        ValueType::Double2 => "Vec2d",
        ValueType::Double3 => "Vec3d",
        ValueType::Double4 => "Vec4d",
        ValueType::Matrix2d => "Matrix2d",
        ValueType::Matrix3d => "Matrix3d",
        ValueType::Matrix4d => "Matrix4d",
        ValueType::Quath => "Quath",
        ValueType::Quatf => "Quatf",
        ValueType::Quatd => "Quatd",
        _ => "",
    }
}

/// The components of a value of `ty` (its storage), and whether they are
/// rows of that many: `None` for a type without components.
fn shape(ty: ValueType) -> Option<(usize, bool)> {
    Some(match ty {
        ValueType::Int2 | ValueType::Half2 | ValueType::Float2 | ValueType::Double2 => (2, false),
        ValueType::Int3 | ValueType::Half3 | ValueType::Float3 | ValueType::Double3 => (3, false),
        ValueType::Int4
        | ValueType::Half4
        | ValueType::Float4
        | ValueType::Double4
        | ValueType::Quath
        | ValueType::Quatf
        | ValueType::Quatd => (4, false),
        ValueType::Matrix2d => (2, true),
        ValueType::Matrix3d => (3, true),
        ValueType::Matrix4d => (4, true),
        _ => return None,
    })
}

/// One value of the storage type `ty`, from a literal that is not an
/// array.
pub(crate) fn element(
    value: &ast::Value<'_>,
    ty: ValueType,
    tokens: &TokenInterner,
) -> Result<Value, MetadataValueError> {
    let ty = ty.storage();
    if let Some((size, rows)) = shape(ty) {
        let ast::Value::Tuple(items) = value else {
            return Err(match value {
                ast::Value::Array(_) => MetadataValueError::ArrayForScalar,
                _ => MetadataValueError::NotEnoughValues,
            });
        };
        let fits = items.len() == size
            && items.iter().all(|item| match item {
                ast::Value::Tuple(row) => {
                    rows && row.len() == size
                        && !row.iter().any(|c| matches!(c, ast::Value::Tuple(_)))
                }
                _ => !rows,
            });
        if !fits {
            return Err(MetadataValueError::TupleDimensions);
        }
        return match dimensioned_value(items, ty.name(), "") {
            Ok(Some(value)) => Ok(value),
            _ => Err(MetadataValueError::Unconvertible { element: None }),
        };
    }
    match value {
        ast::Value::Tuple(_) => return Err(MetadataValueError::TupleForScalar),
        ast::Value::Array(_) => return Err(MetadataValueError::ArrayForScalar),
        _ => {}
    }
    let unconvertible = MetadataValueError::Unconvertible { element: None };
    if let Some(converted) = convert_scalar(value, ty.name()) {
        return converted.map_err(|_| unconvertible);
    }
    Ok(match (ty, value) {
        (ValueType::String, ast::Value::String(text)) => Value::String(Arc::from(&**text)),
        (ValueType::Token, ast::Value::String(text)) => Value::Token(tokens.intern(text)),
        (ValueType::Asset, ast::Value::String(text)) => Value::Asset(Arc::from(&**text)),
        (ValueType::Asset, ast::Value::Asset(text)) => Value::Asset(Arc::from(&**text)),
        (ValueType::PathExpression, ast::Value::String(text)) => {
            Value::PathExpression(Arc::from(&**text))
        }
        _ => return Err(unconvertible),
    })
}

/// The array of `ty` an array literal's `items` are, an error naming the
/// first item that is not one.
fn array(
    items: &[ast::Value<'_>],
    ty: ValueType,
    tokens: &TokenInterner,
) -> Result<Value, MetadataValueError> {
    let values = elements(items, ty, tokens)?;
    Ok(Value::array_with_element(
        values,
        Some(&ty.storage().zero()),
    ))
}

/// The values of `ty` an array literal's `items` are.
fn elements(
    items: &[ast::Value<'_>],
    ty: ValueType,
    tokens: &TokenInterner,
) -> Result<Vec<Value>, MetadataValueError> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            element(item, ty, tokens).map_err(|error| match error {
                MetadataValueError::Unconvertible { .. } => MetadataValueError::Unconvertible {
                    element: Some(index),
                },
                other => other,
            })
        })
        .collect()
}

/// The value of a list-op field's statement: its items, as `op` places
/// them; `None` is an explicit empty list.
fn list_op(
    value: &ast::MetadataValue<'_>,
    item: ListOpItem,
    op: ast::ListOpKind,
    tokens: &TokenInterner,
) -> Result<FieldValue, MetadataValueError> {
    let items: Vec<Value> = match value {
        ast::MetadataValue::None => Vec::new(),
        ast::MetadataValue::Value(ast::Value::Array(items)) => {
            elements(items, item.value_type(), tokens)?
        }
        _ => return Err(MetadataValueError::ScalarForArray),
    };
    fn place<T>(op: ast::ListOpKind, items: Vec<T>) -> Box<ListOp<T>> {
        let mut list = ListOp::default();
        match op {
            ast::ListOpKind::Explicit => list.explicit = Some(items),
            ast::ListOpKind::Prepend => list.prepend = items,
            ast::ListOpKind::Append => list.append = items,
            ast::ListOpKind::Add => list.add = items,
            ast::ListOpKind::Delete => list.delete = items,
            ast::ListOpKind::Reorder => list.reorder = items,
        }
        Box::new(list)
    }
    let values = items.into_iter();
    Ok(match item {
        ListOpItem::Token => FieldValue::TokenListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::Token(token) => Some(token),
                    _ => None,
                })
                .collect(),
        )),
        ListOpItem::String => FieldValue::StringListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::String(text) => Some(text),
                    _ => None,
                })
                .collect(),
        )),
        ListOpItem::Int => FieldValue::IntListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::Int(n) => Some(n),
                    _ => None,
                })
                .collect(),
        )),
        ListOpItem::UInt => FieldValue::UIntListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::UInt(n) => Some(n),
                    _ => None,
                })
                .collect(),
        )),
        ListOpItem::Int64 => FieldValue::Int64ListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::Int64(n) => Some(n),
                    _ => None,
                })
                .collect(),
        )),
        ListOpItem::UInt64 => FieldValue::UInt64ListOp(place(
            op,
            values
                .filter_map(|value| match value {
                    Value::UInt64(n) => Some(n),
                    _ => None,
                })
                .collect(),
        )),
    })
}

/// The value a metadata statement gives a field of `value_type`, as
/// OpenUSD's text parser converts it; `dictionary` converts its entries
/// with `dictionary`. A statement of a field that is not a list op takes
/// the value whatever its list-op keyword, as OpenUSD's does.
pub(crate) fn typed(
    value: &ast::MetadataValue<'_>,
    op: ast::ListOpKind,
    value_type: MetadataType,
    tokens: &TokenInterner,
    dictionary: &mut dyn FnMut(&ast::MetadataValue<'_>) -> Value,
) -> Result<FieldValue, MetadataValueError> {
    let value = match (value_type.storage(), value) {
        (MetadataType::ListOp(item), value) => return list_op(value, item, op, tokens),
        (MetadataType::Dictionary, ast::MetadataValue::Dictionary(_)) => dictionary(value),
        (MetadataType::Dictionary, ast::MetadataValue::Value(ast::Value::Dictionary(_))) => {
            dictionary(value)
        }
        (MetadataType::Dictionary, ast::MetadataValue::None) => {
            return Err(MetadataValueError::NoValue);
        }
        (MetadataType::Dictionary, _) => return Err(MetadataValueError::NotADictionary),
        (_, ast::MetadataValue::None) => return Err(MetadataValueError::NoValue),
        (_, ast::MetadataValue::Dictionary(_)) => {
            return Err(MetadataValueError::Unconvertible { element: None });
        }
        (MetadataType::Array(ty), ast::MetadataValue::Value(ast::Value::Array(items))) => {
            array(items, ty, tokens)?
        }
        (MetadataType::Array(_), _) => return Err(MetadataValueError::ScalarForArray),
        (MetadataType::Value(ty), ast::MetadataValue::Value(value)) => element(value, ty, tokens)?,
        (MetadataType::Value(ty), ast::MetadataValue::String(text)) => {
            element(&ast::Value::String(text.as_str().into()), ty, tokens)?
        }
    };
    Ok(FieldValue::Value(value))
}

/// The value of the metadata literal `literal` (USDA's text of a value,
/// `(1, 2, 3)` or `["a", "b"]`) as a field of `value_type` holds it, as
/// OpenUSD's text parser converts a metadata statement's value: for
/// reading a value written as USDA text, such as a plugin's default.
///
/// Spec: AOUSD Core §16.2 (USDA values), §7.4 (metadata).
///
/// # Errors
///
/// Why the literal is not a value of the type; [`MetadataValueError::Unconvertible`]
/// too for text that is no USDA value.
pub fn metadata_value(
    literal: &str,
    value_type: MetadataType,
    tokens: &TokenInterner,
) -> Result<FieldValue, MetadataValueError> {
    let source = format!("#usda 1.0\n(\n    value = {literal}\n)\n");
    let parsed = crate::parser::parse(&source);
    let entry = parsed
        .diagnostics
        .is_empty()
        .then(|| {
            parsed.layer.metadata.iter().find_map(|meta| match meta {
                ast::LayerMeta::Custom(entry) => Some(entry),
                _ => None,
            })
        })
        .flatten()
        .ok_or(MetadataValueError::Unconvertible { element: None })?;
    typed(
        &entry.value,
        ast::ListOpKind::Explicit,
        value_type,
        tokens,
        &mut |value| dictionary_value(value, tokens),
    )
}

/// A dictionary literal's value, its entries typed as they are written.
fn dictionary_value(value: &ast::MetadataValue<'_>, tokens: &TokenInterner) -> Value {
    let entries = match value {
        ast::MetadataValue::Dictionary(entries)
        | ast::MetadataValue::Value(ast::Value::Dictionary(entries)) => entries,
        _ => return Value::Dictionary(Vec::new()),
    };
    Value::Dictionary(
        entries
            .iter()
            .filter_map(|entry| {
                let ty = entry.type_name.and_then(ValueType::parse)?;
                let value = match ty {
                    (ty, false) => element(&entry.value, ty, tokens).ok()?,
                    (ty, true) => match &entry.value {
                        ast::Value::Array(items) => array(items, ty, tokens).ok()?,
                        _ => return None,
                    },
                };
                Some((Arc::from(&*entry.key), value))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(literal: &str, ty: &str) -> Result<FieldValue, MetadataValueError> {
        metadata_value(
            literal,
            MetadataType::parse(ty).unwrap(),
            &TokenInterner::default(),
        )
    }

    #[test]
    fn literals_convert_as_attribute_values_do() {
        assert_eq!(read("0", "bool"), Ok(FieldValue::Value(Value::Bool(false))));
        assert_eq!(
            read("\"yes\"", "bool"),
            Ok(FieldValue::Value(Value::Bool(true)))
        );
        assert_eq!(read("1", "float"), Ok(FieldValue::Value(Value::Float(1.0))));
        assert_eq!(read("2.5", "int"), Ok(FieldValue::Value(Value::Int(2))));
        assert_eq!(
            read("\"a.usda\"", "asset"),
            Ok(FieldValue::Value(Value::Asset("a.usda".into())))
        );
        assert_eq!(
            read("(1, 2, 3)", "color3f"),
            Ok(FieldValue::Value(Value::Vec3f([1.0, 2.0, 3.0])))
        );
        assert_eq!(
            read("None", "tokenlistop"),
            Ok(FieldValue::TokenListOp(Box::new(ListOp::explicit(
                Vec::new()
            ))))
        );
    }

    #[test]
    fn literals_of_another_shape_or_type_are_refused_as_openusd_refuses_them() {
        for (literal, ty, error) in [
            ("[1]", "bool", MetadataValueError::ArrayForScalar),
            ("1", "int[]", MetadataValueError::ScalarForArray),
            ("(1, 2)", "float", MetadataValueError::TupleForScalar),
            ("(1, 2)", "float3", MetadataValueError::TupleDimensions),
            ("1", "float3", MetadataValueError::NotEnoughValues),
            ("[(1, 2)]", "float3[]", MetadataValueError::TupleDimensions),
            ("None", "float", MetadataValueError::NoValue),
            ("1", "dictionary", MetadataValueError::NotADictionary),
            (
                "3",
                "token",
                MetadataValueError::Unconvertible { element: None },
            ),
            (
                "300",
                "uchar",
                MetadataValueError::Unconvertible { element: None },
            ),
            (
                "\"off\"",
                "bool",
                MetadataValueError::Unconvertible { element: None },
            ),
            (
                "[1, \"b\"]",
                "int[]",
                MetadataValueError::Unconvertible { element: Some(1) },
            ),
            (
                "[1]",
                "tokenlistop",
                MetadataValueError::Unconvertible { element: Some(0) },
            ),
        ] {
            assert_eq!(read(literal, ty), Err(error), "{ty} = {literal}");
        }
    }
}
