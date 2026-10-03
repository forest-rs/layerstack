// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse attribute authoring through ordinary mapped transactions.
//!
//! This module owns constant-run suppression; [`crate::edit::Address`] owns
//! namespace/time mapping and [`Transaction`] owns validation and undo. Values
//! are compared to the first value in each run, preventing tolerance drift.
//! ```
//! use layerstack::{InMemoryStore, LayerId, Value, edit::{EditTarget, Transaction}};
//! use layerstack::sparse_writer::SparseAttributeWriter;
//! let mut store = InMemoryStore::default();
//! let property = store.property_path("/Model.weight");
//! let target = EditTarget::for_layer(LayerId(1));
//! // The host supplies the existing composed default or schema fallback.
//! let mut writer = SparseAttributeWriter::new(target.property(property), Some(Value::Float(0.)));
//! let mut edits = Transaction::new();
//! writer.set_default(&mut edits, Value::Float(0.))?; // suppressed
//! writer.set_time_sample(&mut edits, 1., Value::Float(0.))?;
//! writer.set_time_sample(&mut edits, 2., Value::Float(1.))?; // writes 1:0 and 2:1
//! // Apply edits to the store/live stage after declaring the layer and property.
//! # Ok::<(), layerstack::sparse_writer::SparseWriterError>(())
//! ```
//!
//! AOUSD Core §7.6.4.2, §12.3–12.5; OpenUSD `UsdUtilsSparseAttrValueWriter`.

use crate::{
    Value,
    edit::{Address, Transaction},
};

/// An invalid ordering or nonfinite time supplied to a sparse writer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SparseWriterError {
    /// Defaults cannot be changed after the first numeric sample.
    DefaultAfterSamples,
    /// Sample times must be finite.
    NonfiniteTime,
    /// Samples must arrive in strictly increasing stage-time order.
    NonIncreasingTime {
        /// Previous submitted time, including suppressed samples.
        previous: f64,
        /// Rejected time.
        requested: f64,
    },
}
impl core::fmt::Display for SparseWriterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid sparse sample: {self:?}")
    }
}
impl core::error::Error for SparseWriterError {}

/// Authors one attribute with redundant samples omitted.
///
/// Supply its effective default (including schema fallback) when constructing
/// the writer. A default equal to that value produces no edit. Numeric samples
/// preserve both ends of constant runs when a later value changes, so linear
/// interpolation does not start changing prematurely. No final flush is needed:
/// the final run extrapolates constantly from its first authored sample.
///
/// The writer records **queued** edits. Keep using it only if those transactions
/// are committed in order. Clone before staging a transaction if it may be
/// discarded, restoring that clone on failure. Recreate after external edits,
/// target changes or changes to the fallback. Property creation is the caller's
/// responsibility; ordinary transaction validation checks declarations/types.
#[derive(Clone, Debug)]
pub struct SparseAttributeWriter {
    address: Address,
    previous_value: Option<Value>,
    previous_time: Option<f64>,
    previous_written: bool,
}
impl SparseAttributeWriter {
    /// Starts a writer at an explicit mapped property address and effective
    /// default. This queues no edits and reads no scene state.
    #[must_use]
    pub fn new(address: Address, existing_default: Option<Value>) -> Self {
        Self {
            address,
            previous_value: existing_default,
            previous_time: None,
            previous_written: true,
        }
    }
    /// Queues a default only when different from the effective default.
    /// Returns the number of appended edits (zero or one). Invalid ordering
    /// leaves both the transaction and writer unchanged.
    pub fn set_default(
        &mut self,
        transaction: &mut Transaction,
        value: Value,
    ) -> Result<usize, SparseWriterError> {
        if self.previous_time.is_some() {
            return Err(SparseWriterError::DefaultAfterSamples);
        }
        let changed = self
            .previous_value
            .as_ref()
            .is_none_or(|v| !sparse_values_close(v, &value));
        if changed {
            transaction.set_default(self.address.clone(), value.clone());
            self.previous_value = Some(value);
        }
        Ok(usize::from(changed))
    }
    /// Queues a finite, strictly increasing numeric sample in address time
    /// (stage time for a mapped address, layer time for a spec address).
    /// Returns the number of appended edits: zero for a repeated value, one for
    /// a change, or two when the preceding constant run needs its final endpoint.
    /// Invalid times leave both the writer and transaction unchanged.
    pub fn set_time_sample(
        &mut self,
        transaction: &mut Transaction,
        time: f64,
        value: Value,
    ) -> Result<usize, SparseWriterError> {
        if !time.is_finite() {
            return Err(SparseWriterError::NonfiniteTime);
        }
        if let Some(previous) = self.previous_time
            && time <= previous
        {
            return Err(SparseWriterError::NonIncreasingTime {
                previous,
                requested: time,
            });
        }
        let mut written = 0;
        if self
            .previous_value
            .as_ref()
            .is_some_and(|v| sparse_values_close(v, &value))
        {
            self.previous_written = false;
        } else {
            if !self.previous_written
                && let (Some(previous), Some(previous_time)) =
                    (&self.previous_value, self.previous_time)
            {
                transaction.set_time_sample(self.address.clone(), previous_time, previous.clone());
                written += 1;
            }
            transaction.set_time_sample(self.address.clone(), time, value.clone());
            written += 1;
            self.previous_value = Some(value);
            self.previous_written = true;
        }
        self.previous_time = Some(time);
        Ok(written)
    }
}

/// Whether two values can share a sparse authoring run.
///
/// Matches OpenUSD's componentwise **absolute** tolerances: half `1e-2`, float
/// `1e-6`, double `1e-12`, with a strict `<` comparison. Vectors, matrices,
/// quaternions and their arrays use their component precision; other values use
/// exact equality. Precision kinds are not coerced. Nonfinite floating values
/// never compare close, and `Null` never represents an existing value.
/// OpenUSD `usdUtils/sparseValueWriter.cpp`, `GfIsClose` in `gf/math.h`.
#[must_use]
pub fn sparse_values_close(a: &Value, b: &Value) -> bool {
    if let (Some(a), Some(b)) = (a.array_ref(), b.array_ref()) {
        if let (Some(a), Some(b)) = (a.typed(), b.typed())
            && core::mem::discriminant(&a.element_kind())
                != core::mem::discriminant(&b.element_kind())
        {
            return false;
        }
        return a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(a, b)| sparse_values_close(&a, &b));
    }
    fn close(a: f64, b: f64, epsilon: f64) -> bool {
        (a - b).abs() < epsilon
    }
    fn doubles(a: &[f64], b: &[f64]) -> bool {
        a.iter().zip(b).all(|(&a, &b)| close(a, b, 1e-12))
    }
    fn floats(a: &[f32], b: &[f32]) -> bool {
        a.iter()
            .zip(b)
            .all(|(&a, &b)| close(f64::from(a), f64::from(b), 1e-6))
    }
    fn halves(a: &[u16], b: &[u16]) -> bool {
        a.iter().zip(b).all(|(&a, &b)| {
            close(
                f64::from(crate::half::to_f32(a)),
                f64::from(crate::half::to_f32(b)),
                1e-2,
            )
        })
    }
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Double(a), Value::Double(b)) => close(*a, *b, 1e-12),
        (Value::Float(a), Value::Float(b)) => close(f64::from(*a), f64::from(*b), 1e-6),
        (Value::Half(a), Value::Half(b)) => halves(&[*a], &[*b]),
        (Value::Vec2d(a), Value::Vec2d(b)) => doubles(a, b),
        (Value::Vec3d(a), Value::Vec3d(b)) => doubles(a, b),
        (Value::Vec4d(a), Value::Vec4d(b)) | (Value::Quatd(a), Value::Quatd(b)) => doubles(a, b),
        (Value::Matrix2d(a), Value::Matrix2d(b)) => doubles(a.as_slice(), b.as_slice()),
        (Value::Matrix3d(a), Value::Matrix3d(b)) => doubles(a.as_slice(), b.as_slice()),
        (Value::Matrix4d(a), Value::Matrix4d(b)) => doubles(a.as_slice(), b.as_slice()),
        (Value::Vec2f(a), Value::Vec2f(b)) => floats(a, b),
        (Value::Vec3f(a), Value::Vec3f(b)) => floats(a, b),
        (Value::Vec4f(a), Value::Vec4f(b)) | (Value::Quatf(a), Value::Quatf(b)) => floats(a, b),
        (Value::Vec2h(a), Value::Vec2h(b)) => halves(a, b),
        (Value::Vec3h(a), Value::Vec3h(b)) => halves(a, b),
        (Value::Vec4h(a), Value::Vec4h(b)) | (Value::Quath(a), Value::Quath(b)) => halves(a, b),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, edit::EditTarget,
    };
    use alloc::vec;
    fn fixture() -> (InMemoryStore, crate::PropertyPath, SparseAttributeWriter) {
        let mut store = InMemoryStore::default();
        let property = store.property_path("/P.value");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(property.prim_path(), PrimSpec::def());
        layer.set_property(
            property,
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.))),
        );
        store.insert_layer(layer);
        let writer = SparseAttributeWriter::new(
            EditTarget::for_layer(LayerId(1)).property(property),
            Some(Value::Float(0.)),
        );
        (store, property, writer)
    }
    #[test]
    fn preserves_run_endpoints_and_suppresses_fallback_with_undo() {
        // Pinned C++ sparse writer: default 0; samples 0,0,2,2,2,4 => 1:0,2:2,4:2,5:4.
        let (mut store, property, mut writer) = fixture();
        let original = store.layers[&LayerId(1)].clone();
        let mut transaction = Transaction::new();
        assert_eq!(
            writer
                .set_default(&mut transaction, Value::Float(0.))
                .unwrap(),
            0
        );
        for (i, v) in [0., 0., 2., 2., 2., 4.].into_iter().enumerate() {
            writer
                .set_time_sample(
                    &mut transaction,
                    f64::from(u32::try_from(i).unwrap()),
                    Value::Float(v),
                )
                .unwrap();
        }
        let undo = transaction.apply(&mut store).unwrap();
        let property_spec = store.layers[&LayerId(1)].property(property).unwrap();
        assert_eq!(property_spec.default, None);
        assert_eq!(
            property_spec.time_samples.as_ref().unwrap().as_slice(),
            &[
                (1., Value::Float(0.)),
                (2., Value::Float(2.)),
                (4., Value::Float(2.)),
                (5., Value::Float(4.))
            ]
        );
        undo.apply(&mut store).unwrap();
        assert_eq!(store.layers[&LayerId(1)], original);
    }
    #[test]
    fn suppressed_defaults_keep_the_authored_tolerance_anchor() {
        // OpenUSD 26.08 sparse writer: close submissions do not replace the
        // last authored comparison value; their error must not accumulate.
        let (mut store, property, mut writer) = fixture();
        let mut transaction = Transaction::new();
        assert_eq!(
            writer.set_default(&mut transaction, Value::Float(0.75e-6)),
            Ok(0)
        );
        assert_eq!(
            writer.set_time_sample(&mut transaction, 1., Value::Float(1.5e-6)),
            Ok(1)
        );
        transaction.apply(&mut store).unwrap();
        let spec = store.layers[&LayerId(1)].property(property).unwrap();
        assert_eq!(spec.default, None);
        assert_eq!(
            spec.time_samples.as_ref().unwrap().as_slice(),
            &[(1., Value::Float(1.5e-6))]
        );

        let (mut store, property, mut writer) = fixture();
        let mut transaction = Transaction::new();
        assert_eq!(
            writer.set_default(&mut transaction, Value::Float(0.75e-6)),
            Ok(0)
        );
        assert_eq!(
            writer.set_default(&mut transaction, Value::Float(1.5e-6)),
            Ok(1)
        );
        transaction.apply(&mut store).unwrap();
        assert_eq!(
            store.layers[&LayerId(1)]
                .property(property)
                .unwrap()
                .default,
            Some(Value::Float(1.5e-6))
        );
    }

    #[test]
    fn rejected_times_do_not_advance_or_append() {
        let (_, _, mut writer) = fixture();
        let mut tx = Transaction::new();
        writer
            .set_time_sample(&mut tx, 1., Value::Float(1.))
            .unwrap();
        for time in [1., 0., f64::NAN, f64::INFINITY] {
            assert!(
                writer
                    .set_time_sample(&mut tx, time, Value::Float(2.))
                    .is_err()
            );
            assert_eq!(tx.len(), 1);
        }
        assert_eq!(
            writer.set_default(&mut tx, Value::Float(3.)),
            Err(SparseWriterError::DefaultAfterSamples)
        );
        assert_eq!(
            writer
                .set_time_sample(&mut tx, 2., Value::Float(2.))
                .unwrap(),
            1
        );
    }
    #[test]
    fn precision_composites_and_nonfinite_values_match_cpp() {
        assert!(sparse_values_close(
            &Value::Float(0.),
            &Value::Float(0.000_000_5)
        ));
        assert!(!sparse_values_close(
            &Value::Double(0.),
            &Value::Double(0.000_000_5)
        ));
        assert!(sparse_values_close(
            &Value::Half(0),
            &Value::Half(crate::half::from_f32(0.005))
        ));
        assert!(sparse_values_close(
            &Value::Vec3f([0.; 3]),
            &Value::Vec3f([0., 0.000_000_5, 0.])
        ));
        assert!(sparse_values_close(
            &Value::array(vec![Value::Float(0.)]),
            &Value::array(vec![Value::Float(0.000_000_5)])
        ));
        assert!(!sparse_values_close(
            &Value::Double(f64::INFINITY),
            &Value::Double(f64::INFINITY)
        ));
        assert!(!sparse_values_close(
            &Value::Float(100_000.),
            &Value::Float(100_000.01)
        ));
    }
}
