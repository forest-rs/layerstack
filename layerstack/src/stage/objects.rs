// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed generic USD views and caller-owned retained attribute evaluation.
//! AOUSD Core §11–§13; OpenUSD `UsdPrim`, `UsdAttribute`, `UsdRelationship`,
//! `UsdAttributeQuery`. Views borrow immutable snapshots; queries never hide
//! synchronization or keep a mutable stage borrowed between evaluations.
use super::*;
use crate::{FromValue, PropertyKind, TargetPath};

/// A generic object in a composed snapshot.
#[derive(Clone, Copy, Debug)]
pub enum Object<'a> {
    /// A populated prim.
    Prim(Prim<'a>),
    /// An authored or schema-defined attribute.
    Attribute(Attribute<'a>),
    /// An authored or schema-defined relationship.
    Relationship(Relationship<'a>),
}
/// Borrowed prim view, including generic properties and composed status.
#[derive(Clone, Copy)]
#[doc(alias = "UsdPrim")]
pub struct Prim<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    path: PathId,
}
impl core::fmt::Debug for Prim<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Prim")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
/// Borrowed attribute view. Creation checks its composed property kind.
#[derive(Clone, Copy)]
#[doc(alias = "UsdAttribute")]
pub struct Attribute<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    path: PropertyPath,
}
impl core::fmt::Debug for Attribute<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Attribute")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
/// Borrowed relationship view. Creation checks its composed property kind.
#[derive(Clone, Copy)]
#[doc(alias = "UsdRelationship")]
pub struct Relationship<'a> {
    stage: &'a Stage,
    store: &'a dyn LayerStore,
    path: PropertyPath,
}
impl core::fmt::Debug for Relationship<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Relationship")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
impl Stage {
    /// Captured token/path domain affinity. Public composition always supplies
    /// this identity; internal synthetic snapshots may have no source store.
    pub fn store_identity(&self) -> Option<&crate::StoreIdentity> {
        self.store_identity.as_ref()
    }
    /// Views a populated prim in this immutable snapshot.
    #[doc(alias = "UsdStage::GetPrimAtPath")]
    #[doc(alias = "GetPrimAtPath")]
    pub fn prim<'a>(&'a self, path: PathId, store: &'a dyn LayerStore) -> Option<Prim<'a>> {
        self.has_prim(path).then_some(Prim {
            stage: self,
            store,
            path,
        })
    }
    /// Views a concrete prim or property after checking its composed existence.
    #[doc(alias = "UsdStage::GetObjectAtPath")]
    #[doc(alias = "GetObjectAtPath")]
    pub fn object<'a>(&'a self, path: TargetPath, store: &'a dyn LayerStore) -> Option<Object<'a>> {
        match path {
            TargetPath::Prim(path) => self.prim(path, store).map(Object::Prim),
            TargetPath::Property(path) => match self.property_kind(path)? {
                PropertyKind::Attribute => Some(Object::Attribute(Attribute {
                    stage: self,
                    store,
                    path,
                })),
                PropertyKind::Relationship => Some(Object::Relationship(Relationship {
                    stage: self,
                    store,
                    path,
                })),
            },
        }
    }
}
impl<'a> Prim<'a> {
    /// Concrete stage path.
    #[doc(alias = "UsdPrim::GetPath")]
    #[doc(alias = "GetPath")]
    pub fn path(self) -> PathId {
        self.path
    }
    /// Composed status, captured by this view's immutable snapshot.
    pub fn status(self) -> PrimStatus {
        self.stage
            .prim_status(self.path, self.store)
            .expect("populated view")
    }
    /// Authored type name; fallback mapping does not change it.
    #[doc(alias = "UsdPrim::GetTypeName")]
    #[doc(alias = "GetTypeName")]
    pub fn type_name(self) -> Option<TokenId> {
        self.stage.resolve_type_name(self.path)
    }
    /// Effective schema type, including `fallbackPrimTypes` mapping.
    pub fn schema_type_name(self) -> Option<TokenId> {
        let identity = &self
            .stage
            .prims
            .get(&self.path)?
            .type_info
            .as_ref()?
            .identity;
        identity.mapped_type_name.or(identity.type_name)
    }
    /// Generic attribute with this USD name, including schema-defined properties.
    #[doc(alias = "UsdPrim::GetAttribute")]
    #[doc(alias = "GetAttribute")]
    pub fn attribute(self, name: &str) -> Option<Attribute<'a>> {
        let path = PropertyPath::new(self.path, self.store.tokens().lookup(name)?);
        (self.stage.property_kind(path) == Some(PropertyKind::Attribute)).then_some(Attribute {
            stage: self.stage,
            store: self.store,
            path,
        })
    }
    /// Generic relationship with this USD name.
    #[doc(alias = "UsdPrim::GetRelationship")]
    #[doc(alias = "GetRelationship")]
    pub fn relationship(self, name: &str) -> Option<Relationship<'a>> {
        let path = PropertyPath::new(self.path, self.store.tokens().lookup(name)?);
        (self.stage.property_kind(path) == Some(PropertyKind::Relationship)).then_some(
            Relationship {
                stage: self.stage,
                store: self.store,
                path,
            },
        )
    }
    /// All authored and schema-defined property names in composed order.
    #[doc(alias = "UsdPrim::GetPropertyNames")]
    #[doc(alias = "GetPropertyNames")]
    pub fn property_names(self) -> Vec<TokenId> {
        self.stage.property_names(self.path, self.store)
    }
    /// Direct populated children, with an explicit instance-proxy policy.
    #[doc(alias = "UsdPrim::GetChildren")]
    #[doc(alias = "GetChildren")]
    pub fn children(self, instance_proxies: bool) -> impl Iterator<Item = Prim<'a>> {
        let children = if self.stage.is_instance(self.path) && !instance_proxies {
            &[][..]
        } else {
            self.stage.all_children_of(self.path).unwrap_or_default()
        };
        children
            .iter()
            .filter_map(move |&path| self.stage.prim(path, self.store))
    }
}
impl<'a> Attribute<'a> {
    /// Concrete property path.
    pub fn path(self) -> PropertyPath {
        self.path
    }
    /// The value type name (`float`, `color3f[]`): the type the prim's
    /// schemas define the attribute with, else the strongest type an
    /// opinion declares. `None` when neither declares one. It names the
    /// Rust type [`Self::value`] converts to: a `float` reads as `f32`.
    ///
    /// ```
    /// # use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Variability};
    /// # let mut store = InMemoryStore::default();
    /// # let (world, radius) = (store.path("/World"), store.tokens.intern("radius"));
    /// # let mut layer = Layer::new(LayerId(1));
    /// # layer.insert_prim(world, PrimSpec::def().with_property(radius,
    /// #     PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())));
    /// # store.insert_layer(layer);
    /// # let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let radius = stage.view(&store).attribute("/World.radius").unwrap();
    /// assert_eq!(radius.type_name(), Some("double"));
    /// assert_eq!(radius.variability(), Variability::Varying);
    /// ```
    ///
    /// OpenUSD: `UsdAttribute::GetTypeName`, which consults the prim
    /// definition before authored opinions (`UsdStage::_GetAttrTypeImpl`).
    ///
    /// Spec: AOUSD Core §7.6.4.1.1 (`typeName`), §12.2.2 (the strongest
    /// opinion), §13.3.2 (prim definitions).
    #[doc(alias = "UsdAttribute::GetTypeName")]
    #[doc(alias = "GetTypeName")]
    pub fn type_name(self) -> Option<&'a str> {
        let (prim, name) = (self.path.prim_path(), self.path.property());
        let stage: &'a Stage = self.stage;
        stage
            .property_definition_ref(prim, name)
            .and_then(|defined| defined.type_name.as_ref())
            .or_else(|| stage.prims.get(&prim)?.property_type_for(&name))
            .map(PropertyType::type_name)
    }
    /// Whether the value may vary over time: the variability the prim's
    /// schemas define the attribute with, else the weakest opinion's.
    ///
    /// OpenUSD: `UsdAttribute::GetVariability`
    /// (`UsdStage::_GetAttrVariabilityImpl`).
    ///
    /// Spec: AOUSD Core §7.6.4.1.2 (`variability`), §12.2.3 (the weakest
    /// opinion decides).
    #[doc(alias = "UsdAttribute::GetVariability")]
    #[doc(alias = "GetVariability")]
    pub fn variability(self) -> Variability {
        let (prim, name) = (self.path.prim_path(), self.path.property());
        if let Some(defined) = self.stage.property_definition_ref(prim, name) {
            return defined.variability;
        }
        self.stage
            .resolve_property_declaration(prim, name)
            .map(|declared| declared.variability)
            .unwrap_or_default()
    }
    /// Composed value with captured schema fallback and optional provenance.
    /// Numeric arrays remain retained until the caller selects materialization.
    #[doc(alias = "UsdAttribute::Get")]
    #[doc(alias = "Get")]
    pub fn get(self, time: Time) -> Option<Resolved<Value>> {
        self.stage
            .read_property(self.path, time, |v| Some(v.clone()))
    }
    /// The value at `time` as a `T`, schema fallback included. At the
    /// default time this is the strongest default that converts
    /// ([`FromValue`]); at a time code, the strongest source's value if it
    /// converts. `None` when nothing converts, the value is blocked, or
    /// `time`'s samples hold none.
    ///
    /// ```
    /// # use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Time, Value};
    /// # let mut store = InMemoryStore::default();
    /// # let (world, radius) = (store.path("/World"), store.tokens.intern("radius"));
    /// # let double = PropertyType::parse("double").unwrap();
    /// # let mut layer = Layer::new(LayerId(1));
    /// # layer.insert_prim(world, PrimSpec::def().with_property(radius,
    /// #     PropertySpec::typed_attribute(double).with_default(Value::Double(2.5))));
    /// # store.insert_layer(layer);
    /// # let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let attribute = stage.view(&store).attribute("/World.radius").unwrap();
    /// assert_eq!(attribute.value::<f64>(Time::Default), Some(2.5));
    /// assert_eq!(attribute.value::<f32>(Time::Default), None);
    /// ```
    ///
    /// OpenUSD: `UsdAttribute::Get<T>`.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    pub fn value<T: FromValue>(self, time: Time) -> Option<T> {
        let tokens = self.store.tokens();
        self.stage
            .read_property(self.path, time, |v| T::from_value(v, tokens))
            .map(|resolved| resolved.value)
    }
    /// The value at `time` beneath `layer`, as a `T`, with the source it
    /// came from: what the attribute would read without `layer`'s opinions
    /// and those stronger than them (see [`Stage::read_property_below`]).
    pub fn value_below<T: FromValue>(self, layer: LayerId, time: Time) -> Option<Resolved<T>> {
        let tokens = self.store.tokens();
        self.stage
            .read_property_below(self.path, layer, time, |v| T::from_value(v, tokens))
    }
    /// Reads and materializes the selected numeric array, exposing decode failure.
    #[doc(alias = "UsdAttribute::Get")]
    #[doc(alias = "Get")]
    pub fn try_get(self, time: Time) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        checked_read(self.stage, self.path, time)
    }
    /// Raw composed attribute connections, independent of its value.
    #[doc(alias = "UsdAttribute::GetConnections")]
    #[doc(alias = "GetConnections")]
    pub fn connections(self) -> Vec<TargetPath> {
        self.stage
            .resolve_target_list_path(self.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Why the connections are what they are: each opinion's paths, and
    /// those composition dropped, with why (see
    /// [`Stage::explain_targets`]).
    pub fn explain_connections(self) -> Option<TargetsExplanation> {
        self.stage.explain_targets(self.path, self.store)
    }
    /// Composed property metadata, including schema fallbacks.
    #[doc(alias = "UsdAttribute::GetMetadata")]
    #[doc(alias = "GetMetadata")]
    pub fn metadata(self, key: TokenId) -> Option<Resolved<ResolvedValue>> {
        self.stage
            .resolve_property_metadata(self.path.prim_path(), self.path.property(), key)
    }
    /// Caller-owned retained query for this concrete attribute path.
    pub fn query(self) -> AttributeQuery {
        AttributeQuery::new(self.path)
    }
    /// Distinct composed sample times in stage time, including value clips.
    #[doc(alias = "UsdAttribute::GetTimeSamples")]
    #[doc(alias = "GetTimeSamples")]
    pub fn sample_times(self) -> Vec<f64> {
        self.stage
            .property_sample_times(self.path.prim_path(), self.path.property())
    }
    /// Effective sample times within open, closed or unbounded stage-time bounds.
    /// Filters the composed grid; does not add evaluation times or brackets.
    /// NaN bounds and reversed intervals select no samples.
    #[doc(alias = "UsdAttribute::GetTimeSamplesInInterval")]
    #[doc(alias = "GetTimeSamplesInInterval")]
    pub fn sample_times_in_range(self, interval: impl core::ops::RangeBounds<f64>) -> Vec<f64> {
        self.stage.property_sample_times_in_range(
            self.path.prim_path(),
            self.path.property(),
            interval,
        )
    }
    /// Conservative whether a numeric-time query can vary with time.
    #[doc(alias = "UsdAttribute::ValueMightBeTimeVarying")]
    #[doc(alias = "ValueMightBeTimeVarying")]
    pub fn might_be_time_varying(self) -> bool {
        self.stage
            .property_might_be_time_varying(self.path.prim_path(), self.path.property())
    }
}
impl Relationship<'_> {
    /// Concrete property path.
    pub fn path(self) -> PropertyPath {
        self.path
    }
    /// Ordered composed targets without forwarding.
    #[doc(alias = "UsdRelationship::GetTargets")]
    #[doc(alias = "GetTargets")]
    pub fn targets(self) -> Vec<TargetPath> {
        self.stage
            .resolve_target_list_path(self.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Why the targets are what they are: each opinion's paths, and those
    /// composition dropped, with why (see [`Stage::explain_targets`]).
    pub fn explain_targets(self) -> Option<TargetsExplanation> {
        self.stage.explain_targets(self.path, self.store)
    }
    /// Ordered terminal targets after recursively forwarding relationships.
    #[doc(alias = "UsdRelationship::GetForwardedTargets")]
    #[doc(alias = "GetForwardedTargets")]
    pub fn forwarded_targets(self) -> Vec<TargetPath> {
        self.stage.forwarded_relationship_targets(self.path)
    }
}
/// Work performed by one retained query, independent of stage counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttributeQueryWork {
    /// Uncached evaluations, including absence, failures and schema fallbacks.
    pub evaluations: u64,
    /// Same-time evaluations reused from an unchanged snapshot identity.
    pub cache_hits: u64,
}

/// Retained composition identity of one prim, including absence and schema data.
///
/// Comparing this evidence never resolves values or scans geometry. A change to
/// any opinion on this prim invalidates it conservatively. Use store-local paths
/// only with their original store's interners. This is neither authored identity
/// nor geometry-content identity. AOUSD Core §10 (composition), §13.3 (schemas).
#[derive(Clone, Debug)]
pub struct PrimSnapshot {
    store_identity: Option<crate::StoreIdentity>,
    path: PathId,
    index: Option<Arc<crate::prim_index::PrimIndexData>>,
    type_info: Option<Arc<PrimTypeInfo>>,
}
impl Stage {
    /// Retains the immutable records currently describing this prim or absence.
    pub fn prim_snapshot(&self, path: PathId) -> PrimSnapshot {
        let current = self.prims.get(&path);
        PrimSnapshot {
            store_identity: self.store_identity.clone(),
            path,
            index: current.map(|i| i.data.clone()),
            type_info: current.and_then(|i| i.type_info.clone()),
        }
    }
}
impl PrimSnapshot {
    /// Whether this current stage still has the captured records and schema data.
    pub fn is_current(&self, stage: &Stage) -> bool {
        let current = stage.prims.get(&self.path);
        self.store_identity.as_ref() == stage.store_identity()
            && same_arc(self.index.as_ref(), current.map(|i| &i.data))
            && same_arc(
                self.type_info.as_ref(),
                current.and_then(|i| i.type_info.as_ref()),
            )
    }
}
#[derive(Clone, Debug)]
struct QueryStamp {
    store_identity: Option<crate::StoreIdentity>,
    index: Option<Arc<crate::prim_index::PrimIndexData>>,
    type_info: Option<Arc<PrimTypeInfo>>,
    clips: crate::value_clips::ClipQueryIdentity,
    provenance: bool,
}
impl QueryStamp {
    fn matches(&self, stage: &Stage, path: PropertyPath) -> bool {
        let current = stage.prims.get(&path.prim_path());
        self.store_identity.as_ref() == stage.store_identity()
            && same_arc(self.index.as_ref(), current.map(|i| &i.data))
            && same_arc(
                self.type_info.as_ref(),
                current.and_then(|i| i.type_info.as_ref()),
            )
            && self.provenance == stage.with_provenance
            && stage
                .clips
                .query_identity_matches(&self.clips, path.prim_path(), path.property())
    }
}
fn same_arc<T>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
#[derive(Clone, Debug)]
struct CachedQuery {
    time: Time,
    checked: bool,
    value: Result<Option<Resolved<Value>>, crate::ArrayReadError>,
}
/// Caller-owned same-time attribute cache, refreshed by immutable snapshot
/// identity on every read. No change notice can be missed: even a replacement
/// stage invalidates the cached value. Store-local paths must stay in one store.
/// Unrelated prim edits preserve hits; changes to this prim or prepared clips
/// invalidate conservatively. Memory is bounded to one result and its sources.
#[derive(Clone, Debug)]
#[doc(alias = "UsdAttributeQuery")]
pub struct AttributeQuery {
    path: PropertyPath,
    cached: Option<CachedQuery>,
    stamp: Option<QueryStamp>,
    work: AttributeQueryWork,
}
impl AttributeQuery {
    /// Retains a concrete path without borrowing a stage across edits.
    pub fn new(path: PropertyPath) -> Self {
        Self {
            path,
            cached: None,
            stamp: None,
            work: AttributeQueryWork::default(),
        }
    }
    /// Queried concrete attribute path.
    pub fn path(&self) -> PropertyPath {
        self.path
    }
    /// Whether a checked read at this time can reuse its captured snapshot.
    /// This checks immutable identities, including clip dependencies, without
    /// resolving an attribute or materializing its numeric storage.
    pub fn is_current(&self, stage: &Stage, time: Time) -> bool {
        self.stamp
            .as_ref()
            .is_some_and(|s| s.matches(stage, self.path))
            && self
                .cached
                .as_ref()
                .is_some_and(|c| c.time == time && c.checked)
    }
    /// Evaluates against the supplied current snapshot, refreshing as needed.
    /// The caller synchronizes `LiveStage` before passing its snapshot.
    #[doc(alias = "UsdAttributeQuery::Get")]
    #[doc(alias = "Get")]
    pub fn get(&mut self, stage: &Stage, time: Time) -> Option<Resolved<Value>> {
        self.evaluate(stage, time, false).ok().flatten()
    }
    /// Evaluates and materializes the selected numeric array with typed failure.
    /// Repeated reads of the same mode/time/snapshot reuse failures as well as values.
    #[doc(alias = "UsdAttributeQuery::Get")]
    #[doc(alias = "Get")]
    pub fn try_get(
        &mut self,
        stage: &Stage,
        time: Time,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        self.evaluate(stage, time, true)
    }
    fn evaluate(
        &mut self,
        stage: &Stage,
        time: Time,
        checked: bool,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        if self
            .stamp
            .as_ref()
            .is_some_and(|s| s.matches(stage, self.path))
            && let Some(cached) = &self.cached
            && cached.time == time
            && cached.checked == checked
        {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
            return cached.value.clone();
        }
        let current = stage.prims.get(&self.path.prim_path());
        self.stamp = Some(QueryStamp {
            store_identity: stage.store_identity.clone(),
            index: current.map(|i| i.data.clone()),
            type_info: current.and_then(|i| i.type_info.clone()),
            clips: stage
                .clips
                .query_identity(self.path.prim_path(), self.path.property()),
            provenance: stage.with_provenance,
        });
        let value = if stage.property_kind(self.path) != Some(PropertyKind::Attribute) {
            Ok(None)
        } else if checked {
            checked_read(stage, self.path, time)
        } else {
            Ok(stage.read_property(self.path, time, |v| Some(v.clone())))
        };
        self.cached = Some(CachedQuery {
            time,
            checked,
            value: value.clone(),
        });
        self.work.evaluations = self.work.evaluations.saturating_add(1);
        value
    }
    /// Drops retained source/value state while preserving cumulative counters.
    pub fn clear(&mut self) {
        self.cached = None;
        self.stamp = None;
    }
    /// Cumulative query work for measuring reuse.
    pub fn work(&self) -> AttributeQueryWork {
        self.work
    }
}
fn materialize(
    mut value: Option<Resolved<Value>>,
) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
    if let Some(Resolved { value, .. }) = &mut value {
        materialize_selected(value)?;
    }
    Ok(value)
}

pub(super) fn check_decode(
    stage: &Stage,
    path: PropertyPath,
    time: Time,
) -> Result<(), crate::ArrayReadError> {
    match time {
        Time::Default => {
            stage.try_resolve_property_path(path)?;
        }
        Time::At {
            code,
            interpolation,
        } => {
            stage.try_resolve_property_path_at_time(path, code, interpolation)?;
        }
    }
    Ok(())
}
fn checked_read(
    stage: &Stage,
    path: PropertyPath,
    time: Time,
) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
    check_decode(stage, path, time)?;
    materialize(stage.read_property(path, time, |v| Some(v.clone())))
}
