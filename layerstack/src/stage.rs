// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

use alloc::{sync::Arc, vec, vec::Vec};

use hashbrown::HashMap;

use invalidation::InvalidationGraph;

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

/// Chains the list ops of `values` (strongest first) whose variant matches
/// `strongest`, or returns `None` when `strongest` is not a list op.
///
/// Spec: AOUSD Core §12.2.6 (list op resolution).
fn resolve_field_list<'a>(
    strongest: &FieldValue,
    values: impl Iterator<Item = &'a FieldValue> + Clone,
) -> Option<ResolvedValue> {
    fn chain<'a, T: Clone + Eq + 'a>(
        values: impl Iterator<Item = &'a FieldValue>,
        pick: impl Fn(&'a FieldValue) -> Option<&'a ListOp<T>>,
    ) -> Vec<T> {
        resolve_list_chain::<T>(&[], values.filter_map(pick).cloned())
    }
    fn wrap<T>(items: Vec<T>, value: impl Fn(T) -> Value) -> ResolvedValue {
        ResolvedValue::ValueList(items.into_iter().map(value).collect())
    }
    Some(match strongest {
        FieldValue::Value(_) => return None,
        FieldValue::TokenListOp(_) => ResolvedValue::TokenList(chain(values, |v| match v {
            FieldValue::TokenListOp(list) => Some(list),
            _ => None,
        })),
        FieldValue::PathListOp(_) => ResolvedValue::PathList(chain(values, |v| match v {
            FieldValue::PathListOp(list) => Some(list),
            _ => None,
        })),
        FieldValue::StringListOp(_) => wrap(
            chain(values, |v| match v {
                FieldValue::StringListOp(list) => Some(list),
                _ => None,
            }),
            Value::String,
        ),
        FieldValue::IntListOp(_) => wrap(
            chain(values, |v| match v {
                FieldValue::IntListOp(list) => Some(list),
                _ => None,
            }),
            Value::Int,
        ),
        FieldValue::UIntListOp(_) => wrap(
            chain(values, |v| match v {
                FieldValue::UIntListOp(list) => Some(list),
                _ => None,
            }),
            Value::UInt,
        ),
        FieldValue::Int64ListOp(_) => wrap(
            chain(values, |v| match v {
                FieldValue::Int64ListOp(list) => Some(list),
                _ => None,
            }),
            Value::Int64,
        ),
        FieldValue::UInt64ListOp(_) => wrap(
            chain(values, |v| match v {
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
        }
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
    /// disappears, or its children differ in membership or order.
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
        let mut sites = hashbrown::HashSet::new();
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

    /// Returns prims affected by opinions from the given layer.
    #[must_use]
    pub fn prims_affected_by_layer(&self, layer: LayerId) -> Vec<PathId> {
        self.deps
            .as_ref()
            .and_then(|d| d.layer_to_prims.get(&layer))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns layers that contribute opinions to the given prim.
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
    /// connections) are skipped.
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
        self.resolve_value_at_time_by(prim, field, time, interp, Lookup::Metadata)
    }

    fn resolve_value_at_time_by(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        lookup: Lookup,
    ) -> Option<Resolved<Value>> {
        let (index, opinions) = self.opinions(prim, field, lookup)?;

        match resolve_sparse_value(
            opinions,
            SparseQuery::AtTime {
                time,
                interp,
                fallback: None,
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

        for opinion in opinions {
            // Apply the opinion's accumulated layer offset to remap the query
            // time before sampling.
            let mapped_time = opinion.layer_offset.map_time(time);
            let value = if let Some(samples) = opinion.value.time_samples() {
                interpolate_samples(samples, mapped_time, interp)?
            } else if let Some(spline) = opinion.value.spline() {
                // A spline that evaluates to nothing (block extrapolation or
                // a blocked segment) yields no value.
                spline_to_value(spline, spline.evaluate(mapped_time)?)
            } else if let Some(value) = opinion.value.default_value() {
                value.clone()
            } else {
                continue;
            };
            // A block in effect at the query time, as a sample or a default,
            // resolves to no value.
            if value == Value::Blocked {
                return None;
            }
            return Some(Resolved {
                value,
                provenance: self.provenance_for(field, opinion),
            });
        }

        None
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

        let type_name = self.resolve_type_name(prim, store);
        let applied = api_schemas_token
            .and_then(|tok| self.resolve_token_list(prim, tok))
            .map(|r| r.value)
            .unwrap_or_default();
        let fallback = registry.resolve_fallback(type_name, &applied, field);

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
        SplineDataType::Half => Value::Half(half_from_f64(val)),
    }
}

/// Convert an `f64` to IEEE 754 half-precision bits (no_std-compatible).
///
/// This is a simplified conversion that handles normal, denormal, infinity,
/// and NaN cases.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "intentional bit manipulation for f16 conversion"
)]
fn half_from_f64(v: f64) -> u16 {
    // Convert through f32 first for simplicity.
    let f = v as f32;
    let bits = f.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    let frac = bits & 0x007F_FFFF;

    if exp <= 0 {
        // Denormal or zero.
        if exp < -10 {
            sign as u16
        } else {
            let f_shifted = (frac | 0x0080_0000) >> (1 - exp);
            (sign | (f_shifted >> 13)) as u16
        }
    } else if exp >= 31 {
        // Infinity or NaN.
        if frac == 0 {
            (sign | 0x7C00) as u16
        } else {
            (sign | 0x7C00 | (frac >> 13)) as u16
        }
    } else {
        (sign | ((exp as u32) << 10) | (frac >> 13)) as u16
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
    let authored = opinions
        .iter()
        .filter_map(|opinion| opinion.value.default_value())
        .take_while(|value| !matches!(value, Value::Blocked))
        .filter_map(|value| match value {
            Value::Dictionary(entries) => Some(entries.as_slice()),
            _ => None,
        });
    combine_dictionary_chain(authored.chain(fallback))
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
            is_local: true,
            arc_kind: crate::prim_index::ArcKind::Local,
            nested_arc_kind: None,
            namespace_depth: 1,
            authored: true,
            arc_list_index: 0,
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
        registry.register(
            crate::schema::SchemaDefinition::typed(mesh).with_property(field, array_value(&[5, 6])),
        );

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_source(key.clone());
        index.add_property_type(field, key.clone(), int_array_type());
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
