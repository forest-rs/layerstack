// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Internal value-resolution helpers shared by [`crate::stage`].
//!
//! This module owns sparse-family detection and strong-over-weak folding for
//! attribute value families that require composed sparse opinions.

use crate::{
    doc::{FieldValue, InterpolationType, Value},
    prim_index::Opinion,
    property::PropertyType,
};

/// Internal query modes for sparse value resolution.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SparseQuery<'a> {
    /// Resolve default/fallback opinions without a specific sample time.
    Default {
        /// Optional weakest dense seed, such as a schema fallback.
        fallback: Option<&'a Value>,
    },
    /// Resolve a time-varying query at a specific time.
    AtTime {
        /// Query time.
        time: f64,
        /// Interpolation mode.
        interp: InterpolationType,
    },
}

/// Result of attempting sparse-family resolution.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SparseResolveResult {
    /// No sparse-composable family applied to this query.
    NotApplicable,
    /// A blocking opinion suppressed the value.
    Blocked,
    /// Sparse composition produced a dense resolved value.
    Resolved(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SparseValueFamily {
    Array,
}

#[derive(Clone, Debug)]
enum OpinionFoldStep {
    Blocked,
    Member(Value),
    Skip,
    Stop,
}

/// Attempts sparse-family resolution for the given opinion chain.
///
/// This keeps sparse-family detection and strong-over-weak folding out of
/// [`crate::stage::Stage`]. If no sparse family applies, returns
/// [`SparseResolveResult::NotApplicable`].
pub(crate) fn resolve_sparse_value(
    opinions: &[Opinion],
    query: SparseQuery<'_>,
    property_type: Option<&PropertyType>,
) -> SparseResolveResult {
    let Some(family) = SparseValueFamily::for_query(opinions, query) else {
        return SparseResolveResult::NotApplicable;
    };
    family.resolve(opinions, query, property_type)
}

impl SparseValueFamily {
    fn for_query(opinions: &[Opinion], query: SparseQuery<'_>) -> Option<Self> {
        match query {
            SparseQuery::Default { fallback } => {
                if let Some(opinion) = opinions.first() {
                    Self::for_default_field_value(&opinion.value)
                } else {
                    fallback.and_then(Self::for_value)
                }
            }
            SparseQuery::AtTime { .. } => opinions.iter().find_map(Self::for_time_opinion),
        }
    }

    fn for_default_field_value(value: &FieldValue) -> Option<Self> {
        match value {
            FieldValue::Value(value) => Self::for_value(value),
            _ => None,
        }
    }

    fn for_time_opinion(opinion: &Opinion) -> Option<Self> {
        match &opinion.value {
            FieldValue::Value(value) => Self::for_value(value),
            FieldValue::TimeSamples(samples) => {
                samples.iter().find_map(|(_, value)| Self::for_value(value))
            }
            _ => None,
        }
    }

    fn for_value(value: &Value) -> Option<Self> {
        match value {
            Value::Array(_) | Value::ArrayEdit(_) => Some(Self::Array),
            _ => None,
        }
    }

    fn resolve(
        self,
        opinions: &[Opinion],
        query: SparseQuery<'_>,
        property_type: Option<&PropertyType>,
    ) -> SparseResolveResult {
        match query {
            SparseQuery::Default { fallback } => {
                self.resolve_default(opinions, fallback, property_type)
            }
            SparseQuery::AtTime { time, interp } => {
                self.resolve_at_time(opinions, time, interp, property_type)
            }
        }
    }

    fn resolve_default(
        self,
        opinions: &[Opinion],
        fallback: Option<&Value>,
        property_type: Option<&PropertyType>,
    ) -> SparseResolveResult {
        self.fold_sparse_members(
            opinions,
            fallback,
            property_type,
            |opinion| match &opinion.value {
                FieldValue::Value(Value::Blocked) => OpinionFoldStep::Blocked,
                FieldValue::Value(value) if self.matches(value) => {
                    OpinionFoldStep::Member(value.clone())
                }
                FieldValue::Value(_) => OpinionFoldStep::Stop,
                _ => OpinionFoldStep::Skip,
            },
        )
    }

    fn resolve_at_time(
        self,
        opinions: &[Opinion],
        time: f64,
        interp: InterpolationType,
        property_type: Option<&PropertyType>,
    ) -> SparseResolveResult {
        self.fold_sparse_members(opinions, None, property_type, |opinion| {
            match &opinion.value {
                FieldValue::Value(Value::Blocked) => OpinionFoldStep::Blocked,
                FieldValue::Value(value) if self.matches(value) => {
                    OpinionFoldStep::Member(value.clone())
                }
                FieldValue::TimeSamples(samples) => {
                    let mapped_time = opinion.layer_offset.map_time(time);
                    match interpolate_samples(samples, mapped_time, interp) {
                        Some(value) if self.matches(&value) => OpinionFoldStep::Member(value),
                        _ => OpinionFoldStep::Skip,
                    }
                }
                _ => OpinionFoldStep::Skip,
            }
        })
    }

    fn fold_sparse_members(
        self,
        opinions: &[Opinion],
        fallback: Option<&Value>,
        property_type: Option<&PropertyType>,
        mut next_step: impl FnMut(&Opinion) -> OpinionFoldStep,
    ) -> SparseResolveResult {
        let mut acc: Option<Value> = None;

        for opinion in opinions {
            match next_step(opinion) {
                OpinionFoldStep::Blocked => return SparseResolveResult::Blocked,
                OpinionFoldStep::Member(value) => {
                    acc = match acc {
                        Some(strong) => self.compose_over(&strong, value, property_type),
                        None => Some(value),
                    };
                    if let Some(value) = acc.as_ref()
                        && self.is_dense(value)
                    {
                        break;
                    }
                }
                OpinionFoldStep::Skip => {}
                OpinionFoldStep::Stop => break,
            }
        }

        if let Some(fallback) = fallback {
            let needs_fallback = acc.as_ref().is_none_or(|value| !self.is_dense(value));
            if needs_fallback {
                acc = match acc {
                    Some(strong) => self.compose_over(&strong, fallback.clone(), property_type),
                    None => Some(fallback.clone()),
                };
            }
        }

        match self.materialize(acc, property_type) {
            Some(value) => SparseResolveResult::Resolved(value),
            None => SparseResolveResult::NotApplicable,
        }
    }

    fn matches(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Self::Array, Value::Array(_) | Value::ArrayEdit(_))
        )
    }

    fn is_dense(self, value: &Value) -> bool {
        matches!((self, value), (Self::Array, Value::Array(_)))
    }

    fn compose_over(
        self,
        strong: &Value,
        weak: Value,
        property_type: Option<&PropertyType>,
    ) -> Option<Value> {
        match (self, strong, weak) {
            (Self::Array, Value::Array(items), _) => Some(Value::Array(items.clone())),
            (Self::Array, Value::ArrayEdit(edit), Value::Array(items)) => {
                Some(Value::Array(edit.compose_over_array(&items, property_type)))
            }
            (Self::Array, Value::ArrayEdit(edit), Value::ArrayEdit(weak_edit)) => {
                Some(Value::ArrayEdit(edit.compose_over(&weak_edit)))
            }
            _ => None,
        }
    }

    fn materialize(
        self,
        value: Option<Value>,
        property_type: Option<&PropertyType>,
    ) -> Option<Value> {
        match (self, value) {
            (Self::Array, Some(Value::ArrayEdit(edit))) => {
                Some(Value::Array(edit.compose_over_array(&[], property_type)))
            }
            (_, other) => other,
        }
    }
}

/// Interpolates a value from sorted time samples at the given time.
///
/// Spec: AOUSD Core §12.5 (interpolation methods).
pub(crate) fn interpolate_samples(
    samples: &[(f64, Value)],
    time: f64,
    interp: InterpolationType,
) -> Option<Value> {
    if samples.is_empty() {
        return None;
    }

    match samples
        .binary_search_by(|(t, _)| t.partial_cmp(&time).unwrap_or(core::cmp::Ordering::Equal))
    {
        Ok(idx) => Some(samples[idx].1.clone()),
        Err(idx) => {
            if idx == 0 {
                Some(samples[0].1.clone())
            } else if idx >= samples.len() {
                Some(samples.last().expect("non-empty samples").1.clone())
            } else {
                match interp {
                    InterpolationType::Held => Some(samples[idx - 1].1.clone()),
                    InterpolationType::Linear => lerp_values(
                        &samples[idx - 1].1,
                        &samples[idx].1,
                        samples[idx - 1].0,
                        samples[idx].0,
                        time,
                    ),
                }
            }
        }
    }
}

/// Linear interpolation between two values. Falls back to held for
/// non-numeric types.
fn lerp_values(a: &Value, b: &Value, t_a: f64, t_b: f64, t: f64) -> Option<Value> {
    let alpha = if (t_b - t_a).abs() < f64::EPSILON {
        0.0
    } else {
        (t - t_a) / (t_b - t_a)
    };

    match (a, b) {
        (Value::Double(va), Value::Double(vb)) => Some(Value::Double(va + (vb - va) * alpha)),
        #[allow(
            clippy::cast_possible_truncation,
            reason = "f64→f32 intentional for single-precision lerp"
        )]
        (Value::Float(va), Value::Float(vb)) => {
            let alpha_f = alpha as f32;
            Some(Value::Float(va + (vb - va) * alpha_f))
        }
        (Value::TimeCode(va), Value::TimeCode(vb)) => Some(Value::TimeCode(va + (vb - va) * alpha)),
        (Value::Int64(va), Value::Int64(vb)) => {
            let result = *va as f64 + (*vb as f64 - *va as f64) * alpha;
            #[allow(clippy::cast_possible_truncation, reason = "clamped by f64 range")]
            let i = lerp_round(result) as i64;
            Some(Value::Int64(i))
        }
        (Value::Int(va), Value::Int(vb)) => {
            let result = *va as f64 + (*vb as f64 - *va as f64) * alpha;
            #[allow(clippy::cast_possible_truncation, reason = "clamped by f64 range")]
            let i = lerp_round(result) as i32;
            Some(Value::Int(i))
        }
        _ => Some(a.clone()),
    }
}

/// Round-to-nearest for lerp results (no_std-compatible).
fn lerp_round(v: f64) -> f64 {
    if v >= 0.0 { v + 0.5 } else { v - 0.5 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LayerId, LayerOffset, OpinionKey, TokenId,
        array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex},
        interner::TokenInterner,
        path::{Path, PathId, PathInterner},
        prim_index::{ArcKind, Opinion},
    };
    use alloc::sync::Arc;
    use alloc::vec;

    fn array_value(values: &[i32]) -> Value {
        Value::Array(values.iter().copied().map(Value::Int).collect())
    }

    fn int_array_type() -> PropertyType {
        PropertyType::new(Arc::<str>::from("int"), true, Value::Int(0))
    }

    fn test_key(layer: LayerId, lookup_path: PathId) -> OpinionKey {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path =
            crate::spec_path::SpecPath::parse("/A", &mut tokens, &mut paths).expect("spec path");
        OpinionKey {
            is_local: true,
            arc_kind: ArcKind::Local,
            nested_arc_kind: None,
            namespace_depth: 1,
            authored: true,
            arc_list_index: 0,
            layer_strength: 0,
            layer_id: layer,
            lookup_path,
            spec_path,
        }
    }

    fn test_ids() -> (PathId, TokenId) {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("valid path"));
        let field = tokens.intern("x");
        (spec_path, field)
    }

    fn array_opinion(
        spec_path: PathId,
        field: TokenId,
        value: FieldValue,
        layer_strength: u16,
    ) -> Opinion {
        Opinion {
            key: OpinionKey {
                layer_strength,
                ..test_key(LayerId(1), spec_path)
            },
            field,
            value,
            layer_offset: LayerOffset::IDENTITY,
        }
    }

    #[test]
    fn dense_value_terminates_sparse_fold() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Write {
                        src: ArrayEditOperand::Literal(Value::Int(9)),
                        index: ArrayIndex::Position(0),
                    }],
                })),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::Int(7)),
                        index: ArrayIndex::End,
                    }],
                })),
                2,
            ),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "once the fold reaches a dense array, weaker sparse opinions must not affect the result"
        );
    }

    #[test]
    fn fallback_seeds_sparse_default_resolution() {
        let (spec_path, field) = test_ids();
        let opinions = vec![array_opinion(
            spec_path,
            field,
            FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(Value::Int(9)),
                    index: ArrayIndex::Position(0),
                }],
            })),
            0,
        )];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default {
                fallback: Some(&array_value(&[1, 2])),
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "schema fallback should act as the weakest dense seed for sparse array resolution"
        );
    }

    #[test]
    fn held_time_samples_use_same_sparse_fold_as_defaults() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![
                    (
                        0.0,
                        Value::ArrayEdit(ArrayEdit {
                            ops: vec![ArrayEditOp::Write {
                                src: ArrayEditOperand::Literal(Value::Int(9)),
                                index: ArrayIndex::Position(0),
                            }],
                        }),
                    ),
                    (2.0, Value::ArrayEdit(ArrayEdit::default())),
                ]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::AtTime {
                time: 1.0,
                interp: InterpolationType::Held,
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "time-sampled sparse opinions should fold over weaker dense values using the same family logic"
        );
    }

    #[test]
    fn blocking_value_still_blocks_sparse_resolution() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 0),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::AtTime {
                time: 1.0,
                interp: InterpolationType::Held,
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Blocked,
            "blocking opinions must suppress weaker sparse-family values"
        );
    }

    #[test]
    fn sparse_over_sparse_fold_matches_grouped_composition() {
        let (spec_path, field) = test_ids();
        let strong = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(8)),
                index: ArrayIndex::Position(1),
            }],
        };
        let weak = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        };

        let full_chain = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(strong.clone())),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(weak.clone())),
                1,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let grouped_chain = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(strong.compose_over(&weak))),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let full = resolve_sparse_value(
            &full_chain,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        let grouped = resolve_sparse_value(
            &grouped_chain,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            full, grouped,
            "sparse family folding should preserve associative grouped composition"
        );
    }
}
