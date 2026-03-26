//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

use alloc::{sync::Arc, vec, vec::Vec};

use hashbrown::HashMap;

use invalidation::InvalidationGraph;

use crate::{
    dependency_map::{ArcDependency, CompositionDeps},
    doc::{
        FieldValue, InterpolationType, LayerId, LayerStore, Specifier, Value,
        combine_dictionary_chain,
    },
    interner::TokenId,
    listop::{ListOp, resolve_list_chain},
    path::{PathId, PropertyPath},
    prim_index::{Opinion, PrimIndex},
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
    /// A path list value resolved by chaining `ListOps`.
    PathList(Vec<PathId>),
    /// A dictionary value resolved by combining opinions.
    ///
    /// Spec: AOUSD Core §6.6.2.1 (dictionary combining), §12.2.5.
    Dictionary(Vec<(Arc<str>, Value)>),
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
#[derive(Debug)]
pub struct Stage {
    prims: HashMap<PathId, PrimIndex>,
    children: HashMap<PathId, Vec<PathId>>,
    with_provenance: bool,
    deps: Option<CompositionDeps>,
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
        }
    }

    /// Merges prims and children from a partial (scoped) composition into this stage.
    ///
    /// Entries in `partial` overwrite entries in `self` for the same key.
    /// Dependency data is not merged — the caller is responsible for
    /// incremental updates.
    pub(crate) fn merge_from(&mut self, partial: Self) {
        for (path, index) in partial.prims {
            self.prims.insert(path, index);
        }
        for (parent, kids) in partial.children {
            self.children.insert(parent, kids);
        }
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

    /// Resolves a field on a prim.
    ///
    /// Returns scalar and dictionary values. For `ListOp` fields, use
    /// [`Stage::resolve_token_list`].
    #[must_use]
    pub fn resolve_field(&self, prim: PathId, field: TokenId) -> Option<Resolved<Value>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::Scalar(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Dictionary(d) => Some(Resolved {
                value: Value::Dictionary(d),
                provenance: resolved.provenance,
            }),
            ResolvedValue::TokenList(_) | ResolvedValue::PathList(_) => None,
        }
    }

    /// Resolves a token `ListOp` field on a prim.
    #[must_use]
    pub fn resolve_token_list(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Vec<TokenId>>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::TokenList(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Scalar(_)
            | ResolvedValue::PathList(_)
            | ResolvedValue::Dictionary(_) => None,
        }
    }

    /// Resolves a path `ListOp` field on a prim.
    #[must_use]
    pub fn resolve_path_list(&self, prim: PathId, field: TokenId) -> Option<Resolved<Vec<PathId>>> {
        let resolved = self.resolve_value(prim, field)?;
        match resolved.value {
            ResolvedValue::PathList(v) => Some(Resolved {
                value: v,
                provenance: resolved.provenance,
            }),
            ResolvedValue::Scalar(_)
            | ResolvedValue::TokenList(_)
            | ResolvedValue::Dictionary(_) => None,
        }
    }

    /// Resolves a field on a prim.
    ///
    /// - Scalar fields return the strongest scalar opinion.
    /// - Token list fields chain `ListOps` across all contributing opinions.
    ///
    /// Spec: AOUSD Core §12 (value resolution).
    #[must_use]
    pub fn resolve_value(&self, prim: PathId, field: TokenId) -> Option<Resolved<ResolvedValue>> {
        let index = self.prims.get(&prim)?;
        let opinions = index.opinions_by_field.get(&field)?;
        let strongest = opinions.first()?;

        if matches!(strongest.value, FieldValue::Value(Value::Blocked)) {
            // Value block: suppress all weaker opinions, return no value.
            // Spec: AOUSD Core §12.3 (value blocking).
            return None;
        }

        match resolve_sparse_value(
            opinions,
            SparseQuery::Default { fallback: None },
            index.property_type_for(&field),
        ) {
            SparseResolveResult::Resolved(value) => {
                return Some(Resolved {
                    value: ResolvedValue::Scalar(value),
                    provenance: self.provenance_for(field, strongest),
                });
            }
            SparseResolveResult::Blocked => return None,
            SparseResolveResult::NotApplicable => {}
        }

        match &strongest.value {
            FieldValue::Value(Value::Dictionary(_)) => {
                // Dictionary combining: merge all dictionary opinions.
                // Spec: AOUSD Core §6.6.2.1, §12.2.5.
                let dicts = opinions.iter().filter_map(|op| match &op.value {
                    FieldValue::Value(Value::Dictionary(d)) => Some(d.clone()),
                    _ => None,
                });
                let combined = combine_dictionary_chain(dicts);
                Some(Resolved {
                    value: ResolvedValue::Dictionary(combined),
                    provenance: self.provenance_for(field, strongest),
                })
            }
            FieldValue::Value(v) => Some(Resolved {
                value: ResolvedValue::Scalar(v.clone()),
                provenance: self.provenance_for(field, strongest),
            }),
            FieldValue::TokenListOp(_) => {
                let ops: Vec<ListOp<TokenId>> = opinions
                    .iter()
                    .filter_map(|op| match &op.value {
                        FieldValue::TokenListOp(list) => Some(list.clone()),
                        _ => None,
                    })
                    .collect();
                Some(Resolved {
                    value: ResolvedValue::TokenList(resolve_list_chain::<TokenId>(&[], ops)),
                    provenance: self.provenance_for(field, strongest),
                })
            }
            FieldValue::PathListOp(_) => {
                let ops: Vec<ListOp<PathId>> = opinions
                    .iter()
                    .filter_map(|op| match &op.value {
                        FieldValue::PathListOp(list) => Some(list.clone()),
                        _ => None,
                    })
                    .collect();
                Some(Resolved {
                    value: ResolvedValue::PathList(resolve_list_chain::<PathId>(&[], ops)),
                    provenance: self.provenance_for(field, strongest),
                })
            }
            FieldValue::TimeSamples(_) | FieldValue::Spline(_) => {
                // When resolved without a time, timeSamples/splines return no
                // scalar value. Use resolve_value_at_time() for time-varying queries.
                None
            }
        }
    }

    /// Resolves a time-varying field on a prim at a specific time.
    ///
    /// `TimeSamples` take priority over default values per §12.3. The strongest
    /// opinion with timeSamples is used. If no timeSamples exist, falls back to
    /// `resolve_value`.
    ///
    /// Spec: AOUSD Core §12.3.2.2 (timeSamples), §12.5 (interpolation).
    #[must_use]
    pub fn resolve_value_at_time(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
    ) -> Option<Resolved<Value>> {
        let index = self.prims.get(&prim)?;
        let opinions = index.opinions_by_field.get(&field)?;

        match resolve_sparse_value(
            opinions,
            SparseQuery::AtTime { time, interp },
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

        // Per §12.3: check each spec in strength order for timeSamples first,
        // then fall back to default value.
        for opinion in opinions {
            match &opinion.value {
                FieldValue::Value(Value::Blocked) => return None,
                FieldValue::TimeSamples(samples) => {
                    // Apply the opinion's accumulated layer offset to remap
                    // the query time before interpolating.
                    //
                    // Spec: §12.3.2.1 (layer offset/scale remap time).
                    let mapped_time = opinion.layer_offset.map_time(time);
                    let value = interpolate_samples(samples, mapped_time, interp);
                    return value.map(|v| Resolved {
                        value: v,
                        provenance: self.provenance_for(field, opinion),
                    });
                }
                _ => {}
            }
        }

        // No timeSamples found: check for spline opinions (§12.3.3).
        // Splines sit between timeSamples and default in resolution priority.
        for opinion in opinions {
            if let FieldValue::Spline(spline) = &opinion.value {
                let mapped_time = opinion.layer_offset.map_time(time);
                if let Some(val) = spline.evaluate(mapped_time) {
                    let value = spline_to_value(spline, val);
                    return Some(Resolved {
                        value,
                        provenance: self.provenance_for(field, opinion),
                    });
                }
                // Spline returned None (Block extrapolation) — no value.
                return None;
            }
        }

        // No timeSamples or splines found: fall back to scalar default.
        for opinion in opinions {
            if let FieldValue::Value(v) = &opinion.value
                && *v != Value::Blocked
            {
                return Some(Resolved {
                    value: v.clone(),
                    provenance: self.provenance_for(field, opinion),
                });
            }
        }

        None
    }

    /// Returns the sorted opinion stack for `(prim, field)` (strongest-first).
    ///
    /// This is intended for inspection/debugging and mirrors the "stack of
    /// opinions" described by the spec.
    ///
    /// Spec: AOUSD Core §12 (value resolution) and §10.4 (strength ordering).
    #[must_use]
    pub fn explain_field(&self, prim: PathId, field: TokenId) -> Option<&[Opinion]> {
        let index = self.prims.get(&prim)?;
        let opinions = index.opinions_by_field.get(&field)?;
        Some(opinions.as_slice())
    }

    /// Resolves a concrete property path.
    #[must_use]
    pub fn resolve_property_path(
        &self,
        property_path: PropertyPath,
    ) -> Option<Resolved<ResolvedValue>> {
        self.resolve_value(property_path.prim_path(), property_path.property())
    }

    /// Resolves a concrete property path at a specific time.
    #[must_use]
    pub fn resolve_property_path_at_time(
        &self,
        property_path: PropertyPath,
        time: f64,
        interp: InterpolationType,
    ) -> Option<Resolved<Value>> {
        self.resolve_value_at_time(
            property_path.prim_path(),
            property_path.property(),
            time,
            interp,
        )
    }

    /// Returns the sorted opinion stack for a concrete property path.
    #[must_use]
    pub fn explain_property_path(&self, property_path: PropertyPath) -> Option<&[Opinion]> {
        self.explain_field(property_path.prim_path(), property_path.property())
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

    /// Returns the composed prim stack as `(layer_id, spec_path)` pairs (strongest-first).
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
            let Some(spec) = layer.prims.get(&key.lookup_path) else {
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
            let Some(spec) = layer.prims.get(&key.lookup_path) else {
                continue;
            };
            if let Some(tn) = spec.type_name {
                return Some(tn);
            }
        }
        None
    }

    /// Resolves a field on a prim with schema fallback.
    ///
    /// Like [`Stage::resolve_value`], but when no authored opinion exists,
    /// consults the schema registry for a fallback value based on the prim's
    /// resolved type name and applied API schemas.
    ///
    /// `api_schemas_token` is the interned token for `"apiSchemas"`. Pass it
    /// so the resolver can look up applied API schemas on the prim. If `None`,
    /// only the typed schema (and its built-ins / auto-applies) are consulted.
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
        let authored = self
            .prims
            .get(&prim)
            .and_then(|index| index.opinions_by_field.get(&field));

        let type_name = self.resolve_type_name(prim, store);
        let applied = api_schemas_token
            .and_then(|tok| self.resolve_token_list(prim, tok))
            .map(|r| r.value)
            .unwrap_or_default();
        let fallback = registry.resolve_fallback(type_name, &applied, field);

        if let Some(opinions) = authored {
            let strongest = opinions.first()?;
            if matches!(strongest.value, FieldValue::Value(Value::Blocked)) {
                // fall through to schema fallback
            } else {
                let property_type = self.prims.get(&prim)?.property_type_for(&field);
                let fallback_value = match fallback.as_ref() {
                    Some(FieldValue::Value(value)) => Some(value),
                    _ => None,
                };
                match resolve_sparse_value(
                    opinions,
                    SparseQuery::Default {
                        fallback: fallback_value,
                    },
                    property_type,
                ) {
                    SparseResolveResult::Resolved(value) => {
                        return Some(Resolved {
                            value: ResolvedValue::Scalar(value),
                            provenance: self.provenance_for(field, strongest),
                        });
                    }
                    SparseResolveResult::Blocked => {}
                    SparseResolveResult::NotApplicable => {}
                }
                if let Some(resolved) = self.resolve_value(prim, field) {
                    return Some(resolved);
                }
            }
        }

        // No authored opinion — consult the schema registry.
        let fallback = fallback?;

        Some(Resolved {
            value: match fallback {
                FieldValue::Value(Value::Dictionary(d)) => ResolvedValue::Dictionary(d),
                FieldValue::Value(v) => ResolvedValue::Scalar(v),
                FieldValue::TokenListOp(op) => {
                    ResolvedValue::TokenList(resolve_list_chain::<TokenId>(&[], [op]))
                }
                FieldValue::PathListOp(op) => {
                    ResolvedValue::PathList(resolve_list_chain::<PathId>(&[], [op]))
                }
                FieldValue::TimeSamples(_) | FieldValue::Spline(_) => return None,
            },
            provenance: None,
        })
    }

    /// Resolves a scalar field on a prim with schema fallback.
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
            ResolvedValue::TokenList(_) | ResolvedValue::PathList(_) => None,
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

/// An iterator for deterministic stage traversal.
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
            })),
            layer_offset: LayerOffset::IDENTITY,
        });
        index.add_opinion(Opinion {
            key: OpinionKey {
                layer_strength: 1,
                ..key.clone()
            },
            field,
            value: FieldValue::Value(array_value(&[1, 2])),
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
            value: FieldValue::TimeSamples(vec![
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
            value: FieldValue::Value(array_value(&[1, 2])),
            layer_offset: LayerOffset::IDENTITY,
        });

        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        assert_eq!(
            stage
                .resolve_value_at_time(prim, field, 2.5, InterpolationType::Held)
                .expect("resolved override")
                .value,
            array_value(&[9, 2])
        );
        assert_eq!(
            stage
                .resolve_value_at_time(prim, field, 3.5, InterpolationType::Held)
                .expect("resolved reset")
                .value,
            array_value(&[1, 2])
        );
    }
}
