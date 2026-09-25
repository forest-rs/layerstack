// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value explanations: why a property or metadata field has its value.
//!
//! Each `explain_*` query on [`Stage`] mirrors one `resolve_*` query and
//! answers with the same value, plus the opinions resolution consulted and
//! the part each one played. It is the counterpart of OpenUSD's
//! `UsdAttribute::GetResolveInfo` (`UsdResolveInfo`, `pxr/usd/usd/resolveInfo.h`),
//! which names the source of an attribute's value and the node and layer it
//! came from; a [`ValueExplanation`] additionally lists every consulted
//! opinion, including those a stronger value, a block or an incompatible
//! operation kept from contributing, and how sparse array edits,
//! dictionaries and list ops folded. OpenUSD's `UsdAttributeQuery`
//! (`pxr/usd/usd/attributeQuery.h`) caches a resolve info to speed up
//! repeated reads; an explanation is a one-off diagnostic and is never cached
//! or consulted by resolution.
//!
//! The folds themselves are [`opinionated`]'s: explanation runs the same
//! chains through the kernel's reporting entry points
//! ([`opinionated::resolve_family_chain_report`],
//! [`opinionated::combine_dictionary_chain_report`] and
//! [`opinionated::resolve_ordered_chain_report`]) and attaches layer, spec
//! and composition-node provenance to their events. Resolution itself never
//! reports, so it pays nothing for this.
//!
//! Spec: AOUSD Core §12 (value resolution): §12.2.5 (dictionaries combine),
//! §12.2.6 (list ops), §12.3.1 (default values), §12.3.2 (time samples and
//! splines), §12.3.6 (blocks), §13.3.2.4 (schema fallbacks).

use alloc::{sync::Arc, vec, vec::Vec};

use opinionated::{
    ChainOpinion, DictionaryEvent, FamilyEvent, IgnoreReason, OpinionOp, Resolution,
    ResolutionEvent, combine_dictionary_chain_report, resolve_ordered_chain_report,
};

use super::{
    ListChainer, Lookup, ResolvedValue, Stage, chain_field_list, dictionary_chain, value_at_time,
};
use crate::{
    doc::{
        FieldValue, InterpolationType, LayerId, LayerOffset, LayerStore, Value, ValueDictionaries,
    },
    interner::TokenId,
    listop::ListOp,
    path::{PathId, PropertyPath},
    prim_index::{ArcKind, Opinion, OpinionValue, PrimIndex},
    prim_index_graph::PrimNode,
    property::{PropertySpec, PropertyType},
    schema::SchemaRegistry,
    spec_path::SpecPath,
    value_resolution::{
        SampleFold, SparseQuery, SparseResolveResult, explain_sparse_value, reads_array_family,
        sample_bracket,
    },
};

/// Why a property or metadata field has its value.
///
/// Returned by the `explain_*` queries of [`Stage`], each of which mirrors a
/// `resolve_*` query: [`ValueExplanation::value`] is exactly the value that
/// query resolves.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueExplanation<'s, T> {
    /// The resolved value; `None` when the query resolves no value.
    pub value: Option<T>,
    /// Where the value comes from.
    pub source: ValueSource,
    /// `true` when authored sparse array edits or dictionary entries
    /// composed over the schema fallback, the weakest seed of the fold.
    pub seeded_by_fallback: bool,
    /// Every opinion the query consulted, strongest first, with its part in
    /// the result. Opinions that author nothing the query reads (a property
    /// spec with only connections, for a value) are left out.
    pub opinions: Vec<ExplainedOpinion<'s>>,
}

impl<T> ValueExplanation<'_, T> {
    /// The consulted opinions that contributed to the value.
    pub fn contributors(&self) -> impl Iterator<Item = &ExplainedOpinion<'_>> {
        self.opinions
            .iter()
            .filter(|opinion| matches!(opinion.role, OpinionRole::Contributed(_)))
    }
}

/// Where a resolved value comes from.
///
/// OpenUSD: `UsdResolveInfoSource` (`pxr/usd/usd/resolveInfo.h`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueSource {
    /// No value: nothing authored answers the query, or a block is in
    /// effect and there is no schema fallback.
    None,
    /// Default-time opinions: authored defaults (with any sparse array edits
    /// composed over them), metadata values, dictionaries and list ops.
    ///
    /// Spec: AOUSD Core §12.2, §12.3.1.
    Default,
    /// Time samples bracketing the query time.
    ///
    /// Spec: AOUSD Core §12.3.2.2 (time samples), §12.5 (interpolation).
    TimeSamples {
        /// Stage time of the lower bracketing sample; `-inf` when a default
        /// holds before the first composed sample.
        lower: f64,
        /// Stage time of the upper bracketing sample; equal to `lower` when
        /// one sample holds.
        upper: f64,
        /// The interpolation the query asked for. Samples closer than
        /// `1e-6` in layer time, and element types that do not
        /// interpolate, hold the lower sample under linear interpolation.
        interpolation: InterpolationType,
        /// `true` when the composed lower sample's sparse edits composed
        /// over the schema fallback, or the fallback stood in for it.
        lower_seeded_by_fallback: bool,
        /// As `lower_seeded_by_fallback`, for the composed upper sample.
        upper_seeded_by_fallback: bool,
    },
    /// A spline evaluated at the query time.
    ///
    /// Spec: AOUSD Core §12.3.3.
    Spline,
    /// The schema fallback, with no authored contribution.
    ///
    /// Spec: AOUSD Core §12.3.5, §13.3.2.4.
    Fallback,
}

/// One consulted opinion and its part in a resolved value.
///
/// Two explained opinions are equal when they name the same opinion value,
/// the same graph node (by identity), and play the same part.
#[derive(Clone, Debug)]
pub struct ExplainedOpinion<'s> {
    /// The opinion: its layer, spec path and composition node
    /// ([`Opinion::key`]), the layer offset its arcs apply
    /// ([`Opinion::layer_offset`]), and what it authors.
    pub opinion: &'s Opinion,
    /// The node of the prim's composition graph the opinion was read from
    /// ([`Stage::explain_prim_graph`]): its arc kind and site.
    pub node: Option<&'s PrimNode>,
    /// The opinion's part in the value.
    pub role: OpinionRole,
    /// For a time query, the part each of the opinion's samples played, one
    /// per composed sample folded (the lower and, when interpolating, the
    /// upper). Empty for default-time queries.
    pub samples: Vec<SampleUse>,
}

impl PartialEq for ExplainedOpinion<'_> {
    fn eq(&self, other: &Self) -> bool {
        let same_node = match (self.node, other.node) {
            (Some(a), Some(b)) => core::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_node
            && self.opinion == other.opinion
            && self.role == other.role
            && self.samples == other.samples
    }
}

impl ExplainedOpinion<'_> {
    /// The layer that authors the opinion.
    ///
    /// The stage names layers by [`LayerId`]; a host maps them to asset
    /// identifiers with [`crate::AssetResolver::resolved_path`].
    #[must_use]
    pub fn layer(&self) -> LayerId {
        self.opinion.key.layer_id
    }

    /// The spec path the opinion is authored at in its layer.
    #[must_use]
    pub fn spec_path(&self) -> &SpecPath {
        &self.opinion.key.spec_path
    }

    /// The kind of arc that reaches the opinion's node.
    #[must_use]
    pub fn arc_kind(&self) -> Option<ArcKind> {
        self.node.map(PrimNode::arc_kind)
    }

    /// The offset mapping stage time to the opinion's layer time.
    ///
    /// Spec: AOUSD Core §12.3.2.1.
    #[must_use]
    pub fn layer_offset(&self) -> LayerOffset {
        self.opinion.layer_offset
    }
}

/// One sample an opinion offered a time query, and its part in the fold.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleUse {
    /// Stage time of the sample, after the opinion's layer offset; `-inf`
    /// for a default.
    pub time: f64,
    /// The sample's part in the fold of one composed sample.
    pub role: OpinionRole,
}

/// An opinion's part in a resolved value.
#[derive(Clone, Debug, PartialEq)]
pub enum OpinionRole {
    /// The opinion contributed to the value.
    Contributed(Contribution),
    /// The opinion is a block in effect: it cut off every weaker opinion.
    /// Stronger sparse edits still compose over the schema fallback, or an
    /// empty array.
    ///
    /// A spline that evaluates to nothing, or an empty time-sample map,
    /// acts as a block. Spec: AOUSD Core §12.3.6.
    Block,
    /// The opinion was consulted but did not contribute.
    Ignored(IgnoreCause),
}

/// What a contributing opinion contributed.
#[derive(Clone, Debug, PartialEq)]
pub enum Contribution {
    /// A dense value: the strongest value wins, or the base that stronger
    /// sparse array edits compose over.
    Value,
    /// A sparse array edit, applied over the weaker value.
    ///
    /// OpenUSD: `VtArrayEdit`; sparse-array-edits proposal.
    ArrayEdit,
    /// A dictionary combined with the other dictionaries of the chain.
    ///
    /// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
    Dictionary(DictionaryMerge),
    /// A list op edit, applied over the weaker list.
    ///
    /// Spec: AOUSD Core §12.2.6 (list op resolution), §12.4.
    ListEdit,
}

/// How one dictionary opinion combined with the others.
///
/// Key paths run from the combined dictionary's root down to an entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DictionaryMerge {
    /// Entries this opinion supplied: no stronger opinion holds them. A
    /// supplied nested dictionary is supplied whole.
    pub supplied: Vec<KeyPath>,
    /// Nested dictionaries of this opinion that combined with stronger
    /// ones; their entries appear in the other lists.
    pub merged: Vec<KeyPath>,
    /// Entries of this opinion a stronger value won over.
    pub overridden: Vec<KeyPath>,
}

/// Keys from a dictionary's root down to one entry.
pub type KeyPath = Vec<Arc<str>>;

/// Why a consulted opinion did not contribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreCause {
    /// A stronger opinion ended the fold first: a dense value, a sample that
    /// does not compose, or a value of another kind.
    Shadowed,
    /// A stronger block cut it off.
    ///
    /// Spec: AOUSD Core §12.3.6.
    CutOffByBlock,
    /// Its value is of another kind than the one resolved: a scalar under an
    /// array fold, or another kind of list op.
    Incompatible,
}

impl Stage {
    /// Explains [`Stage::resolve_value`]: why a prim metadata field, such as
    /// `customData` or `apiSchemas`, has its value.
    ///
    /// Returns `None` when no opinion authors the field.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.2.5, §12.2.6.
    #[must_use]
    pub fn explain_value(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<ValueExplanation<'_, ResolvedValue>> {
        self.explain_value_by(prim, field, Lookup::Metadata)
    }

    /// Explains [`Stage::resolve_property_path`]: why a property has its
    /// default-time value, or a relationship its targets.
    ///
    /// Returns `None` when no opinion authors the property.
    ///
    /// Spec: AOUSD Core §12.3.1 (default values), §12.4 (relationships).
    #[must_use]
    pub fn explain_property_value(
        &self,
        property_path: PropertyPath,
    ) -> Option<ValueExplanation<'_, ResolvedValue>> {
        self.explain_value_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Explains [`Stage::resolve_property_path_at_time`]: why an attribute
    /// has its value at stage time `time`.
    ///
    /// Returns `None` when no opinion authors the property.
    ///
    /// Spec: AOUSD Core §12.3.2 (time samples, then splines, then the
    /// default), §12.3.2.1 (layer offsets), §12.5 (interpolation).
    #[must_use]
    pub fn explain_property_value_at_time(
        &self,
        property_path: PropertyPath,
        time: f64,
        interp: InterpolationType,
    ) -> Option<ValueExplanation<'_, Value>> {
        let prim = property_path.prim_path();
        let name = property_path.property();
        let (index, opinions) = self.opinions(prim, name, Lookup::Property)?;
        Some(self.explain_at_time(index, name, opinions, time, interp, None))
    }

    /// Explains [`Stage::resolve_value_with_schema`]: like
    /// [`Stage::explain_property_value`], falling back to the schema.
    ///
    /// Returns `None` when no opinion authors the property and the schema
    /// has no fallback for it.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    #[must_use]
    pub fn explain_value_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<ValueExplanation<'_, ResolvedValue>> {
        let index = self.prims.get(&prim);
        let authored = index.and_then(|index| index.property_opinions(field));
        let fallback = self.schema_fallback(prim, field, store, registry, api_schemas_token);

        let mut explained = None;
        if let (Some(index), Some(opinions)) = (index, authored) {
            let is_value_field = matches!(
                opinions.first()?.value,
                OpinionValue::Field(FieldValue::Value(_)) | OpinionValue::Property(_)
            ) && !opinions[0]
                .value
                .as_property()
                .is_some_and(PropertySpec::is_relationship);
            let explanation = if is_value_field {
                let fallback_value = match fallback.as_ref() {
                    Some(FieldValue::Value(value)) => Some(value),
                    _ => None,
                };
                self.explain_default(
                    index,
                    opinions,
                    index.property_type_for(&field),
                    fallback_value,
                )
            } else {
                self.explain_value_by(prim, field, Lookup::Property)?
            };
            if explanation.value.is_some() {
                return Some(explanation);
            }
            explained = Some(explanation);
        }

        let Some(fallback) = fallback else {
            return explained;
        };
        let value = match fallback {
            FieldValue::Value(Value::Dictionary(d)) => Some(ResolvedValue::Dictionary(
                crate::doc::combine_dictionary_chain([d]),
            )),
            FieldValue::Value(v) => Some(ResolvedValue::Scalar(v)),
            list => super::resolve_field_list(&list, core::iter::once(&list)),
        };
        Some(with_fallback(explained, value))
    }

    /// Explains [`Stage::resolve_value_at_time_with_schema`]: like
    /// [`Stage::explain_property_value_at_time`], falling back to the
    /// schema.
    ///
    /// Returns `None` when no opinion authors the property and the schema
    /// has no fallback for it.
    ///
    /// Spec: AOUSD Core §12.3.5 (fallback values), §12.3.6 (blocks),
    /// §13.3.2.4 (fallback value resolution).
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors resolve_value_at_time_with_schema"
    )]
    pub fn explain_value_at_time_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<ValueExplanation<'_, Value>> {
        let fallback = self.schema_fallback(prim, field, store, registry, api_schemas_token);
        let seed = match &fallback {
            Some(FieldValue::Value(value)) => Some(value),
            _ => None,
        };
        let explained = self
            .opinions(prim, field, Lookup::Property)
            .map(|(index, opinions)| {
                self.explain_at_time(index, field, opinions, time, interp, seed)
            });
        if explained.as_ref().is_some_and(|e| e.value.is_some()) {
            return explained;
        }
        // Without a fallback, an authored block (or nothing authored at
        // `time`) still explains why there is no value.
        let Some(fallback) = fallback else {
            return explained;
        };
        let value = match fallback {
            FieldValue::Value(Value::Dictionary(entries)) => {
                Some(Value::Dictionary(crate::doc::combine_dictionary_chain([
                    entries.as_slice(),
                ])))
            }
            FieldValue::Value(value) => Some(value),
            _ => None,
        };
        Some(with_fallback(explained, value))
    }

    /// Mirrors [`Stage::resolve_value`]'s dispatch.
    fn explain_value_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<ValueExplanation<'_, ResolvedValue>> {
        let (index, opinions) = self.opinions(prim, field, lookup)?;
        let strongest = opinions.first()?;
        match &strongest.value {
            OpinionValue::Property(spec) if spec.is_relationship() => {
                return Some(explain_targets(index, opinions));
            }
            OpinionValue::Field(FieldValue::PathListOp(_)) => {
                return Some(explain_targets(index, opinions));
            }
            OpinionValue::Field(list) if list.is_list_op() => {
                return Some(explain_field_list(index, opinions, list));
            }
            OpinionValue::Field(_) | OpinionValue::Property(_) => {}
        }
        Some(self.explain_default(index, opinions, index.property_type_for(&field), None))
    }

    /// Mirrors the default-time resolution of a chain of opinions.
    fn explain_default<'s>(
        &self,
        index: &'s PrimIndex,
        opinions: &'s [Opinion],
        property_type: Option<&PropertyType>,
        fallback: Option<&Value>,
    ) -> ValueExplanation<'s, ResolvedValue> {
        let mut roles = Roles::new(index, opinions);
        let reads = |opinion: &Opinion| opinion.value.default_value().is_some();

        if let Some(fold) = crate::path_expression::fold_default(opinions, fallback) {
            let seeded = roles.apply_expression_fold(&fold, reads);
            let source = match (&fold.value, fold.contributors.first()) {
                (None, _) => ValueSource::None,
                (Some(_), Some(None)) => ValueSource::Fallback,
                (Some(_), _) => ValueSource::Default,
            };
            let value = fold.value.map(ResolvedValue::Scalar);
            return roles.finish(value, source, seeded);
        }

        let sparse =
            explain_sparse_value(opinions, SparseQuery::Default { fallback }, property_type);
        match sparse.result {
            SparseResolveResult::NotApplicable => {}
            result => {
                let value = match result {
                    SparseResolveResult::Resolved(value) => Some(ResolvedValue::Scalar(value)),
                    _ => None,
                };
                roles.apply_sparse(&sparse.folds, false, reads);
                return roles.finish_sparse(value, &sparse.folds, fallback.is_some(), None);
            }
        }

        let Some(strongest) = opinions
            .iter()
            .find_map(|opinion| opinion.value.default_value())
        else {
            return roles.finish(None, ValueSource::None, false);
        };
        match strongest {
            // Spec: AOUSD Core §12.3.6 (blocked attributes).
            Value::Blocked => {
                roles.apply_strongest_wins(reads, true);
                roles.finish(None, ValueSource::None, false)
            }
            Value::Dictionary(_) => {
                let seed = fallback.and_then(|fallback| match fallback {
                    Value::Dictionary(seed) => Some(seed.as_slice()),
                    _ => None,
                });
                let (value, seeded) = roles.apply_dictionaries(seed);
                roles.finish(
                    Some(ResolvedValue::Dictionary(value)),
                    ValueSource::Default,
                    seeded,
                )
            }
            value => {
                let value = value.clone();
                roles.apply_strongest_wins(reads, false);
                roles.finish(
                    Some(ResolvedValue::Scalar(value)),
                    ValueSource::Default,
                    false,
                )
            }
        }
    }

    /// Mirrors [`Stage::resolve_value_at_time`] over one chain of opinions.
    fn explain_at_time<'s>(
        &self,
        index: &'s PrimIndex,
        field: TokenId,
        opinions: &'s [Opinion],
        time: f64,
        interp: InterpolationType,
        fallback: Option<&Value>,
    ) -> ValueExplanation<'s, Value> {
        let mut roles = Roles::new(index, opinions);
        let reads = |opinion: &Opinion| {
            opinion.value.time_samples().is_some()
                || opinion.value.spline().is_some()
                || opinion.value.default_value().is_some()
        };

        if let Some(fold) = crate::path_expression::fold_at_time(opinions, time, interp, fallback) {
            let seeded = roles.apply_expression_fold(&fold, reads);
            let mut source = None;
            for &position in fold.contributors.iter().flatten() {
                let used = roles.record_samples(position, time, interp);
                source.get_or_insert_with(|| {
                    let winner = &opinions[position];
                    if let Some((lower, upper)) = used {
                        ValueSource::TimeSamples {
                            lower,
                            upper,
                            interpolation: interp,
                            lower_seeded_by_fallback: false,
                            upper_seeded_by_fallback: false,
                        }
                    } else if winner.value.spline().is_some() {
                        ValueSource::Spline
                    } else {
                        ValueSource::Default
                    }
                });
            }
            let source = match fold.value {
                Some(_) => source.unwrap_or(ValueSource::Fallback),
                None => ValueSource::None,
            };
            return roles.finish(fold.value, source, seeded);
        }

        let sparse = explain_sparse_value(
            opinions,
            SparseQuery::AtTime {
                time,
                interp,
                fallback,
            },
            index.property_type_for(&field),
        );
        match sparse.result {
            SparseResolveResult::NotApplicable => {}
            result => {
                let value = match result {
                    SparseResolveResult::Resolved(value) => Some(value),
                    _ => None,
                };
                roles.apply_sparse(&sparse.folds, true, reads);
                return roles.finish_sparse(value, &sparse.folds, fallback.is_some(), Some(interp));
            }
        }

        // The strongest opinion that authors samples, a spline or a default
        // answers; every weaker one is hidden behind it.
        let Some((winner, value)) = opinions
            .iter()
            .find_map(|opinion| Some((opinion, value_at_time(opinion, time, interp)?)))
        else {
            return roles.finish(None, ValueSource::None, false);
        };
        roles.apply_strongest_wins(reads, value.is_none());
        let source = if value.is_none() {
            ValueSource::None
        } else if let Some((lower, upper)) = opinions
            .iter()
            .position(|o| core::ptr::eq(o, winner))
            .and_then(|position| roles.record_samples(position, time, interp))
        {
            ValueSource::TimeSamples {
                lower,
                upper,
                interpolation: interp,
                lower_seeded_by_fallback: false,
                upper_seeded_by_fallback: false,
            }
        } else if winner.value.spline().is_some() {
            ValueSource::Spline
        } else {
            ValueSource::Default
        };
        roles.finish(value, source, false)
    }
}

/// Explains a composed target (or connection) list.
///
/// Mirrors `Stage::resolve_target_list` and chains through
/// [`resolve_ordered_chain_report`].
///
/// Spec: AOUSD Core §12.2.6 (list op resolution), §12.4 (relationships and
/// connections).
fn explain_targets<'s>(
    index: &'s PrimIndex,
    opinions: &'s [Opinion],
) -> ValueExplanation<'s, ResolvedValue> {
    let mut roles = Roles::new(index, opinions);
    let with_targets: Vec<usize> = (0..opinions.len())
        .filter(|&i| opinions[i].value.targets().is_some())
        .collect();
    let is_relationship = opinions.iter().any(|op| {
        op.value
            .as_property()
            .is_some_and(PropertySpec::is_relationship)
    });
    if with_targets.is_empty() && !is_relationship {
        return roles.finish(None, ValueSource::None, false);
    }
    let ops = with_targets
        .iter()
        .map(|&i| opinions[i].value.targets().cloned());
    let (value, events) = report_list_chain(ops);
    roles.apply_events(&events, &with_targets);
    roles.finish(
        Some(ResolvedValue::PathList(value)),
        ValueSource::Default,
        false,
    )
}

/// Explains a chained list-op metadata field, such as `apiSchemas`.
///
/// Mirrors `resolve_field_list` over the same selection.
fn explain_field_list<'s>(
    index: &'s PrimIndex,
    opinions: &'s [Opinion],
    strongest: &FieldValue,
) -> ValueExplanation<'s, ResolvedValue> {
    let mut roles = Roles::new(index, opinions);
    let fields: Vec<usize> = (0..opinions.len())
        .filter(|&i| opinions[i].value.as_field().is_some())
        .collect();
    let mut chainer = ReportLists::default();
    let value = chain_field_list(
        strongest,
        opinions.iter().filter_map(|op| op.value.as_field()),
        &mut chainer,
    );
    roles.apply_events(&chainer.events, &fields);
    let source = if value.is_some() {
        ValueSource::Default
    } else {
        ValueSource::None
    };
    roles.finish(value, source, false)
}

/// Chains list ops strongest first through [`resolve_ordered_chain_report`].
///
/// `None` entries hold another kind of list op; the report names them
/// incompatible. Event provenance is the position in `ops`.
fn report_list_chain<T: Clone + Eq>(
    ops: impl Iterator<Item = Option<ListOp<T>>>,
) -> (Vec<T>, Vec<ResolutionEvent<usize>>) {
    let ops: Vec<OpinionOp<(), T, ()>> = ops
        .map(|op| op.map_or(OpinionOp::Set(()), OpinionOp::List))
        .collect();
    let positions: Vec<usize> = (0..ops.len()).collect();
    let chain = ops
        .iter()
        .zip(&positions)
        .map(|(op, provenance)| ChainOpinion { op, provenance });
    let report = resolve_ordered_chain_report(chain);
    let value = match report.resolution {
        Resolution::Resolved(resolved) => resolved.value.as_list().map(<[T]>::to_vec),
        Resolution::Absent | Resolution::Blocked { .. } => None,
    };
    (value.unwrap_or_default(), report.events)
}

/// The explanation [`ListChainer`]: chains through
/// [`resolve_ordered_chain_report`] and keeps its events.
#[derive(Default)]
struct ReportLists {
    events: Vec<ResolutionEvent<usize>>,
}

impl ListChainer for ReportLists {
    fn chain<'a, T: Clone + Eq + 'a>(
        &mut self,
        values: impl Iterator<Item = &'a FieldValue>,
        pick: impl Fn(&'a FieldValue) -> Option<&'a ListOp<T>>,
    ) -> Vec<T> {
        let (value, events) = report_list_chain(values.map(|value| pick(value).cloned()));
        self.events = events;
        value
    }
}

/// Completes a fallback explanation from the authored one, if any.
fn with_fallback<'s, T>(
    explained: Option<ValueExplanation<'s, T>>,
    value: Option<T>,
) -> ValueExplanation<'s, T> {
    let opinions = explained.map(|e| e.opinions).unwrap_or_default();
    let source = if value.is_some() {
        ValueSource::Fallback
    } else {
        ValueSource::None
    };
    ValueExplanation {
        value,
        source,
        seeded_by_fallback: false,
        opinions,
    }
}

/// Maps a family-kernel event onto an opinion's role.
fn family_role<P>(event: &FamilyEvent<P>) -> (&P, OpinionRole) {
    match event {
        FamilyEvent::ContributedDense { provenance } => {
            (provenance, OpinionRole::Contributed(Contribution::Value))
        }
        FamilyEvent::ContributedSparse { provenance } => (
            provenance,
            OpinionRole::Contributed(Contribution::ArrayEdit),
        ),
        FamilyEvent::StoppedByBlock { provenance } => (provenance, OpinionRole::Block),
        FamilyEvent::Ignored { provenance, .. } => {
            (provenance, OpinionRole::Ignored(IgnoreCause::Incompatible))
        }
    }
}

/// Maps an ordered-chain event onto an opinion's role.
fn chain_role(event: &ResolutionEvent<usize>) -> (usize, OpinionRole) {
    match event {
        ResolutionEvent::Contributed { provenance, kind } => (
            *provenance,
            OpinionRole::Contributed(match kind {
                opinionated::OpinionKind::List => Contribution::ListEdit,
                opinionated::OpinionKind::Dictionary => {
                    Contribution::Dictionary(DictionaryMerge::default())
                }
                opinionated::OpinionKind::Set | opinionated::OpinionKind::Block => {
                    Contribution::Value
                }
            }),
        ),
        ResolutionEvent::StoppedByBlock { provenance } => (*provenance, OpinionRole::Block),
        ResolutionEvent::Ignored { provenance, reason } => (
            *provenance,
            OpinionRole::Ignored(match reason {
                IgnoreReason::WeakerThanSet => IgnoreCause::Shadowed,
                IgnoreReason::WeakerThanBlock => IgnoreCause::CutOffByBlock,
                IgnoreReason::IncompatibleOperation { .. } => IgnoreCause::Incompatible,
            }),
        ),
    }
}

/// Collects each opinion's role while an explanation is built.
struct Roles<'s> {
    index: &'s PrimIndex,
    opinions: &'s [Opinion],
    roles: Vec<Option<OpinionRole>>,
    samples: Vec<Vec<SampleUse>>,
}

impl<'s> Roles<'s> {
    fn new(index: &'s PrimIndex, opinions: &'s [Opinion]) -> Self {
        Self {
            index,
            opinions,
            roles: vec![None; opinions.len()],
            samples: vec![Vec::new(); opinions.len()],
        }
    }

    /// Records ordered-chain events whose provenance is a position in
    /// `members`, which maps to positions in the opinions.
    fn apply_events(&mut self, events: &[ResolutionEvent<usize>], members: &[usize]) {
        for event in events {
            let (member, role) = chain_role(event);
            if let Some(&position) = members.get(member) {
                self.roles[position] = Some(role);
            }
        }
    }

    /// Records a strongest-wins chain over the opinions `reads` selects,
    /// whose strongest member is a value, or a block when `blocks`.
    ///
    /// Spec: AOUSD Core §12.2 (the strongest opinion wins), §12.3.6.
    fn apply_strongest_wins(&mut self, reads: impl Fn(&Opinion) -> bool, blocks: bool) {
        let members: Vec<usize> = (0..self.opinions.len())
            .filter(|&i| reads(&self.opinions[i]))
            .collect();
        // Only the strongest member's kind matters: every weaker member is
        // shadowed by a value or cut off by a block.
        let ops: Vec<OpinionOp<(), (), ()>> = (0..members.len())
            .map(|i| {
                if i == 0 && blocks {
                    OpinionOp::Block
                } else {
                    OpinionOp::Set(())
                }
            })
            .collect();
        let chain = ops
            .iter()
            .zip(&members)
            .map(|(op, provenance)| ChainOpinion { op, provenance });
        let report = resolve_ordered_chain_report(chain);
        for event in &report.events {
            let (position, role) = chain_role(event);
            self.roles[position] = Some(role);
        }
    }

    /// Records the samples of the opinion at `position` that answer a query
    /// at stage time `time` as contributing, and returns their stage times;
    /// `None` when the opinion authors no time samples.
    fn record_samples(
        &mut self,
        position: usize,
        time: f64,
        interp: InterpolationType,
    ) -> Option<(f64, f64)> {
        let opinion = &self.opinions[position];
        let samples = opinion.value.time_samples()?;
        let offset = opinion.layer_offset;
        let to_stage = |index: usize| samples[index].0 * offset.scale + offset.offset;
        let (lower, upper) =
            sample_bracket(samples, offset.map_time(time), interp).unwrap_or_default();
        let (lower, upper) = (to_stage(lower), to_stage(upper));
        let role = OpinionRole::Contributed(Contribution::Value);
        self.samples[position].push(SampleUse {
            time: lower,
            role: role.clone(),
        });
        if upper != lower {
            self.samples[position].push(SampleUse { time: upper, role });
        }
        Some((lower, upper))
    }

    /// Records a path expression fold over the opinions `reads` selects:
    /// the strongest expression and each weaker one a `%_` spliced in
    /// contributed; a block that ended the fold blocked, cutting off every
    /// weaker opinion; the other opinions were shadowed, or incompatible
    /// where the fold met a value that is not a path expression. Returns
    /// whether the schema fallback contributed along with authored opinions.
    ///
    /// Spec: AOUSD Core §12.3, §12.3.6. OpenUSD:
    /// `SdfPathExpression::ComposeOver`.
    fn apply_expression_fold(
        &mut self,
        fold: &crate::path_expression::Fold,
        reads: impl Fn(&Opinion) -> bool,
    ) -> bool {
        use crate::path_expression::Stop;
        let block = match fold.stopped_at {
            Some((Some(position), Stop::Block)) => Some(position),
            _ => None,
        };
        for position in 0..self.opinions.len() {
            if !reads(&self.opinions[position]) {
                continue;
            }
            let role = if fold.contributors.contains(&Some(position)) {
                OpinionRole::Contributed(Contribution::Value)
            } else if fold.stopped_at == Some((Some(position), Stop::Block)) {
                OpinionRole::Block
            } else if fold.stopped_at == Some((Some(position), Stop::Incompatible)) {
                OpinionRole::Ignored(IgnoreCause::Incompatible)
            } else if block.is_some_and(|block| position > block) {
                OpinionRole::Ignored(IgnoreCause::CutOffByBlock)
            } else {
                OpinionRole::Ignored(IgnoreCause::Shadowed)
            };
            self.roles[position] = Some(role);
        }
        fold.value.is_some()
            && fold.contributors.contains(&None)
            && fold.contributors.iter().any(Option::is_some)
    }

    /// Records the sparse-array folds of `explain_sparse_value`.
    ///
    /// The kernel reports the members it visited. The consulted opinions it
    /// never reached are hidden behind the member that ended the (lower)
    /// fold: cut off after a block, otherwise shadowed, or incompatible when
    /// they offer values outside the array family.
    fn apply_sparse(
        &mut self,
        folds: &[SampleFold],
        at_time: bool,
        reads: impl Fn(&Opinion) -> bool,
    ) {
        for fold in folds {
            for event in &fold.events {
                let (pos, role) = family_role(event);
                if !reads(&self.opinions[pos.opinion]) {
                    continue;
                }
                if at_time {
                    self.samples[pos.opinion].push(SampleUse {
                        time: pos.time,
                        role: role.clone(),
                    });
                }
                let current = &mut self.roles[pos.opinion];
                if rank(Some(&role)) > rank(current.as_ref()) {
                    *current = Some(role);
                }
            }
        }
        let cut_off = folds.first().is_some_and(|fold| {
            matches!(fold.events.last(), Some(FamilyEvent::StoppedByBlock { .. }))
        });
        for (position, opinion) in self.opinions.iter().enumerate() {
            if self.roles[position].is_some() || !reads(opinion) {
                continue;
            }
            let cause = if cut_off {
                IgnoreCause::CutOffByBlock
            } else if !reads_array_family(opinion, at_time) {
                IgnoreCause::Incompatible
            } else {
                IgnoreCause::Shadowed
            };
            self.roles[position] = Some(OpinionRole::Ignored(cause));
        }
    }

    /// Records the dictionaries of `dictionary_chain` combined over `seed`,
    /// and the roles of the other consulted defaults. Returns the combined
    /// value and whether the seed supplied any entry.
    ///
    /// Spec: AOUSD Core §6.6.2.1, §12.2.5, §12.3.6.
    fn apply_dictionaries(
        &mut self,
        seed: Option<&[(Arc<str>, Value)]>,
    ) -> (Vec<(Arc<str>, Value)>, bool) {
        // Which defaults take part, and why the others do not, is the
        // ordered chain's dictionary rule: blocks cut off, other values are
        // incompatible.
        self.apply_strongest_wins_dictionary();
        let chain = dictionary_chain(self.opinions)
            .map(|(position, entries)| (entries, Some(position)))
            .chain(seed.map(|seed| (seed, None)));
        let report = combine_dictionary_chain_report(&ValueDictionaries, chain);
        let mut merges: Vec<Option<DictionaryMerge>> = vec![None; self.opinions.len()];
        let mut seeded = false;
        for event in report.events {
            let (provenance, key_path, list): (
                _,
                _,
                fn(&mut DictionaryMerge) -> &mut Vec<KeyPath>,
            ) = match event {
                DictionaryEvent::Supplied {
                    provenance,
                    key_path,
                } => {
                    seeded |= provenance.is_none();
                    (provenance, key_path, |m| &mut m.supplied)
                }
                DictionaryEvent::Merged {
                    provenance,
                    key_path,
                } => (provenance, key_path, |m| &mut m.merged),
                DictionaryEvent::Overridden {
                    provenance,
                    key_path,
                } => (provenance, key_path, |m| &mut m.overridden),
            };
            if let Some(position) = provenance {
                list(merges[position].get_or_insert_with(DictionaryMerge::default)).push(key_path);
            }
        }
        for (position, merge) in merges.into_iter().enumerate() {
            if let Some(merge) = merge {
                self.roles[position] =
                    Some(OpinionRole::Contributed(Contribution::Dictionary(merge)));
            }
        }
        (report.value, seeded)
    }

    /// Records which defaults take part in a dictionary chain, and why the
    /// others do not, through the ordered chain's dictionary rule: a block
    /// cuts off weaker opinions, other values are incompatible.
    fn apply_strongest_wins_dictionary(&mut self) {
        let members: Vec<usize> = (0..self.opinions.len())
            .filter(|&i| self.opinions[i].value.default_value().is_some())
            .collect();
        let ops: Vec<OpinionOp<(), (), ()>> = members
            .iter()
            .map(|&i| match self.opinions[i].value.default_value() {
                Some(Value::Blocked) => OpinionOp::Block,
                Some(Value::Dictionary(_)) => OpinionOp::Dictionary(Vec::new()),
                _ => OpinionOp::Set(()),
            })
            .collect();
        let chain = ops
            .iter()
            .zip(&members)
            .map(|(op, provenance)| ChainOpinion { op, provenance });
        let report = resolve_ordered_chain_report(chain);
        for event in &report.events {
            let (position, role) = chain_role(event);
            self.roles[position] = Some(role);
        }
    }

    /// Finishes a sparse-array explanation.
    fn finish_sparse<T>(
        self,
        value: Option<T>,
        folds: &[SampleFold],
        has_fallback: bool,
        interp: Option<InterpolationType>,
    ) -> ValueExplanation<'s, T> {
        let contributed = self
            .roles
            .iter()
            .any(|role| matches!(role, Some(OpinionRole::Contributed(_))));
        // Each composed sample is folded on its own: one may end at a dense
        // value while the other composes over the fallback seed.
        let seeded = |fold: &SampleFold| fold.uses_seed(has_fallback);
        let source = match (&value, contributed) {
            (None, _) => ValueSource::None,
            (Some(_), false) => ValueSource::Fallback,
            (Some(_), true) => match (interp, folds) {
                (Some(interpolation), [first, .., last]) => ValueSource::TimeSamples {
                    lower: first.time,
                    upper: last.time,
                    interpolation,
                    lower_seeded_by_fallback: seeded(first),
                    upper_seeded_by_fallback: seeded(last),
                },
                (Some(interpolation), [only]) if only.time.is_finite() => {
                    ValueSource::TimeSamples {
                        lower: only.time,
                        upper: only.time,
                        interpolation,
                        lower_seeded_by_fallback: seeded(only),
                        upper_seeded_by_fallback: seeded(only),
                    }
                }
                _ => ValueSource::Default,
            },
        };
        let seeded = value.is_some() && contributed && folds.iter().any(seeded);
        self.finish(value, source, seeded)
    }

    fn finish<T>(
        self,
        value: Option<T>,
        source: ValueSource,
        seeded_by_fallback: bool,
    ) -> ValueExplanation<'s, T> {
        let graph = &self.index.graph;
        let opinions = self
            .opinions
            .iter()
            .zip(self.roles)
            .zip(self.samples)
            .filter_map(|((opinion, role), samples)| {
                Some(ExplainedOpinion {
                    opinion,
                    node: graph.node(opinion.key.node),
                    role: role?,
                    samples,
                })
            })
            .collect();
        ValueExplanation {
            value,
            source,
            seeded_by_fallback,
            opinions,
        }
    }
}

/// Orders roles for aggregating an opinion's samples: a contribution
/// outranks a block, which outranks being ignored.
fn rank(role: Option<&OpinionRole>) -> u8 {
    match role {
        None => 0,
        Some(OpinionRole::Ignored(_)) => 1,
        Some(OpinionRole::Block) => 2,
        Some(OpinionRole::Contributed(_)) => 3,
    }
}
