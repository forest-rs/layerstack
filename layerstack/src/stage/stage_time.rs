// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values read through layer offsets: `timecode` and `duration` values in
//! stage time.
//!
//! A layer offset maps a layer's times into the stage's, and a `timecode`
//! value is a time, so value resolution maps it with its opinion's offset
//! before composing, as it maps sample times. A `duration` value is a length
//! of time, so the offset scales it but does not shift it. OpenUSD 26.11 does
//! the same for every value it resolves (`Usd_ApplyLayerOffsetToValue`,
//! `pxr/usd/usd/valueUtils.h`, and `SdfLayerOffset::operator*(GfDuration)`,
//! `pxr/usd/sdf/layerOffset.cpp`): `timecode` and `duration` values, alone,
//! in arrays, in sparse array edits and in dictionaries.
//!
//! Spec: AOUSD Core §12.3.2.1 (a layer's time `t` is stage time
//! `t * scale + offset`).

use alloc::{borrow::Cow, vec::Vec};

use crate::{
    TimeSamples,
    array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand},
    doc::{FieldEntry, FieldValue, LayerOffset, Value},
    prim_index::{Opinion, OpinionRef, OpinionValue, Opinions},
    property::PropertySpec,
};

/// Maps a layer time to stage time through `offset`: the inverse of
/// [`LayerOffset::map_time`].
pub(crate) fn to_stage_time(offset: LayerOffset, time: f64) -> f64 {
    time * offset.scale + offset.offset
}

/// Whether `value` is a time-valued scalar kind: a `timecode` or a
/// `duration`.
fn is_time_valued(value: &Value) -> bool {
    matches!(value, Value::TimeCode(_) | Value::Duration(_))
}

/// Rewrites every leaf `leaf` maps of a native array of time-valued
/// elements, each wrapped by `wrap`; `None` when none maps.
fn map_time_array(
    items: &[f64],
    wrap: fn(f64) -> Value,
    leaf: &mut dyn FnMut(&Value) -> Option<Value>,
) -> Option<Value> {
    let (index, first) = items
        .iter()
        .enumerate()
        .find_map(|(index, time)| leaf(&wrap(*time)).map(|value| (index, value)))?;
    let prefix = items[..index].iter().map(|time| wrap(*time));
    let tail = items[index + 1..]
        .iter()
        .map(|time| leaf(&wrap(*time)).unwrap_or(wrap(*time)));
    Some(Value::array_from_iter(
        prefix.chain(core::iter::once(first)).chain(tail),
        Some(&wrap(0.0)),
    ))
}

/// Rewrites every leaf value `leaf` maps, through arrays, dictionaries and
/// the literals of sparse array edits; `None` when none maps.
pub(crate) fn map_leaves(
    value: &Value,
    leaf: &mut dyn FnMut(&Value) -> Option<Value>,
) -> Option<Value> {
    match value {
        Value::Array(items) => map_all(items, leaf).map(Value::Array),
        Value::TypedArray(crate::TypedArray::TimeCode(items)) => {
            map_time_array(items, Value::TimeCode, leaf)
        }
        Value::TypedArray(crate::TypedArray::Duration(items)) => {
            map_time_array(items, Value::Duration, leaf)
        }
        Value::TypedArray(array @ crate::TypedArray::Deferred(_))
            if is_time_valued(&array.element_kind()) =>
        {
            if let Some(mapped) = leaf(value) {
                return Some(mapped);
            }
            let native = array.try_materialize().ok()?;
            map_leaves(&Value::TypedArray(native.clone()), leaf)
        }
        Value::TypedArray(_) => None,
        // An array of another element kind holds no time code; anything
        // else is mapped as it decodes.
        Value::Deferred(deferred) => {
            if deferred
                .element_kind()
                .is_some_and(|kind| !is_time_valued(&kind))
            {
                return None;
            }
            map_leaves(deferred.try_materialize().ok()?, leaf)
        }
        Value::Dictionary(entries) => {
            let mut changed = false;
            let mapped = entries
                .iter()
                .map(
                    |(key, item)| match item.as_value().and_then(|item| map_leaves(item, leaf)) {
                        Some(item) => {
                            changed = true;
                            (key.clone(), item.into())
                        }
                        None => (key.clone(), item.clone()),
                    },
                )
                .collect();
            changed.then_some(Value::Dictionary(mapped))
        }
        Value::ArrayEdit(_) | Value::TypedArrayEdit(_) => {
            let edit = value.array_edit_ref()?;
            // OpenUSD `Usd_ApplyLayerOffsetToValue`: retime explicit
            // literals only. An implicit resize fill remains type-default zero.
            let mut changed = false;
            let mut operand = |operand: &ArrayEditOperand| match operand {
                ArrayEditOperand::Literal(item) => match map_leaves(item, leaf) {
                    Some(item) => {
                        changed = true;
                        ArrayEditOperand::Literal(item)
                    }
                    None => operand.clone(),
                },
                ArrayEditOperand::CopyFrom(_) => operand.clone(),
            };
            let ops = edit
                .ops
                .iter()
                .map(|op| match op {
                    ArrayEditOp::Write { src, index } => ArrayEditOp::Write {
                        src: operand(src),
                        index: *index,
                    },
                    ArrayEditOp::Insert { src, index } => ArrayEditOp::Insert {
                        src: operand(src),
                        index: *index,
                    },
                    ArrayEditOp::MinSizeFill { len, fill } => ArrayEditOp::MinSizeFill {
                        len: *len,
                        fill: match operand(&ArrayEditOperand::Literal(fill.clone())) {
                            ArrayEditOperand::Literal(fill) => fill,
                            ArrayEditOperand::CopyFrom(_) => fill.clone(),
                        },
                    },
                    ArrayEditOp::ResizeFill { len, fill } => ArrayEditOp::ResizeFill {
                        len: *len,
                        fill: match operand(&ArrayEditOperand::Literal(fill.clone())) {
                            ArrayEditOperand::Literal(fill) => fill,
                            ArrayEditOperand::CopyFrom(_) => fill.clone(),
                        },
                    },
                    other => other.clone(),
                })
                .collect();
            changed.then(|| {
                let edit = ArrayEdit { ops };
                match value.array_edit_type() {
                    Some(ty) => Value::typed_array_edit(edit, ty.clone()),
                    None => Value::ArrayEdit(edit),
                }
            })
        }
        other => leaf(other),
    }
}

fn map_all(items: &[Value], leaf: &mut dyn FnMut(&Value) -> Option<Value>) -> Option<Vec<Value>> {
    let mut out: Option<Vec<Value>> = None;
    for (i, item) in items.iter().enumerate() {
        if let Some(mapped) = map_leaves(item, leaf) {
            out.get_or_insert_with(|| items[..i].to_vec()).push(mapped);
        } else if let Some(out) = &mut out {
            out.push(item.clone());
        }
    }
    out
}

/// `value` with its `timecode` and `duration` values in stage time; `None`
/// when it holds none or `offset` is the identity.
pub(crate) fn retime_value(value: &Value, offset: LayerOffset) -> Option<Value> {
    retime_value_with(value, offset, offset.scale)
}

/// `value` with its `timecode` values mapped through `offset` and its
/// `duration` values multiplied by `duration_scale`; `None` when nothing
/// changes. Value clips scale durations by their time mapping segment's
/// slope even where the segment holds the clip still, so the two can differ
/// (OpenUSD 26.11 `clip.cpp` `_GetTimeScale`).
pub(crate) fn retime_value_with(
    value: &Value,
    offset: LayerOffset,
    duration_scale: f64,
) -> Option<Value> {
    if offset.is_identity() && duration_scale == 1.0 {
        return None;
    }
    map_leaves(value, &mut |leaf| retime_leaf(leaf, offset, duration_scale))
}

fn retime_leaf(value: &Value, offset: LayerOffset, duration_scale: f64) -> Option<Value> {
    match value {
        Value::TimeCode(time) => Some(Value::TimeCode(to_stage_time(offset, *time))),
        // A duration scales but does not shift (OpenUSD 26.11
        // `SdfLayerOffset::operator*(GfDuration)`, `layerOffset.cpp:72`).
        Value::Duration(length) => Some(Value::Duration(length * duration_scale)),
        Value::TypedArray(crate::TypedArray::Deferred(source)) => {
            source.retimed(offset).map(Value::TypedArray)
        }
        _ => None,
    }
}

/// The field with every leaf `leaf` maps rewritten; `None` when none maps.
fn map_field(
    value: &FieldValue,
    leaf: &mut dyn FnMut(&Value) -> Option<Value>,
) -> Option<FieldValue> {
    match value {
        FieldValue::Value(value) => map_leaves(value, leaf).map(FieldValue::Value),
        _ => None,
    }
}

/// The property spec with every leaf `leaf` maps rewritten in its default,
/// sample values and metadata; `None` when none maps.
fn map_property(
    spec: &PropertySpec,
    leaf: &mut dyn FnMut(&Value) -> Option<Value>,
) -> Option<PropertySpec> {
    let default = spec
        .default
        .as_ref()
        .and_then(|value| map_leaves(value, leaf));
    let samples = spec.time_samples.get().and_then(|samples| {
        // Copied only from the first sample that maps.
        let mut out: Option<Vec<(f64, Value)>> = None;
        for (i, (time, value)) in samples.iter().enumerate() {
            match (map_leaves(value, leaf), &mut out) {
                (Some(mapped), Some(out)) => out.push((*time, mapped)),
                (Some(mapped), None) => {
                    let mut started = samples[..i].to_vec();
                    started.push((*time, mapped));
                    out = Some(started);
                }
                (None, Some(out)) => out.push((*time, value.clone())),
                (None, None) => {}
            }
        }
        out
    });
    let mut metadata_changed = false;
    let metadata: Vec<FieldEntry> = spec
        .metadata
        .iter()
        .map(|entry| match map_field(&entry.value, leaf) {
            Some(value) => {
                metadata_changed = true;
                FieldEntry {
                    name: entry.name,
                    value,
                }
            }
            None => entry.clone(),
        })
        .collect();
    if default.is_none() && samples.is_none() && !metadata_changed {
        return None;
    }
    let mut spec = spec.clone();
    if default.is_some() {
        spec.default = default;
    }
    if let Some(samples) = samples {
        spec.time_samples = samples.into();
    }
    spec.metadata = metadata.into();
    Some(spec)
}

/// The opinion with every leaf value `leaf` maps rewritten, in its default,
/// samples and metadata; `None` when none maps.
pub(crate) fn map_opinion(
    opinion: &Opinion,
    leaf: &mut dyn FnMut(&Value) -> Option<Value>,
) -> Option<Opinion> {
    let value = match &opinion.value {
        OpinionValue::Field(field) => OpinionValue::Field(map_field(field, leaf)?),
        OpinionValue::Property(spec) => OpinionValue::from(map_property(spec, leaf)?),
    };
    Some(Opinion {
        value,
        ..opinion.clone()
    })
}

/// Whether `value` is or holds a `timecode` or `duration`, in arrays,
/// dictionaries and the literals of sparse array edits. Reads without copying; decodes a
/// deferred value only when its source cannot tell.
fn holds_time_value(value: &Value) -> bool {
    match value {
        Value::TimeCode(_) | Value::Duration(_) => true,
        Value::Array(items) => items.iter().any(holds_time_value),
        Value::TypedArray(array) => is_time_valued(&array.element_kind()),
        Value::Dictionary(entries) => entries
            .iter()
            .any(|(_, v)| v.as_value().is_some_and(holds_time_value)),
        // Decodes only a value whose kind its source does not know.
        Value::Deferred(deferred) => match deferred.element_kind() {
            Some(kind) => holds_time_value(&kind),
            None => deferred.try_materialize().is_ok_and(holds_time_value),
        },
        Value::ArrayEdit(_) | Value::TypedArrayEdit(_) => {
            value.array_edit_ref().is_some_and(|edit| {
                edit.ops.iter().any(|op| match op {
                    ArrayEditOp::Write { src, .. } | ArrayEditOp::Insert { src, .. } => {
                        matches!(src, ArrayEditOperand::Literal(v) if holds_time_value(v))
                    }
                    ArrayEditOp::MinSizeFill { fill, .. }
                    | ArrayEditOp::ResizeFill { fill, .. } => holds_time_value(fill),
                    _ => false,
                })
            })
        }
        _ => false,
    }
}

/// Whether reading `opinion` through its offset changes actual `timecode`
/// or `duration` values. Declarations are hints, not proof of the stored kind: low-level
/// source data can contain time codes under a different declared type.
/// Native arrays of other kinds answer this check without visiting elements.
// Inlined so the identity test, which settles most opinions, runs inside
// `opinions_in_stage_time`'s loop instead of as a call per opinion.
#[inline]
fn needs_retiming(opinion: OpinionRef<'_>) -> bool {
    !opinion.layer_offset().is_identity() && holds_time_values(opinion)
}

/// Whether `opinion` holds a `timecode` or `duration` value anywhere.
fn holds_time_values(opinion: OpinionRef<'_>) -> bool {
    let field = |value: &FieldValue| matches!(value, FieldValue::Value(v) if holds_time_value(v));
    match opinion.value() {
        OpinionValue::Field(value) => field(value),
        OpinionValue::Property(spec) => {
            spec.metadata.iter().any(|entry| field(&entry.value))
                || spec.default.as_ref().is_some_and(holds_time_value)
                || spec
                    .time_samples
                    .get()
                    .into_iter()
                    .flatten()
                    .any(|(_, v)| holds_time_value(v))
        }
    }
}

/// The opinion's value in stage time; `None` when nothing changes.
fn retime_opinion(opinion: OpinionRef<'_>) -> Option<Opinion> {
    let offset = opinion.layer_offset();
    map_opinion(opinion.opinion(), &mut |leaf| {
        retime_leaf(leaf, offset, offset.scale)
    })
}

/// `opinions` with every `timecode` and `duration` value in stage time,
/// borrowed, with nothing copied, when none needs mapping. Actual storage
/// kinds determine whether values need mapping; native arrays of other kinds
/// are not traversed.
/// The opinions keep their sources: `opinions.with_records` reads them.
///
/// Spec: AOUSD Core §12.3.2.1.
pub(crate) fn opinions_in_stage_time(opinions: Opinions<'_>) -> Cow<'_, [Opinion]> {
    let Some(first) = opinions.iter().position(needs_retiming) else {
        return Cow::Borrowed(opinions.records());
    };
    let mut out = opinions.records()[..first].to_vec();
    out.extend(opinions.iter().skip(first).map(|opinion| {
        if needs_retiming(opinion) {
            retime_opinion(opinion).unwrap_or_else(|| opinion.opinion().clone())
        } else {
            opinion.opinion().clone()
        }
    }));
    Cow::Owned(out)
}

/// Animation blocks suppress only weaker animation. Keep opinion positions
/// and authored defaults, preserving provenance and sparse composition.
/// OpenUSD `UsdStage::_GetResolveInfoWithClipsImpl`; Core §12.3.
pub(crate) fn animation_opinions(opinions: &[Opinion]) -> Cow<'_, [Opinion]> {
    let Some(block) = opinions.iter().position(|op| op.value.blocks_animation()) else {
        return Cow::Borrowed(opinions);
    };
    let mut out = opinions.to_vec();
    for opinion in &mut out[block + 1..] {
        if let OpinionValue::Property(spec) = &mut opinion.value {
            let spec = alloc::sync::Arc::make_mut(spec);
            spec.time_samples = TimeSamples::none();
            spec.spline = None;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Opinions with nothing to retime are borrowed, never copied, however
    /// many samples they hold.
    #[test]
    fn opinions_without_timecodes_are_borrowed() {
        use crate::{
            doc::LayerId,
            path::{Path, PathInterner},
            prim_index::{OpinionKey, OpinionSource, SourceId},
            prim_index_graph::NodeId,
            property::PropertyType,
            spec_path::SpecPath,
        };
        let paths = PathInterner::default();
        let path = paths.intern(Path::root());
        let source = OpinionSource {
            key: OpinionKey {
                node: NodeId::ROOT,
                layer_strength: 0,
                layer_id: LayerId(1),
                lookup_path: path,
                spec_path: SpecPath::from_prim_path(path),
            },
            layer_offset: LayerOffset {
                offset: 5.0,
                scale: 2.0,
            },
        };
        let sources = [source];
        let opinion = |spec: PropertySpec| Opinion {
            source: SourceId::at(0),
            field: crate::interner::TokenInterner::default().intern("a"),
            value: OpinionValue::from(spec),
        };
        let samples: Vec<(f64, Value)> = (0..10_000)
            .map(|i| (f64::from(i), Value::Double(0.5)))
            .collect();
        let doubles = PropertyType::parse("double").unwrap();
        let typed = [opinion(
            PropertySpec::typed_attribute(doubles.clone()).with_time_samples(samples.clone()),
        )];
        let view = |records| Opinions::new(records, &sources);
        assert!(matches!(
            opinions_in_stage_time(view(&typed)),
            Cow::Borrowed(_)
        ));
        // An untyped over also retains its actual double values.
        let untyped = [opinion(
            PropertySpec::attribute().with_time_samples(samples),
        )];
        assert!(matches!(
            opinions_in_stage_time(view(&untyped)),
            Cow::Borrowed(_)
        ));
        // A `timecode` attribute is mapped.
        let timecode = PropertyType::parse("timecode").unwrap();
        let timecodes = [opinion(
            PropertySpec::typed_attribute(timecode).with_default(Value::TimeCode(1.0)),
        )];
        let Cow::Owned(mapped) = opinions_in_stage_time(view(&timecodes)) else {
            panic!("mapped");
        };
        assert_eq!(mapped[0].value.default_value(), Some(&Value::TimeCode(7.0)));
    }

    #[test]
    fn sparse_timecode_offsets_map_literals_without_changing_implicit_fills() {
        let ty = crate::PropertyType::parse("timecode[]").unwrap();
        let resize = Value::typed_array_edit(
            ArrayEdit {
                ops: vec![ArrayEditOp::Resize { len: 2 }],
            },
            ty.clone(),
        );
        let offset = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        assert_eq!(retime_value(&resize, offset), None);
        let explicit = Value::typed_array_edit(
            ArrayEdit {
                ops: vec![
                    ArrayEditOp::Resize { len: 2 },
                    ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::TimeCode(0.0)),
                        index: crate::ArrayIndex::End,
                    },
                ],
            },
            ty.clone(),
        );
        let mapped = retime_value(&explicit, offset).unwrap();
        assert_eq!(mapped.array_edit_type(), Some(&ty));
        assert_eq!(
            crate::array_edit::apply_to_array(
                mapped.array_edit_ref().unwrap(),
                &[],
                mapped.array_edit_type()
            ),
            vec![
                Value::TimeCode(0.0),
                Value::TimeCode(0.0),
                Value::TimeCode(10.0)
            ]
        );
    }

    #[test]
    fn timecodes_move_into_stage_time_wherever_they_are() {
        let offset = LayerOffset {
            offset: 2.0,
            scale: 2.0,
        };
        assert_eq!(offset.map_time(to_stage_time(offset, 10.0)), 10.0);
        let value = Value::Dictionary(vec![
            ("cue".into(), Value::TimeCode(5.0).into()),
            (
                "marks".into(),
                Value::Array(vec![Value::TimeCode(1.0), Value::TimeCode(3.0)]).into(),
            ),
            ("count".into(), Value::Int(5).into()),
        ]);
        assert_eq!(
            retime_value(&value, offset),
            Some(Value::Dictionary(vec![
                ("cue".into(), Value::TimeCode(12.0).into()),
                (
                    "marks".into(),
                    Value::Array(vec![Value::TimeCode(4.0), Value::TimeCode(8.0)]).into(),
                ),
                ("count".into(), Value::Int(5).into()),
            ]))
        );
        assert_eq!(retime_value(&Value::Int(5), offset), None);
        assert_eq!(
            retime_value(&Value::TimeCode(5.0), LayerOffset::IDENTITY),
            None
        );
        let edit = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::TimeCode(1.0)),
                index: crate::array_edit::ArrayIndex::Position(0),
            }],
        });
        assert_eq!(
            retime_value(&edit, offset),
            Some(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Insert {
                    src: ArrayEditOperand::Literal(Value::TimeCode(4.0)),
                    index: crate::array_edit::ArrayIndex::Position(0),
                }],
            }))
        );
    }

    #[test]
    fn durations_scale_without_shifting() {
        let offset = LayerOffset {
            offset: 2.0,
            scale: 2.0,
        };
        let value = Value::Dictionary(vec![
            ("span".into(), Value::Duration(5.0).into()),
            ("cue".into(), Value::TimeCode(5.0).into()),
        ]);
        assert_eq!(
            retime_value(&value, offset),
            Some(Value::Dictionary(vec![
                ("span".into(), Value::Duration(10.0).into()),
                ("cue".into(), Value::TimeCode(12.0).into()),
            ]))
        );
        let spans = Value::array_from_iter(
            [Value::Duration(1.0), Value::Duration(-3.0)],
            Some(&Value::Duration(0.0)),
        );
        assert_eq!(
            retime_value(&spans, offset),
            Some(Value::array_from_iter(
                [Value::Duration(2.0), Value::Duration(-6.0)],
                Some(&Value::Duration(0.0)),
            ))
        );
        // A shift alone leaves durations as they are.
        let shift = LayerOffset {
            offset: 7.0,
            scale: 1.0,
        };
        assert_eq!(
            retime_value(&spans, shift),
            Some(spans.clone()),
            "mapped, though to the same lengths"
        );
    }
}
