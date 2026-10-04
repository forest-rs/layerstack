// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed generic USD views and caller-owned retained attribute evaluation.
//! AOUSD Core §11–§13; OpenUSD `UsdPrim`, `UsdAttribute`, `UsdRelationship`,
//! `UsdAttributeQuery`. Views borrow immutable snapshots; queries never hide
//! synchronization or keep a mutable stage borrowed between evaluations.
use super::*;
use crate::{PropertyKind, TargetPath};

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
#[derive(Clone, Copy, Debug)]
pub struct Attribute<'a> {
    stage: &'a Stage,
    path: PropertyPath,
}
/// Borrowed relationship view. Creation checks its composed property kind.
#[derive(Clone, Copy, Debug)]
pub struct Relationship<'a> {
    stage: &'a Stage,
    path: PropertyPath,
}
impl Stage {
    /// Views a populated prim in this immutable snapshot.
    pub fn prim<'a>(&'a self, path: PathId, store: &'a dyn LayerStore) -> Option<Prim<'a>> {
        self.has_prim(path).then_some(Prim {
            stage: self,
            store,
            path,
        })
    }
    /// Views a concrete prim or property after checking its composed existence.
    pub fn object<'a>(&'a self, path: TargetPath, store: &'a dyn LayerStore) -> Option<Object<'a>> {
        match path {
            TargetPath::Prim(path) => self.prim(path, store).map(Object::Prim),
            TargetPath::Property(path) => match self.property_kind(path)? {
                PropertyKind::Attribute => Some(Object::Attribute(Attribute { stage: self, path })),
                PropertyKind::Relationship => {
                    Some(Object::Relationship(Relationship { stage: self, path }))
                }
            },
        }
    }
}
impl<'a> Prim<'a> {
    /// Concrete stage path.
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
    pub fn type_name(self) -> Option<TokenId> {
        self.stage.resolve_type_name(self.path, self.store)
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
    pub fn attribute(self, name: &str) -> Option<Attribute<'a>> {
        let path = PropertyPath::new(self.path, self.store.tokens().lookup(name)?);
        (self.stage.property_kind(path) == Some(PropertyKind::Attribute)).then_some(Attribute {
            stage: self.stage,
            path,
        })
    }
    /// Generic relationship with this USD name.
    pub fn relationship(self, name: &str) -> Option<Relationship<'a>> {
        let path = PropertyPath::new(self.path, self.store.tokens().lookup(name)?);
        (self.stage.property_kind(path) == Some(PropertyKind::Relationship)).then_some(
            Relationship {
                stage: self.stage,
                path,
            },
        )
    }
    /// All authored and schema-defined property names in composed order.
    pub fn property_names(self) -> Vec<TokenId> {
        self.stage.property_names(self.path, self.store)
    }
    /// Direct populated children, with an explicit instance-proxy policy.
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
impl Attribute<'_> {
    /// Concrete property path.
    pub fn path(self) -> PropertyPath {
        self.path
    }
    /// Composed value with captured schema fallback and optional provenance.
    /// Numeric arrays remain retained until the caller selects materialization.
    pub fn get(self, time: Time) -> Option<Resolved<Value>> {
        self.stage
            .read_property(self.path, time, |v| Some(v.clone()))
    }
    /// Reads and materializes the selected numeric array, exposing decode failure.
    pub fn try_get(self, time: Time) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        checked_read(self.stage, self.path, time)
    }
    /// Raw composed attribute connections, independent of its value.
    pub fn connections(self) -> Vec<TargetPath> {
        self.stage
            .resolve_target_list_path(self.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Authored property metadata, composed separately from its value.
    pub fn metadata(self, key: TokenId) -> Option<Resolved<ResolvedValue>> {
        self.stage
            .resolve_property_metadata(self.path.prim_path(), self.path.property(), key)
    }
    /// Caller-owned retained query for this concrete attribute path.
    pub fn query(self) -> AttributeQuery {
        AttributeQuery::new(self.path)
    }
    /// Distinct composed sample times in stage time, including value clips.
    pub fn sample_times(self) -> Vec<f64> {
        self.stage
            .property_sample_times(self.path.prim_path(), self.path.property())
    }
    /// Conservative whether a numeric-time query can vary with time.
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
    pub fn targets(self) -> Vec<TargetPath> {
        self.stage
            .resolve_target_list_path(self.path)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    /// Ordered terminal targets after recursively forwarding relationships.
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
#[derive(Clone, Debug)]
struct QueryStamp {
    index: Option<Arc<crate::prim_index::PrimIndexData>>,
    type_info: Option<Arc<PrimTypeInfo>>,
    clips: crate::value_clips::ClipQueryIdentity,
    provenance: bool,
}
impl QueryStamp {
    fn matches(&self, stage: &Stage, path: PropertyPath) -> bool {
        let current = stage.prims.get(&path.prim_path());
        same_arc(self.index.as_ref(), current.map(|i| &i.data))
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
    /// Evaluates against the supplied current snapshot, refreshing as needed.
    /// The caller synchronizes `LiveStage` before passing its snapshot.
    pub fn get(&mut self, stage: &Stage, time: Time) -> Option<Resolved<Value>> {
        self.evaluate(stage, time, false).ok().flatten()
    }
    /// Evaluates and materializes the selected numeric array with typed failure.
    /// Repeated reads of the same mode/time/snapshot reuse failures as well as values.
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
    if let Some(Resolved {
        value: Value::TypedArray(array),
        ..
    }) = &mut value
    {
        *array = array.try_materialize().map_err(Clone::clone)?.clone();
    }
    Ok(value)
}

fn check_decode(
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
