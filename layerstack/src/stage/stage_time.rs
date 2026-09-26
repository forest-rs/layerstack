// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values read through layer offsets: `timecode` values in stage time.
//!
//! A layer offset maps a layer's times into the stage's, and a `timecode`
//! value is a time, so value resolution maps it with its opinion's offset
//! before composing, as it maps sample times. OpenUSD does the same for
//! every value it resolves (`Usd_ApplyLayerOffsetToValue`,
//! `pxr/usd/usd/valueUtils.h`): `timecode` values, alone, in arrays, in
//! sparse array edits and in dictionaries.
//!
//! Spec: AOUSD Core §12.3.2.1 (a layer's time `t` is stage time
//! `t * scale + offset`).

use alloc::{borrow::Cow, vec::Vec};

use crate::{
    array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand},
    doc::{FieldEntry, FieldValue, LayerOffset, Value},
    prim_index::{Opinion, OpinionValue},
    property::PropertySpec,
    property::PropertyType,
};

/// Maps a layer time to stage time through `offset`: the inverse of
/// [`LayerOffset::map_time`].
pub(crate) fn to_stage_time(offset: LayerOffset, time: f64) -> f64 {
    time * offset.scale + offset.offset
}

/// Rewrites every leaf value `leaf` maps, through arrays, dictionaries and
/// the literals of sparse array edits; `None` when none maps.
pub(crate) fn map_leaves(
    value: &Value,
    leaf: &mut impl FnMut(&Value) -> Option<Value>,
) -> Option<Value> {
    match value {
        Value::Array(items) => map_all(items, leaf).map(Value::Array),
        Value::Dictionary(entries) => {
            let mut changed = false;
            let mapped = entries
                .iter()
                .map(|(key, item)| match map_leaves(item, leaf) {
                    Some(item) => {
                        changed = true;
                        (key.clone(), item)
                    }
                    None => (key.clone(), item.clone()),
                })
                .collect();
            changed.then_some(Value::Dictionary(mapped))
        }
        Value::ArrayEdit(edit) => {
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
            changed.then_some(Value::ArrayEdit(ArrayEdit { ops }))
        }
        other => leaf(other),
    }
}

fn map_all(items: &[Value], leaf: &mut impl FnMut(&Value) -> Option<Value>) -> Option<Vec<Value>> {
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

/// `value` with its `timecode` values in stage time; `None` when it holds
/// none or `offset` is the identity.
pub(crate) fn retime_value(value: &Value, offset: LayerOffset) -> Option<Value> {
    if offset.is_identity() {
        return None;
    }
    map_leaves(value, &mut |leaf| match leaf {
        Value::TimeCode(time) => Some(Value::TimeCode(to_stage_time(offset, *time))),
        _ => None,
    })
}

/// The field with every leaf `leaf` maps rewritten; `None` when none maps.
fn map_field(
    value: &FieldValue,
    leaf: &mut impl FnMut(&Value) -> Option<Value>,
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
    leaf: &mut impl FnMut(&Value) -> Option<Value>,
) -> Option<PropertySpec> {
    let default = spec
        .default
        .as_ref()
        .and_then(|value| map_leaves(value, leaf));
    let samples = spec.time_samples.as_ref().and_then(|samples| {
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
    if samples.is_some() {
        spec.time_samples = samples;
    }
    spec.metadata = metadata;
    Some(spec)
}

/// The opinion with every leaf value `leaf` maps rewritten, in its default,
/// samples and metadata; `None` when none maps.
pub(crate) fn map_opinion(
    opinion: &Opinion,
    leaf: &mut impl FnMut(&Value) -> Option<Value>,
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

/// Whether `value` is or holds a `timecode`, in arrays, dictionaries and
/// the literals of sparse array edits. Reads without copying.
fn holds_timecode(value: &Value) -> bool {
    match value {
        Value::TimeCode(_) => true,
        Value::Array(items) => items.iter().any(holds_timecode),
        Value::Dictionary(entries) => entries.iter().any(|(_, v)| holds_timecode(v)),
        Value::ArrayEdit(edit) => edit.ops.iter().any(|op| match op {
            ArrayEditOp::Write { src, .. } | ArrayEditOp::Insert { src, .. } => {
                matches!(src, ArrayEditOperand::Literal(v) if holds_timecode(v))
            }
            ArrayEditOp::MinSizeFill { fill, .. } | ArrayEditOp::ResizeFill { fill, .. } => {
                holds_timecode(fill)
            }
            _ => false,
        }),
        _ => false,
    }
}

/// Whether reading `opinion` through its offset changes it: it is read
/// through an offset and holds a `timecode` value. An attribute's default
/// and samples are read only when its type is `timecode` (or unknown), so
/// the samples of any other attribute are never visited.
fn needs_retiming(opinion: &Opinion, property_type: Option<&PropertyType>) -> bool {
    if opinion.layer_offset.is_identity() {
        return false;
    }
    let field = |value: &FieldValue| matches!(value, FieldValue::Value(v) if holds_timecode(v));
    match &opinion.value {
        OpinionValue::Field(value) => field(value),
        OpinionValue::Property(spec) => {
            let values_may = spec
                .type_name
                .as_ref()
                .or(property_type)
                .is_none_or(|t| &*t.type_name == "timecode");
            spec.metadata.iter().any(|entry| field(&entry.value))
                || (values_may
                    && (spec.default.as_ref().is_some_and(holds_timecode)
                        || spec
                            .time_samples
                            .iter()
                            .flatten()
                            .any(|(_, v)| holds_timecode(v))))
        }
    }
}

/// The opinion's value in stage time; `None` when nothing changes.
fn retime_opinion(opinion: &Opinion) -> Option<Opinion> {
    let offset = opinion.layer_offset;
    map_opinion(opinion, &mut |leaf| match leaf {
        Value::TimeCode(time) => Some(Value::TimeCode(to_stage_time(offset, *time))),
        _ => None,
    })
}

/// `opinions` with every `timecode` value in stage time, borrowed, with
/// nothing copied, when none needs mapping. `property_type` is the
/// composed type of the property the opinions are of, if any: the values
/// of an attribute of another type than `timecode` are not visited.
///
/// Spec: AOUSD Core §12.3.2.1.
pub(crate) fn opinions_in_stage_time<'o>(
    opinions: &'o [Opinion],
    property_type: Option<&PropertyType>,
) -> Cow<'o, [Opinion]> {
    let Some(first) = opinions
        .iter()
        .position(|opinion| needs_retiming(opinion, property_type))
    else {
        return Cow::Borrowed(opinions);
    };
    let mut out = opinions[..first].to_vec();
    out.extend(opinions[first..].iter().map(|opinion| {
        if needs_retiming(opinion, property_type) {
            retime_opinion(opinion).unwrap_or_else(|| opinion.clone())
        } else {
            opinion.clone()
        }
    }));
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
            prim_index::OpinionKey,
            prim_index_graph::NodeId,
            property::PropertyType,
            spec_path::SpecPath,
        };
        let mut paths = PathInterner::default();
        let path = paths.intern(Path::root());
        let key = OpinionKey {
            node: NodeId::ROOT,
            layer_strength: 0,
            layer_id: LayerId(1),
            lookup_path: path,
            spec_path: SpecPath::from_prim_path(path, &paths),
        };
        let opinion = |spec: PropertySpec| Opinion {
            key: key.clone(),
            field: crate::interner::TokenInterner::default().intern("a"),
            value: OpinionValue::from(spec),
            layer_offset: LayerOffset {
                offset: 5.0,
                scale: 2.0,
            },
        };
        let samples: Vec<(f64, Value)> = (0..10_000)
            .map(|i| (f64::from(i), Value::Double(0.5)))
            .collect();
        let doubles = PropertyType::new("double", false, Value::Double(0.0));
        let typed = [opinion(
            PropertySpec::typed_attribute(doubles.clone()).with_time_samples(samples.clone()),
        )];
        assert!(matches!(
            opinions_in_stage_time(&typed, None),
            Cow::Borrowed(_)
        ));
        // An untyped over is known to be a double from the composed type.
        let untyped = [opinion(
            PropertySpec::attribute().with_time_samples(samples),
        )];
        assert!(matches!(
            opinions_in_stage_time(&untyped, Some(&doubles)),
            Cow::Borrowed(_)
        ));
        // A `timecode` attribute is mapped.
        let timecode = PropertyType::new("timecode", false, Value::TimeCode(0.0));
        let timecodes = [opinion(
            PropertySpec::typed_attribute(timecode).with_default(Value::TimeCode(1.0)),
        )];
        let Cow::Owned(mapped) = opinions_in_stage_time(&timecodes, None) else {
            panic!("mapped");
        };
        assert_eq!(mapped[0].value.default_value(), Some(&Value::TimeCode(7.0)));
    }

    #[test]
    fn timecodes_move_into_stage_time_wherever_they_are() {
        let offset = LayerOffset {
            offset: 2.0,
            scale: 2.0,
        };
        assert_eq!(offset.map_time(to_stage_time(offset, 10.0)), 10.0);
        let value = Value::Dictionary(vec![
            ("cue".into(), Value::TimeCode(5.0)),
            (
                "marks".into(),
                Value::Array(vec![Value::TimeCode(1.0), Value::TimeCode(3.0)]),
            ),
            ("count".into(), Value::Int(5)),
        ]);
        assert_eq!(
            retime_value(&value, offset),
            Some(Value::Dictionary(vec![
                ("cue".into(), Value::TimeCode(12.0)),
                (
                    "marks".into(),
                    Value::Array(vec![Value::TimeCode(4.0), Value::TimeCode(8.0)]),
                ),
                ("count".into(), Value::Int(5)),
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
}
