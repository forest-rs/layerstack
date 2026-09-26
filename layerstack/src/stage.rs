// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

mod explain;

pub use explain::{
    Contribution, DictionaryMerge, ExplainedOpinion, IgnoreCause, KeyPath, OpinionRole, SampleUse,
    ValueExplanation, ValueSource,
};

use alloc::{sync::Arc, vec, vec::Vec};

use hashbrown::{HashMap, HashSet};

use invalidation::InvalidationGraph;

use crate::variant_fallbacks::VariantFallbacks;
use crate::{
    composition_error::CompositionError,
    dependency_map::{ArcDependency, CompositionDeps},
    doc::{
        FieldValue, InterpolationType, LayerId, LayerStore, Specifier, Value,
        combine_dictionary_chain,
    },
    interner::TokenId,
    listop::{ListOp, resolve_list_chain},
    path::{PathId, PropertyPath, TargetPath},
    prim_index::{Opinion, OpinionKey, OpinionValue, PrimIndex},
    prim_index_graph::PrimIndexGraph,
    property::{PropertyKind, PropertySpec, PropertyType, Variability},
    schema::SchemaRegistry,
    spec_path::SpecPath,
    spline::{SplineData, SplineDataType},
    value_resolution::{
        SparseQuery, SparseResolveResult, interpolate_samples, resolve_sparse_value,
    },
};

/// Provenance information for resolved values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// The layer whose opinion was strongest.
    pub layer: LayerId,
    /// The spec path in that layer.
    pub spec_path: SpecPath,
    /// The field that was resolved.
    pub field: TokenId,
}

/// A resolved value (optionally with provenance).
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved<T> {
    /// The resolved value.
    pub value: T,
    /// Optional provenance for inspectors.
    pub provenance: Option<Provenance>,
}

impl<T> Resolved<T> {
    /// Returns a reference to the resolved value.
    pub fn value(&self) -> &T {
        &self.value
    }
}

/// A resolved field value.
///
/// Spec: AOUSD Core §12 (value resolution), including §12.4 for `ListOps`.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedValue {
    /// A scalar value (strongest wins).
    Scalar(Value),
    /// A token list value resolved by chaining `ListOps`.
    TokenList(Vec<TokenId>),
    /// A relationship or connection target list resolved by chaining `ListOps`.
    PathList(Vec<TargetPath>),
    /// A dictionary value resolved by combining opinions.
    ///
    /// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
    Dictionary(Vec<(Arc<str>, Value)>),
    /// A string or integer list resolved by chaining `ListOps` (for
    /// example `clipSets` or `inactiveIds`); each element keeps its value
    /// type (`Value::String`, `Value::Int`, `Value::UInt`, `Value::Int64` or
    /// `Value::UInt64`).
    ///
    /// Spec: AOUSD Core §12.2.6 (list op resolution).
    ValueList(Vec<Value>),
}

/// Chains the list ops a [`resolve_field_list`] query selects.
///
/// Resolution chains through [`LeanLists`]; explanation chains the same
/// selection through a chainer that also reports each opinion's part.
pub(crate) trait ListChainer {
    /// Chains the list ops `pick` selects from `values`, strongest first.
    /// Values `pick` rejects hold another kind of list op and are skipped.
    fn chain<'a, T: Clone + Eq + 'a>(
        &mut self,
        values: impl Iterator<Item = &'a FieldValue>,
        pick: impl Fn(&'a FieldValue) -> Option<&'a ListOp<T>>,
    ) -> Vec<T>;
}

/// The resolution [`ListChainer`]: chains without reporting.
struct LeanLists;

impl ListChainer for LeanLists {
    #[inline(always)]
    fn chain<'a, T: Clone + Eq + 'a>(
        &mut self,
        values: impl Iterator<Item = &'a FieldValue>,
        pick: impl Fn(&'a FieldValue) -> Option<&'a ListOp<T>>,
    ) -> Vec<T> {
        resolve_list_chain::<T>(&[], values.filter_map(pick).cloned())
    }
}

/// Chains the list ops of `values` (strongest first) whose variant matches
/// `strongest`, or returns `None` when `strongest` is not a list op.
///
/// Spec: AOUSD Core §12.2.6 (list op resolution).
fn resolve_field_list<'a>(
    strongest: &FieldValue,
    values: impl Iterator<Item = &'a FieldValue> + Clone,
) -> Option<ResolvedValue> {
    chain_field_list(strongest, values, &mut LeanLists)
}

/// [`resolve_field_list`] through any [`ListChainer`].
pub(crate) fn chain_field_list<'a>(
    strongest: &FieldValue,
    values: impl Iterator<Item = &'a FieldValue> + Clone,
    chainer: &mut impl ListChainer,
) -> Option<ResolvedValue> {
    fn wrap<T>(items: Vec<T>, value: impl Fn(T) -> Value) -> ResolvedValue {
        ResolvedValue::ValueList(items.into_iter().map(value).collect())
    }
    Some(match strongest {
        FieldValue::Value(_) => return None,
        FieldValue::TokenListOp(_) => {
            ResolvedValue::TokenList(chainer.chain(values, |v| match v {
                FieldValue::TokenListOp(list) => Some(list),
                _ => None,
            }))
        }
        FieldValue::PathListOp(_) => ResolvedValue::PathList(chainer.chain(values, |v| match v {
            FieldValue::PathListOp(list) => Some(list),
            _ => None,
        })),
        FieldValue::StringListOp(_) => wrap(
            chainer.chain(values, |v| match v {
                FieldValue::StringListOp(list) => Some(list),
                _ => None,
            }),
            Value::String,
        ),
        FieldValue::IntListOp(_) => wrap(
            chainer.chain(values, |v| match v {
                FieldValue::IntListOp(list) => Some(list),
                _ => None,
            }),
            Value::Int,
        ),
        FieldValue::UIntListOp(_) => wrap(
            chainer.chain(values, |v| match v {
                FieldValue::UIntListOp(list) => Some(list),
                _ => None,
            }),
            Value::UInt,
        ),
        FieldValue::Int64ListOp(_) => wrap(
            chainer.chain(values, |v| match v {
                FieldValue::Int64ListOp(list) => Some(list),
                _ => None,
            }),
            Value::Int64,
        ),
        FieldValue::UInt64ListOp(_) => wrap(
            chainer.chain(values, |v| match v {
                FieldValue::UInt64ListOp(list) => Some(list),
                _ => None,
            }),
            Value::UInt64,
        ),
    })
}

/// Which of a prim's same-named objects a query reads.
///
/// Spec: AOUSD Core §7.3 (a property spec is a child of the prim spec, a
/// metadata field a field of it; the two may share a name).
#[derive(Clone, Copy, Debug)]
enum Lookup {
    /// Only the property.
    Property,
    /// Only the prim metadata field.
    Metadata,
}

/// How a composed property is declared.
///
/// See [`Stage::resolve_property_declaration`].
///
/// Spec: AOUSD Core §12.2.2–§12.2.4.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDeclaration {
    /// Attribute or relationship.
    pub kind: PropertyKind,
    /// The declared attribute type, if any.
    pub type_name: Option<PropertyType>,
    /// The resolved variability.
    pub variability: Variability,
    /// Whether any opinion declares the property `custom`.
    pub custom: bool,
}

/// Controls partial population.
#[derive(Clone, Debug, Default)]
pub struct PopulationMask {
    /// Include these prim paths (and their ancestors).
    pub include: Vec<PathId>,
}

/// Options for stage composition and population.
#[derive(Clone, Debug, Default)]
pub struct StageOptions {
    /// Optional population mask.
    pub mask: Option<PopulationMask>,
    /// Whether resolution APIs return provenance.
    pub with_provenance: bool,
    /// Whether to record dependency edges during composition.
    pub with_dependencies: bool,
    /// Variant fallback selections: for each variant set name, the variant
    /// names to select, in order of preference, where no opinion selects a
    /// variant of that set. Empty by default, so a set without a selection
    /// contributes nothing.
    ///
    /// Where a prim's composition finds no selection for a set, the first
    /// fallback that names a variant of the set at the prim is selected;
    /// a selection authored anywhere in the prim's composition, in any
    /// layer stack, wins over it.
    ///
    /// Spec: AOUSD Core §10.3.2.5.1 selects only from opinions; fallbacks
    /// follow OpenUSD's `PcpCache::SetVariantFallbacks` and
    /// `UsdStage::SetGlobalVariantFallbacks` (see
    /// [`crate::variant_fallbacks`]).
    pub variant_fallbacks: VariantFallbacks,
}

/// A composed stage: read-only facade over composition results.
///
/// Build a `Stage` with [`Stage::compose`], then query resolved values
/// with [`Stage::resolve_field`] or traverse the prim hierarchy with
/// [`Stage::traverse`].
///
/// ```
/// use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value};
///
/// let mut store = InMemoryStore::default();
/// let color = store.tokens.intern("color");
/// let prim = store.path("/Sphere");
///
/// let mut layer = Layer::new(LayerId(1));
/// layer.insert_prim(prim, PrimSpec::def().with_field(color, Value::string("red")));
/// store.insert_layer(layer);
///
/// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
/// assert!(stage.has_prim(prim));
///
/// let resolved = stage.resolve_field(prim, color).unwrap();
/// assert_eq!(resolved.value, Value::string("red"));
/// ```
#[derive(Debug)]
pub struct Stage {
    prims: HashMap<PathId, PrimIndex>,
    children: HashMap<PathId, Vec<PathId>>,
    with_provenance: bool,
    deps: Option<CompositionDeps>,
    errors: Vec<CompositionError>,
    /// The prims composed as instances, whose descendants hold only the
    /// opinions of the instance's own arcs.
    instances: HashSet<PathId>,
    /// The variant fallbacks the stage was composed with
    /// ([`StageOptions::variant_fallbacks`]).
    variant_fallbacks: VariantFallbacks,
}

impl Stage {
    /// Composes a stage from a root layer.
    pub fn compose(store: &mut dyn LayerStore, root: LayerId, options: StageOptions) -> Self {
        crate::compose::compose_stage(store, root, options)
    }

    pub(crate) fn from_parts(
        prims: HashMap<PathId, PrimIndex>,
        children: HashMap<PathId, Vec<PathId>>,
        with_provenance: bool,
        deps: Option<CompositionDeps>,
    ) -> Self {
        Self {
            prims,
            children,
            with_provenance,
            deps,
            errors: Vec::new(),
            instances: HashSet::new(),
            variant_fallbacks: VariantFallbacks::default(),
        }
    }

    /// Records the variant fallbacks the stage was composed with.
    pub(crate) fn with_variant_fallbacks(mut self, fallbacks: VariantFallbacks) -> Self {
        self.variant_fallbacks = fallbacks;
        self
    }

    /// Records the prims composed as instances.
    pub(crate) fn with_instances(mut self, instances: HashSet<PathId>) -> Self {
        self.instances = instances;
        self
    }

    /// Attaches the composition errors found while building this stage.
    pub(crate) fn with_composition_errors(mut self, errors: Vec<CompositionError>) -> Self {
        self.errors = errors;
        self
    }

    /// Returns the composition errors found while composing this stage, in
    /// the order they were found.
    ///
    /// Composition errors are not fatal: whatever an error names was ignored
    /// and everything else was composed as normal, so the stage is usable
    /// either way.
    ///
    /// Spec: AOUSD Core §10.6 (composition errors).
    #[must_use]
    pub fn composition_errors(&self) -> &[CompositionError] {
        &self.errors
    }

    /// Replaces the prim indexes of `recomposed` with those from a partial
    /// (population-masked) composition.
    ///
    /// Only the listed prims are taken from `partial`. A masked composition
    /// also composes the ancestors and arc sources it needs, but its child
    /// lists hold only masked prims, so they must never replace this stage's
    /// complete lists; hierarchy is left untouched. Callers must detect edits
    /// that change hierarchy (see [`Stage::hierarchy_diverges`]) and rebuild
    /// instead. Dependency data is not merged; the caller updates it.
    ///
    /// Arc errors of the recomposed prims are replaced by the partial
    /// composition's. Sublayer cycle errors are kept: layer stacks change
    /// only through structural edits, which rebuild the whole stage.
    pub(crate) fn merge_prims_from(&mut self, mut partial: Self, recomposed: &[PathId]) {
        for path in recomposed {
            if let Some(index) = partial.prims.remove(path) {
                self.prims.insert(*path, index);
            }
            if partial.instances.contains(path) {
                self.instances.insert(*path);
            } else {
                self.instances.remove(path);
            }
        }
        let is_recomposed =
            |error: &CompositionError| error.prim().is_some_and(|prim| recomposed.contains(&prim));
        self.errors.retain(|error| !is_recomposed(error));
        self.errors.extend(
            partial
                .errors
                .into_iter()
                .filter(|error| is_recomposed(error)),
        );
    }

    /// Returns `true` if a partial composition shows that recomposing
    /// `recomposed` changes hierarchy: a recomposed prim appears or
    /// disappears, its children differ in membership or order, or it becomes
    /// or stops being an instance, which recomposes all its descendants.
    ///
    /// The partial composition's mask must include the current children of
    /// every recomposed prim, so its child lists for those prims are complete
    /// with respect to this stage. Paths this stage has never populated (for
    /// example children introduced by a new variant selection) are outside
    /// the mask and invisible here; such edits must be reported as structural
    /// changes.
    pub(crate) fn hierarchy_diverges(&self, partial: &Self, recomposed: &[PathId]) -> bool {
        recomposed.iter().any(|prim| {
            self.has_prim(*prim) != partial.has_prim(*prim)
                || self.children_of(*prim).unwrap_or(&[])
                    != partial.children_of(*prim).unwrap_or(&[])
                || self.instances.contains(prim) != partial.instances.contains(prim)
        })
    }

    /// Returns the source sites that contribute specs or opinions to `prim`,
    /// as `(layer, prim path within that layer)` pairs, deduplicated.
    ///
    /// Both the lookup path and the namespace path of each provenance spec
    /// path are reported: variant-branch opinions are looked up on the variant
    /// host but authored at the child path. Over-reporting is intended; this
    /// feeds invalidation, where a missed site is a correctness bug and an
    /// extra site only costs recomposition.
    pub(crate) fn source_sites(&self, prim: PathId) -> Vec<(LayerId, PathId)> {
        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        let keys = index
            .sources
            .iter()
            .chain(index.opinions_by_field.values().flatten().map(|op| &op.key));
        let mut sites = HashSet::new();
        for key in keys {
            sites.insert((key.layer_id, key.lookup_path));
            sites.insert((key.layer_id, key.spec_path.prim_path()));
        }
        sites.into_iter().collect()
    }

    /// Returns all prim paths present in the stage.
    pub(crate) fn prim_paths(&self) -> impl Iterator<Item = PathId> + '_ {
        self.prims.keys().copied()
    }

    /// Takes ownership of the composition dependency data.
    ///
    /// Returns `None` if composition was not run with
    /// [`StageOptions::with_dependencies`] enabled, or if the data has
    /// already been taken.
    pub(crate) fn take_deps(&mut self) -> Option<CompositionDeps> {
        self.deps.take()
    }

    /// Returns `true` if dependency tracking was enabled for this composition.
    #[must_use]
    pub fn has_dependencies(&self) -> bool {
        self.deps.is_some()
    }

    /// Returns a reference to the dependency graph if composition was run
    /// with [`StageOptions::with_dependencies`] enabled.
    ///
    /// The [`InvalidationGraph`] is the single source of truth for the
    /// dependency topology: "if prim A changes, which prims need
    /// recomposition?"
    #[must_use]
    pub fn graph(&self) -> Option<&InvalidationGraph<PathId>> {
        self.deps.as_ref().map(|d| &d.graph)
    }

    /// Returns all arc dependencies (diagnostic/inspection API).
    #[must_use]
    pub fn arc_dependencies(&self) -> Vec<ArcDependency> {
        self.deps
            .as_ref()
            .map(|d| d.arcs.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns arc dependencies targeting the given prim.
    #[must_use]
    pub fn arcs_targeting(&self, prim: PathId) -> Vec<ArcDependency> {
        self.deps
            .as_ref()
            .map(|d| {
                d.arcs
                    .iter()
                    .filter(|a| a.target == prim)
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns prims affected by the given layer: those that receive
    /// opinions from it, and those that a reference or payload it authors
    /// reaches, which its layer offset retimes.
    #[must_use]
    pub fn prims_affected_by_layer(&self, layer: LayerId) -> Vec<PathId> {
        self.deps
            .as_ref()
            .and_then(|d| d.layer_to_prims.get(&layer))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns layers that affect the given prim: those that contribute
    /// opinions to it, and those that author a reference or payload that
    /// reaches it (see [`Stage::prims_affected_by_layer`]).
    #[must_use]
    pub fn layers_affecting_prim(&self, prim: PathId) -> Vec<LayerId> {
        self.deps
            .as_ref()
            .and_then(|d| d.prim_to_layers.get(&prim))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Resolves a prim metadata field (never a property; see
    /// [`Stage::resolve_value`]).
    ///
    /// Returns scalar and dictionary values. For `ListOp` fields, use
    /// [`Stage::resolve_token_list`] or [`Stage::resolve_target_list`].
    #[must_use]
    pub fn resolve_field(&self, prim: PathId, field: TokenId) -> Option<Resolved<Value>> {
        self.resolve_field_by(prim, field, Lookup::Metadata)
    }

    fn resolve_field_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<Resolved<Value>> {
        let resolved = self.resolve_value_by(prim, field, lookup)?;
        match resolved.value {
            ResolvedValue::Scalar(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Dictionary(d) => Some(Resolved {
                value: Value::Dictionary(d),
                provenance: resolved.provenance,
            }),
            ResolvedValue::TokenList(_)
            | ResolvedValue::PathList(_)
            | ResolvedValue::ValueList(_) => None,
        }
    }

    /// Resolves a token `ListOp` prim metadata field, such as `apiSchemas`.
    #[must_use]
    pub fn resolve_token_list(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<TokenId>>> {
        self.resolve_token_list_by(prim, field, Lookup::Metadata)
    }

    fn resolve_token_list_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<Resolved<Vec<TokenId>>> {
        let resolved = self.resolve_value_by(prim, field, lookup)?;
        match resolved.value {
            ResolvedValue::TokenList(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Scalar(_)
            | ResolvedValue::PathList(_)
            | ResolvedValue::Dictionary(_)
            | ResolvedValue::ValueList(_) => None,
        }
    }

    /// Resolves a path list-op prim metadata field. For the connections of
    /// an attribute or the targets of a relationship, use
    /// [`Stage::resolve_target_list_path`].
    ///
    /// For an attribute the composed list is its connection paths; for a
    /// relationship, its target paths. Every opinion that authors targets contributes to the
    /// list-op chain, independently of any value the attribute also authors:
    /// connections never participate in attribute value resolution.
    ///
    /// Returns `None` when no opinion authors targets, except for a declared
    /// relationship, whose composed target list is then empty.
    ///
    /// Spec: AOUSD Core §7.6.4.2.3 (attributes may have a value, a
    /// connection, or both), §12.2.6 (list op resolution), §12.4
    /// (relationships and attribute connections).
    #[must_use]
    pub fn resolve_target_list(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<TargetPath>>> {
        self.resolve_targets_by(prim, field, Lookup::Metadata)
    }

    fn resolve_targets_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<Resolved<Vec<TargetPath>>> {
        let (_, opinions) = self.opinions(prim, field, lookup)?;
        let strongest_with_targets = opinions.iter().find(|op| op.value.targets().is_some());
        let is_relationship = opinions.iter().any(|op| {
            op.value
                .as_property()
                .is_some_and(PropertySpec::is_relationship)
        });
        if strongest_with_targets.is_none() && !is_relationship {
            return None;
        }
        let ops: Vec<ListOp<TargetPath>> = opinions
            .iter()
            .filter_map(|op| op.value.targets().cloned())
            .collect();
        Some(Resolved {
            value: resolve_list_chain::<TargetPath>(&[], ops),
            provenance: self.provenance_for(field, strongest_with_targets.unwrap_or(&opinions[0])),
        })
    }

    /// Resolves a target-path `ListOp` field on a prim.
    ///
    /// This alias is kept for older call-sites that still think of these as
    /// generic path lists. Prefer [`Stage::resolve_target_list`].
    #[must_use]
    pub fn resolve_path_list(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<TargetPath>>> {
        self.resolve_target_list(prim, field)
    }

    /// Resolves a prim metadata field.
    ///
    /// The name-based queries (this one, [`Stage::resolve_field`],
    /// [`Stage::resolve_token_list`], [`Stage::resolve_target_list`],
    /// [`Stage::resolve_value_at_time`], [`Stage::explain_field`] and
    /// [`Stage::resolve_dictionary`]) read prim metadata only. Properties are
    /// read through the property-path queries, such as
    /// [`Stage::resolve_property_path`]. A prim may author a metadata field
    /// and a property with the same name (`kind` or `apiSchemas`, for
    /// example); the two never stand in for each other.
    ///
    /// Default-time rules, shared with the property-path queries:
    ///
    /// - A metadata field or attribute resolves to the strongest authored
    ///   default: property opinions that author no default (only time samples,
    ///   a spline or connections) are skipped, never treated as a value.
    ///   Dictionaries combine; sparse array edits compose.
    /// - A relationship resolves to its composed target list.
    /// - Token and path list-op fields chain their list ops.
    ///
    /// Time samples and splines never answer a default-time query; use
    /// [`Stage::resolve_value_at_time`] for numeric times.
    ///
    /// Spec: AOUSD Core §7.3 (a property spec is a child of the prim spec,
    /// a metadata field a field of it).
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.3.1 (default
    /// values: "the specs for that attribute in each composed layer are
    /// queried for an authored default value"), §12.4 (relationships).
    /// OpenUSD reads only `default` fields at the default time
    /// (`ProcessLayerAtDefault` in `pxr/usd/usd/stage.cpp`).
    #[must_use]
    pub fn resolve_value(&self, prim: PathId, field: TokenId) -> Option<Resolved<ResolvedValue>> {
        self.resolve_value_by(prim, field, Lookup::Metadata)
    }

    /// Returns the prim index and the opinions `lookup` selects.
    fn opinions(
        &self,
        prim: PathId,
        name: TokenId,
        lookup: Lookup,
    ) -> Option<(&PrimIndex, &[Opinion])> {
        let index = self.prims.get(&prim)?;
        let opinions = match lookup {
            Lookup::Property => index.property_opinions(name),
            Lookup::Metadata => index.metadata_opinions(name),
        }?;
        Some((index, opinions))
    }

    fn resolve_value_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<Resolved<ResolvedValue>> {
        let (index, opinions) = self.opinions(prim, field, lookup)?;
        let strongest = opinions.first()?;

        match &strongest.value {
            OpinionValue::Property(spec) if spec.is_relationship() => {
                let targets = self.resolve_targets_by(prim, field, lookup)?;
                return Some(Resolved {
                    value: ResolvedValue::PathList(targets.value),
                    provenance: targets.provenance,
                });
            }
            OpinionValue::Field(FieldValue::PathListOp(_)) => {
                let targets = self.resolve_targets_by(prim, field, lookup)?;
                return Some(Resolved {
                    value: ResolvedValue::PathList(targets.value),
                    provenance: targets.provenance,
                });
            }
            OpinionValue::Field(list) if list.is_list_op() => {
                let values = opinions.iter().filter_map(|op| op.value.as_field());
                return Some(Resolved {
                    value: resolve_field_list(list, values)?,
                    provenance: self.provenance_for(field, strongest),
                });
            }
            OpinionValue::Field(_) | OpinionValue::Property(_) => {}
        }

        self.resolve_default(field, opinions, index.property_type_for(&field), None)
    }

    /// Resolves the default-time value of a chain of opinions, optionally
    /// over a schema fallback.
    fn resolve_default(
        &self,
        field: TokenId,
        opinions: &[Opinion],
        property_type: Option<&PropertyType>,
        fallback: Option<&Value>,
    ) -> Option<Resolved<ResolvedValue>> {
        let strongest_default = opinions
            .iter()
            .find(|opinion| opinion.value.default_value().is_some());

        // Spec: AOUSD Core §12.3 (a path expression's `%_` composes over
        // the next weaker one).
        if let Some(fold) = crate::path_expression::fold_default(opinions, fallback) {
            return Some(Resolved {
                value: ResolvedValue::Scalar(fold.value?),
                provenance: strongest_default.and_then(|op| self.provenance_for(field, op)),
            });
        }

        match resolve_sparse_value(opinions, SparseQuery::Default { fallback }, property_type) {
            SparseResolveResult::Resolved(value) => {
                return Some(Resolved {
                    value: ResolvedValue::Scalar(value),
                    provenance: strongest_default.and_then(|op| self.provenance_for(field, op)),
                });
            }
            SparseResolveResult::Blocked => return None,
            SparseResolveResult::NotApplicable => {}
        }

        let strongest_default = strongest_default?;
        match strongest_default.value.default_value()? {
            // Value block: suppress all weaker opinions, return no value.
            // Spec: AOUSD Core §12.3.6 (blocked attributes).
            Value::Blocked => None,
            Value::Dictionary(_) => Some(Resolved {
                value: ResolvedValue::Dictionary(resolve_dictionary_chain(
                    opinions,
                    fallback.and_then(|fallback| match fallback {
                        Value::Dictionary(seed) => Some(seed.as_slice()),
                        _ => None,
                    }),
                )),
                provenance: self.provenance_for(field, strongest_default),
            }),
            value => Some(Resolved {
                value: ResolvedValue::Scalar(value.clone()),
                provenance: self.provenance_for(field, strongest_default),
            }),
        }
    }

    /// Resolves a time-varying field on a prim at a specific numeric time.
    ///
    /// Opinions are visited strongest first. For each, authored time samples
    /// answer the query; failing those, a spline; failing that, the authored
    /// default. The first opinion that authors any of them wins, so a
    /// stronger default hides weaker samples, and a spec's own samples hide
    /// its default. Opinions that author none of them (for example only
    /// connections) are skipped. Its samples hold or interpolate with the
    /// element rules of arrays: integers hold, floating-point scalars,
    /// vectors and matrices interpolate, and samples closer than `1e-6` in
    /// layer time hold the lower one.
    ///
    /// Array-valued attributes compose instead: every opinion's samples
    /// bracketing `time` compose strongest over weakest (sparse array edits
    /// over dense arrays), and the composed bracketing samples are then held
    /// or interpolated, as in OpenUSD.
    ///
    /// Spec: AOUSD Core §12.3.2 (time based: time samples have priority over
    /// splines), §12.3.2.1 (layer offset and scale), §12.3.3 (splines),
    /// §12.3.6 (blocked samples), §12.5 (interpolation). OpenUSD applies the
    /// same per-spec order in `ProcessLayerAtTime` (`pxr/usd/usd/stage.cpp`).
    #[must_use]
    pub fn resolve_value_at_time(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
    ) -> Option<Resolved<Value>> {
        self.resolve_value_at_time_by(prim, field, time, interp, Lookup::Metadata, None)
    }

    /// Resolves the authored value of `field` at numeric `time`, with
    /// `fallback` seeding sparse array edits. Returns `None` when nothing is
    /// authored at `time` or a block is in effect; callers with a schema
    /// fallback then use it.
    fn resolve_value_at_time_by(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        lookup: Lookup,
        fallback: Option<&Value>,
    ) -> Option<Resolved<Value>> {
        let (index, opinions) = self.opinions(prim, field, lookup)?;

        // Spec: AOUSD Core §12.3 (a path expression's `%_` composes over
        // the next weaker one at every time).
        if let Some(fold) = crate::path_expression::fold_at_time(opinions, time, interp, fallback) {
            let strongest = fold.contributors.first().copied().flatten();
            return Some(Resolved {
                value: fold.value?,
                provenance: strongest.and_then(|i| self.provenance_for(field, &opinions[i])),
            });
        }

        match resolve_sparse_value(
            opinions,
            SparseQuery::AtTime {
                time,
                interp,
                fallback,
            },
            index.property_type_for(&field),
        ) {
            SparseResolveResult::Resolved(value) => {
                return Some(Resolved {
                    value,
                    provenance: self.provenance_for(field, opinions.first()?),
                });
            }
            SparseResolveResult::Blocked => return None,
            SparseResolveResult::NotApplicable => {}
        }

        let (opinion, value) = opinions
            .iter()
            .find_map(|opinion| Some((opinion, value_at_time(opinion, time, interp)?)))?;
        Some(Resolved {
            value: value?,
            provenance: self.provenance_for(field, opinion),
        })
    }

    /// Resolves a metadata field of a composed property, such as
    /// `interpolation`, `customData` or `limits`.
    ///
    /// The strongest property opinion that authors `key` wins. Dictionaries
    /// combine recursively across all opinions, so a stronger `limits.soft`
    /// minimum keeps a weaker `limits.soft` maximum; a value block discards
    /// weaker opinions. Token and path list ops chain.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.2.5 (dictionaries
    /// combine), §12.2.6 (list ops). The UI hints proposal relies on the
    /// same combining for nested `limits` dictionaries
    /// (`OpenUSD-proposals/proposals/ui-hints/README.md`).
    #[must_use]
    pub fn resolve_property_metadata(
        &self,
        prim: PathId,
        property: TokenId,
        key: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
        let opinions = self.prims.get(&prim)?.property_opinions(property)?;
        let authored: Vec<(&Opinion, &FieldValue)> = opinions
            .iter()
            .filter_map(|op| Some((op, op.value.as_property()?.metadata(key)?)))
            .collect();
        let (strongest, value) = *authored.first()?;
        let provenance = self.provenance_for(property, strongest);
        let value = match value {
            FieldValue::Value(Value::Blocked) => return None,
            FieldValue::Value(Value::Dictionary(_)) => {
                let dictionaries = authored
                    .iter()
                    .map_while(|(_, value)| match value {
                        FieldValue::Value(Value::Blocked) => None,
                        other => Some(other),
                    })
                    .filter_map(|value| match value {
                        FieldValue::Value(Value::Dictionary(entries)) => Some(entries.as_slice()),
                        _ => None,
                    });
                ResolvedValue::Dictionary(combine_dictionary_chain(dictionaries))
            }
            FieldValue::Value(value) => ResolvedValue::Scalar(value.clone()),
            list => resolve_field_list(list, authored.iter().map(|(_, value)| *value))?,
        };
        Some(Resolved { value, provenance })
    }

    /// Resolves how a composed property is declared: its kind, type,
    /// variability and `custom` qualifier.
    ///
    /// Returns `None` when no property spec contributes to `property` (for
    /// example when only prim metadata of that name is authored).
    ///
    /// - The kind and type come from the strongest property opinion that
    ///   authors them.
    /// - `custom` is `true` if any opinion authors it (Core §12.2.4).
    /// - Variability comes from the weakest opinion (Core §12.2.3), since no
    ///   prim definition is consulted here.
    ///
    /// Spec: AOUSD Core §12.2.2–§12.2.4.
    #[must_use]
    pub fn resolve_property_declaration(
        &self,
        prim: PathId,
        property: TokenId,
    ) -> Option<PropertyDeclaration> {
        let index = self.prims.get(&prim)?;
        let opinions = index.property_opinions(property)?;
        let mut specs = opinions.iter().filter_map(|op| op.value.as_property());
        let strongest = specs.next()?;
        let mut declaration = PropertyDeclaration {
            kind: strongest.kind,
            type_name: index.property_type_for(&property).cloned(),
            variability: strongest.variability,
            custom: strongest.custom,
        };
        for spec in specs {
            declaration.custom |= spec.custom;
            declaration.variability = spec.variability;
        }
        Some(declaration)
    }

    /// Resolves the property ordering (`reorder properties`) of a composed
    /// prim: the strongest authored `propertyOrder`.
    ///
    /// OpenUSD sorts composed property names and then moves the names listed
    /// here to the front, in order (`UsdPrim::ApplyPropertyOrder`,
    /// `pxr/usd/usd/prim.cpp`).
    ///
    /// Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`), §12.2 (strongest
    /// opinion).
    #[must_use]
    pub fn resolve_property_order(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Option<Vec<TokenId>> {
        use crate::spec_path::SpecComponent;

        let index = self.prims.get(&prim)?;
        index.sources.iter().find_map(|source| {
            let spec = store.layer(source.layer_id)?.source_prim_spec(
                source.lookup_path,
                &source.spec_path,
                store.paths(),
            )?;
            match source.spec_path.components().last() {
                Some(SpecComponent::VariantSelection { set, variant }) => spec
                    .variant_sets
                    .get(set)?
                    .variants
                    .get(variant)?
                    .property_order
                    .clone(),
                _ => spec.property_order.clone(),
            }
        })
    }

    /// Returns the sorted opinion stack for `(prim, field)` (strongest-first).
    ///
    /// This is intended for inspection/debugging and mirrors the "stack of
    /// opinions" described by the spec.
    ///
    /// Spec: AOUSD Core §12 (value resolution) and §10.4 (strength ordering).
    #[must_use]
    pub fn explain_field(&self, prim: PathId, field: TokenId) -> Option<&[Opinion]> {
        self.opinions(prim, field, Lookup::Metadata)
            .map(|(_, opinions)| opinions)
    }

    /// Resolves a concrete property path.
    #[must_use]
    pub fn resolve_property_path(
        &self,
        property_path: PropertyPath,
    ) -> Option<Resolved<ResolvedValue>> {
        self.resolve_value_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Resolves a scalar or dictionary field via concrete [`PropertyPath`].
    #[must_use]
    pub fn resolve_field_path(&self, property_path: PropertyPath) -> Option<Resolved<Value>> {
        self.resolve_field_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Resolves a target-list field via concrete [`PropertyPath`].
    #[must_use]
    pub fn resolve_target_list_path(
        &self,
        property_path: PropertyPath,
    ) -> Option<Resolved<Vec<TargetPath>>> {
        self.resolve_targets_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Resolves a concrete property path at a specific time.
    #[must_use]
    pub fn resolve_property_path_at_time(
        &self,
        property_path: PropertyPath,
        time: f64,
        interp: InterpolationType,
    ) -> Option<Resolved<Value>> {
        self.resolve_value_at_time_by(
            property_path.prim_path(),
            property_path.property(),
            time,
            interp,
            Lookup::Property,
            None,
        )
    }

    /// Returns the sorted opinion stack for a concrete property path.
    #[must_use]
    pub fn explain_property_path(&self, property_path: PropertyPath) -> Option<&[Opinion]> {
        self.opinions(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
        .map(|(_, opinions)| opinions)
    }

    /// Returns `true` if the stage contains opinions for a concrete property path.
    #[must_use]
    pub fn has_property_path(&self, property_path: PropertyPath) -> bool {
        self.explain_property_path(property_path).is_some()
    }

    /// Traverses prims in a deterministic preorder.
    pub fn traverse(&self, root: PathId) -> Traverse<'_> {
        Traverse::new(self, root)
    }

    /// Returns the direct children of `prim` in deterministic order.
    ///
    /// This is an inspection API intended for conformance and debugging.
    ///
    /// Spec: AOUSD Core §11 (stage population) requires deterministic traversal.
    #[must_use]
    pub fn children_of(&self, prim: PathId) -> Option<&[PathId]> {
        self.children.get(&prim).map(|v| v.as_slice())
    }

    /// Returns every source that contributes to `prim`, strongest first.
    ///
    /// This is the full ordered source stack: one [`OpinionKey`] per
    /// contributing `(arc, layer, spec)` site, with repeated sites kept. A
    /// site reached through two arcs (a reference diamond, or a layer that
    /// appears twice in a layer stack) appears twice, as it does in
    /// OpenUSD's `PcpPrimIndex::GetPrimStack()`
    /// (`pxr/usd/pcp/primIndex.h`). [`Stage::prim_stack`] is the
    /// deduplicated `(layer, spec)` projection of this stack.
    ///
    /// This is an inspection API intended for conformance and debugging,
    /// the prim-level counterpart of [`Stage::explain_field`].
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering).
    #[must_use]
    pub fn explain_prim(&self, prim: PathId) -> Option<&[OpinionKey]> {
        self.prims.get(&prim).map(|index| index.sources.as_slice())
    }

    /// Returns the composition graph of `prim`: one node per arc expansion
    /// that contributes to it, with its arc kind, layer stack, site and
    /// parent. Every [`OpinionKey::node`] of the prim's opinions and sources
    /// names a node of this graph.
    ///
    /// This is an inspection API intended for conformance and debugging;
    /// see [`PrimIndexGraph`] for how the graph relates to strength order.
    /// It mirrors OpenUSD's `PcpPrimIndex::GetGraph()`
    /// (`pxr/usd/pcp/primIndex.h`).
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering).
    #[must_use]
    pub fn explain_prim_graph(&self, prim: PathId) -> Option<&PrimIndexGraph> {
        self.prims.get(&prim).map(|index| &index.graph)
    }

    /// Returns the composed prim stack as `(layer_id, spec_path)` pairs (strongest-first).
    ///
    /// Each `(layer, spec)` site appears once, at its strongest position; use
    /// [`Stage::explain_prim`] for the full stack with repeated sites.
    ///
    /// This is an inspection API intended for conformance and debugging.
    ///
    /// Spec: AOUSD Core §11 (stage population) and §10.4 (strength ordering).
    #[must_use]
    pub fn prim_stack(&self, prim: PathId) -> Option<Vec<(LayerId, SpecPath)>> {
        use hashbrown::HashSet;

        let index = self.prims.get(&prim)?;
        let mut out = Vec::new();
        let mut seen_pairs = HashSet::<(LayerId, SpecPath)>::new();
        for key in &index.sources {
            let pair = (key.layer_id, key.spec_path.clone());
            if seen_pairs.insert(pair.clone()) {
                out.push(pair);
            }
        }
        Some(out)
    }

    /// Returns the variant selections that govern the composed prim
    /// `prim`, keyed by variant set: for each set, the strongest selection
    /// authored on any site of the prim's index, including selections
    /// authored inside selected variants, and for a declared set without
    /// one, the variant the stage's fallbacks select
    /// ([`StageOptions::variant_fallbacks`]). Empty when `prim` is not on
    /// the stage or selects nothing.
    ///
    /// An authored selection is reported whether or not the variant it
    /// names exists.
    ///
    /// OpenUSD: `UsdVariantSet::GetVariantSelection`
    /// (`pxr/usd/usd/variantSets.h`), which reports the variant composition
    /// selected, fallbacks included; `UsdVariantSets::GetAllVariantSelections`
    /// reports the authored selections alone.
    ///
    /// Spec: AOUSD Core §10.5 (variant selection).
    #[must_use]
    pub fn variant_selections(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> HashMap<TokenId, TokenId> {
        self.prims
            .get(&prim)
            .map(|index| {
                crate::compose::strength_ordered_variant_selections(
                    store,
                    &self.variant_fallbacks,
                    index,
                )
            })
            .unwrap_or_default()
    }

    /// Returns `true` if the stage contains a prim at `path`.
    #[must_use]
    pub fn has_prim(&self, path: PathId) -> bool {
        self.prims.contains_key(&path)
    }

    /// Resolves the specifier for a composed prim.
    ///
    /// Specifier resolution follows special rules per §12.2.1:
    /// - If all contributing opinions are `over`, the prim is *undefining* → `Over`.
    /// - If the strongest defining opinion is `class`, the prim is *abstractly defining* → `Class`.
    /// - If the strongest defining opinion is `def`, the prim is *concretely defining* → `Def`.
    ///
    /// Spec: AOUSD Core §12.2.1 (specifier resolution), §7.6.
    #[must_use]
    pub fn resolve_specifier(&self, prim: PathId, store: &dyn LayerStore) -> Option<Specifier> {
        let index = self.prims.get(&prim)?;
        let mut strongest_defining: Option<Specifier> = None;

        // Walk sources in strength order (strongest first) and find the
        // strongest defining opinion (def or class).
        for key in &index.sources {
            let Some(layer) = store.layer(key.layer_id) else {
                continue;
            };
            let Some(spec) = layer.source_prim_spec(key.lookup_path, &key.spec_path, store.paths())
            else {
                continue;
            };
            match spec.specifier {
                Some(Specifier::Def) | Some(Specifier::Class) => {
                    if strongest_defining.is_none() {
                        strongest_defining = spec.specifier;
                    }
                }
                Some(Specifier::Over) | None => {}
            }
        }

        Some(strongest_defining.unwrap_or(Specifier::Over))
    }

    /// Returns `true` if the prim is *defined* per §11.5.
    ///
    /// A prim is defined if its resolved specifier is `def` or `class`
    /// (i.e. not purely `over`).
    #[must_use]
    pub fn is_defined(&self, prim: PathId, store: &dyn LayerStore) -> bool {
        matches!(
            self.resolve_specifier(prim, store),
            Some(Specifier::Def) | Some(Specifier::Class)
        )
    }

    /// Returns `true` if the prim is *abstract* (specifier resolves to `class`).
    #[must_use]
    pub fn is_abstract(&self, prim: PathId, store: &dyn LayerStore) -> bool {
        matches!(self.resolve_specifier(prim, store), Some(Specifier::Class))
    }

    /// Resolves the type name for a composed prim.
    ///
    /// Returns the strongest opinion's type name. If no contributing source
    /// has a type name, returns `None`.
    ///
    /// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
    #[must_use]
    pub fn resolve_type_name(&self, prim: PathId, store: &dyn LayerStore) -> Option<TokenId> {
        let index = self.prims.get(&prim)?;
        for key in &index.sources {
            let Some(layer) = store.layer(key.layer_id) else {
                continue;
            };
            let Some(spec) = layer.source_prim_spec(key.lookup_path, &key.spec_path, store.paths())
            else {
                continue;
            };
            if let Some(tn) = spec.type_name {
                return Some(tn);
            }
        }
        None
    }

    /// Resolves a property on a prim with schema fallback.
    ///
    /// Like [`Stage::resolve_property_path`], but when no authored opinion exists,
    /// consults the schema registry for a fallback value based on the prim's
    /// resolved type name and applied API schemas.
    ///
    /// `api_schemas_token` is the interned token for `"apiSchemas"`. Pass it
    /// so the resolver can look up applied API schemas on the prim. If `None`,
    /// only the typed schema (and its built-ins / auto-applies) are consulted.
    ///
    /// Only properties are read here; the applied schemas come from the
    /// prim metadata field `apiSchemas`, never from a property that happens
    /// to share its name.
    ///
    /// A strongest default block resolves the fallback (Core §12.3.6), as it
    /// does at numeric times ([`Stage::resolve_value_at_time_with_schema`]).
    /// OpenUSD 26.08 resolves no value at the default time there: the named
    /// divergence `default-time-block-hides-fallback`
    /// (`docs/generic-sparse-composition.md`, "Divergences From OpenUSD").
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    #[must_use]
    pub fn resolve_value_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<Resolved<ResolvedValue>> {
        let index = self.prims.get(&prim);
        let authored = index.and_then(|index| index.property_opinions(field));
        let fallback = self.schema_fallback(prim, field, store, registry, api_schemas_token);

        if let (Some(index), Some(opinions)) = (index, authored) {
            let is_value_field = matches!(
                opinions.first()?.value,
                OpinionValue::Field(FieldValue::Value(_)) | OpinionValue::Property(_)
            ) && !opinions[0]
                .value
                .as_property()
                .is_some_and(PropertySpec::is_relationship);
            if is_value_field {
                // A dictionary fallback is the weakest opinion in the
                // combining chain, as in OpenUSD's
                // `MetadataValueComposer::ConsumeUsdFallback`; an array
                // fallback seeds sparse edits. A block falls through to the
                // fallback itself.
                //
                // Spec: AOUSD Core §6.6.2.1, §12.3.6, §13.3.2.4 (fallback
                // value resolution).
                let fallback_value = match fallback.as_ref() {
                    Some(FieldValue::Value(value)) => Some(value),
                    _ => None,
                };
                if let Some(resolved) = self.resolve_default(
                    field,
                    opinions,
                    index.property_type_for(&field),
                    fallback_value,
                ) {
                    return Some(resolved);
                }
            } else if let Some(resolved) = self.resolve_value_by(prim, field, Lookup::Property) {
                return Some(resolved);
            }
        }

        // No authored opinion — consult the schema registry.
        let fallback = fallback?;

        Some(Resolved {
            value: match fallback {
                FieldValue::Value(Value::Dictionary(d)) => {
                    ResolvedValue::Dictionary(combine_dictionary_chain([d]))
                }
                FieldValue::Value(v) => ResolvedValue::Scalar(v),
                list => resolve_field_list(&list, core::iter::once(&list))?,
            },
            provenance: None,
        })
    }

    /// Resolves a scalar property on a prim with schema fallback.
    ///
    /// Like [`Stage::resolve_field`], but falls back to the schema registry.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    #[must_use]
    pub fn resolve_field_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<Resolved<Value>> {
        let resolved =
            self.resolve_value_with_schema(prim, field, store, registry, api_schemas_token)?;
        match resolved.value {
            ResolvedValue::Scalar(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Dictionary(d) => Some(Resolved {
                value: Value::Dictionary(d),
                provenance: resolved.provenance,
            }),
            ResolvedValue::TokenList(_)
            | ResolvedValue::PathList(_)
            | ResolvedValue::ValueList(_) => None,
        }
    }

    /// Resolves a property on a prim at a numeric time with schema fallback.
    ///
    /// Like [`Stage::resolve_property_path_at_time`], with the default-time
    /// fallback contract of [`Stage::resolve_value_with_schema`]:
    ///
    /// - When nothing is authored at `time`, or a block is in effect there,
    ///   the schema fallback resolves. A sampled block counts like a default
    ///   block: Core §12.3.6 resolves a block to the fallback, and §16.2.16.3
    ///   gives blocked time samples "the same semantics as when blocking the
    ///   default attribute value".
    /// - An array fallback is the weakest dense seed that sparse array edits
    ///   compose over, whether their samples compose over no weaker opinion or
    ///   over a block, sampled or default.
    /// - Otherwise authored samples, splines and defaults resolve exactly as
    ///   in [`Stage::resolve_property_path_at_time`]; opinions hidden behind a
    ///   dense value or block are never evaluated.
    ///
    /// OpenUSD 26.08 agrees except after a sampled block, where it resolves
    /// no value and composes stronger edits over the empty array: the named
    /// divergence `sampled-block-drops-fallback`
    /// (`docs/generic-sparse-composition.md`, "Divergences From OpenUSD").
    ///
    /// `api_schemas_token` is as for [`Stage::resolve_value_with_schema`].
    ///
    /// Spec: AOUSD Core §12.3.2 (time-based resolution), §12.3.5 (fallback
    /// values), §12.3.6 (blocked attributes), §13.3.2.4 (fallback value
    /// resolution), §16.2.16.3 (blocked time samples).
    #[must_use]
    pub fn resolve_value_at_time_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<Resolved<Value>> {
        let fallback = self.schema_fallback(prim, field, store, registry, api_schemas_token);
        let seed = match &fallback {
            Some(FieldValue::Value(value)) => Some(value),
            _ => None,
        };
        if let Some(resolved) =
            self.resolve_value_at_time_by(prim, field, time, interp, Lookup::Property, seed)
        {
            return Some(resolved);
        }
        let value = match fallback? {
            FieldValue::Value(Value::Dictionary(entries)) => {
                Value::Dictionary(combine_dictionary_chain([entries.as_slice()]))
            }
            FieldValue::Value(value) => value,
            _ => return None,
        };
        Some(Resolved {
            value,
            provenance: None,
        })
    }

    /// The schema fallback for `field` on `prim`: from its resolved type name
    /// and, when `api_schemas_token` is given, its applied API schemas.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    fn schema_fallback(
        &self,
        prim: PathId,
        field: TokenId,
        store: &dyn LayerStore,
        registry: &SchemaRegistry,
        api_schemas_token: Option<TokenId>,
    ) -> Option<FieldValue> {
        let type_name = self.resolve_type_name(prim, store);
        let applied = api_schemas_token
            .and_then(|tok| self.resolve_token_list(prim, tok))
            .map(|r| r.value)
            .unwrap_or_default();
        registry.resolve_fallback(type_name, &applied, field)
    }

    /// Resolves a dictionary-valued field on a prim, combining opinions.
    ///
    /// Returns `None` if the field does not exist or is not dictionary-valued.
    ///
    /// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "Resolved<Vec<(Arc<str>, Value)>> is the natural return type"
    )]
    pub fn resolve_dictionary(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<(Arc<str>, Value)>>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::Dictionary(d) => Some(Resolved {
                value: d,
                provenance: resolved.provenance,
            }),
            _ => None,
        }
    }

    fn provenance_for(&self, field: TokenId, strongest: &Opinion) -> Option<Provenance> {
        self.with_provenance.then_some(Provenance {
            layer: strongest.key.layer_id,
            spec_path: strongest.key.spec_path.clone(),
            field,
        })
    }
}

/// An iterator for deterministic preorder stage traversal.
///
/// Yields each [`PathId`] starting from the root, visiting children in
/// authored order before moving to sibling subtrees. Created by
/// [`Stage::traverse`].
#[derive(Debug)]
pub struct Traverse<'a> {
    stage: &'a Stage,
    stack: Vec<PathId>,
}

impl<'a> Traverse<'a> {
    fn new(stage: &'a Stage, root: PathId) -> Self {
        Self {
            stage,
            stack: vec![root],
        }
    }
}

impl Iterator for Traverse<'_> {
    type Item = PathId;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.stack.pop()?;
        if let Some(children) = self.stage.children.get(&next) {
            for child in children.iter().rev() {
                self.stack.push(*child);
            }
        }
        Some(next)
    }
}

/// The value `opinion` offers a non-sparse query at stage time `time`: its
/// time samples, else its spline, else its default.
///
/// Returns `None` when the opinion authors none of them, so weaker opinions
/// answer, and `Some(None)` when it answers with no value: a block in effect
/// at `time` (a blocked sample or default, or a spline that evaluates to
/// nothing), which hides every weaker opinion.
///
/// Spec: AOUSD Core §12.3.2 (time samples, then splines, then the
/// default), §12.3.2.1 (layer offsets), §12.3.6 (blocked attributes).
pub(crate) fn value_at_time(
    opinion: &Opinion,
    time: f64,
    interp: InterpolationType,
) -> Option<Option<Value>> {
    // Apply the opinion's accumulated layer offset to remap the query time
    // before sampling.
    let mapped_time = opinion.layer_offset.map_time(time);
    let value = if let Some(samples) = opinion.value.time_samples() {
        interpolate_samples(samples, mapped_time, interp)
    } else if let Some(spline) = opinion.value.spline() {
        // A spline that evaluates to nothing (block extrapolation or a
        // blocked segment) yields no value.
        spline
            .evaluate(mapped_time)
            .map(|value| spline_to_value(spline, value))
    } else {
        Some(opinion.value.default_value()?.clone())
    };
    // A block in effect at the query time, as a sample or a default,
    // resolves to no value.
    Some(value.filter(|value| *value != Value::Blocked))
}

/// Convert a spline evaluation result to the appropriate [`Value`] type
/// based on the spline's data type.
#[allow(
    clippy::cast_possible_truncation,
    reason = "f64→f32 intentional for single-precision splines"
)]
fn spline_to_value(spline: &SplineData, val: f64) -> Value {
    match spline.data_type {
        SplineDataType::Double | SplineDataType::Unspecified => Value::Double(val),
        SplineDataType::Float => Value::Float(val as f32),
        SplineDataType::Half => Value::Half(crate::half::from_f64(val)),
    }
}

/// Combines the dictionary opinions of a chain whose strongest opinion is a
/// dictionary, optionally over a schema `fallback` seed.
///
/// `layerstack` selects the participating opinions; the recursive combining
/// itself is delegated to `opinionated` through [`combine_dictionary_chain`].
/// A value block discards every weaker opinion (AOUSD Core §12.3.6); stronger
/// dictionaries still combine over the fallback. Non-dictionary opinions are
/// skipped.
///
/// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
fn resolve_dictionary_chain(
    opinions: &[Opinion],
    fallback: Option<&[(Arc<str>, Value)]>,
) -> Vec<(Arc<str>, Value)> {
    let authored = dictionary_chain(opinions).map(|(_, entries)| entries);
    combine_dictionary_chain(authored.chain(fallback))
}

/// The authored dictionaries [`resolve_dictionary_chain`] combines, strongest
/// first, each with its position in `opinions`: every dictionary default
/// stronger than the strongest blocking default.
pub(crate) fn dictionary_chain(
    opinions: &[Opinion],
) -> impl Iterator<Item = (usize, &[(Arc<str>, Value)])> {
    opinions
        .iter()
        .enumerate()
        .filter_map(|(position, opinion)| Some((position, opinion.value.default_value()?)))
        .take_while(|(_, value)| !matches!(value, Value::Blocked))
        .filter_map(|(position, value)| match value {
            Value::Dictionary(entries) => Some((position, entries.as_slice())),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LayerOffset, OpinionKey,
        array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex},
        interner::TokenInterner,
        path::{Path, PathInterner},
        property::PropertyType,
        spec_path::SpecPath,
    };
    use alloc::sync::Arc;
    use alloc::vec;

    /// Test-only opinion payload: a property authoring only time samples.
    fn samples(samples: Vec<(f64, Value)>) -> OpinionValue {
        OpinionValue::from(PropertySpec {
            time_samples: Some(samples),
            ..PropertySpec::default()
        })
    }

    fn array_value(values: &[i32]) -> Value {
        Value::Array(values.iter().copied().map(Value::Int).collect())
    }

    fn int_array_type() -> PropertyType {
        PropertyType::new(Arc::<str>::from("int"), true, Value::Int(0))
    }

    fn test_key(layer: LayerId, lookup_path: PathId) -> OpinionKey {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path = SpecPath::parse("/A", &mut tokens, &mut paths).expect("spec path");
        OpinionKey {
            node: crate::prim_index_graph::NodeId::ROOT,
            layer_strength: 0,
            layer_id: layer,
            lookup_path,
            spec_path,
        }
    }

    #[test]
    fn resolves_sparse_array_edit_over_dense_default() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("valid path"));
        let field = tokens.intern("x");

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_property_type(field, key.clone(), int_array_type());
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(Value::Int(9)),
                    index: ArrayIndex::Position(0),
                }],
            }))
            .into(),
            layer_offset: LayerOffset::IDENTITY,
        });
        index.add_opinion(Opinion {
            key: OpinionKey {
                layer_strength: 1,
                ..key.clone()
            },
            field,
            value: FieldValue::Value(array_value(&[1, 2])).into(),
            layer_offset: LayerOffset::IDENTITY,
        });

        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        let resolved = stage.resolve_field(prim, field).expect("resolved value");
        assert_eq!(resolved.value, array_value(&[9, 2]));
    }

    #[test]
    fn resolves_time_sampled_sparse_array_edits() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("valid path"));
        let field = tokens.intern("x");

        let identity = Value::ArrayEdit(ArrayEdit::default());
        let override_sample = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(9)),
                index: ArrayIndex::Position(0),
            }],
        });

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_property_type(field, key.clone(), int_array_type());
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: samples(vec![
                (0.0, identity.clone()),
                (2.0, override_sample),
                (3.0, identity),
            ]),
            layer_offset: LayerOffset::IDENTITY,
        });
        index.add_opinion(Opinion {
            key: OpinionKey {
                layer_strength: 1,
                ..key.clone()
            },
            field,
            value: PropertySpec::attribute()
                .with_default(array_value(&[1, 2]))
                .into(),
            layer_offset: LayerOffset::IDENTITY,
        });

        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        assert_eq!(
            stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim, field),
                    2.5,
                    InterpolationType::Held
                )
                .expect("resolved override")
                .value,
            array_value(&[9, 2])
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim, field),
                    3.5,
                    InterpolationType::Held
                )
                .expect("resolved reset")
                .value,
            array_value(&[1, 2])
        );
    }

    #[test]
    fn sampled_block_hides_weaker_array_at_time() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("valid path"));
        let field = tokens.intern("x");

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_property_type(field, key.clone(), int_array_type());
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: samples(vec![(0.0, Value::Blocked)]),
            layer_offset: LayerOffset::IDENTITY,
        });
        index.add_opinion(Opinion {
            key: OpinionKey {
                layer_strength: 1,
                ..key.clone()
            },
            field,
            value: PropertySpec::attribute()
                .with_default(array_value(&[42]))
                .into(),
            layer_offset: LayerOffset::IDENTITY,
        });

        // Spec: AOUSD Core §12.3.6 (individual time samples can be blocked).
        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        for time in [0.0, 1.0] {
            assert_eq!(
                stage.resolve_value_at_time(prim, field, time, InterpolationType::Held),
                None,
                "the sampled block must hide the weaker array at t={time}"
            );
        }
    }

    /// Builds a `Mesh`-typed `/A` whose `x` array field carries `opinions`
    /// (strongest first) and whose schema fallback is `[5, 6]`.
    fn schema_fallback_fixture(
        opinions: Vec<FieldValue>,
    ) -> (
        Stage,
        crate::doc::InMemoryStore,
        SchemaRegistry,
        PathId,
        TokenId,
    ) {
        schema_fallback_fixture_of(opinions, array_value(&[5, 6]), int_array_type())
    }

    /// Like [`schema_fallback_fixture`], for a property of `property_type`
    /// whose schema fallback is `fallback`.
    fn schema_fallback_fixture_of(
        opinions: Vec<FieldValue>,
        fallback: Value,
        property_type: PropertyType,
    ) -> (
        Stage,
        crate::doc::InMemoryStore,
        SchemaRegistry,
        PathId,
        TokenId,
    ) {
        let mut store = crate::doc::InMemoryStore::default();
        let prim = store.path("/A");
        let field = store.tokens.intern("x");
        let mesh = store.tokens.intern("Mesh");

        let mut layer = crate::doc::Layer::new(LayerId(1));
        layer.insert_prim(
            prim,
            crate::doc::PrimSpec {
                type_name: Some(mesh),
                ..crate::doc::PrimSpec::default()
            },
        );
        store.insert_layer(layer);

        let mut registry = SchemaRegistry::new();
        registry
            .register(crate::schema::SchemaDefinition::typed(mesh).with_property(field, fallback));

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_source(key.clone());
        index.add_property_type(field, key.clone(), property_type);
        for (strength, value) in opinions.into_iter().enumerate() {
            index.add_opinion(Opinion {
                key: OpinionKey {
                    layer_strength: u16::try_from(strength).expect("small fixture"),
                    ..key.clone()
                },
                field,
                // Schema fallbacks are for properties: author each value as an
                // attribute default.
                value: match value {
                    FieldValue::Value(default) => {
                        PropertySpec::attribute().with_default(default).into()
                    }
                    other => other.into(),
                },
                layer_offset: LayerOffset::IDENTITY,
            });
        }

        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        (stage, store, registry, prim, field)
    }

    /// A path expression's `%_` composes over the next weaker opinion, and
    /// over the schema fallback once no authored opinion is left, through
    /// every resolution and explanation entry point.
    ///
    /// Spec: AOUSD Core §12.3, §13.3.2.4. OpenUSD:
    /// `SdfPathExpression::ComposeOver`.
    #[test]
    fn path_expressions_compose_over_the_schema_fallback() {
        let expression = |text: &str| Value::PathExpression(text.into());
        let (stage, store, registry, prim, field) = schema_fallback_fixture_of(
            vec![
                FieldValue::Value(expression("/Strong %_")),
                FieldValue::Value(expression("%_ /Weak")),
            ],
            expression("/Fallback"),
            PropertyType::new(Arc::<str>::from("pathExpression"), false, expression("")),
        );
        let property = PropertyPath::new(prim, field);
        let authored = Some(expression("/Strong /Weak"));
        let composed = expression("/Strong (/Fallback /Weak)");

        assert_eq!(
            stage.resolve_field_path(property).map(|r| r.value),
            authored
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(property, 1.0, InterpolationType::Held)
                .map(|r| r.value),
            authored
        );
        assert_eq!(
            stage
                .resolve_value_with_schema(prim, field, &store, &registry, None)
                .map(|r| r.value),
            Some(ResolvedValue::Scalar(composed.clone()))
        );
        assert_eq!(
            stage
                .resolve_value_at_time_with_schema(
                    prim,
                    field,
                    1.0,
                    InterpolationType::Held,
                    &store,
                    &registry,
                    None
                )
                .map(|r| r.value),
            Some(composed.clone())
        );
        let explained = stage
            .explain_value_with_schema(prim, field, &store, &registry, None)
            .expect("authored");
        assert_eq!(
            explained.value,
            Some(ResolvedValue::Scalar(composed.clone()))
        );
        assert!(explained.seeded_by_fallback);
        assert_eq!(explained.contributors().count(), 2);
        let explained = stage
            .explain_value_at_time_with_schema(
                prim,
                field,
                1.0,
                InterpolationType::Held,
                &store,
                &registry,
                None,
            )
            .expect("authored");
        assert_eq!(explained.value, Some(composed));
        assert_eq!(explained.contributors().count(), 2);
    }

    fn append_edit(value: i32) -> Value {
        Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(value)),
                index: ArrayIndex::End,
            }],
        })
    }

    /// A block discards weaker authored opinions (AOUSD Core §12.3.6), but
    /// stronger sparse edits still compose over the weakest dense value that
    /// survives it, the schema fallback or the empty array (sparse-array-edits
    /// proposal, "Value Resolution").
    #[test]
    fn stronger_array_edit_over_block_materializes_over_fallback() {
        let (stage, store, registry, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);

        let without_schema = stage
            .resolve_property_path(PropertyPath::new(prim, field))
            .expect("edit resolves");
        assert_eq!(
            without_schema.value,
            ResolvedValue::Scalar(array_value(&[7])),
            "without a schema fallback the edit materializes over the empty array"
        );

        let with_schema = stage
            .resolve_value_with_schema(prim, field, &store, &registry, None)
            .expect("edit resolves");
        assert_eq!(
            with_schema.value,
            ResolvedValue::Scalar(array_value(&[5, 6, 7])),
            "the edit composes over the schema fallback, never over the blocked [1, 2]"
        );
    }

    #[test]
    fn strongest_array_block_resolves_to_schema_fallback() {
        let (stage, store, registry, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(array_value(&[1, 2])),
        ]);

        assert_eq!(
            stage.resolve_property_path(PropertyPath::new(prim, field)),
            None
        );
        let with_schema = stage
            .resolve_value_with_schema(prim, field, &store, &registry, None)
            .expect("fallback resolves");
        assert_eq!(
            with_schema.value,
            ResolvedValue::Scalar(array_value(&[5, 6])),
            "a strongest block yields the schema fallback unmodified (AOUSD Core §12.3.6)"
        );
    }

    /// The schema-aware time query shares the default-time fallback
    /// contract: edits over a block compose over the fallback, and a
    /// strongest block resolves the fallback itself.
    ///
    /// Spec: AOUSD Core §12.3.6 (blocked attributes), §13.3.2.4 (fallback
    /// value resolution).
    #[test]
    fn time_query_with_schema_shares_the_fallback_contract() {
        let resolve = |stage: &Stage, store, registry, prim, field| {
            [InterpolationType::Held, InterpolationType::Linear].map(|interp| {
                stage
                    .resolve_value_at_time_with_schema(
                        prim, field, 1.0, interp, store, registry, None,
                    )
                    .map(|resolved| resolved.value)
            })
        };
        let (stage, store, registry, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, &store, &registry, prim, field),
            [Some(array_value(&[5, 6, 7])), Some(array_value(&[5, 6, 7]))],
            "the edit composes over the fallback, never over the blocked [1, 2]"
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim, field),
                    1.0,
                    InterpolationType::Held
                )
                .map(|resolved| resolved.value),
            Some(array_value(&[7])),
            "without a schema the edit composes over the empty array"
        );

        let (stage, store, registry, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, &store, &registry, prim, field),
            [Some(array_value(&[5, 6])), Some(array_value(&[5, 6]))],
            "a strongest block resolves the fallback unmodified"
        );
    }

    /// A layer sublayered twice contributes its spec twice, as in OpenUSD's
    /// prim stack for the supplemental `BasicDuplicateSublayer` fixture;
    /// `prim_stack` keeps only the strongest occurrence.
    #[test]
    fn explain_prim_keeps_repeated_sites() {
        use crate::doc::{InMemoryStore, Layer, PrimSpec, SublayerEntry};

        let mut store = InMemoryStore::default();
        let prim = store.path("/B");
        let mut root = Layer::new(LayerId(1));
        root.sublayers = vec![
            SublayerEntry::new(LayerId(2)),
            SublayerEntry::new(LayerId(2)),
        ];
        store.insert_layer(root);
        let mut shared = Layer::new(LayerId(2));
        shared.insert_prim(prim, PrimSpec::def());
        store.insert_layer(shared);

        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let full: Vec<_> = stage
            .explain_prim(prim)
            .expect("composed prim")
            .iter()
            .map(|key| (key.layer_id, key.spec_path.clone()))
            .collect();
        let spec = SpecPath::from_prim_path(prim, &store.paths);
        assert_eq!(
            full,
            [(LayerId(2), spec.clone()), (LayerId(2), spec.clone())]
        );
        assert_eq!(stage.prim_stack(prim), Some(vec![(LayerId(2), spec)]));
    }
}
