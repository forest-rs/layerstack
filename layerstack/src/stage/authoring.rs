// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prospective property authoring: where a write would sit, and when its
//! value would show.
//!
//! This module owns composition strength and OpenUSD's value-source
//! selection for a write not yet made, not value evaluation or permission to
//! write. Raw transactions may deliberately author masked values.

use alloc::{sync::Arc, vec::Vec};
use core::{fmt, ops::Bound};

use crate::{
    ArcKind, FieldValue, InterpolationType, LayerId, LayerOffset, LayerStore, NodeId, Opinion,
    OpinionKey, OpinionValue, PathId, PathInterner, PrimIndexGraph, PropertyKind, PropertyPath,
    SpecPath, Stage, Time, TimeSample, TokenId, Value,
    edit::EditTarget,
    prim_index::{OpinionRef, OpinionSource, PrimIndex},
    value_clips::{AuthoringClipSet, ClipAssetRequest},
    value_resolution::sample_bracket,
};

/// The slot a prospective write authors at its target site.
///
/// Within one site's opinion OpenUSD reads time samples before a spline
/// before a default (`UsdStage::_ResolveInfoResolver::ProcessLayerAtTime`),
/// so whether a write shows depends on its slot as well as its site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthoringWrite {
    /// A default value, as `UsdAttribute::Set(value)` at the default time.
    Default,
    /// A time sample at each inspected time code, as
    /// `UsdAttribute::Set(value, time)`.
    TimeSample,
    /// A scalar spline, as `UsdAttribute::SetSpline`.
    Spline,
}

/// The stage times an inspection answers for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuthoringTimes {
    /// The default time, at which only defaults are read
    /// (`UsdTimeCode::Default()`).
    Default,
    /// One finite stage time code.
    At(f64),
    /// Every stage time code from `start` to `end`, both included; either
    /// may be infinite.
    Range {
        /// The first time code.
        start: f64,
        /// The last time code, not before `start`.
        end: f64,
    },
}

/// A prospective attribute write and the times it should show at.
///
/// ```
/// use layerstack::{AuthoringQuery, AuthoringTimes, AuthoringWrite};
/// // Would a default show on frames 1 to 24, as well as at the default time?
/// let frames = AuthoringQuery::default_value().over(1., 24.);
/// assert_eq!(frames.write, AuthoringWrite::Default);
/// assert_eq!(frames.times, AuthoringTimes::Range { start: 1., end: 24. });
/// ```
///
/// Build it as a struct literal; it has no default, so a literal names
/// every field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoringQuery {
    /// The slot the write authors.
    pub write: AuthoringWrite,
    /// When the write should show.
    pub times: AuthoringTimes,
    /// How values between time samples are read, as for a value query.
    pub interpolation: InterpolationType,
}

impl AuthoringQuery {
    /// A default write, inspected at the default time.
    #[must_use]
    pub fn default_value() -> Self {
        Self {
            write: AuthoringWrite::Default,
            times: AuthoringTimes::Default,
            interpolation: InterpolationType::Linear,
        }
    }

    /// A time sample written at `code`, inspected there.
    #[must_use]
    pub fn time_sample(code: f64) -> Self {
        Self {
            write: AuthoringWrite::TimeSample,
            times: AuthoringTimes::At(code),
            interpolation: InterpolationType::Linear,
        }
    }

    /// A spline write, inspected at every time code.
    #[must_use]
    pub fn spline() -> Self {
        Self {
            write: AuthoringWrite::Spline,
            times: AuthoringTimes::Range {
                start: f64::NEG_INFINITY,
                end: f64::INFINITY,
            },
            interpolation: InterpolationType::Linear,
        }
    }

    /// This write, inspected at the time code `code`.
    #[must_use]
    pub fn at(self, code: f64) -> Self {
        Self {
            times: AuthoringTimes::At(code),
            ..self
        }
    }

    /// This write, inspected at every time code from `start` to `end`.
    #[must_use]
    pub fn over(self, start: f64, end: f64) -> Self {
        Self {
            times: AuthoringTimes::Range { start, end },
            ..self
        }
    }

    /// This write, read with held interpolation.
    #[must_use]
    pub fn held(self) -> Self {
        Self {
            interpolation: InterpolationType::Held,
            ..self
        }
    }
}

impl From<Time> for AuthoringQuery {
    /// [`Time::Default`] asks about a default write at the default time;
    /// [`Time::At`] about a time sample written at its time code.
    fn from(time: Time) -> Self {
        match time {
            Time::Default => Self::default_value(),
            Time::At {
                code,
                interpolation,
            } => Self {
                interpolation,
                ..Self::time_sample(code)
            },
        }
    }
}

/// The authored slot a source supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthoringSourceKind {
    /// An authored default that is not a block.
    Default,
    /// A value block (`None`) as the default: source selection ends there,
    /// with the schema fallback or no value (`UsdAttribute::Block`).
    Block,
    /// Nonempty time samples, a blocked sample among them; their keys and
    /// kinds are inspected without decoding.
    TimeSamples,
    /// A scalar spline, including blocked segments and extrapolation.
    Spline,
    /// An animation block: weaker samples, splines and clips are skipped
    /// and weaker defaults read (`UsdAttribute::BlockAnimation`).
    AnimationBlock,
    /// A value clip set. Once its manifest declares the attribute varying,
    /// it supplies the value at every time code, whichever clip is active
    /// (`Usd_ClipSet::ContainsValueForAttribute`).
    ValueClips {
        /// The clip-set name on the metadata owner.
        set_name: Arc<str>,
        /// The assets to resolve and load before the set's participation
        /// is known: its manifest, or, without one, the clips the manifest
        /// is generated from. Empty when resident metadata settles it; no
        /// clip value is read either way.
        pending: Arc<[ClipAssetRequest]>,
    },
}

/// Whether a write's value can still show through a stronger source.
///
/// This describes the source's shape, not compatibility with an
/// unspecified proposed value. A dense foreign-kind value may be ignored by
/// a dictionary or array fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceComposability {
    /// The source supplies the whole value: a dense value, a block, or a
    /// slot its own site reads before the written one.
    Dense,
    /// The source composes over weaker values: an array edit or dictionary,
    /// or a path expression with a weaker reference, at either bracketing
    /// sample (OpenUSD `_GetSamplesComposability`).
    Composable,
    /// Not known from resident metadata (a clip set awaiting assets, a
    /// path-expression clip), or, over a range, dense at some times and
    /// composable at others; [`AuthoringInspection::spans`] says when.
    Unknown,
}

/// The time samples a source's value at the inspected time comes from.
///
/// `Eq` compares the times bit for bit, as [`LayerOffset`]'s does.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SampleBracket {
    /// The stage time of the lower bracketing sample.
    pub lower: f64,
    /// The stage time of the upper bracketing sample; equal to `lower`
    /// when one sample holds: at it, before the first or after the last
    /// sample, or with held interpolation.
    pub upper: f64,
}

impl PartialEq for SampleBracket {
    fn eq(&self, other: &Self) -> bool {
        self.lower.to_bits() == other.lower.to_bits()
            && self.upper.to_bits() == other.upper.to_bits()
    }
}

impl Eq for SampleBracket {}

impl SampleBracket {
    /// Whether the value interpolates between two samples.
    #[must_use]
    pub fn is_interpolated(&self) -> bool {
        self.lower < self.upper
    }
}

/// One source that outranks a prospective write at some inspected time,
/// without materializing its values.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuthoringSource {
    /// Authoring layer, variant-qualified spec path and node of the prim's
    /// composition graph. For clips this is the metadata anchor, not a clip
    /// layer.
    pub key: OpinionKey,
    /// The arc of [`OpinionKey::node`].
    pub arc: ArcKind,
    /// Whether this is the target's own site, whose `kind` slot OpenUSD
    /// reads before the written one: samples over a default or a spline,
    /// or a spline over a default.
    pub same_site: bool,
    /// The slot it supplies.
    pub kind: AuthoringSourceKind,
    /// Whether the write can still show through it.
    pub composability: SourceComposability,
}

/// Inspected stage times that share one answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpanTimes {
    /// The default time.
    Default,
    /// The time codes between two bounds.
    Codes {
        /// The lower bound.
        start: Bound<f64>,
        /// The upper bound.
        end: Bound<f64>,
    },
}

impl SpanTimes {
    /// Whether the time code `code` is in this span.
    #[must_use]
    pub fn contains(&self, code: f64) -> bool {
        match self {
            Self::Default => false,
            Self::Codes { start, end } => {
                let above = match start {
                    Bound::Included(start) => code >= *start,
                    Bound::Excluded(start) => code > *start,
                    Bound::Unbounded => true,
                };
                let below = match end {
                    Bound::Included(end) => code <= *end,
                    Bound::Excluded(end) => code < *end,
                    Bound::Unbounded => true,
                };
                above && below
            }
        }
    }
}

/// What a prospective write would show over a span of inspected times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthoringVerdict {
    /// The target does not contribute, as [`AuthoringInspection::target`]
    /// says.
    Inactive,
    /// No source outranks the write: it would be the value.
    Strongest,
    /// Only composing sources outrank it: it would show, edited by them.
    ComposedOver,
    /// [`AuthoringInspection::stronger_sources`]`[by]`, the strongest dense
    /// one, supplies the value instead.
    Masked {
        /// The index of the masking source.
        by: usize,
    },
    /// [`AuthoringInspection::stronger_sources`]`[by]` may mask the write
    /// or compose over it; resident metadata does not tell.
    Unknown {
        /// The index of the strongest such source.
        by: usize,
    },
}

/// Where a source stands against the prospective write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceStanding {
    /// Stronger: [`AuthoringInspection::stronger_sources`]`[index]`.
    Stronger {
        /// The index of the source.
        index: usize,
    },
    /// The target's own site: the write edits it.
    Target,
    /// Weaker: the write would win over it.
    Weaker,
    /// The target does not contribute, so the two do not compete.
    Unrelated,
}

/// Where the value comes from before the write: OpenUSD's
/// `UsdAttribute::GetResolveInfo(time)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentSource {
    /// An authored opinion supplies the value, or blocks it
    /// ([`AuthoringSourceKind::Block`], whereupon the schema fallback or no
    /// value is read).
    Authored {
        /// Its layer, spec path and node.
        key: OpinionKey,
        /// The slot it supplies.
        kind: AuthoringSourceKind,
        /// Where it stands against the write.
        standing: SourceStanding,
        /// For [`AuthoringTimes::At`] and [`AuthoringSourceKind::TimeSamples`],
        /// the samples the value comes from there, interpolated across
        /// when [`SampleBracket::is_interpolated`]; `None` otherwise, clips
        /// included ([`crate::Attribute::clip_source`] names those).
        bracket: Option<SampleBracket>,
    },
    /// Nothing authored supplies a value, so any write that contributes
    /// would win. `schema` says whether the prim definition has a fallback
    /// value to read meanwhile (`UsdResolveInfoSourceFallback`) or there is
    /// none (`UsdResolveInfoSourceNone`).
    Fallback {
        /// Whether a schema fallback is read.
        schema: bool,
    },
}

/// The answer for inspected times that share it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AuthoringSpan {
    /// The times.
    pub times: SpanTimes,
    /// What the write would show there.
    pub verdict: AuthoringVerdict,
    /// Where the value comes from there before the write.
    pub current: CurrentSource,
}

/// Where a prospective attribute write participates in this stage snapshot,
/// and when it would show.
///
/// Sources are the slots OpenUSD would read before the written one; the
/// value explanation APIs name the contributors of the current value.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AuthoringInspection {
    /// The strongest occurrence of the mapped target site, including a site
    /// whose layer has no prim or property spec yet. `None` means the target
    /// does not contribute: for example a muted layer or unselected variant.
    pub target: Option<OpinionKey>,
    /// Whether composition culled the target's site from the prim's graph:
    /// no layer of its stack has a spec there yet, so the prim's
    /// [`composition_graph`](crate::Prim::composition_graph) has no node
    /// for it, though a write there would add one. The target's
    /// [`OpinionKey::node`] then names a node of
    /// [`Stage::expanded_prim_graph`]; the stronger sources' keys name
    /// nodes of the prim's graph either way.
    pub target_culled: bool,
    /// The query answered.
    pub query: AuthoringQuery,
    /// Every source that outranks the written slot at some inspected time,
    /// in strength order, the target's own site last. Sources past the
    /// strongest dense one are listed too: removing that one would not
    /// make the write show. Empty when the target does not contribute.
    pub stronger_sources: Vec<AuthoringSource>,
    /// The inspected times in order, split where the answer changes: one
    /// span for [`AuthoringTimes::Default`] and [`AuthoringTimes::At`].
    pub spans: Vec<AuthoringSpan>,
}

impl AuthoringInspection {
    /// Whether the target contributes and nothing outranks the write at any
    /// inspected time. Does not validate the proposed value's type or the
    /// destination's editability.
    #[must_use]
    pub fn is_strongest(&self) -> bool {
        self.target.is_some()
            && self
                .spans
                .iter()
                .all(|span| span.verdict == AuthoringVerdict::Strongest)
    }

    /// The span holding the time code `code`, if it was inspected.
    #[must_use]
    pub fn span_at(&self, code: f64) -> Option<&AuthoringSpan> {
        self.spans.iter().find(|span| span.times.contains(code))
    }
}

/// An invalid prospective authoring query.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthoringInspectionError {
    /// The store's token/path domains do not belong to this stage snapshot.
    ForeignStore,
    /// The stage has no composed prim at this path.
    MissingPrim(PathId),
    /// The store does not contain the target layer.
    MissingLayer(LayerId),
    /// The edit target does not map the property into its namespace.
    Unmappable(PropertyPath),
    /// A composed relationship cannot be inspected as an attribute value.
    NotAttribute(PropertyPath),
    /// A time code is not finite, a range bound is NaN, or a range ends
    /// before it starts.
    InvalidTime,
    /// A time sample or spline write was asked about at the default time,
    /// where only defaults are read.
    AnimationAtDefault,
}

impl fmt::Display for AuthoringInspectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignStore => {
                f.write_str("authoring inspection store belongs to another token/path domain")
            }
            Self::MissingPrim(path) => write!(f, "no composed prim at {path:?}"),
            Self::MissingLayer(layer) => write!(f, "target layer {layer:?} is absent"),
            Self::Unmappable(path) => write!(f, "edit target does not map {path:?}"),
            Self::NotAttribute(path) => write!(f, "{path:?} is a relationship"),
            Self::InvalidTime => {
                f.write_str("authoring inspection requires finite time codes and an ordered range")
            }
            Self::AnimationAtDefault => {
                f.write_str("time samples and splines are not read at the default time")
            }
        }
    }
}

impl core::error::Error for AuthoringInspectionError {}

impl Stage {
    /// Inspects a prospective attribute write through `target`: whether it
    /// would show at the inspected times, which sources outrank it, and
    /// where the value comes from meanwhile.
    ///
    /// Unlike a value explanation, the target need not author a spec. The graph
    /// and resident layer-stack ordering establish its prospective strength.
    /// Repeated occurrences of the same mapped site use its strongest position.
    /// A site composition culled from the prim's graph, as no layer of its
    /// stack has a spec there yet, is placed in the prim's graph composed
    /// without culling ([`Self::expanded_prim_graph`],
    /// [`AuthoringInspection::target_culled`]), where a write would put it.
    ///
    /// Sources are read in strength order as OpenUSD's value resolution
    /// reads them (`UsdStage::_GetResolveInfo`): at a time code a site
    /// supplies its samples, else its spline, else its default; a value
    /// block ends the search; an animation block skips weaker samples,
    /// splines and clips; a clip set anchored at a site with no opinion
    /// supplies every time code once its manifest declares the attribute.
    /// At the default time only defaults are read. A stronger opinion's
    /// samples therefore mask a weaker default, and the target's own samples
    /// mask a default written beside them.
    ///
    /// This reads snapshot metadata only: no asset loading, value decoding,
    /// interpolation or layer mutation. Clip participation comes from the
    /// prepared manifest; a set whose metadata is not resident lists the
    /// assets to load ([`AuthoringSourceKind::ValueClips`]). Mapping may
    /// intern paths in `store`. Inspect again after applying edits or
    /// changing residency; pending transaction operations are not part of
    /// the stage snapshot.
    ///
    /// AOUSD Core §10.4 (strength), §12.3 (source selection), §12.3.7
    /// (clips); OpenUSD `UsdPrim::MakeResolveTargetStrongerThanEditTarget`.
    ///
    /// ```
    /// use layerstack::{AuthoringQuery, AuthoringVerdict, InMemoryStore, Layer, LayerId,
    ///     PrimSpec, PropertySpec, Stage, StageOptions, SublayerEntry, Value,
    ///     edit::EditTarget};
    /// let mut store = InMemoryStore::default();
    /// let prim = store.path("/Settings");
    /// let gain = store.tokens.intern("gain");
    /// let mut root = Layer::new(LayerId(1));
    /// let animated = PropertySpec::attribute().with_time_samples(vec![(0., Value::Double(1.))]);
    /// root.insert_prim(prim, PrimSpec::def().with_property(gain, animated));
    /// root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    /// store.insert_layer(root);
    /// store.insert_layer(Layer::new(LayerId(2)));
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let property = store.property_path("/Settings.gain");
    /// let weak = EditTarget::for_layer(LayerId(2));
    /// // The weak layer need not author a spec; its default shows at the default time...
    /// let default = stage.inspect_property_authoring(
    ///     &mut store, property, &weak, AuthoringQuery::default_value(),
    /// ).unwrap();
    /// assert!(default.is_strongest());
    /// // ...but not at time codes, where the root layer's samples are read.
    /// let frames = stage.inspect_property_authoring(
    ///     &mut store, property, &weak, AuthoringQuery::default_value().over(1., 24.),
    /// ).unwrap();
    /// assert_eq!(frames.spans[0].verdict, AuthoringVerdict::Masked { by: 0 });
    /// ```
    ///
    /// # Errors
    /// Returns an error for absent prims/layers, unmappable targets,
    /// relationships, nonfinite or unordered times, and animation asked
    /// about at the default time. An inactive mapped site is a successful
    /// inspection with [`AuthoringInspection::target`] set to `None`.
    pub fn inspect_property_authoring(
        &self,
        store: &mut dyn LayerStore,
        property: PropertyPath,
        target: &EditTarget,
        query: AuthoringQuery,
    ) -> Result<AuthoringInspection, AuthoringInspectionError> {
        if self
            .store_identity
            .as_ref()
            .is_some_and(|identity| *identity != store.identity())
        {
            return Err(AuthoringInspectionError::ForeignStore);
        }
        match query.times {
            AuthoringTimes::Default if query.write != AuthoringWrite::Default => {
                return Err(AuthoringInspectionError::AnimationAtDefault);
            }
            AuthoringTimes::At(code) if !code.is_finite() => {
                return Err(AuthoringInspectionError::InvalidTime);
            }
            AuthoringTimes::Range { start, end }
                if start.is_nan()
                    || end.is_nan()
                    || start > end
                    || start == end && !start.is_finite() =>
            {
                return Err(AuthoringInspectionError::InvalidTime);
            }
            _ => {}
        }
        let prim = property.prim_path();
        let index = self
            .prims
            .get(&prim)
            .ok_or(AuthoringInspectionError::MissingPrim(prim))?;
        store
            .layer(target.layer())
            .ok_or(AuthoringInspectionError::MissingLayer(target.layer()))?;
        if self
            .resolve_property_declaration(prim, property.property())
            .is_some_and(|declaration| declaration.kind == PropertyKind::Relationship)
            || self
                .property_definition_ref(prim, property.property())
                .is_some_and(|definition| definition.kind == PropertyKind::Relationship)
        {
            return Err(AuthoringInspectionError::NotAttribute(property));
        }
        let mapped = target
            .map_property_to_spec_path(property, store.paths())
            .ok_or(AuthoringInspectionError::Unmappable(property))?;
        let mapped_prim = mapped.prim_spec();
        // A site composition culled, as it has no spec yet, is placed in
        // the prim's index composed without culling, as OpenUSD's resolve
        // targets are (`UsdPrim::_MakeResolveTargetFromEditTarget`).
        // A site the graph holds but excludes stays excluded there: a culled
        // occurrence of it would have the same specs, none.
        let placed =
            match self.target_site(store.paths(), prim, index, &mapped_prim, target.layer()) {
                Ok(site) => Some((site, None)),
                Err(Unplaced::Excluded) => None,
                Err(Unplaced::Absent) => {
                    let expanded = self.expanded_prim_index(store, prim);
                    self.target_site(store.paths(), prim, &expanded, &mapped_prim, target.layer())
                        .ok()
                        .map(|site| (site, Some(expanded)))
                }
            };
        let (site, expanded) = match placed {
            Some((site, expanded)) => (Some(site), expanded),
            None => (None, None),
        };
        // Whether a key of the prim's index is stronger than the site, in
        // the graph the site is placed in.
        let culled = expanded.as_ref().map(|expanded| {
            let nodes = node_map(&index.graph, &expanded.graph);
            (&expanded.graph, nodes)
        });
        let place = |key: &OpinionKey| {
            let Some(site) = &site else {
                return Place::Unrelated;
            };
            let order = match &culled {
                None => index.graph.cmp_keys(key, site),
                // A node the expanded graph does not hold is reported, as a
                // write might not win over it.
                Some((graph, nodes)) => nodes.get(key.node.index()).copied().flatten().map_or(
                    core::cmp::Ordering::Less,
                    |node| {
                        let mut key = key.clone();
                        key.node = node;
                        graph.cmp_keys(&key, site)
                    },
                ),
            };
            match order {
                core::cmp::Ordering::Less => Place::Stronger,
                core::cmp::Ordering::Equal => Place::Target,
                core::cmp::Ordering::Greater => Place::Weaker,
            }
        };
        let (stronger_sources, spans) =
            Resolution::new(self, index, property, &place, site.as_ref(), query).inspect();
        Ok(AuthoringInspection {
            target: site,
            target_culled: expanded.is_some(),
            query,
            stronger_sources,
            spans,
        })
    }

    /// The composition graph of `prim` with no node culled, at the prim or
    /// its ancestors: the nodes of [`Prim::composition_graph`] and those
    /// composition culls because no layer of their stack has a spec at
    /// their site yet, each where its arc places it. A write through an
    /// [`EditTarget`] to such a site would add its node to the prim's
    /// graph; [`AuthoringInspection::target`] names it here
    /// ([`AuthoringInspection::target_culled`]). `None` when the stage has
    /// no such prim or `store` is not its store.
    ///
    /// This composes the prim and its ancestors again, reading only
    /// `store`; it does not change the stage.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering). OpenUSD:
    /// `UsdPrim::ComputeExpandedPrimIndex`.
    ///
    /// [`Prim::composition_graph`]: crate::Prim::composition_graph
    #[doc(alias = "UsdPrim::ComputeExpandedPrimIndex")]
    #[doc(alias = "ComputeExpandedPrimIndex")]
    pub fn expanded_prim_graph(
        &self,
        store: &mut dyn LayerStore,
        prim: PathId,
    ) -> Option<PrimIndexGraph> {
        if !self.has_prim(prim)
            || self
                .store_identity
                .as_ref()
                .is_some_and(|identity| *identity != store.identity())
        {
            return None;
        }
        Some(self.expanded_prim_index(store, prim).graph)
    }

    /// The index of `prim` composed with no node culled
    /// ([`crate::prim_indexing::expanded_prim_index`]), from the layers
    /// this stage reads: muted layers stay hidden.
    fn expanded_prim_index(&self, store: &mut dyn LayerStore, prim: PathId) -> PrimIndex {
        let root = self.root_layer.unwrap_or(LayerId::UNRESOLVED);
        let controlled = super::loading::ControlledStore {
            inner: store,
            root,
            muted: &self.options.muted_layers,
        };
        crate::prim_indexing::expanded_prim_index(&controlled, root, &self.options, prim)
    }

    /// The strongest occurrence, in `index`, of the site `mapped_prim` of
    /// `layer` that a write could author an opinion of `prim` at.
    fn target_site(
        &self,
        paths: &PathInterner,
        prim: PathId,
        index: &PrimIndex,
        mapped_prim: &SpecPath,
        layer: LayerId,
    ) -> Result<OpinionKey, Unplaced> {
        // The namespace depths of the prim's instance ancestors, found by
        // walking up rather than scanning the stage's instances.
        let mut instance_depths = Vec::new();
        let mut parent = paths.resolve(prim).parent();
        while let Some(path) = parent {
            if paths
                .lookup(&path)
                .is_some_and(|ancestor| self.instances.contains(&ancestor))
            {
                instance_depths.push(u16::try_from(path.depth()).unwrap_or(u16::MAX));
            }
            parent = path.parent();
        }
        let mut sites = Vec::new();
        let mut held = false;
        for (id, node) in index.graph.nodes() {
            if node.site() != mapped_prim
                || node.arc_kind() == ArcKind::Relocates
                || index.graph.is_inert_specializes_site(id)
            {
                continue;
            }
            let Some(layer_strength) = node.layer_strength(layer) else {
                continue;
            };
            held = true;
            // Instance descendants cannot regain occurrence-local opinions by
            // authoring a previously specless layer (AOUSD Core §11.3.3).
            if instance_depths
                .iter()
                .any(|&depth| !index.graph.has_direct_dependency(paths, id, depth))
            {
                continue;
            }
            sites.push(OpinionKey {
                node: id,
                layer_strength,
                layer_id: layer,
                lookup_path: mapped_prim.prim_path(),
                spec_path: mapped_prim.clone(),
            });
        }
        // When composition selected a canonical occurrence of a duplicated
        // site, use that occurrence rather than a graph-only duplicate. Scope
        // this evidence to its stack: a different stack may legitimately have
        // no specs at the same namespace path yet.
        sites.retain(|site| {
            let node = index.graph.node(site.node).expect("candidate graph node");
            let matching_source = |source: &&OpinionSource| {
                index
                    .graph
                    .node(source.key.node)
                    .is_some_and(|source_node| {
                        source_node.site() == node.site()
                            && source_node.layer_stack_identifier() == node.layer_stack_identifier()
                    })
            };
            let mut sources = index.sources.iter().filter(matching_source).peekable();
            sources.peek().is_none() || sources.any(|source| source.key.node == site.node)
        });
        index.graph.sort_keys(&mut sites);
        sites.into_iter().next().ok_or(if held {
            Unplaced::Excluded
        } else {
            Unplaced::Absent
        })
    }
}

/// Why a graph has no node a write through an edit target could author at.
enum Unplaced {
    /// No node of the graph has the target's site in a stack with its layer.
    Absent,
    /// Nodes have it, but none may take the write: an instance's
    /// descendant's node outside its prototype, or a duplicate of the
    /// occurrence composition selected.
    Excluded,
}

/// The node of `expanded` that each node of `graph` is, by node index:
/// `expanded` holds the nodes of `graph` and the ones composition culled,
/// so a node is the child of its parent's node there with the same arc
/// kind, layer stack, site and place among its siblings.
fn node_map(graph: &PrimIndexGraph, expanded: &PrimIndexGraph) -> Vec<Option<NodeId>> {
    let mut map = alloc::vec![None; graph.len()];
    let (Some(root), Some(expanded_root)) = (graph.root(), expanded.root()) else {
        return map;
    };
    let mut pending = alloc::vec![(root, expanded_root)];
    while let Some((node, at)) = pending.pop() {
        map[node.index()] = Some(at);
        let (Some(node), Some(at)) = (graph.node(node), expanded.node(at)) else {
            continue;
        };
        for &child in node.children() {
            let Some(found) = graph.node(child) else {
                continue;
            };
            let same = at.children().iter().copied().find(|&candidate| {
                expanded.node(candidate).is_some_and(|candidate| {
                    candidate.arc_kind() == found.arc_kind()
                        && candidate.layer_stack_identifier() == found.layer_stack_identifier()
                        && candidate.site() == found.site()
                        && candidate.sibling_index() == found.sibling_index()
                })
            });
            if let Some(same) = same {
                pending.push((child, same));
            }
        }
    }
    map
}

/// Where a site stands against the target; walk order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Stronger,
    Target,
    Weaker,
    Unrelated,
}

/// One site's property opinion and the clip sets anchored with it, which
/// OpenUSD reads only when the site has no opinion
/// (`_GetResolvedValueAtTimeWithClipsImpl`).
struct Group<'a> {
    key: OpinionKey,
    place: Place,
    opinion: Option<OpinionRef<'a>>,
    clips: Vec<usize>,
}

/// A slot one site supplies; ordered as a site reads them, clips last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    TimeSamples,
    Spline,
    AnimationBlock,
    Block,
    Default,
    Clips(usize),
    Written,
}

/// One slot read at one time.
#[derive(Clone, Copy, Debug)]
struct Contribution {
    group: usize,
    slot: Slot,
    shape: SourceComposability,
    bracket: Option<SampleBracket>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    Default,
    At(f64),
}

/// OpenUSD's source selection over one property's sites, with and without
/// a prospective write at the target's (`UsdStage::_ResolveInfoResolver`).
struct Resolution<'a> {
    stage: &'a Stage,
    index: &'a PrimIndex,
    prim: PathId,
    field: TokenId,
    groups: Vec<Group<'a>>,
    sets: Vec<AuthoringClipSet>,
    query: AuthoringQuery,
}

impl<'a> Resolution<'a> {
    fn new(
        stage: &'a Stage,
        index: &'a PrimIndex,
        property: PropertyPath,
        place: &dyn Fn(&OpinionKey) -> Place,
        site: Option<&OpinionKey>,
        query: AuthoringQuery,
    ) -> Self {
        let (prim, field) = (property.prim_path(), property.property());
        let mut groups: Vec<_> = index
            .property_opinions(field)
            .unwrap_or_default()
            .iter()
            .map(|opinion| {
                let key = opinion.key();
                Group {
                    place: place(&key),
                    key,
                    opinion: Some(opinion),
                    clips: Vec::new(),
                }
            })
            .collect();
        let sets = stage.clips.authoring_sets(prim, field);
        for (at, set) in sets.iter().enumerate() {
            match groups.iter_mut().find(|group| {
                group.key.node == set.key.node && group.key.layer_strength == set.key.layer_strength
            }) {
                Some(group) => group.clips.push(at),
                None => groups.push(Group {
                    key: set.key.clone(),
                    place: place(&set.key),
                    opinion: None,
                    clips: alloc::vec![at],
                }),
            }
        }
        if let Some(site) = site
            && !groups.iter().any(|group| group.place == Place::Target)
        {
            groups.push(Group {
                key: site.clone(),
                place: Place::Target,
                opinion: None,
                clips: Vec::new(),
            });
        }
        groups.sort_by(|a, b| {
            a.place
                .cmp(&b.place)
                .then_with(|| index.graph.cmp_keys(&a.key, &b.key))
        });
        Self {
            stage,
            index,
            prim,
            field,
            groups,
            sets,
            query,
        }
    }

    /// The stronger sources and the answer over the inspected times.
    fn inspect(&self) -> (Vec<AuthoringSource>, Vec<AuthoringSpan>) {
        let pieces = match self.query.times {
            AuthoringTimes::Default => alloc::vec![(SpanTimes::Default, When::Default)],
            AuthoringTimes::At(code) => alloc::vec![(point(code), When::At(code))],
            AuthoringTimes::Range { start, end } => self.pieces(start, end),
        };
        let active = self.groups.iter().any(|group| group.place == Place::Target);
        let answers: Vec<_> = pieces
            .into_iter()
            .map(|(times, when)| {
                let after = if active {
                    self.walk(when, Some(self.query.write))
                } else {
                    Vec::new()
                };
                let before = self.walk(when, None).into_iter().next();
                (times, after, before)
            })
            .collect();
        // The slots read before the written one: stronger sites' and the
        // target's own.
        let outranking = |contribution: &&Contribution| {
            contribution.slot != Slot::Written
                && self.groups[contribution.group].place <= Place::Target
        };
        let considered = || {
            answers.iter().flat_map(|(_, after, before)| {
                after.iter().filter(outranking).chain(
                    before
                        .iter()
                        .filter(|c| self.groups[c.group].place == Place::Stronger),
                )
            })
        };
        let mut ids: Vec<(usize, Slot)> = considered().map(|c| (c.group, c.slot)).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut shapes: Vec<Option<SourceComposability>> = alloc::vec![None; ids.len()];
        for contribution in considered() {
            let Ok(at) = ids.binary_search(&(contribution.group, contribution.slot)) else {
                continue;
            };
            shapes[at] = match shapes[at] {
                Some(shape) if shape != contribution.shape => Some(SourceComposability::Unknown),
                _ => Some(contribution.shape),
            };
        }
        let single = matches!(self.query.times, AuthoringTimes::At(_));
        let sources = ids
            .iter()
            .zip(shapes)
            .map(|(&(group, slot), shape)| {
                let key = self.slot_key(group, slot);
                AuthoringSource {
                    arc: self.arc(&key),
                    key,
                    same_site: self.groups[group].place == Place::Target,
                    kind: self.kind(slot),
                    composability: shape.unwrap_or(SourceComposability::Unknown),
                }
            })
            .collect();
        let index_of = |c: &Contribution| ids.binary_search(&(c.group, c.slot)).ok();
        let mut spans: Vec<AuthoringSpan> = Vec::new();
        for (times, after, before) in answers {
            let verdict = if active {
                verdict(
                    after
                        .iter()
                        .filter(outranking)
                        .filter_map(|c| Some((index_of(c)?, c.shape))),
                )
            } else {
                AuthoringVerdict::Inactive
            };
            let current = match before {
                None => CurrentSource::Fallback {
                    schema: self
                        .stage
                        .property_definition_ref(self.prim, self.field)
                        .is_some_and(|definition| definition.fallback.is_some()),
                },
                Some(contribution) => CurrentSource::Authored {
                    key: self.slot_key(contribution.group, contribution.slot),
                    kind: self.kind(contribution.slot),
                    standing: match self.groups[contribution.group].place {
                        Place::Stronger => SourceStanding::Stronger {
                            index: index_of(&contribution).unwrap_or_default(),
                        },
                        Place::Target => SourceStanding::Target,
                        Place::Weaker => SourceStanding::Weaker,
                        Place::Unrelated => SourceStanding::Unrelated,
                    },
                    bracket: contribution.bracket.filter(|_| single),
                },
            };
            if let Some(last) = spans.last_mut()
                && last.verdict == verdict
                && last.current == current
                && let (SpanTimes::Codes { end, .. }, SpanTimes::Codes { end: next, .. }) =
                    (&mut last.times, times)
            {
                *end = next;
                continue;
            }
            spans.push(AuthoringSpan {
                times,
                verdict,
                current,
            });
        }
        (sources, spans)
    }

    /// The slots OpenUSD reads at `when`, in order. With `write`, every slot
    /// of a stronger site and of the target's, through the written one;
    /// without, the first slot read, where the value comes from.
    fn walk(&self, when: When, write: Option<AuthoringWrite>) -> Vec<Contribution> {
        let mut out = Vec::new();
        let mut animation_blocked = false;
        // An animation block masks written animation, not a written default.
        let report_animation_block = write.is_some_and(|write| write != AuthoringWrite::Default);
        for (at, group) in self.groups.iter().enumerate() {
            if write.is_some() && group.place > Place::Target {
                break;
            }
            let written = write.filter(|_| group.place == Place::Target);
            self.read_site(
                at,
                group,
                when,
                written,
                report_animation_block,
                &mut animation_blocked,
                &mut out,
            );
            if written.is_some() || write.is_none() && !out.is_empty() {
                break;
            }
        }
        out
    }

    /// One site, as `ProcessLayerAtTime`, `ProcessLayerAtDefault` and
    /// `ProcessClips` read it. With `written`, the site's opinion is the
    /// target's with the written slot added.
    #[allow(
        clippy::too_many_arguments,
        reason = "one resolver step's state, threaded explicitly"
    )]
    fn read_site(
        &self,
        at: usize,
        group: &Group<'_>,
        when: When,
        written: Option<AuthoringWrite>,
        report_animation_block: bool,
        animation_blocked: &mut bool,
        out: &mut Vec<Contribution>,
    ) {
        let mut push = |slot, shape, bracket| {
            out.push(Contribution {
                group: at,
                slot,
                shape,
                bracket,
            });
        };
        let opinion = group.opinion;
        let samples = opinion.and_then(|opinion| opinion.value().time_samples());
        let spline = opinion.is_some_and(|opinion| opinion.value().spline().is_some());
        let When::At(code) = when else {
            // The default time reads defaults only; an animation block
            // there is passed over (`ProcessLayerAtDefault`).
            if written.is_some() {
                push(Slot::Written, SourceComposability::Dense, None);
            } else if let Some(opinion) = opinion {
                match default_slot(opinion.opinion()) {
                    DefaultSlot::Value(shape) => push(Slot::Default, shape, None),
                    DefaultSlot::Block => push(Slot::Block, SourceComposability::Dense, None),
                    DefaultSlot::None | DefaultSlot::AnimationBlock => {}
                }
            }
            return;
        };
        if let Some(write) = written {
            // The target's own slots that its site reads before the written
            // one mask it whole: they compose over weaker sites, not over
            // the same site's other slots.
            if !*animation_blocked && write != AuthoringWrite::TimeSample {
                if let (Some(opinion), Some(samples)) = (opinion, samples) {
                    let (_, bracket) = self.read_samples(opinion, samples, code);
                    push(Slot::TimeSamples, SourceComposability::Dense, bracket);
                } else if spline && write == AuthoringWrite::Default {
                    push(Slot::Spline, SourceComposability::Dense, None);
                }
            }
            push(Slot::Written, SourceComposability::Dense, None);
            return;
        }
        let mut found = false;
        if let Some(opinion) = opinion {
            if let Some(samples) = samples.filter(|_| !*animation_blocked) {
                found = true;
                let (shape, bracket) = self.read_samples(opinion, samples, code);
                push(Slot::TimeSamples, shape, bracket);
            } else if spline && !*animation_blocked {
                found = true;
                push(Slot::Spline, SourceComposability::Dense, None);
            } else {
                match default_slot(opinion.opinion()) {
                    DefaultSlot::Value(shape) => {
                        found = true;
                        push(Slot::Default, shape, None);
                    }
                    DefaultSlot::Block => {
                        found = true;
                        push(Slot::Block, SourceComposability::Dense, None);
                    }
                    DefaultSlot::AnimationBlock => {
                        found = true;
                        *animation_blocked = true;
                        if report_animation_block {
                            push(Slot::AnimationBlock, SourceComposability::Dense, None);
                        }
                    }
                    DefaultSlot::None => {}
                }
            }
        }
        if found || *animation_blocked {
            return;
        }
        for &set in &group.clips {
            let shape = self.sets[set]
                .entry
                .map_or(SourceComposability::Unknown, |entry| {
                    self.stage.clips.authoring_composability(
                        self.prim,
                        self.field,
                        entry,
                        code,
                        self.query.interpolation,
                    )
                });
            push(Slot::Clips(set), shape, None);
        }
    }

    /// Whether a weaker value shows through `samples` at the stage time
    /// `code`, and which samples are read there.
    fn read_samples(
        &self,
        opinion: OpinionRef<'_>,
        samples: &[TimeSample],
        code: f64,
    ) -> (SourceComposability, Option<SampleBracket>) {
        let offset = opinion.layer_offset();
        let Some((lower, upper)) =
            sample_bracket(samples, offset.map_time(code), self.query.interpolation)
        else {
            return (SourceComposability::Dense, None);
        };
        let composes = |at: usize| shape(&samples[at].1) == SourceComposability::Composable;
        let shape = if composes(lower) || composes(upper) {
            SourceComposability::Composable
        } else {
            SourceComposability::Dense
        };
        let bracket = SampleBracket {
            lower: stage_time(offset, samples[lower].0),
            upper: stage_time(offset, samples[upper].0),
        };
        (shape, Some(bracket))
    }

    /// The pieces of `start..=end` over which no source's shape changes:
    /// each time a source's samples change kind, and the open stretches
    /// between, with a time to read each stretch at.
    fn pieces(&self, start: f64, end: f64) -> Vec<(SpanTimes, When)> {
        if start == end {
            return alloc::vec![(point(start), When::At(start))];
        }
        let mut cuts = Vec::new();
        for group in &self.groups {
            let Some(opinion) = group.opinion else {
                continue;
            };
            let Some(samples) = opinion.value().time_samples() else {
                continue;
            };
            let first = shape(&samples[0].1);
            if samples.iter().any(|(_, value)| shape(value) != first) {
                let offset = opinion.layer_offset();
                cuts.extend(samples.iter().map(|(time, _)| stage_time(offset, *time)));
            }
        }
        for set in &self.sets {
            if let Some(entry) = set.entry {
                cuts.extend_from_slice(
                    self.stage
                        .clips
                        .authoring_breakpoints(self.prim, self.field, entry),
                );
            }
        }
        cuts.retain(|time| *time > start && *time < end);
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let bound = |time: f64| {
            if time.is_finite() {
                Bound::Excluded(time)
            } else {
                Bound::Unbounded
            }
        };
        let mut out = Vec::new();
        if start.is_finite() {
            out.push((point(start), When::At(start)));
        }
        let mut previous = start;
        for time in cuts.into_iter().chain([end]) {
            let inside = match (previous.is_finite(), time.is_finite()) {
                (true, true) => previous + (time - previous) / 2.,
                (false, true) => time - 1.,
                (true, false) => previous + 1.,
                (false, false) => 0.,
            };
            out.push((
                SpanTimes::Codes {
                    start: bound(previous),
                    end: bound(time),
                },
                When::At(inside),
            ));
            if time.is_finite() {
                out.push((point(time), When::At(time)));
            }
            previous = time;
        }
        out
    }

    fn slot_key(&self, group: usize, slot: Slot) -> OpinionKey {
        match slot {
            Slot::Clips(set) => self.sets[set].key.clone(),
            _ => self.groups[group].key.clone(),
        }
    }

    fn kind(&self, slot: Slot) -> AuthoringSourceKind {
        match slot {
            Slot::TimeSamples => AuthoringSourceKind::TimeSamples,
            Slot::Spline => AuthoringSourceKind::Spline,
            Slot::AnimationBlock => AuthoringSourceKind::AnimationBlock,
            Slot::Block => AuthoringSourceKind::Block,
            Slot::Default | Slot::Written => AuthoringSourceKind::Default,
            Slot::Clips(set) => AuthoringSourceKind::ValueClips {
                set_name: self.sets[set].set_name.clone(),
                pending: self.sets[set].pending.clone(),
            },
        }
    }

    fn arc(&self, key: &OpinionKey) -> ArcKind {
        self.index
            .graph
            .node(key.node)
            .map_or(ArcKind::Local, |node| node.arc_kind())
    }
}

/// The verdict from the outranking slots read at one time, in order, with
/// their source indices.
fn verdict(outranking: impl Iterator<Item = (usize, SourceComposability)>) -> AuthoringVerdict {
    let mut unknown = None;
    let mut composed = false;
    for (index, shape) in outranking {
        match shape {
            SourceComposability::Dense => return AuthoringVerdict::Masked { by: index },
            SourceComposability::Unknown => {
                unknown.get_or_insert(index);
            }
            SourceComposability::Composable => composed = true,
        }
    }
    match unknown {
        Some(by) => AuthoringVerdict::Unknown { by },
        None if composed => AuthoringVerdict::ComposedOver,
        None => AuthoringVerdict::Strongest,
    }
}

fn point(code: f64) -> SpanTimes {
    SpanTimes::Codes {
        start: Bound::Included(code),
        end: Bound::Included(code),
    }
}

/// The stage time of a sample at `time` in a layer `offset` places.
fn stage_time(offset: LayerOffset, time: f64) -> f64 {
    time * offset.scale + offset.offset
}

/// What an opinion's default is, read without decoding it.
enum DefaultSlot {
    None,
    AnimationBlock,
    Block,
    Value(SourceComposability),
}

fn default_slot(opinion: &Opinion) -> DefaultSlot {
    let value = match &opinion.value {
        OpinionValue::Property(spec) => spec.default.as_ref(),
        OpinionValue::Field(FieldValue::Value(value)) => Some(value),
        OpinionValue::Field(_) => None,
    };
    match value {
        None => DefaultSlot::None,
        Some(Value::AnimationBlock) => DefaultSlot::AnimationBlock,
        Some(Value::Blocked) => DefaultSlot::Block,
        Some(value) => DefaultSlot::Value(shape(value)),
    }
}

/// Whether a weaker value shows through `value`, from its kind alone. A
/// value an adapter keeps encoded ([`Value::Deferred`]) is a dense array or
/// a metadata dictionary, never an attribute's sparse edit, so it is dense
/// and nothing is decoded; OpenUSD likewise classifies a default by its
/// stored type (`Usd_HasDefault`, `VtValueTypeCanComposeOver`).
fn shape(value: &Value) -> SourceComposability {
    if value.array_edit_ref().is_some() || matches!(value, Value::Dictionary(_)) {
        SourceComposability::Composable
    } else if let Value::PathExpression(text) = value {
        match crate::PathExpression::parse(text) {
            Ok(expression) if expression.contains_weaker_reference() => {
                SourceComposability::Composable
            }
            _ => SourceComposability::Dense,
        }
    } else {
        SourceComposability::Dense
    }
}

#[cfg(test)]
mod tests;
