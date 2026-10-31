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

use crate::sort;
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

/// Identifies the site a composed prim's opinions are read from and ranks
/// it.
///
/// The site's arc path lives in the prim's [`PrimIndexGraph`]: `node`
/// names the node the site belongs to, and the node, then
/// `layer_strength` within the node's layer stack, rank the site against
/// the prim's other sites. The remaining fields identify the authored spec.
///
/// Spec: AOUSD Core §10.4 (strength ordering).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OpinionKey {
    /// The node of the prim's [`PrimIndexGraph`] this site belongs to
    /// (see [`crate::Prim::composition_graph`]).
    pub node: NodeId,
    /// Strength position within the node's layer stack (0 is strongest).
    pub layer_strength: u16,
    /// Source layer identifier.
    pub layer_id: LayerId,
    /// Concrete prim path used to look up the authored `PrimSpec`.
    pub lookup_path: PathId,
    /// Source spec path identity used for composed provenance: the prim
    /// spec of a source, or the property or field of one opinion
    /// ([`OpinionRef::key`]).
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

/// A site a composed prim's opinions are read from: one layer of the layer
/// stack of one node of the prim's composition graph, and the offset that
/// maps the layer's times into stage time.
///
/// A prim lists its sources strongest first ([`crate::Prim::sources`]),
/// and each of its [`Opinion`]s names the one it was read from by its
/// [`SourceId`]: the opinions read from one spec share its key and offset.
///
/// OpenUSD: an entry of `PcpPrimIndex`'s prim stack (`Pcp_CompressedSdSite`,
/// a node and a layer of its layer stack), whose node and layer give its
/// strength and offset.
///
/// Spec: AOUSD Core §10.4 (strength ordering), §12.3.2.1 (layer offset and
/// scale).
#[derive(Clone, Debug, PartialEq)]
pub struct OpinionSource {
    /// The site and its strength. Its spec path names the prim spec the
    /// source's opinions are read from.
    pub key: OpinionKey,
    /// The offset from the source layer's times to stage times: the
    /// offsets of every arc leading to the source's node composed with the
    /// layer's own offset in the node's layer stack (§12.3.2.1).
    pub layer_offset: LayerOffset,
}

/// The position of an opinion's source among the sources of its prim,
/// which are strongest first: of two opinions of one prim, the one whose
/// source comes first is the stronger.
///
/// It indexes the sources of the list the opinion was read from
/// ([`Opinions::sources`], [`crate::Prim::sources`]).
///
/// Spec: AOUSD Core §10.4 (strength ordering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(u32);

impl SourceId {
    /// The id of the source at `position`.
    pub(crate) fn at(position: usize) -> Self {
        Self(u32::try_from(position).expect("prim index source count overflow"))
    }

    /// The source's position in the sources it indexes.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What one source of a composed prim authors for one of its fields: a
/// metadata field or a property.
///
/// The opinion names its source, whose key and layer offset it shares with
/// the other opinions read from it; [`Opinions`] pairs each opinion with
/// its source ([`OpinionRef`]). Two opinions are equal when they name the
/// same source position and author the same value; compare
/// [`OpinionRef`]s to compare opinions of different prims.
#[derive(Clone, Debug, PartialEq)]
pub struct Opinion {
    /// The source the opinion was read from (see [`Opinions::source`]).
    pub source: SourceId,
    /// The field token being authored: a prim metadata field name or a
    /// property name.
    pub field: TokenId,
    /// The authored content this opinion contributes.
    pub value: OpinionValue,
}

/// Opinions of a composed prim, strongest first, with the sources they
/// were read from: the opinions of one field
/// ([`crate::Property::opinions`], [`crate::Prim::metadata_opinions`]).
///
/// Iterating yields each opinion with its source ([`OpinionRef`]).
///
/// OpenUSD: `UsdProperty::GetPropertyStackWithLayerOffsets`.
///
/// Spec: AOUSD Core §10.4 (strength ordering), §12.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opinions<'a> {
    records: &'a [Opinion],
    sources: &'a [OpinionSource],
}

impl<'a> Opinions<'a> {
    /// `records`, whose sources `sources` holds.
    pub(crate) fn new(records: &'a [Opinion], sources: &'a [OpinionSource]) -> Self {
        debug_assert!(
            records
                .iter()
                .all(|opinion| opinion.source.index() < sources.len()),
            "every opinion's source is one of the sources"
        );
        Self { records, sources }
    }

    /// The number of opinions.
    #[must_use]
    pub fn len(self) -> usize {
        self.records.len()
    }

    /// Whether there are no opinions.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.records.is_empty()
    }

    /// The opinion at `position` (0 is the strongest), with its source.
    #[must_use]
    pub fn get(self, position: usize) -> Option<OpinionRef<'a>> {
        self.records.get(position).map(|opinion| self.pair(opinion))
    }

    /// The strongest opinion, with its source.
    #[must_use]
    pub fn first(self) -> Option<OpinionRef<'a>> {
        self.get(0)
    }

    /// The opinions, strongest first, each with its source.
    pub fn iter(self) -> OpinionIter<'a> {
        OpinionIter {
            records: self.records.iter(),
            sources: self.sources,
        }
    }

    /// The opinion records, strongest first, without their sources.
    #[must_use]
    pub fn records(self) -> &'a [Opinion] {
        self.records
    }

    /// The sources the opinions index: every source of their prim, those
    /// that author none of these opinions included.
    #[must_use]
    pub fn sources(self) -> &'a [OpinionSource] {
        self.sources
    }

    /// The source `opinion`, one of these opinions, was read from.
    #[must_use]
    pub fn source(self, opinion: &Opinion) -> &'a OpinionSource {
        &self.sources[opinion.source.index()]
    }

    /// The opinions from `start` on.
    #[must_use]
    pub(crate) fn from(self, start: usize) -> Self {
        Self {
            records: &self.records[start..],
            sources: self.sources,
        }
    }

    /// `records`, opinions read from these opinions' sources (copies of
    /// some of them, with other values), with those sources.
    pub(crate) fn with_records<'b>(self, records: &'b [Opinion]) -> Opinions<'b>
    where
        'a: 'b,
    {
        Opinions::new(records, self.sources)
    }

    /// `opinion`, one of these opinions, with its source.
    pub(crate) fn pair(self, opinion: &'a Opinion) -> OpinionRef<'a> {
        OpinionRef {
            opinion,
            source: self.source(opinion),
        }
    }
}

/// Two lists are equal when they hold equal opinions ([`OpinionRef`]) in
/// the same order.
impl PartialEq for Opinions<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<'a> IntoIterator for Opinions<'a> {
    type Item = OpinionRef<'a>;
    type IntoIter = OpinionIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An iterator over [`Opinions`], strongest first.
#[derive(Clone, Debug)]
pub struct OpinionIter<'a> {
    records: core::slice::Iter<'a, Opinion>,
    sources: &'a [OpinionSource],
}

impl<'a> OpinionIter<'a> {
    fn pair(&self, opinion: &'a Opinion) -> OpinionRef<'a> {
        OpinionRef {
            opinion,
            source: &self.sources[opinion.source.index()],
        }
    }
}

impl<'a> Iterator for OpinionIter<'a> {
    type Item = OpinionRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let opinion = self.records.next()?;
        Some(self.pair(opinion))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.records.size_hint()
    }
}

impl DoubleEndedIterator for OpinionIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let opinion = self.records.next_back()?;
        Some(self.pair(opinion))
    }
}

impl ExactSizeIterator for OpinionIter<'_> {}

/// One opinion with the source it was read from.
///
/// Two are equal when they author the same value for the same field from
/// sources with the same key and layer offset.
#[derive(Clone, Copy, Debug)]
pub struct OpinionRef<'a> {
    opinion: &'a Opinion,
    source: &'a OpinionSource,
}

impl<'a> OpinionRef<'a> {
    /// The opinion record.
    #[must_use]
    pub fn opinion(self) -> &'a Opinion {
        self.opinion
    }

    /// The field the opinion authors: a prim metadata field name or a
    /// property name.
    #[must_use]
    pub fn field(self) -> TokenId {
        self.opinion.field
    }

    /// What the opinion authors.
    #[must_use]
    pub fn value(self) -> &'a OpinionValue {
        &self.opinion.value
    }

    /// The source the opinion was read from.
    #[must_use]
    pub fn source(self) -> &'a OpinionSource {
        self.source
    }

    /// The node of the prim's composition graph the opinion was read from.
    #[must_use]
    pub fn node(self) -> NodeId {
        self.source.key.node
    }

    /// The layer that authors the opinion.
    #[must_use]
    pub fn layer_id(self) -> LayerId {
        self.source.key.layer_id
    }

    /// The offset from the opinion's layer times to stage times, its
    /// source's.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offset and scale).
    #[must_use]
    pub fn layer_offset(self) -> LayerOffset {
        self.source.layer_offset
    }

    /// The spec path the opinion is authored at: its source's prim spec
    /// with the opinion's field as the property (`/World.radius`). A
    /// metadata field is named the same way (`/World.kind`). A source whose
    /// spec path already names a property or field names the opinion's
    /// spec as it is.
    #[must_use]
    pub fn spec_path(self) -> SpecPath {
        let path = &self.source.key.spec_path;
        // Only a value clip's opinions, whose source is the clip set's
        // `clips` metadata opinion, have such a source.
        if path.property().is_some() {
            path.clone()
        } else {
            path.with_property(self.opinion.field)
        }
    }

    /// The opinion's key: its source's, with [`Self::spec_path`] as the
    /// spec path.
    #[must_use]
    pub fn key(self) -> OpinionKey {
        self.source.key.with_spec_path(self.spec_path())
    }
}

impl PartialEq for OpinionRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.opinion.field == other.opinion.field
            && self.opinion.value == other.opinion.value
            && self.source.layer_offset == other.source.layer_offset
            && self.key() == other.key()
    }
}

/// An opinion with its source, owned: one no prim index keeps, such as a
/// value clip's, whose source is the clip set's metadata anchor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourcedOpinion {
    pub(crate) source: OpinionSource,
    /// The opinion, naming [`Self::source`] as its first source.
    pub(crate) opinion: Opinion,
}

impl SourcedOpinion {
    /// An opinion of `field` authoring `value`, read from `source`.
    pub(crate) fn new(source: OpinionSource, field: TokenId, value: OpinionValue) -> Self {
        Self {
            source,
            opinion: Opinion {
                source: SourceId(0),
                field,
                value,
            },
        }
    }

    /// The opinion with its source.
    pub(crate) fn as_ref(&self) -> OpinionRef<'_> {
        OpinionRef {
            opinion: &self.opinion,
            source: &self.source,
        }
    }
}

/// Opinions with the sources they index, owned: a chain no prim index
/// keeps, such as a property's opinions merged with value clip opinions.
#[derive(Clone, Debug, Default)]
pub(crate) struct OpinionList {
    pub(crate) records: Vec<Opinion>,
    pub(crate) sources: Vec<OpinionSource>,
}

impl OpinionList {
    /// The opinions and their sources.
    pub(crate) fn view(&self) -> Opinions<'_> {
        Opinions::new(&self.records, &self.sources)
    }

    /// The id of a source equal to `source`, added when there is none.
    pub(crate) fn source_id(&mut self, source: &OpinionSource) -> SourceId {
        let at = match self.sources.iter().position(|known| known == source) {
            Some(at) => at,
            None => {
                self.sources.push(source.clone());
                self.sources.len() - 1
            }
        };
        SourceId::at(at)
    }

    /// Appends an opinion of `field` authoring `value`, read from `source`.
    pub(crate) fn push(&mut self, source: &OpinionSource, field: TokenId, value: OpinionValue) {
        let source = self.source_id(source);
        self.records.push(Opinion {
            source,
            field,
            value,
        });
    }

    /// Appends `opinion`, read from its source.
    pub(crate) fn push_ref(&mut self, opinion: OpinionRef<'_>) {
        self.push(opinion.source, opinion.field(), opinion.value().clone());
    }

    /// A list of `opinions`, in order, each source once.
    #[cfg(test)]
    pub(crate) fn chain<'a>(opinions: impl IntoIterator<Item = &'a SourcedOpinion>) -> Self {
        let mut list = Self::default();
        for opinion in opinions {
            list.push_ref(opinion.as_ref());
        }
        list
    }
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
    /// A [`Value::Deferred`] value is decoded, once, and the decoded value
    /// returned ([`Value::materialized`]); one that fails to decode is
    /// returned as it is. [`Self::has_default_value`] answers whether there
    /// is one without decoding it.
    ///
    /// Spec: AOUSD Core §12.3.1 (default values ignore time samples).
    #[must_use]
    pub fn default_value(&self) -> Option<&Value> {
        self.authored_default().map(Value::materialized)
    }

    /// Whether [`Self::default_value`] returns a value, without decoding a
    /// deferred one.
    #[must_use]
    pub fn has_default_value(&self) -> bool {
        self.authored_default().is_some()
    }

    /// The default value as authored, a deferred one undecoded.
    fn authored_default(&self) -> Option<&Value> {
        match self {
            Self::Field(FieldValue::Value(value)) => Some(value),
            Self::Field(_) => None,
            Self::Property(spec) => spec
                .default
                .as_ref()
                .filter(|value| !matches!(value, Value::AnimationBlock)),
        }
    }

    /// Whether numeric resolution switches to weaker defaults at this site.
    /// Same-site samples or a spline take precedence over its default marker.
    /// OpenUSD `_GetResolveInfoWithClipsImpl`; AOUSD Core §12.3.
    #[must_use]
    pub fn blocks_animation(&self) -> bool {
        // A tag test rather than `== Some(Value::AnimationBlock)`: the
        // comparison would call `Value::eq`, a large function in another
        // codegen unit, for every opinion of every timed read.
        self.as_property().is_some_and(|spec| {
            spec.default
                .as_ref()
                .is_some_and(|value| matches!(value.materialized(), Value::AnimationBlock))
                && self.time_samples().is_none()
                && spec.spline.is_none()
        })
    }

    /// Returns the authored, non-empty time samples of a property opinion.
    ///
    /// An explicitly authored empty sample map contributes no value, as in
    /// OpenUSD's `_HasTimeSamples` (`pxr/usd/usd/stage.cpp`).
    #[must_use]
    pub fn time_samples(&self) -> Option<&[TimeSample]> {
        match self {
            Self::Property(spec) => spec.time_samples.get().filter(|s| !s.is_empty()),
            Self::Field(_) => None,
        }
    }

    /// Returns the authored spline of a property opinion.
    #[must_use]
    pub fn spline(&self) -> Option<&SplineData> {
        match self {
            Self::Property(spec) => spec.spline.as_deref(),
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
            Self::Property(spec) => spec.targets.as_deref(),
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
/// when they share a name (`def "P" (kind = "component") { double kind; }`),
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
    /// The type name the index's sources author
    /// ([`crate::Stage::source_type_name`]), found when it was indexed;
    /// `None` for an index built otherwise.
    pub(crate) type_name: Option<Option<TokenId>>,
    /// The arc expansions contributing to the prim; every key below names
    /// one of its nodes.
    pub(crate) graph: PrimIndexGraph,
    /// Namespace-independent records. Equivalent prototype members share
    /// these; mutation detaches while older stages retain their snapshot.
    pub(crate) data: Arc<PrimIndexData>,
    /// What the arguments of the dynamic payloads the index follows were
    /// composed from; `None` when it follows none.
    pub(crate) dynamic: Option<Arc<crate::dynamic_format::DynamicDependency>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PrimIndexData {
    /// Every opinion, each naming one of [`Self::sources`].
    pub(crate) opinions: Vec<Opinion>,
    /// Each field's opinions, a range of [`Self::opinions`]
    /// ([`Self::opinions_in`]), sorted by field.
    pub(crate) fields: Vec<(FieldKey, Range<u32>)>,
    /// The sites the opinions are read from, strongest first once
    /// finalized ([`PrimIndex::finalize`]).
    pub(crate) sources: Vec<OpinionSource>,
}

impl PrimIndexData {
    /// The opinions of one of [`Self::fields`]' ranges.
    pub(crate) fn opinions_in(&self, range: &Range<u32>) -> Opinions<'_> {
        Opinions::new(&self.opinions[span(range)], &self.sources)
    }

    /// Exact record identity for sharing; public value equality stays IEEE.
    pub(crate) fn same_records(&self, other: &Self) -> bool {
        use crate::edit::same::Same;
        self.fields == other.fields
            && self.sources.len() == other.sources.len()
            && self.sources.iter().zip(&other.sources).all(|(a, b)| {
                a.key == b.key
                    && a.layer_offset.offset.to_bits() == b.layer_offset.offset.to_bits()
                    && a.layer_offset.scale.to_bits() == b.layer_offset.scale.to_bits()
            })
            && self.opinions.len() == other.opinions.len()
            && self.opinions.iter().zip(&other.opinions).all(|(a, b)| {
                a.source == b.source
                    && a.field == b.field
                    && match (&a.value, &b.value) {
                        (OpinionValue::Property(a), OpinionValue::Property(b)) => {
                            Arc::ptr_eq(a, b) || a.as_ref().same(b)
                        }
                        (OpinionValue::Field(a), OpinionValue::Field(b)) => a.same(b),
                        _ => false,
                    }
            })
    }
}

/// `range` as indices of [`PrimIndexData::opinions`].
fn span(range: &Range<u32>) -> Range<usize> {
    range.start as usize..range.end as usize
}

/// The position `at` in [`PrimIndexData::opinions`], as a field range
/// holds it.
fn position(at: usize) -> u32 {
    u32::try_from(at).expect("prim index opinion count overflow")
}

impl core::ops::Deref for PrimIndex {
    type Target = PrimIndexData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl core::ops::DerefMut for PrimIndex {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.data)
    }
}

impl PrimIndex {
    /// An empty prim index over `graph`.
    pub(crate) fn new(graph: PrimIndexGraph) -> Self {
        // Not `..Self::default()`: an empty graph allocates its shape.
        Self {
            type_info: None,
            type_name: None,
            graph,
            data: Arc::default(),
            dynamic: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn add_opinion(&mut self, opinion: Opinion) {
        self.add_opinions(core::iter::once(opinion));
    }

    /// Gives back the room the sources did not use, and the opinions' when
    /// they left much of theirs unused.
    pub(crate) fn trim(&mut self) {
        let data = Arc::make_mut(&mut self.data);
        data.sources.shrink_to_fit();
        // Opinions grown by a few batches are copied back only when they
        // left more than a quarter unused.
        if data.opinions.capacity() - data.opinions.len() > data.opinions.len() / 4 {
            data.opinions.shrink_to_fit();
        }
    }

    /// Appends one batch before finalization, preserving equal-key
    /// insertion order. Detach shared snapshots once and reserve the first
    /// batch exactly, by its size hint, so sparse prims do not reserve four
    /// large records.
    pub(crate) fn add_opinions(&mut self, opinions: impl Iterator<Item = Opinion>) {
        let len = opinions.size_hint().0;
        let mut opinions = opinions.peekable();
        if opinions.peek().is_none() {
            return;
        }
        let data = Arc::make_mut(&mut self.data);
        debug_assert!(data.fields.is_empty(), "append before finalization");
        if data.opinions.capacity() == 0 {
            data.opinions.reserve_exact(len);
        }
        data.opinions.extend(opinions);
    }

    /// The opinions of the property `name`.
    pub(crate) fn property_opinions(&self, name: TokenId) -> Option<Opinions<'_>> {
        self.field_opinions(FieldKey::Property(name))
    }

    /// The opinions of the prim metadata field `key`.
    pub(crate) fn metadata_opinions(&self, key: TokenId) -> Option<Opinions<'_>> {
        self.field_opinions(FieldKey::Metadata(key))
    }

    fn field_range(&self, field: FieldKey) -> Option<Range<usize>> {
        let slot = self
            .fields
            .binary_search_by_key(&field, |(key, _)| *key)
            .ok()?;
        Some(span(&self.fields[slot].1))
    }

    fn field_opinions(&self, field: FieldKey) -> Option<Opinions<'_>> {
        let range = self.field_range(field)?;
        Some(Opinions::new(&self.opinions[range], &self.sources))
    }

    pub(crate) fn field_opinions_mut(&mut self, field: FieldKey) -> Option<&mut [Opinion]> {
        let range = self.field_range(field)?;
        Some(&mut self.opinions[range])
    }

    /// Rebuilds ranges after grouping or removing opinions. No authored data
    /// moves into a second representation: fields only index this buffer.
    fn index_fields(&mut self) {
        let data = Arc::make_mut(&mut self.data);
        data.fields.clear();
        let runs = data
            .opinions
            .chunk_by(|a, b| FieldKey::of(a) == FieldKey::of(b))
            .count();
        data.fields.reserve_exact(runs);
        for (at, opinion) in data.opinions.iter().enumerate() {
            let field = FieldKey::of(opinion);
            let end = position(at + 1);
            if let Some((last, range)) = data.fields.last_mut()
                && *last == field
            {
                range.end = end;
            } else {
                data.fields.push((field, position(at)..end));
            }
        }
    }

    /// Groups fields while preserving insertion order within each stack.
    fn group_by_field(&mut self) {
        let data = Arc::make_mut(&mut self.data);
        if data
            .opinions
            .is_sorted_by(|a, b| FieldKey::of(a) <= FieldKey::of(b))
        {
            self.index_fields();
            return;
        }
        // Count field contributions before moving payloads. Sorting only
        // field identities avoids comparing graph keys across unrelated fields.
        let mut keys: Vec<FieldKey> = data.opinions.iter().map(FieldKey::of).collect();
        sort::sort_unstable(&mut keys);
        keys.dedup();
        data.fields.clear();
        data.fields.reserve_exact(keys.len());
        data.fields.extend(keys.iter().map(|&key| (key, 0..0)));
        for opinion in &data.opinions {
            let field = keys
                .binary_search(&FieldKey::of(opinion))
                .expect("counted field");
            data.fields[field].1.end += 1;
        }
        let mut offset = 0;
        for (_, range) in &mut data.fields {
            let count = range.end;
            *range = offset..offset;
            offset += count;
        }
        let mut order = alloc::vec![0; data.opinions.len()];
        for (source, opinion) in data.opinions.iter().enumerate() {
            let field = keys
                .binary_search(&FieldKey::of(opinion))
                .expect("counted field");
            let range = &mut data.fields[field].1;
            order[range.end as usize] = source;
            range.end += 1;
        }
        // Each cycle places records in their destination field once. The
        // increasing source indices preserve equal-key insertion order.
        for start in 0..order.len() {
            let mut current = start;
            while order[current] != start {
                let next = order[current];
                data.opinions.swap(current, next);
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

    /// Keeps the opinions `keep` accepts; the sources stay.
    pub(crate) fn retain_opinions(&mut self, keep: impl FnMut(&Opinion) -> bool) {
        let before = self.opinions.len();
        Arc::make_mut(&mut self.data).opinions.retain(keep);
        if self.opinions.len() != before && !self.fields.is_empty() {
            self.index_fields();
        }
    }

    /// Adds a source for opinions to name, returning its id.
    pub(crate) fn add_source(&mut self, source: OpinionSource) -> SourceId {
        let id = SourceId::at(self.sources.len());
        self.sources.push(source);
        id
    }

    /// Adds an opinion read from the site `key` through `layer_offset`,
    /// adding a source for them unless the index has one: `key`'s spec path
    /// names the opinion's property or field, its source's the prim spec.
    #[cfg(test)]
    pub(crate) fn add_keyed(
        &mut self,
        key: OpinionKey,
        layer_offset: LayerOffset,
        field: TokenId,
        value: OpinionValue,
    ) {
        let spec_path = key.spec_path.prim_spec();
        let source = OpinionSource {
            key: key.with_spec_path(spec_path),
            layer_offset,
        };
        let source = match self.sources.iter().position(|known| *known == source) {
            Some(at) => SourceId::at(at),
            None => self.add_source(source),
        };
        self.add_opinion(Opinion {
            source,
            field,
            value,
        });
    }

    /// Makes room for `layers` sources, unless there is room for some
    /// already; [`Self::trim`] gives back what is not used.
    pub(crate) fn reserve_sources(&mut self, layers: usize) {
        if self.sources.capacity() == 0 {
            Arc::make_mut(&mut self.data).sources.reserve_exact(layers);
        }
    }

    /// Returns the type of the strongest surviving property declaration.
    /// Untyped opinions do not hide weaker declarations (AOUSD Core §12).
    /// Stage queries read finalized opinions, already strongest first.
    pub(crate) fn property_type_for(&self, field: &TokenId) -> Option<&PropertyType> {
        self.property_opinions(*field)?
            .records()
            .iter()
            .find_map(|opinion| opinion.value.as_property()?.type_name.as_ref())
    }

    /// Groups opinions by field, sorting the sources strongest first by
    /// [`PrimIndexGraph::cmp_keys`] and then each field's opinions by their
    /// sources, so that equally strong opinions keep their insertion order.
    pub(crate) fn finalize(&mut self) {
        self.group_by_field();
        let graph = &self.graph;
        let data = Arc::make_mut(&mut self.data);
        if !data
            .sources
            .is_sorted_by(|a, b| graph.cmp_keys(&a.key, &b.key).is_le())
        {
            // `order[new]` is the source that moves to `new`; `moved[old]`
            // the position the source at `old` moves to. The sort is
            // stable, so equally strong sources keep their order.
            let mut order: Vec<usize> = (0..data.sources.len()).collect();
            sort::sort_by(&mut order, |&a, &b| {
                graph.cmp_keys(&data.sources[a].key, &data.sources[b].key)
            });
            let mut moved = alloc::vec![0; order.len()];
            for (new, &old) in order.iter().enumerate() {
                moved[old] = new;
            }
            for opinion in &mut data.opinions {
                opinion.source = SourceId::at(moved[opinion.source.index()]);
            }
            // Each swap puts one source in its place.
            for at in 0..moved.len() {
                while moved[at] != at {
                    let to = moved[at];
                    data.sources.swap(at, to);
                    moved.swap(at, to);
                }
            }
        }
        for (_, range) in &data.fields {
            sort::sort_by_key(&mut data.opinions[span(range)], |opinion| opinion.source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStore;

    #[test]
    fn batched_opinions_preserve_shared_snapshots_and_equal_key_order() {
        let store = InMemoryStore::default();
        let prim = store.path("/Emitter");
        let field = store.tokens.intern("value");
        let site = SpecPath::from_prim_path(prim);
        let mut index = PrimIndex::new(PrimIndexGraph::from_arcs(&site, 1, []));
        let source = index.add_source(OpinionSource {
            key: OpinionKey {
                node: NodeId::ROOT,
                layer_strength: 0,
                layer_id: LayerId(1),
                lookup_path: prim,
                spec_path: site.clone(),
            },
            layer_offset: LayerOffset::IDENTITY,
        });
        let opinion = |value| Opinion {
            source,
            field,
            value: PropertySpec::attribute()
                .with_default(Value::Int(value))
                .into(),
        };
        index.add_opinion(opinion(1));
        let mut snapshot = index.clone();
        index.add_opinions([opinion(2), opinion(3)].into_iter());
        snapshot.finalize();
        index.finalize();
        let values = |index: &PrimIndex| {
            index
                .property_opinions(field)
                .unwrap()
                .iter()
                .map(|op| op.value().as_property().unwrap().default.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(values(&snapshot), alloc::vec![Some(Value::Int(1))]);
        assert_eq!(
            values(&index),
            alloc::vec![
                Some(Value::Int(1)),
                Some(Value::Int(2)),
                Some(Value::Int(3))
            ]
        );
    }

    #[test]
    fn field_identity_strength_and_removal_survive_grouping() {
        let store = InMemoryStore::default();
        let prim = store.path("/Emitter");
        let a = store.tokens.intern("a");
        let b = store.tokens.intern("b");
        let site = SpecPath::from_prim_path(prim);
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
            index.add_keyed(key(strength), LayerOffset::IDENTITY, field, value);
        }
        // A deep stack also exercises grouping without moving large payloads
        // at every comparison, including a repeated key whose order matters.
        for strength in 0..40 {
            index.add_keyed(
                key(strength),
                LayerOffset::IDENTITY,
                b,
                PropertySpec::attribute().with_default(Value::Int(9)).into(),
            );
        }
        index.finalize();
        assert_eq!(
            index.property_opinions(b).unwrap().records()[0]
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
            index
                .property_opinions(a)
                .unwrap()
                .first()
                .unwrap()
                .source()
                .key
                .layer_strength,
            0,
            "strong layer comes first"
        );
        assert_eq!(
            index.metadata_opinions(a).unwrap().len(),
            1,
            "metadata stays separate"
        );
        let strongest = index.sources[0].key.clone();
        assert_eq!(strongest.layer_strength, 0, "sources are strongest first");
        index.retain_opinions(|opinion| {
            !(opinion.field == b
                || FieldKey::of(opinion) == FieldKey::Property(a)
                    && opinion.source == SourceId::at(0))
        });
        assert!(
            index.property_opinions(b).is_none(),
            "removed fields disappear"
        );
        assert_eq!(
            index.property_opinions(a).unwrap().records()[0]
                .value
                .as_property()
                .unwrap()
                .default,
            Some(Value::Int(1)),
            "removing the winner exposes the weaker property"
        );
        assert_eq!(
            index.metadata_opinions(a).unwrap().records()[0].value,
            OpinionValue::Field(FieldValue::Value(Value::Int(3))),
            "a property removal preserves metadata with the same token and key"
        );
    }
}
