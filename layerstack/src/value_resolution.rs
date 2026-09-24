// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Internal value-resolution helpers shared by [`crate::stage`].
//!
//! This module owns sparse-family detection and chain construction for
//! attribute value families that require composed sparse opinions. The
//! strong-over-weak fold itself is delegated to [`opinionated`]'s family
//! kernel via [`resolve_family_chain`]: [`ArrayFamily`] expresses the sparse
//! array-edit semantics over [`OpinionFamily`] and samples each authored
//! opinion as the kernel reaches it (the kernel itself is time-agnostic).

use alloc::vec::Vec;

use opinionated::{
    FamilyMember, FamilyResolution, IgnoreReason, OpinionFamily, OpinionKind, resolve_family_chain,
};

use crate::{
    array_edit::ArrayEdit,
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

/// How [`ArrayFamily`] reads one authored opinion into a family member.
///
/// The kernel is time-agnostic, so sampling happens inside
/// [`ArrayFamily::classify`]: an opinion is only sampled when the fold actually
/// reaches it.
#[derive(Clone, Copy, Debug)]
enum Sampling {
    /// Read default values; time samples do not participate.
    Default,
    /// Read default values and time samples at `time`.
    AtTime {
        /// Query time, before each opinion's layer offset is applied.
        time: f64,
        /// Interpolation mode.
        interp: InterpolationType,
    },
}

/// The sparse array-edit family, expressed over [`opinionated`]'s kernel.
///
/// The family folds authored [`Opinion`]s directly. [`Value::Array`] is the
/// dense member, [`Value::ArrayEdit`] the sparse edit, and [`Value::Blocked`]
/// the block, whether authored as a default or as a time sample. The seed is
/// the schema fallback when one is present (materialized over the empty array
/// if it is itself an edit), otherwise the empty array. The carried
/// [`PropertyType`] lets edits perform typed materialization
/// (`minsize`/`resize` fill values) during apply.
///
/// Because the kernel pulls opinions lazily and stops at the first dense
/// member or block, opinions hidden behind them are never sampled or cloned,
/// and each participating value is cloned (or sampled) exactly once.
#[derive(Clone, Copy, Debug)]
struct ArrayFamily<'a> {
    /// Typed property metadata for edit materialization.
    property_type: Option<&'a PropertyType>,
    /// Optional weakest dense seed, such as a schema fallback.
    fallback: Option<&'a Value>,
    /// How opinions are read into members.
    sampling: Sampling,
}

impl ArrayFamily<'_> {
    fn classify_borrowed(value: &Value) -> FamilyMember<Vec<Value>, ArrayEdit> {
        match value {
            Value::Array(items) => FamilyMember::Dense(items.clone()),
            Value::ArrayEdit(edit) => FamilyMember::Sparse(edit.clone()),
            Value::Blocked => FamilyMember::Block,
            _ => Self::foreign(),
        }
    }

    fn classify_owned(value: Value) -> FamilyMember<Vec<Value>, ArrayEdit> {
        match value {
            Value::Array(items) => FamilyMember::Dense(items),
            Value::ArrayEdit(edit) => FamilyMember::Sparse(edit),
            // A sampled block blocks exactly like an authored default block.
            //
            // Spec: AOUSD Core §12.3.6 (blocked attributes: individual time
            // samples can be blocked).
            Value::Blocked => FamilyMember::Block,
            _ => Self::foreign(),
        }
    }

    fn foreign() -> FamilyMember<Vec<Value>, ArrayEdit> {
        // `OpinionKind` cannot name domain families, so the reason degrades
        // to a set-over-set mismatch. The reason is never observed: the lean
        // kernel entry point records no events.
        FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
            resolved: OpinionKind::Set,
            ignored: OpinionKind::Set,
        })
    }
}

impl OpinionFamily<Opinion> for ArrayFamily<'_> {
    type Value = Vec<Value>;
    type Edit = ArrayEdit;

    fn classify(&self, opinion: &Opinion) -> FamilyMember<Self::Value, Self::Edit> {
        match (&opinion.value, self.sampling) {
            (FieldValue::Value(value), _) => Self::classify_borrowed(value),
            (FieldValue::TimeSamples(samples), Sampling::AtTime { time, interp }) => {
                // Spec: AOUSD Core §12.3.2.1 (layer offset and scale).
                let mapped_time = opinion.layer_offset.map_time(time);
                match interpolate_samples(samples, mapped_time, interp) {
                    Some(value) => Self::classify_owned(value),
                    None => Self::foreign(),
                }
            }
            _ => Self::foreign(),
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        let mut value = base;
        edit.apply_in_place(&mut value, self.property_type);
        value
    }

    fn seed(&self) -> Self::Value {
        match self.fallback {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::ArrayEdit(edit)) => edit.compose_over_array(&[], self.property_type),
            _ => Vec::new(),
        }
    }
}

/// Attempts sparse-family resolution for the given opinion chain.
///
/// This keeps sparse-family detection and strong-over-weak folding out of
/// [`crate::stage::Stage`]. If no sparse family applies, returns
/// [`SparseResolveResult::NotApplicable`].
///
/// Block semantics follow AOUSD Core §12.3.6 (blocked attributes) and the
/// sparse-array-edits proposal (`OpenUSD-proposals/proposals/sparse-array-edits`,
/// "Generalized Value Resolution with Composed Sparse Opinions" and "Value
/// Resolution"). The fold runs strongest to weakest and stops at the first
/// dense value or block. A block discards every weaker authored opinion; when
/// nothing stronger contributed, the result is [`SparseResolveResult::Blocked`]
/// and callers fall back to the schema fallback. Sparse edits stronger than
/// the block still compose over the weakest dense value that survives it: the
/// fallback seed if one is supplied, otherwise the empty array, since the
/// proposal requires a resolved array value to always be dense.
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
        match self {
            Self::Array => match query {
                SparseQuery::Default { fallback } => {
                    let family = ArrayFamily {
                        property_type,
                        fallback,
                        sampling: Sampling::Default,
                    };
                    // A stronger dense default outside the family ends the
                    // chain: weaker family members stay hidden behind it, and
                    // any accumulated edits materialize over the seed.
                    fold_array_chain(
                        &family,
                        opinions
                            .iter()
                            .take_while(|opinion| !is_foreign_default(&opinion.value)),
                    )
                }
                SparseQuery::AtTime { time, interp } => {
                    let family = ArrayFamily {
                        property_type,
                        fallback: None,
                        sampling: Sampling::AtTime { time, interp },
                    };
                    fold_array_chain(&family, opinions.iter())
                }
            },
        }
    }
}

/// Returns `true` for a dense default value outside the array family.
fn is_foreign_default(value: &FieldValue) -> bool {
    matches!(
        value,
        FieldValue::Value(value)
            if !matches!(value, Value::Array(_) | Value::ArrayEdit(_) | Value::Blocked)
    )
}

/// Folds a strongest-to-weakest opinion chain through [`opinionated`]'s
/// family kernel.
///
/// `opinions` is consumed lazily: the kernel stops pulling at the first dense
/// member or block, so weaker opinions are neither sampled nor cloned.
fn fold_array_chain<'o>(
    family: &ArrayFamily<'_>,
    opinions: impl Iterator<Item = &'o Opinion>,
) -> SparseResolveResult {
    match resolve_family_chain(family, opinions.map(|opinion| (opinion, &()))) {
        FamilyResolution::Resolved { value, .. } => {
            SparseResolveResult::Resolved(Value::Array(value))
        }
        FamilyResolution::Blocked { .. } => SparseResolveResult::Blocked,
        // No opinion contributed: an array-family fallback still resolves on
        // its own; otherwise the family does not apply to this chain.
        FamilyResolution::Absent if family.fallback.is_some() => {
            SparseResolveResult::Resolved(Value::Array(family.seed()))
        }
        FamilyResolution::Absent => SparseResolveResult::NotApplicable,
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
    fn stronger_edits_materialize_over_seed_when_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::Int(7)),
                        index: ArrayIndex::End,
                    }],
                })),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[7])),
            "a block hides weaker opinions, but stronger sparse edits still materialize over the seed"
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

    fn write_edit(value: i32, index: i64) -> Value {
        Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(value)),
                index: ArrayIndex::Position(index),
            }],
        })
    }

    /// Folds `opinions` through the production adapter and reports how many
    /// opinions the kernel pulled (and therefore classified or sampled).
    fn fold_counting(
        family: &ArrayFamily<'_>,
        opinions: &[Opinion],
    ) -> (SparseResolveResult, usize) {
        let mut visited = 0;
        let resolved = fold_array_chain(family, opinions.iter().inspect(|_| visited += 1));
        (resolved, visited)
    }

    #[test]
    fn opinions_hidden_by_a_dense_value_are_not_sampled() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![(0.0, write_edit(9, 0))]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![(0.0, array_value(&[5, 5, 5]))]),
                2,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[3])), 3),
        ];
        let property_type = int_array_type();
        let family = ArrayFamily {
            property_type: Some(&property_type),
            fallback: None,
            sampling: Sampling::AtTime {
                time: 0.0,
                interp: InterpolationType::Held,
            },
        };

        let (resolved, visited) = fold_counting(&family, &opinions);
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2]))
        );
        assert_eq!(
            visited, 2,
            "the fold must stop at the first dense value without sampling weaker opinions"
        );
    }

    #[test]
    fn opinions_hidden_by_a_block_are_not_visited() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(write_edit(9, 0)), 0),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
            array_opinion(spec_path, field, FieldValue::Value(write_edit(7, 1)), 3),
        ];
        let property_type = int_array_type();
        let family = ArrayFamily {
            property_type: Some(&property_type),
            fallback: None,
            sampling: Sampling::Default,
        };

        let (_, visited) = fold_counting(&family, &opinions);
        assert_eq!(
            visited, 2,
            "the fold must stop at the first block without visiting weaker opinions"
        );
    }

    fn resolve_at(
        opinions: &[Opinion],
        time: f64,
        interp: InterpolationType,
    ) -> SparseResolveResult {
        resolve_sparse_value(
            opinions,
            SparseQuery::AtTime { time, interp },
            Some(&int_array_type()),
        )
    }

    #[test]
    fn sampled_block_blocks_weaker_dense_array() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![(0.0, Value::Blocked)]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[42])), 1),
        ];

        for time in [-1.0, 0.0, 5.0] {
            for interp in [InterpolationType::Held, InterpolationType::Linear] {
                assert_eq!(
                    resolve_at(&opinions, time, interp),
                    SparseResolveResult::Blocked,
                    "a sampled block must hide the weaker array at t={time} ({interp:?})"
                );
            }
        }
    }

    #[test]
    fn sampled_block_applies_only_where_it_is_the_held_sample() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![
                    (0.0, write_edit(9, 0)),
                    (2.0, Value::Blocked),
                    (4.0, array_value(&[7])),
                ]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let held = InterpolationType::Held;
        assert_eq!(
            resolve_at(&opinions, 1.0, held),
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "held edit sample before the block composes over the weaker array"
        );
        assert_eq!(
            resolve_at(&opinions, 2.0, held),
            SparseResolveResult::Blocked,
            "exact block sample"
        );
        assert_eq!(
            resolve_at(&opinions, 3.0, held),
            SparseResolveResult::Blocked,
            "held block sample"
        );
        assert_eq!(
            resolve_at(&opinions, 3.0, InterpolationType::Linear),
            SparseResolveResult::Blocked,
            "arrays do not interpolate, so a block holds under linear interpolation too"
        );
        assert_eq!(
            resolve_at(&opinions, 4.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "exact dense sample after the block"
        );
    }

    #[test]
    fn layer_offset_maps_query_time_before_sampling_a_block() {
        let (spec_path, field) = test_ids();
        let mut strong = array_opinion(
            spec_path,
            field,
            FieldValue::TimeSamples(vec![(0.0, array_value(&[1])), (1.0, Value::Blocked)]),
            0,
        );
        // Stage time 11 maps to layer time 1, the block sample.
        strong.layer_offset = LayerOffset {
            offset: 10.0,
            scale: 1.0,
        };
        let opinions = vec![
            strong,
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[42])), 1),
        ];

        assert_eq!(
            resolve_at(&opinions, 10.0, InterpolationType::Held),
            SparseResolveResult::Resolved(array_value(&[1]))
        );
        assert_eq!(
            resolve_at(&opinions, 11.0, InterpolationType::Held),
            SparseResolveResult::Blocked
        );
    }

    #[test]
    fn stronger_edits_materialize_over_fallback_seed_when_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let append_seven = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        });
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(append_seven), 0),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default {
                fallback: Some(&array_value(&[5, 6])),
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[5, 6, 7])),
            "the fallback survives the block and seeds the stronger edit; [1, 2] does not"
        );
    }

    #[test]
    fn stronger_sampled_edit_materializes_over_seed_when_sampled_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let append_seven = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        });
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![(0.0, append_seven)]),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                FieldValue::TimeSamples(vec![(0.0, Value::Blocked), (2.0, array_value(&[3]))]),
                1,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let held = InterpolationType::Held;
        assert_eq!(
            resolve_at(&opinions, 0.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "exact block sample: the stronger edit materializes over the empty seed"
        );
        assert_eq!(
            resolve_at(&opinions, 1.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "held block sample: the stronger edit materializes over the empty seed"
        );
        assert_eq!(
            resolve_at(&opinions, 2.0, held),
            SparseResolveResult::Resolved(array_value(&[3, 7])),
            "once the dense sample takes over, the edit composes over it"
        );
    }
}
