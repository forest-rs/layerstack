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

use alloc::{boxed::Box, vec::Vec};

use hashbrown::HashMap;

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
    /// Relocates arc (not implemented in v0.1).
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
    /// A property spec with all of its authored slots.
    Property(Box<PropertySpec>),
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
        Self::Property(Box::new(spec))
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// The arc expansions contributing to the prim; every key below names
    /// one of its nodes.
    pub(crate) graph: PrimIndexGraph,
    pub(crate) opinions_by_field: HashMap<FieldKey, Vec<Opinion>>,
    /// Every contributing property declaration, keyed by field. The
    /// composed type is the strongest surviving declaration's, so filtering
    /// out an opinion's declaration lets a weaker one take over.
    pub(crate) property_types_by_field: HashMap<TokenId, Vec<(OpinionKey, PropertyType)>>,
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
        self.opinions_by_field
            .entry(FieldKey::of(&opinion))
            .or_default()
            .push(opinion);
    }

    /// The opinions of the property `name`.
    pub(crate) fn property_opinions(&self, name: TokenId) -> Option<&[Opinion]> {
        self.opinions_by_field
            .get(&FieldKey::Property(name))
            .map(Vec::as_slice)
    }

    /// The opinions of the prim metadata field `key`.
    pub(crate) fn metadata_opinions(&self, key: TokenId) -> Option<&[Opinion]> {
        self.opinions_by_field
            .get(&FieldKey::Metadata(key))
            .map(Vec::as_slice)
    }

    pub(crate) fn add_source(&mut self, key: OpinionKey) {
        self.sources.push(key);
    }

    pub(crate) fn add_property_type(
        &mut self,
        field: TokenId,
        key: OpinionKey,
        property_type: PropertyType,
    ) {
        self.property_types_by_field
            .entry(field)
            .or_default()
            .push((key, property_type));
    }

    /// Returns the type of the strongest declaration of `field`.
    pub(crate) fn property_type_for(&self, field: &TokenId) -> Option<&PropertyType> {
        self.property_types_by_field
            .get(field)?
            .iter()
            .min_by(|(a, _), (b, _)| self.graph.cmp_keys(a, b))
            .map(|(_, property_type)| property_type)
    }

    /// Keeps only the sources, opinions and property declarations whose key
    /// satisfies `keep`, dropping fields left without opinions or
    /// declarations. `keep` also sees the prim's graph.
    pub(crate) fn retain_keys(
        &mut self,
        mut keep: impl FnMut(&PrimIndexGraph, &OpinionKey) -> bool,
    ) {
        let graph = &self.graph;
        self.sources.retain(|key| keep(graph, key));
        for opinions in self.opinions_by_field.values_mut() {
            opinions.retain(|opinion| keep(graph, &opinion.key));
        }
        self.opinions_by_field
            .retain(|_, opinions| !opinions.is_empty());
        for declarations in self.property_types_by_field.values_mut() {
            declarations.retain(|(key, _)| keep(graph, key));
        }
        self.property_types_by_field
            .retain(|_, declarations| !declarations.is_empty());
    }

    /// Sorts every opinion and source strongest first, by
    /// [`PrimIndexGraph::cmp_keys`].
    pub(crate) fn finalize(&mut self) {
        self.graph.rank();
        let graph = &self.graph;
        for opinions in self.opinions_by_field.values_mut() {
            opinions.sort_by(|a, b| graph.cmp_keys(&a.key, &b.key));
        }
        self.sources.sort_by(|a, b| graph.cmp_keys(a, b));
    }
}
