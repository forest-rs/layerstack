// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained in-memory numeric sources. Structural assembly remains eager.
//! AOUSD Core §16.3.9–§16.3.10: identical `ValueReps` name identical data.

use crate::{
    AssembleResult, DecodeBudget, UsdcError,
    section::CrateSections,
    value_rep::{DecodedField, RawValueRep, decode_field_within},
    value_type::ValueType,
};
use alloc::{
    format,
    sync::{Arc, Weak},
};
use core::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};
use layerstack::{
    ArrayReadError, AssetResolver, DeferredArraySource, HashMap, LayerId, PathInterner,
    TokenInterner, TypedArray, Value,
};
use std::sync::{Mutex, OnceLock};

/// A fully assembled layer whose numeric payloads remain retained in memory.
#[derive(Debug)]
pub struct LazyReadResult {
    /// Authored layer, resolved dependencies and assembly diagnostics.
    pub assembled: AssembleResult,
    /// Decode work, cache storage and retained-input inspection.
    pub values: RetainedValues,
}
/// Shared cache inspection. Dropping this handle does not invalidate the layer.
#[derive(Clone, Debug)]
pub struct RetainedValues {
    pool: Arc<LazyPool>,
}
/// Current cache footprint and cumulative decode work.
/// Excludes structural tables, allocator overhead, spare vector capacity and
/// transformed query-result buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedValueStats {
    /// Encoded file bytes retained by this pool.
    pub input_bytes: usize,
    /// Numeric sources still owned by layers or query results.
    pub live_arrays: usize,
    /// Sources with a cached successful native buffer.
    pub materialized_arrays: usize,
    /// Sources with a cached failure.
    pub failed_arrays: usize,
    /// Initialized native element bytes in successful cached buffers.
    pub element_bytes: usize,
    /// Materialization attempts, including failures; each source attempts once.
    pub decode_attempts: usize,
    /// Remaining units in the file's shared decode budget.
    pub remaining_units: u64,
}
impl RetainedValues {
    /// Inspects caches without decoding or traversing any payload elements.
    pub fn stats(&self) -> RetainedValueStats {
        let mut stats = RetainedValueStats {
            input_bytes: self.pool.bytes.len(),
            decode_attempts: self.pool.attempts.load(Ordering::Relaxed),
            remaining_units: self
                .pool
                .budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remaining(),
            ..RetainedValueStats::default()
        };
        let registry = self
            .pool
            .arrays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for array in registry.values().filter_map(Weak::upgrade) {
            stats.live_arrays += 1;
            match array.cached.get() {
                Some(Ok(native)) => {
                    stats.materialized_arrays += 1;
                    stats.element_bytes += native.element_bytes();
                }
                Some(Err(_)) => stats.failed_arrays += 1,
                None => {}
            }
        }
        stats
    }
}

pub(crate) struct LazyPool {
    bytes: Arc<[u8]>,
    sections: Arc<CrateSections>,
    budget: Mutex<DecodeBudget>,
    // Weak registry avoids a source -> pool -> source ownership cycle.
    arrays: Mutex<HashMap<[u8; 8], Weak<LazyArray>>>,
    attempts: AtomicUsize,
}
impl fmt::Debug for LazyPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyPool")
            .field("input_bytes", &self.bytes.len())
            .field("decode_attempts", &self.attempts.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
impl LazyPool {
    pub(crate) fn register(self: &Arc<Self>, rep: RawValueRep, kind: Value) -> TypedArray {
        let mut arrays = self
            .arrays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = if let Some(source) = arrays.get(&rep.bytes).and_then(Weak::upgrade) {
            source
        } else {
            let source = Arc::new(LazyArray {
                pool: Arc::clone(self),
                rep,
                kind,
                cached: OnceLock::new(),
            });
            arrays.insert(rep.bytes, Arc::downgrade(&source));
            source
        };
        TypedArray::Deferred(source)
    }
}
struct LazyArray {
    pool: Arc<LazyPool>,
    rep: RawValueRep,
    kind: Value,
    cached: OnceLock<Result<TypedArray, ArrayReadError>>,
}
impl fmt::Debug for LazyArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyArray")
            .field("kind", &self.kind)
            .field("cached", &self.cached.get().is_some())
            .finish_non_exhaustive()
    }
}
impl DeferredArraySource for LazyArray {
    fn element_kind(&self) -> Value {
        self.kind.clone()
    }
    fn retimed(&self, offset: layerstack::LayerOffset) -> Option<TypedArray> {
        if !matches!(self.kind, Value::TimeCode(_)) {
            return None;
        }
        Some(TypedArray::Deferred(Arc::new(RetimedArray {
            original: self.pool.register(self.rep, self.kind.clone()),
            offset,
            cached: OnceLock::new(),
        })))
    }
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        self.cached
            .get_or_init(|| {
                self.pool.attempts.fetch_add(1, Ordering::Relaxed);
                let mut budget = self
                    .pool
                    .budget
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let field = decode_field_within(
                    &self.rep,
                    &self.pool.bytes,
                    &self.pool.sections,
                    &mut budget,
                )
                .map_err(|e| match e {
                    UsdcError::DecodeBudgetExceeded { limit } => {
                        ArrayReadError::BudgetExceeded { limit }
                    }
                    e => ArrayReadError::InvalidData(Arc::from(format!(
                        "USDC array at {}: {e}",
                        self.rep.payload_offset()
                    ))),
                })?;
                Ok(match field {
                    DecodedField::FloatArray(array) => crate::numeric_array::float_array(array),
                    DecodedField::IntegerArray(array) => crate::numeric_array::integer_array(array),
                    DecodedField::MathArray(array) => crate::numeric_array::math_array(&array),
                    DecodedField::Value(_) => {
                        return Err(ArrayReadError::InvalidData(Arc::from(
                            "numeric array decoder returned a scalar",
                        )));
                    }
                })
            })
            .as_ref()
    }
}

struct RetimedArray {
    original: TypedArray,
    offset: layerstack::LayerOffset,
    cached: OnceLock<Result<TypedArray, ArrayReadError>>,
}
impl fmt::Debug for RetimedArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetimedArray")
            .field("offset", &self.offset)
            .field("cached", &self.cached.get().is_some())
            .finish_non_exhaustive()
    }
}
impl DeferredArraySource for RetimedArray {
    fn element_kind(&self) -> Value {
        Value::TimeCode(0.)
    }
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        self.cached
            .get_or_init(|| {
                let native = self.original.try_materialize().map_err(Clone::clone)?;
                let values = native.as_timecode().ok_or_else(|| {
                    ArrayReadError::InvalidData(Arc::from("retimed source is not timecode[]"))
                })?;
                // AOUSD Core §12.3.2.1; this is a query result allocation, not
                // another encoded-file decode or part of the retained file cache.
                Ok(TypedArray::TimeCode(Arc::new(
                    values
                        .iter()
                        .map(|time| time * self.offset.scale + self.offset.offset)
                        .collect(),
                )))
            })
            .as_ref()
    }
}

/// Assembles structure and retains numeric defaults and samples for demand reads.
///
/// Input bytes are immutable caller-owned memory. No file I/O, eviction or
/// background work occurs. Each distinct numeric `ValueRep` decodes once,
/// caching success or failure across layer clones, samples and threads.
/// All later decodes share the remaining structural assembly budget.
/// Structural errors fail now; malformed array payloads fail on demand.
/// Use `TypedArray::try_materialize` or the stage's fallible property queries.
pub fn read_usdc_lazy(
    bytes: Arc<[u8]>,
    id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<LazyReadResult, UsdcError> {
    let budget = DecodeBudget::for_input(bytes.len());
    read_usdc_lazy_within(bytes, id, tokens, paths, resolver, budget)
}
/// Retained import with one explicitly owned budget covering assembly and all
/// future decodes. A cached failure never consumes the budget a second time.
pub fn read_usdc_lazy_within(
    bytes: Arc<[u8]>,
    id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
    mut budget: DecodeBudget,
) -> Result<LazyReadResult, UsdcError> {
    let header = crate::header::parse_header(&bytes)?;
    let toc = crate::toc::parse_toc(&bytes, header.toc_offset)?;
    let sections = Arc::new(crate::section::parse_sections(
        &bytes,
        &toc,
        header.crate_version(),
        &mut budget,
    )?);
    let pool = Arc::new(LazyPool {
        bytes,
        sections,
        budget: Mutex::new(DecodeBudget::with_limit(0)),
        arrays: Mutex::new(HashMap::new()),
        attempts: AtomicUsize::new(0),
    });
    budget.lazy = Some(Arc::clone(&pool));
    let assembled = crate::assemble::assemble(
        &pool.bytes,
        &pool.sections,
        id,
        tokens,
        paths,
        resolver,
        &mut budget,
    )?;
    budget.lazy = None;
    *pool
        .budget
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = budget;
    Ok(LazyReadResult {
        assembled,
        values: RetainedValues { pool },
    })
}

pub(crate) fn numeric_kind(kind: ValueType) -> Option<Value> {
    Some(match kind {
        ValueType::Bool => Value::Bool(false),
        ValueType::UChar => Value::UChar(0),
        ValueType::Int => Value::Int(0),
        ValueType::UInt => Value::UInt(0),
        ValueType::Int64 => Value::Int64(0),
        ValueType::UInt64 => Value::UInt64(0),
        ValueType::Half => Value::Half(0),
        ValueType::Float => Value::Float(0.0),
        ValueType::Double => Value::Double(0.0),
        ValueType::TimeCode => Value::TimeCode(0.0),
        ValueType::Vec2f => Value::Vec2f(Default::default()),
        ValueType::Vec3f => Value::Vec3f(Default::default()),
        ValueType::Vec4f => Value::Vec4f(Default::default()),
        ValueType::Vec2d => Value::Vec2d(Default::default()),
        ValueType::Vec3d => Value::Vec3d(Default::default()),
        ValueType::Vec4d => Value::Vec4d(Default::default()),
        ValueType::Vec2h => Value::Vec2h(Default::default()),
        ValueType::Vec3h => Value::Vec3h(Default::default()),
        ValueType::Vec4h => Value::Vec4h(Default::default()),
        ValueType::Vec2i => Value::Vec2i(Default::default()),
        ValueType::Vec3i => Value::Vec3i(Default::default()),
        ValueType::Vec4i => Value::Vec4i(Default::default()),
        ValueType::Quatf => Value::Quatf(Default::default()),
        ValueType::Quatd => Value::Quatd(Default::default()),
        ValueType::Quath => Value::Quath(Default::default()),
        ValueType::Matrix2d => Value::Matrix2d(alloc::boxed::Box::default()),
        ValueType::Matrix3d => Value::Matrix3d(alloc::boxed::Box::default()),
        ValueType::Matrix4d => Value::Matrix4d(alloc::boxed::Box::default()),
        _ => return None,
    })
}
