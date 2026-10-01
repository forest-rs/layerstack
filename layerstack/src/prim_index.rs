// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition result types.
//!
//! Composition produces per-prim indexes (similar to OpenUSD's internal
//! `PrimIndex`) which the [`crate::stage::Stage`] queries for population and
//! value resolution. Each holds the prim's composition graph
//! ([`PrimIndexGraph`]) and its opinions, each keyed by the graph node it
//! came from and sorted strongest first.
//!
//! Spec: AOUSD Core §10 (composition arcs and strength ordering) and §12 (value resolution).

use alloc::{sync::Arc, vec::Vec};

use core::ops::Range;

use crate::{
    doc::{FieldValue, LayerId, LayerOffset, Value},
    interner::TokenId,
    listop::ListOp,
    path::{PathId, TargetPath},
    prim_index_graph::{NodeId, PrimIndexGraph},
    property::{PropertySpec, PropertyType, TimeSample},
    spec_path::SpecPath,
    spline::SplineData,
};

/// Composition arc kind (LIVERPS ordering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArcKind {
    /// Local opinions from the layer stack.
    Local,
    /// Inherits arc.
    Inherits,
    /// Variants arc.
    Variants,
    /// Relocates arc.
    Relocates,
    /// References arc.
    References,
    /// Payloads arc.
    Payloads,
    /// Specializes arc.
    Specializes,
}

impl ArcKind {
    pub(crate) fn strength_rank(self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Inherits => 1,
            Self::Variants => 2,
            Self::Relocates => 3,
            Self::References => 4,
            Self::Payloads => 5,
            Self::Specializes => 6,
        }
    }
}

/// Identifies one authored opinion of a composed prim and ranks it.
///
/// The opinion's arc path lives in the prim's [`PrimIndexGraph`]: `node`
/// names the site the opinion was read from, and the node ranks the
/// opinion against the prim's other opinions. The remaining fields identify
/// the authored spec within that node and break ties between opinions of
/// equally strong nodes.
///
/// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpinionKey {
    /// The node of the prim's [`PrimIndexGraph`] this opinion belongs to
    /// (see [`crate::Stage::explain_prim_graph`]).
    pub node: NodeId,
    /// Strength position within the node's layer stack (0 is strongest).
    pub layer_strength: u16,
    /// Source layer identifier.
    pub layer_id: LayerId,
    /// Concrete prim path used to look up the authored `PrimSpec`.
    pub lookup_path: PathId,
    /// Source spec path identity used for composed provenance.
    pub spec_path: SpecPath,
}

impl OpinionKey {
    /// Returns a clone with a different provenance path.
    #[must_use]
    pub fn with_spec_path(&self, spec_path: SpecPath) -> Self {
        let mut out = self.clone();
        out.spec_path = spec_path;
        out
    }
}

/// An authored opinion for a destination prim+field, with a strength key.
#[derive(Clone, Debug, PartialEq)]
pub struct Opinion {
    /// Strength key used for sorting.
    pub key: OpinionKey,
    /// The field token being authored: a prim metadata field name or a
    /// property name.
    pub field: TokenId,
    /// The authored content this opinion contributes.
    pub value: OpinionValue,
    /// Accumulated layer offset from all arcs leading to this opinion (§12.3.2.1).
    pub layer_offset: LayerOffset,
}

/// The authored content one [`Opinion`] contributes.
///
/// A composed prim keeps prim metadata fields and properties in one name
/// space, keyed by [`Opinion::field`]. A metadata opinion carries its single
/// value; a property opinion carries the whole authored [`PropertySpec`], so
/// its default, time samples, spline, targets and metadata stay separate and
/// value precedence is applied at query time.
///
/// Spec: AOUSD Core §12.2 (metadata resolution), §12.3 (attribute value
/// resolution), §12.4 (relationships and connections).
#[derive(Clone, Debug, PartialEq)]
pub enum OpinionValue {
    /// A prim metadata field value.
    Field(FieldValue),
    /// A shared property snapshot with all of its authored slots.
    /// Composition detaches only for namespace-dependent changes; default-only
    /// edits retain the shared sample and metadata buffers.
    Property(Arc<PropertySpec>),
}

impl OpinionValue {
    /// Returns the value a default-time query reads from this opinion: a
    /// metadata [`FieldValue::Value`], or a property's authored
    /// [`PropertySpec::default`].
    ///
    /// Spec: AOUSD Core §12.3.1 (default values ignore time samples).
    #[must_use]
    pub fn default_value(&self) -> Option<&Value> {
        match self {
            Self::Field(FieldValue::Value(value)) => Some(value),
            Self::Field(_) => None,
            Self::Property(spec) => spec.default.as_ref(),
        }
    }

    /// Returns the authored, non-empty time samples of a property opinion.
    ///
    /// An explicitly authored empty sample map contributes no value, as in
    /// OpenUSD's `_HasTimeSamples` (`pxr/usd/usd/stage.cpp`).
    #[must_use]
    pub fn time_samples(&self) -> Option<&[TimeSample]> {
        match self {
            Self::Property(spec) => spec.time_samples.as_deref().filter(|s| !s.is_empty()),
            Self::Field(_) => None,
        }
    }

    /// Returns the authored spline of a property opinion.
    #[must_use]
    pub fn spline(&self) -> Option<&SplineData> {
        match self {
            Self::Property(spec) => spec.spline.as_ref(),
            Self::Field(_) => None,
        }
    }

    /// Returns the authored target paths: a property's connection or target
    /// path list, or a metadata [`FieldValue::PathListOp`].
    #[must_use]
    pub fn targets(&self) -> Option<&ListOp<TargetPath>> {
        match self {
            Self::Field(FieldValue::PathListOp(list)) => Some(list),
            Self::Field(_) => None,
            Self::Property(spec) => spec.targets.as_ref(),
        }
    }

    /// Returns the metadata field value, for a metadata opinion.
    #[must_use]
    pub fn as_field(&self) -> Option<&FieldValue> {
        match self {
            Self::Field(value) => Some(value),
            Self::Property(_) => None,
        }
    }

    /// Returns the property spec, for a property opinion.
    #[must_use]
    pub fn as_property(&self) -> Option<&PropertySpec> {
        match self {
            Self::Property(spec) => Some(spec),
            Self::Field(_) => None,
        }
    }

    /// Returns `true` when this opinion authors a value block as its default.
    #[must_use]
    pub fn is_blocked_default(&self) -> bool {
        matches!(self.default_value(), Some(Value::Blocked))
    }
}

impl From<FieldValue> for OpinionValue {
    fn from(value: FieldValue) -> Self {
        Self::Field(value)
    }
}

impl From<PropertySpec> for OpinionValue {
    fn from(spec: PropertySpec) -> Self {
        Self::Property(Arc::new(spec))
    }
}

/// The identity of a composed field: a prim metadata field or a property.
///
/// A prim's metadata fields and its properties are different objects even
/// when they share a name (`def "P" (kind = "component") { double kind }`),
/// so they are indexed and resolved apart.
///
/// Spec: AOUSD Core §7.3 (property specs are children of the prim spec;
/// metadata are fields of the spec itself), §7.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum FieldKey {
    /// A prim metadata field.
    Metadata(TokenId),
    /// A property.
    Property(TokenId),
}

impl FieldKey {
    /// The key an opinion is indexed under.
    pub(crate) fn of(opinion: &Opinion) -> Self {
        match opinion.value {
            OpinionValue::Field(_) => Self::Metadata(opinion.field),
            OpinionValue::Property(_) => Self::Property(opinion.field),
        }
    }
}

/// A per-prim composition result, keyed by field identity.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrimIndex {
    /// Schema identity captured with this composed prim snapshot.
    pub(crate) type_info: Option<Arc<crate::stage::PrimTypeInfo>>,
    /// The arc expansions contributing to the prim; every key below names
    /// one of its nodes.
    pub(crate) graph: PrimIndexGraph,
    /// All opinions, built directly here and grouped by field at finalization.
    pub(crate) opinions: Vec<Opinion>,
    /// Sorted field identities and their contiguous opinion ranges.
    pub(crate) fields: Vec<(FieldKey, Range<usize>)>,
    pub(crate) sources: Vec<OpinionKey>,
}

impl PrimIndex {
    /// An empty prim index over `graph`.
    pub(crate) fn new(graph: PrimIndexGraph) -> Self {
        Self {
            graph,
            ..Self::default()
        }
    }

    pub(crate) fn add_opinion(&mut self, opinion: Opinion) {
        debug_assert!(self.fields.is_empty(), "append before finalization");
        // Sparse prims should not reserve four large records on first use.
        if self.opinions.capacity() == 0 {
            self.opinions.reserve_exact(1);
        }
        self.opinions.push(opinion);
    }

    /// The opinions of the property `name`.
    pub(crate) fn property_opinions(&self, name: TokenId) -> Option<&[Opinion]> {
        self.field_opinions(FieldKey::Property(name))
    }

    /// The opinions of the prim metadata field `key`.
    pub(crate) fn metadata_opinions(&self, key: TokenId) -> Option<&[Opinion]> {
        self.field_opinions(FieldKey::Metadata(key))
    }

    fn field_range(&self, field: FieldKey) -> Option<Range<usize>> {
        let slot = self
            .fields
            .binary_search_by_key(&field, |(key, _)| *key)
            .ok()?;
        Some(self.fields[slot].1.clone())
    }

    fn field_opinions(&self, field: FieldKey) -> Option<&[Opinion]> {
        Some(&self.opinions[self.field_range(field)?])
    }

    pub(crate) fn field_opinions_mut(&mut self, field: FieldKey) -> Option<&mut [Opinion]> {
        let range = self.field_range(field)?;
        Some(&mut self.opinions[range])
    }

    /// Rebuilds ranges after grouping or removing opinions. No authored data
    /// moves into a second representation: fields only index this buffer.
    fn index_fields(&mut self) {
        self.fields.clear();
        if self.fields.capacity() == 0 && !self.opinions.is_empty() {
            self.fields.reserve_exact(1);
        }
        for (position, opinion) in self.opinions.iter().enumerate() {
            let field = FieldKey::of(opinion);
            if let Some((last, range)) = self.fields.last_mut()
                && *last == field
            {
                range.end = position + 1;
            } else {
                self.fields.push((field, position..position + 1));
            }
        }
    }

    /// Groups fields while preserving insertion order within each stack.
    fn group_by_field(&mut self) {
        if self
            .opinions
            .is_sorted_by(|a, b| FieldKey::of(a) <= FieldKey::of(b))
        {
            self.index_fields();
            return;
        }
        // Count field contributions before moving payloads. Sorting only
        // field identities avoids comparing graph keys across unrelated fields.
        let mut keys: Vec<FieldKey> = self.opinions.iter().map(FieldKey::of).collect();
        keys.sort_unstable();
        keys.dedup();
        self.fields.clear();
        self.fields.reserve(keys.len());
        self.fields.extend(keys.iter().map(|&key| (key, 0..0)));
        for opinion in &self.opinions {
            let field = keys
                .binary_search(&FieldKey::of(opinion))
                .expect("counted field");
            self.fields[field].1.end += 1;
        }
        let mut offset = 0;
        for (_, range) in &mut self.fields {
            let count = range.end;
            *range = offset..offset;
            offset += count;
        }
        let mut order = alloc::vec![0; self.opinions.len()];
        for (source, opinion) in self.opinions.iter().enumerate() {
            let field = keys
                .binary_search(&FieldKey::of(opinion))
                .expect("counted field");
            let range = &mut self.fields[field].1;
            order[range.end] = source;
            range.end += 1;
        }
        // Each cycle places records in their destination field once. The
        // increasing source indices preserve equal-key insertion order.
        for start in 0..order.len() {
            let mut current = start;
            while order[current] != start {
                let next = order[current];
                self.opinions.swap(current, next);
                order[current] = current;
                current = next;
            }
            order[current] = current;
        }
    }

    /// Groups hand-built indexes that do not have a ranked composition graph.
    #[cfg(test)]
    pub(crate) fn group_fields(&mut self) {
        if self.fields.is_empty() {
            self.group_by_field();
        }
    }

    pub(crate) fn retain_opinions(
        &mut self,
        mut keep: impl FnMut(&PrimIndexGraph, &Opinion) -> bool,
    ) {
        let before = self.opinions.len();
        self.opinions.retain(|opinion| keep(&self.graph, opinion));
        if self.opinions.len() != before && !self.fields.is_empty() {
            self.index_fields();
        }
    }

    pub(crate) fn add_source(&mut self, key: OpinionKey) {
        self.sources.push(key);
    }

    /// Returns the type of the strongest surviving property declaration.
    /// Untyped opinions do not hide weaker declarations (AOUSD Core §12).
    /// Stage queries read finalized opinions, already strongest first.
    pub(crate) fn property_type_for(&self, field: &TokenId) -> Option<&PropertyType> {
        self.property_opinions(*field)?
            .iter()
            .find_map(|opinion| opinion.value.as_property()?.type_name.as_ref())
    }

    /// Keeps sources and opinions, including their declarations, whose key
    /// satisfies `keep`, dropping fields left without opinions. `keep` also
    /// sees the prim's graph.
    pub(crate) fn retain_keys(
        &mut self,
        mut keep: impl FnMut(&PrimIndexGraph, &OpinionKey) -> bool,
    ) {
        self.sources.retain(|key| keep(&self.graph, key));
        self.retain_opinions(|graph, opinion| keep(graph, &opinion.key));
    }

    /// Groups opinions by field, sorting each stack and the sources
    /// strongest first by [`PrimIndexGraph::cmp_keys`].
    pub(crate) fn finalize(&mut self) {
        self.graph.rank();
        self.group_by_field();
        let graph = &self.graph;
        for (_, range) in &self.fields {
            self.opinions[range.clone()].sort_by(|a, b| graph.cmp_keys(&a.key, &b.key));
        }
        self.sources.sort_by(|a, b| graph.cmp_keys(a, b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStore;

    #[test]
    fn field_identity_strength_and_removal_survive_grouping() {
        let mut store = InMemoryStore::default();
        let prim = store.path("/Emitter");
        let a = store.tokens.intern("a");
        let b = store.tokens.intern("b");
        let site = SpecPath::from_prim_path(prim, &store.paths);
        let mut index = PrimIndex::new(PrimIndexGraph::from_arcs(&site, 1, []));
        let key = |strength| OpinionKey {
            node: NodeId::ROOT,
            layer_strength: strength,
            layer_id: LayerId(u64::from(strength) + 1),
            lookup_path: prim,
            spec_path: site.clone(),
        };
        // Interleave fields and author weak before strong. Metadata and a
        // property may use the same token without sharing an opinion stack.
        for (field, strength, value) in [
            (
                a,
                1,
                OpinionValue::from(PropertySpec::attribute().with_default(Value::Int(1))),
            ),
            (a, 0, OpinionValue::Field(FieldValue::Value(Value::Int(3)))),
            (
                b,
                0,
                OpinionValue::from(PropertySpec::attribute().with_default(Value::Int(2))),
            ),
            (
                a,
                0,
                OpinionValue::from(PropertySpec::attribute().with_default(Value::Int(4))),
            ),
        ] {
            index.add_opinion(Opinion {
                key: key(strength),
                field,
                value,
                layer_offset: LayerOffset::IDENTITY,
            });
        }
        // A deep stack also exercises grouping without moving large payloads
        // at every comparison, including a repeated key whose order matters.
        for strength in 0..40 {
            index.add_opinion(Opinion {
                key: key(strength),
                field: b,
                value: PropertySpec::attribute().with_default(Value::Int(9)).into(),
                layer_offset: LayerOffset::IDENTITY,
            });
        }
        index.finalize();
        assert_eq!(
            index.property_opinions(b).unwrap()[0]
                .value
                .as_property()
                .unwrap()
                .default,
            Some(Value::Int(2)),
            "equal keys preserve authored insertion order"
        );
        assert_eq!(
            index.property_opinions(a).unwrap().len(),
            2,
            "both layers contribute to a"
        );
        assert_eq!(
            index.property_opinions(a).unwrap()[0].key.layer_strength,
            0,
            "strong layer comes first"
        );
        assert_eq!(
            index.metadata_opinions(a).unwrap().len(),
            1,
            "metadata stays separate"
        );
        index.retain_opinions(|_, opinion| {
            !(opinion.field == b
                || FieldKey::of(opinion) == FieldKey::Property(a)
                    && opinion.key.layer_strength == 0)
        });
        assert!(
            index.property_opinions(b).is_none(),
            "removed fields disappear"
        );
        assert_eq!(
            index.property_opinions(a).unwrap()[0]
                .value
                .as_property()
                .unwrap()
                .default,
            Some(Value::Int(1)),
            "removing the winner exposes the weaker property"
        );
        assert_eq!(
            index.metadata_opinions(a).unwrap()[0].value,
            OpinionValue::Field(FieldValue::Value(Value::Int(3))),
            "a property removal preserves metadata with the same token and key"
        );
        index.retain_keys(|_, _| false);
        assert!(
            index.property_opinions(a).is_none(),
            "retiring all sources removes property ranges"
        );
        assert!(
            index.metadata_opinions(a).is_none(),
            "retiring all sources removes metadata ranges"
        );
    }
}
