// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained in-memory value sources. Structural assembly remains eager.
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
use layerstack::encoded_bytes::EncodedBytes;
use layerstack::{
    ArrayReadError, AssetResolver, DeferredArraySource, DeferredValue, DeferredValueSource,
    HashMap, LayerId, PathInterner, TokenId, TokenInterner, TypedArray, Value,
};
use std::sync::{Mutex, OnceLock};

/// Text arrays shorter than this many elements decode during the read: one
/// costs less to decode than to retain. The crate format stores arrays this
/// short uncompressed for the same reason (`MinCompressedArraySize`,
/// `pxr/usd/sdf/crateFile.cpp`).
pub const MIN_RETAINED_TEXT_ARRAY: u64 = 16;

/// A fully assembled layer whose large payloads remain retained in memory.
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
    /// Complete backing allocation retained, possibly shared with other members.
    /// Do not sum this across pools without deduplicating their backing owners.
    pub backing_bytes: usize,
    /// Numeric sources still owned by layers or query results.
    pub live_arrays: usize,
    /// Sources with a cached successful native buffer.
    pub materialized_arrays: usize,
    /// Sources with a cached failure.
    pub failed_arrays: usize,
    /// Initialized native element bytes in successful cached buffers.
    pub element_bytes: usize,
    /// Deferred value sources (`token[]` and `string[]` defaults) still
    /// owned by layers or query results.
    pub live_values: usize,
    /// Deferred value sources with a cached decoded value.
    pub materialized_values: usize,
    /// Deferred value sources with a cached failure.
    pub failed_values: usize,
    /// Elements of the cached decoded values.
    pub value_elements: usize,
    /// Materialization attempts of numeric and value sources, including
    /// failures; each source attempts once.
    pub decode_attempts: usize,
    /// Remaining units in the file's shared decode budget.
    pub remaining_units: u64,
}
impl RetainedValues {
    /// Immutable encoded view retained by this pool. Use `shares_backing` to
    /// deduplicate allocation accounting across members and resource views.
    pub fn encoded_bytes(&self) -> &EncodedBytes {
        &self.pool.bytes
    }
    /// Inspects caches without decoding or traversing any payload elements.
    pub fn stats(&self) -> RetainedValueStats {
        let mut stats = RetainedValueStats {
            input_bytes: self.pool.bytes.len(),
            backing_bytes: self.pool.bytes.backing_bytes(),
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
        drop(registry);
        let values = self
            .pool
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for value in values.values().filter_map(Weak::upgrade) {
            stats.live_values += 1;
            match value.cached.get() {
                Some(Ok(decoded)) => {
                    stats.materialized_values += 1;
                    stats.value_elements += decoded.array_ref().map_or(0, |a| a.len());
                }
                Some(Err(_)) => stats.failed_values += 1,
                None => {}
            }
        }
        stats
    }
}

pub(crate) struct LazyPool {
    bytes: EncodedBytes,
    sections: Arc<CrateSections>,
    /// The token id of each token-table entry, interned when the read first
    /// retains a `token[]` value, so that deferred values decode without the
    /// store's interner.
    token_ids: OnceLock<alloc::boxed::Box<[TokenId]>>,
    budget: Mutex<DecodeBudget>,
    // Weak registries avoid a source -> pool -> source ownership cycle.
    arrays: Mutex<HashMap<[u8; 8], Weak<LazyArray>>>,
    values: Mutex<HashMap<[u8; 8], Weak<LazyValue>>>,
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

    /// Retains a `token[]` or `string[]` value ([`retains_text_array`]);
    /// fields sharing one representation share one source. The first
    /// `token[]` value interns the whole token table into `tokens`.
    pub(crate) fn register_value(
        self: &Arc<Self>,
        rep: RawValueRep,
        tokens: &TokenInterner,
    ) -> DeferredValue {
        if matches!(rep.value_type(), Ok(ValueType::Token)) {
            self.token_ids.get_or_init(|| {
                self.sections
                    .tokens
                    .iter()
                    .map(|t| tokens.intern(t))
                    .collect()
            });
        }
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = if let Some(source) = values.get(&rep.bytes).and_then(Weak::upgrade) {
            source
        } else {
            let source = Arc::new(LazyValue {
                pool: Arc::clone(self),
                rep,
                cached: OnceLock::new(),
            });
            values.insert(rep.bytes, Arc::downgrade(&source));
            source
        };
        DeferredValue::new(source)
    }
}

/// Whether a retained read leaves the attribute default `rep` encoded: a
/// `token[]` or `string[]` array of at least [`MIN_RETAINED_TEXT_ARRAY`]
/// elements. A payload whose length cannot be read decodes now, so the read
/// reports it.
pub(crate) fn retains_text_array(rep: &RawValueRep, data: &[u8]) -> bool {
    rep.is_array()
        && !rep.is_array_edit()
        && matches!(rep.value_type(), Ok(ValueType::Token | ValueType::String))
        && crate::value_rep::array_len(rep, data).is_some_and(|len| len >= MIN_RETAINED_TEXT_ARRAY)
}

/// A retained `token[]` or `string[]` attribute default.
struct LazyValue {
    pool: Arc<LazyPool>,
    rep: RawValueRep,
    cached: OnceLock<Result<Value, ArrayReadError>>,
}
impl fmt::Debug for LazyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyValue")
            .field("type", &self.rep.value_type())
            .field("cached", &self.cached.get().is_some())
            .finish_non_exhaustive()
    }
}
impl DeferredValueSource for LazyValue {
    fn element_kind(&self) -> Option<Value> {
        match self.rep.value_type() {
            Ok(ValueType::Token) => Some(Value::Token(TokenId::EMPTY)),
            Ok(ValueType::String) => Some(Value::string("")),
            _ => None,
        }
    }
    /// Decodes to the value an eager read assembles: a `Value::Array` of
    /// tokens or strings, an index outside the tables reading as empty text.
    fn materialize(&self) -> Result<&Value, &ArrayReadError> {
        self.cached
            .get_or_init(|| {
                self.pool.attempts.fetch_add(1, Ordering::Relaxed);
                let pool = &self.pool;
                let mut budget = pool
                    .budget
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let indices = crate::value_rep::decode_text_indices(
                    &self.rep,
                    &pool.bytes,
                    &pool.sections,
                    &mut budget,
                )
                .map_err(|e| match e {
                    UsdcError::DecodeBudgetExceeded { limit } => {
                        ArrayReadError::BudgetExceeded { limit }
                    }
                    e => ArrayReadError::InvalidData(Arc::from(format!(
                        "USDC value at {}: {e}",
                        self.rep.payload_offset()
                    ))),
                })?;
                drop(budget);
                let tokens = matches!(self.rep.value_type(), Ok(ValueType::Token));
                let items = indices.into_iter().map(|index| {
                    let index = index as usize;
                    if tokens {
                        let ids = pool.token_ids.get().map_or(&[][..], |ids| &ids[..]);
                        Value::Token(ids.get(index).copied().unwrap_or(TokenId::EMPTY))
                    } else {
                        Value::String(Arc::from(
                            pool.sections
                                .tokens
                                .get(index)
                                .map_or("", |text| text.as_str()),
                        ))
                    }
                });
                Ok(Value::Array(items.collect()))
            })
            .as_ref()
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
                    DecodedField::MathArray(array) => crate::numeric_array::math_array(&array),
                    DecodedField::Value(crate::value_rep::CrateValue::TypedArray(array)) => array,
                    DecodedField::Value(_)
                    | DecodedField::TokenArray(_)
                    | DecodedField::Retained(_) => {
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

/// Assembles structure and retains large values for demand reads.
///
/// Two kinds of field value stay encoded, each decoded once, on its first
/// read, into a cache its sources share across layer clones, opinions and
/// threads:
///
/// - numeric arrays, anywhere a value appears (defaults, time samples,
///   dictionaries), as [`TypedArray::Deferred`];
/// - attribute defaults that are `token[]` or `string[]` arrays of at least
///   [`MIN_RETAINED_TEXT_ARRAY`] elements, as [`Value::Deferred`].
///
/// Everything composition reads stays eager: specifiers, type names,
/// children and property order, arcs and their list ops, variant sets and
/// selections, relocates, `active`, `instanceable`, `kind`, `apiSchemas`
/// and other metadata, relationship targets and connections. So does every
/// value composition or dependency analysis looks inside: path expressions,
/// asset paths, time codes and dictionaries (`customData` among them), which
/// instancing keys, layer-offset retiming, namespace edits and asset
/// dependency scans walk. Scalars stay eager too: most are inlined in their
/// field's representation, and decoding one costs less than retaining it.
///
/// Input bytes are immutable caller-owned memory. No file I/O, eviction or
/// background work occurs. Each distinct retained `ValueRep` decodes once,
/// caching success or failure across layer clones, samples and threads.
/// All later decodes share the remaining structural assembly budget.
/// Structural errors fail now; malformed retained payloads fail on demand.
/// Use `TypedArray::try_materialize`, `Value::try_materialize` or the
/// stage's fallible property queries. A read that retains a `token[]`
/// value interns the file's whole token table into `tokens`, so deferred
/// values decode without it.
pub fn read_usdc_lazy(
    bytes: Arc<[u8]>,
    id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
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
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: DecodeBudget,
) -> Result<LazyReadResult, UsdcError> {
    read_usdc_lazy_range_within(bytes.into(), id, tokens, paths, resolver, budget)
}
/// Retains a checked view of shared encoded storage without copying its bytes.
/// The budget and once-only cache have the same semantics as
/// [`read_usdc_lazy_within`]. A small file view may retain a large archive;
/// pass [`EncodedBytes::detached`] when independent ownership is preferable.
pub fn read_usdc_lazy_range_within(
    bytes: EncodedBytes,
    id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
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
        token_ids: OnceLock::new(),
        budget: Mutex::new(DecodeBudget::with_limit(0)),
        arrays: Mutex::new(HashMap::new()),
        values: Mutex::new(HashMap::new()),
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
