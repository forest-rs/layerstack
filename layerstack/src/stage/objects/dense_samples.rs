// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::Attribute;
use crate::{Time, TimeSamples, Value, value_resolution::interpolate_samples};

/// An immutable snapshot of an attribute's dense numeric time samples.
///
/// The samples share the original sample storage. Evaluate with
/// [`Self::value_at`] for numeric time codes and use the attribute's ordinary
/// value read for default time. Capture a new snapshot after the source
/// changes, or when switching stages. See [`Attribute::dense_numeric_samples`]
/// for the source eligibility rules.
///
/// Spec: AOUSD Core §12.3.2 (time samples), §12.5 (interpolation).
#[derive(Clone, Debug)]
pub struct DenseNumericSamples(TimeSamples);

impl DenseNumericSamples {
    /// Evaluate this captured source using the query's interpolation mode.
    /// Default time returns `None`.
    #[must_use]
    pub fn value_at(&self, time: Time) -> Option<Value> {
        let Time::At {
            code,
            interpolation,
        } = time
        else {
            return None;
        };
        interpolate_samples(self.0.get()?, code, interpolation)
    }
}

impl Attribute<'_> {
    /// Capture a numeric-time source that can be evaluated independently of
    /// composition. When this returns `None`, use [`Self::get`] to evaluate
    /// the attribute. Default-time and provenance queries use [`Self::get`]
    /// in either case.
    ///
    /// Currently eligible sources have one authored opinion, identity layer
    /// offset, no clip dependency, and a nonempty stream of finite sample
    /// times with numeric scalar, vector, quaternion, or matrix values.
    /// Array declarations and non-numeric defaults use ordinary resolution.
    /// The captured source is immutable. Use a fresh capture after edits that
    /// can change this property or move it in the composed namespace.
    ///
    /// Spec: AOUSD Core §12.3.2 (time samples and layer offsets),
    /// §12.3.4 (value clips), §12.5 (interpolation).
    #[must_use]
    pub fn dense_numeric_samples(self) -> Option<DenseNumericSamples> {
        let property = self.property;
        if property.stage.clips.may_supply_samples(property.path) {
            return None;
        }
        let opinions = property.opinions();
        if opinions.len() != 1 {
            return None;
        }
        let opinion = opinions.first()?;
        if !opinion.layer_offset().is_identity() {
            return None;
        }
        let spec = opinion.value().as_property()?;
        if spec
            .type_name
            .as_ref()
            .is_some_and(crate::PropertyType::is_array)
            || !spec.default.as_ref().is_none_or(is_plain_numeric)
        {
            return None;
        }
        let samples = spec.time_samples.get()?;
        (!samples.is_empty()
            && samples
                .iter()
                .all(|(time, value)| time.is_finite() && is_plain_numeric(value)))
        .then(|| DenseNumericSamples(spec.time_samples.clone()))
    }
}

fn is_plain_numeric(value: &Value) -> bool {
    matches!(
        value,
        Value::UChar(_)
            | Value::Int(_)
            | Value::UInt(_)
            | Value::Int64(_)
            | Value::UInt64(_)
            | Value::Half(_)
            | Value::Float(_)
            | Value::Double(_)
            | Value::Vec2d(_)
            | Value::Vec3d(_)
            | Value::Vec4d(_)
            | Value::Vec2f(_)
            | Value::Vec3f(_)
            | Value::Vec4f(_)
            | Value::Vec2h(_)
            | Value::Vec3h(_)
            | Value::Vec4h(_)
            | Value::Vec2i(_)
            | Value::Vec3i(_)
            | Value::Vec4i(_)
            | Value::Quatd(_)
            | Value::Quatf(_)
            | Value::Quath(_)
            | Value::Matrix2d(_)
            | Value::Matrix3d(_)
            | Value::Matrix4d(_)
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        EditTarget, InMemoryStore, Layer, LayerId, LayerOffset, LiveStage, PrimSpec, PropertyPath,
        PropertySpec, PropertyType, Stage, StageOptions, SublayerEntry, Transaction,
    };

    #[test]
    fn dense_samples_are_exact_and_snapshot_owned() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Signals");
        let token = store.tokens().intern("gain");
        let prop = PropertyPath::new(path, token);
        let id = LayerId(1);
        let attribute = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
            .with_default(Value::Double(100.))
            .with_time_samples(vec![
                (2., Value::Double(4.)),
                (3., Value::Double(5.)),
                (3.000_000_1, Value::Double(7.)),
            ]);
        let mut layer = Layer::new(id);
        layer.insert_prim(path, PrimSpec::def().with_property(token, attribute));
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, id, StageOptions::default());
        let attr = live.stage().view(&store).attribute(prop).unwrap();
        let source = attr.dense_numeric_samples().unwrap();
        assert_eq!(source.value_at(Time::Default), None);
        assert_eq!(attr.get(Time::Default).unwrap().value, Value::Double(100.));
        for time in [
            Time::at(-3.),
            Time::held(2.5),
            Time::at(2.5),
            Time::at(3.),
            Time::at(3.000_000_05),
            Time::at(6.),
        ] {
            assert_eq!(
                source.value_at(time),
                attr.get(time).map(|value| value.value)
            );
        }
        let mut edit = Transaction::new();
        edit.set_time_sample(
            EditTarget::for_layer(id).property(prop),
            2.,
            Value::Double(-4.),
        );
        live.apply(&mut store, &edit).unwrap();
        let attr = live.stage().view(&store).attribute(prop).unwrap();
        assert_eq!(source.value_at(Time::at(2.)), Some(Value::Double(4.)));
        assert_eq!(
            attr.dense_numeric_samples().unwrap().value_at(Time::at(2.)),
            Some(Value::Double(-4.)),
        );
    }

    #[test]
    fn offset_and_blocked_samples_require_ordinary_resolution() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Signals");
        let plain = store.tokens().intern("plain");
        let blocked = store.tokens().intern("blocked");
        let attr = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap());
        let mut source = Layer::new(LayerId(2));
        source.insert_prim(
            path,
            PrimSpec::def()
                .with_property(
                    plain,
                    attr.clone()
                        .with_time_samples(vec![(3., Value::Double(9.))]),
                )
                .with_property(
                    blocked,
                    attr.with_time_samples(vec![(3., Value::Double(2.)), (5., Value::Blocked)]),
                ),
        );
        let mut root = Layer::new(LayerId(1));
        root.sublayers_mut().push(SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 5.,
                scale: 2.,
            },
        ));
        store.insert_layer(source);
        store.insert_layer(root);
        let unoffset = Stage::compose(&mut store, LayerId(2), StageOptions::default());
        let offset = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let attribute = |stage: &Stage, token| {
            let attr = stage
                .view(&store)
                .attribute(PropertyPath::new(path, token))
                .unwrap();
            (
                attr.dense_numeric_samples(),
                attr.get(Time::at(11.)).map(|r| r.value),
            )
        };
        assert!(attribute(&unoffset, plain).0.is_some());
        let (binding, resolved) = attribute(&offset, plain);
        assert!(binding.is_none());
        assert_eq!(resolved, Some(Value::Double(9.)));
        let (binding, resolved) = attribute(&unoffset, blocked);
        assert!(binding.is_none());
        assert_eq!(resolved, None);
    }
}
