// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset path values authored as variable expressions, evaluated as
//! composition reads them.
//!
//! An `asset` value, alone, in an array or in a dictionary, may be a
//! variable expression (`` @`"./${NAME}.png"`@ ``). Composition evaluates
//! each one with the expression variables of the layer stack of the node
//! whose layer authors it, and the stage reads the path it evaluates to,
//! which, when relative, is anchored to that layer as any asset path is.
//! An expression that evaluates to an empty string or to no value, or
//! fails to evaluate, keeps its authored text: the path OpenUSD passes to
//! its resolver (`SdfAssetPath::GetAssetPath`), which resolves to nothing.
//!
//! Spec: AOUSD Core §6.2 (the `asset` type), §9 (asset resolution);
//! §7.6.1.7 reserves `expressionVariables`. OpenUSD: `_AssetPathToStage`
//! in `pxr/usd/usd/stage.cpp`, applied to every field value read through
//! `_FieldValueToStageXf`, and `SdfResolveAssetPaths` in
//! `pxr/usd/sdf/assetPath.cpp`, which evaluates with
//! `SdfVariableExpression::EvaluateTyped<std::string>`.

use alloc::sync::Arc;

use crate::{
    doc::{FieldValue, Value},
    prim_index::OpinionValue,
    property::PropertySpec,
    variable_expression::is_expression,
};

/// Whether `value` is or holds an asset path authored as a variable
/// expression, in arrays and dictionaries at any depth.
///
/// A value an adapter keeps encoded ([`Value::Deferred`]) is never one:
/// format adapters keep asset paths and dictionaries decoded.
pub(crate) fn holds_asset_expression(value: &Value) -> bool {
    match value {
        Value::Asset(path) => is_expression(path),
        Value::Array(items) => items.iter().any(holds_asset_expression),
        Value::Dictionary(entries) => entries
            .iter()
            .any(|(_, entry)| entry.as_value().is_some_and(holds_asset_expression)),
        _ => false,
    }
}

/// Whether an opinion's value holds an asset path expression: a metadata
/// field's value, or a property's default, time samples or metadata.
pub(crate) fn opinion_holds_asset_expression(value: &OpinionValue) -> bool {
    match value {
        OpinionValue::Field(FieldValue::Value(value)) => holds_asset_expression(value),
        OpinionValue::Field(_) => false,
        OpinionValue::Property(spec) => property_holds_asset_expression(spec),
    }
}

/// Whether a property's default, time samples or metadata hold an asset
/// path expression.
pub(crate) fn property_holds_asset_expression(spec: &PropertySpec) -> bool {
    spec.default.as_ref().is_some_and(holds_asset_expression)
        || spec
            .time_samples
            .get()
            .is_some_and(|samples| samples.iter().any(|(_, v)| holds_asset_expression(v)))
        || spec
            .metadata
            .iter()
            .any(|entry| entry.value.as_value().is_some_and(holds_asset_expression))
}

/// Replaces each asset path expression in `value` with the path
/// `evaluate` returns for it, leaving it as authored when that returns
/// `None`. Each element of an array and each dictionary entry is
/// evaluated on its own, as `SdfResolveAssetPaths` evaluates each asset
/// path of a span.
pub(crate) fn evaluate_asset_expressions(
    value: &mut Value,
    evaluate: &mut dyn FnMut(&str) -> Option<Arc<str>>,
) {
    match value {
        Value::Asset(path) => {
            if is_expression(path)
                && let Some(evaluated) = evaluate(path)
            {
                *path = evaluated;
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                evaluate_asset_expressions(item, evaluate);
            }
        }
        Value::Dictionary(entries) => {
            for (_, entry) in entries.iter_mut() {
                if let FieldValue::Value(item) = entry
                    && holds_asset_expression(item)
                {
                    evaluate_asset_expressions(item, evaluate);
                }
            }
        }
        _ => {}
    }
}

/// [`evaluate_asset_expressions`] over an opinion's value: a metadata
/// field's value, or a property's default, time samples and metadata.
pub(crate) fn evaluate_opinion_asset_expressions(
    value: &mut OpinionValue,
    evaluate: &mut dyn FnMut(&str) -> Option<Arc<str>>,
) {
    match value {
        OpinionValue::Field(FieldValue::Value(value)) => {
            evaluate_asset_expressions(value, evaluate);
        }
        OpinionValue::Field(_) => {}
        OpinionValue::Property(spec) => evaluate_property_asset_expressions(spec, evaluate),
    }
}

/// [`evaluate_asset_expressions`] over a property's default, time samples
/// and metadata; `spec` is copied only when it holds an expression.
pub(crate) fn evaluate_property_asset_expressions(
    spec: &mut Arc<PropertySpec>,
    evaluate: &mut dyn FnMut(&str) -> Option<Arc<str>>,
) {
    if !property_holds_asset_expression(spec) {
        return;
    }
    let spec = Arc::make_mut(spec);
    if let Some(default) = spec.default.as_mut()
        && holds_asset_expression(default)
    {
        evaluate_asset_expressions(default, evaluate);
    }
    if spec
        .time_samples
        .get()
        .is_some_and(|samples| samples.iter().any(|(_, v)| holds_asset_expression(v)))
        && let Some(samples) = spec.time_samples.get_mut()
    {
        for (_, sample) in samples {
            if holds_asset_expression(sample) {
                evaluate_asset_expressions(sample, evaluate);
            }
        }
    }
    if spec
        .metadata
        .iter()
        .any(|entry| entry.value.as_value().is_some_and(holds_asset_expression))
    {
        let mut metadata = spec.metadata.to_vec();
        for entry in &mut metadata {
            if let FieldValue::Value(item) = &mut entry.value
                && holds_asset_expression(item)
            {
                evaluate_asset_expressions(item, evaluate);
            }
        }
        spec.metadata = metadata.into();
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    #[test]
    fn evaluates_each_expression_and_keeps_what_evaluates_to_nothing() {
        let mut value = Value::Array(vec![
            Value::Asset("`\"./${NAME}.png\"`".into()),
            Value::Asset("`\"${TEX}\"`".into()),
            Value::Asset("./plain.png".into()),
        ]);
        let mut seen = Vec::new();
        evaluate_asset_expressions(&mut value, &mut |expression| {
            seen.push(alloc::string::String::from(expression));
            expression.contains("NAME").then(|| Arc::from("./wood.png"))
        });
        assert_eq!(
            value,
            Value::Array(vec![
                Value::Asset("./wood.png".into()),
                Value::Asset("`\"${TEX}\"`".into()),
                Value::Asset("./plain.png".into()),
            ])
        );
        assert_eq!(seen, ["`\"./${NAME}.png\"`", "`\"${TEX}\"`"]);
    }

    #[test]
    fn finds_expressions_in_nested_dictionaries() {
        let nested = Value::Dictionary(vec![(
            "l".into(),
            Value::Array(vec![Value::Asset("`\"${TEX}\"`".into())]).into(),
        )]);
        let value = Value::Dictionary(vec![("n".into(), nested.into())]);
        assert!(holds_asset_expression(&value));
        assert!(!holds_asset_expression(&Value::Asset("`".into())));
        assert!(!holds_asset_expression(&Value::String("`\"x\"`".into())));
    }
}
