// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

mod explain;
pub(crate) mod loading;
pub use loading::{LayerMuteError, LoadPolicy, PayloadLoadRules, PayloadRule};
mod prototypes;
pub use prototypes::{CompositionStorage, CompositionWork, Prototype, PrototypeId, PrototypePrim};
pub mod flatten;
pub(crate) mod stage_time;

pub use explain::{
    Contribution, DictionaryMerge, ExplainedOpinion, IgnoreCause, KeyPath, OpinionRole, SampleUse,
    ValueExplanation, ValueSource,
};
pub use flatten::{
    FlattenError, FlattenReport, FlattenRequirements, FlattenVerification, Flattened,
};

use alloc::{
    borrow::Cow,
    sync::{Arc, Weak},
    vec::Vec,
};

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
    prim_index::{ArcKind, Opinion, OpinionKey, OpinionValue, PrimIndex},
    prim_index_graph::{NodeId, PrimIndexGraph},
    property::{PropertyKind, PropertySpec, PropertyType, Time, Variability},
    schema::{CannotApply, PrimDefinition, PropertyDefinition, SchemaRegistry},
    spec_path::SpecPath,
    spline::{SplineData, SplineDataType},
    value_resolution::{
        SampleComposability, SparseQuery, SparseResolveResult, interpolate_samples,
        resolve_sparse_value,
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

// Spec: AOUSD Core §13.3.2.3. Type identity belongs to a composed prim;
// equivalent prims share immutable definitions, independently of query caches.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SchemaIdentity {
    type_name: Option<TokenId>,
    applied: Vec<TokenId>,
}

#[derive(Debug)]
pub(crate) struct PrimTypeInfo {
    identity: SchemaIdentity,
    definition: Arc<PrimDefinition>,
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
        resolve_list_chain::<T>(&[], values.filter_map(pick))
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopulationMask {
    /// Include the subtrees rooted at these prim paths, and their ancestors.
    /// An empty mask includes only the pseudo-root. Selecting `/` includes
    /// the entire stage. Redundant descendant entries do not change the result.
    ///
    /// OpenUSD: `UsdStagePopulationMask::Includes` and `IncludesSubtree`.
    pub include: Vec<PathId>,
}

impl PopulationMask {
    /// Whether a path belongs to a selected subtree or is one of its ancestors.
    /// The pseudo-root is inspectable even for the empty mask.
    #[must_use]
    pub fn includes(&self, path: PathId, paths: &crate::PathInterner) -> bool {
        let path = paths.resolve(path);
        path.depth() == 0
            || self.include.iter().any(|p| {
                let root = paths.resolve(*p);
                path.is_prefix_of(root) || root.is_prefix_of(path)
            })
    }

    /// Whether the complete subtree at this path is selected.
    #[must_use]
    pub fn includes_subtree(&self, path: PathId, paths: &crate::PathInterner) -> bool {
        self.include
            .iter()
            .any(|p| paths.resolve(*p).is_prefix_of(paths.resolve(path)))
    }
}

/// Options for stage composition and population.
#[derive(Clone, Debug, Default)]
pub struct StageOptions {
    /// Optional host-owned session root, stronger than the persistent root and
    /// its sublayers. Shared live-edit layers may be sublayers of this root;
    /// private overrides belong on the session root itself. Each stage chooses
    /// its session independently. This supplies composition, not networking,
    /// persistence, conflict resolution or globally stable layer identities.
    /// OpenUSD: `UsdStage::Open(rootLayer, sessionLayer)`.
    pub session_layer: Option<LayerId>,
    /// Optional population mask.
    pub mask: Option<PopulationMask>,
    /// Stage-local payload inclusion rules. Default construction loads all.
    /// The host still owns asset loading; composition never performs I/O.
    pub load_rules: PayloadLoadRules,
    /// Loaded layer identities whose content and sublayers are excluded.
    /// The store retains them; other stages remain unaffected. The root layer
    /// cannot be muted and a root entry here is ignored. Hosts resolve external
    /// identifiers to stable `LayerId`s before changing these controls.
    pub muted_layers: alloc::collections::BTreeSet<LayerId>,
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
    /// The schemas that give the stage's prims their definitions: their
    /// types, applied schemas and schema-defined properties, whose fallbacks
    /// the schema-aware queries resolve ([`Stage::resolve_value_with_schema`]).
    /// `None`, the default, reads no schema: every prim is typeless and
    /// defines no property.
    ///
    /// The registry's tokens must be those of the store the stage is
    /// composed from. Composition itself never reads schemas.
    ///
    /// Spec: AOUSD Core §13 (schemas), §13.3.2.3 (the prim definition).
    pub schemas: Option<Arc<SchemaRegistry>>,
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
    options: StageOptions,
    loadable: HashSet<PathId>,
    local_layers: Vec<LayerId>,
    used_layers: alloc::collections::BTreeSet<LayerId>,
    used_layer_sites: HashMap<PathId, HashSet<LayerId>>,
    inactive: HashSet<PathId>,
    inactive_children: HashMap<PathId, Vec<PathId>>,
    root_layer: Option<LayerId>,
    clips: crate::value_clips::Catalog,
    prototypes: prototypes::PrototypeTable,
    composition_work: CompositionWork,
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
    /// The schemas the stage was composed with ([`StageOptions::schemas`]).
    schemas: Option<Arc<SchemaRegistry>>,
    type_infos: HashMap<SchemaIdentity, Weak<PrimTypeInfo>>,
}

// Independent sets may share a metadata authoring site and compose sparsely.
#[derive(Clone, Debug)]
struct SelectedClip {
    entry: usize,
    key: OpinionKey,
    query: f64,
}
#[derive(Clone, Debug)]
struct ClipOpinion {
    entry: usize,
    opinion: Opinion,
    query: f64,
    contributes: bool,
}

// Keep exact clip sample types separate from the held/interpolated payloads.
// Pointer equality identifies our shared synthetic property snapshots, even
// when independent clip sets have the same source key.
#[derive(Default)]
struct ClipSampleKinds {
    entries: Vec<(Arc<PropertySpec>, usize)>,
}
impl ClipSampleKinds {
    fn insert(&mut self, opinion: &Opinion, entry: usize) {
        if let OpinionValue::Property(spec) = &opinion.value {
            self.entries.push((Arc::clone(spec), entry));
        }
    }
    fn entry(&self, opinion: &Opinion) -> Option<usize> {
        let OpinionValue::Property(spec) = &opinion.value else {
            return None;
        };
        self.entries
            .iter()
            .find_map(|(known, entry)| Arc::ptr_eq(known, spec).then_some(*entry))
    }
}
struct ClipPlan<'s> {
    opinions: Cow<'s, [Opinion]>,
    source: Option<crate::value_clips::ClipValueSource>,
    selected: Option<SelectedClip>,
    kinds: ClipSampleKinds,
}

impl Stage {
    /// Composes a stage from a root layer.
    ///
    /// Captures each prim's type identity with its composed opinions. With
    /// schemas ([`StageOptions::schemas`]), this also interns the
    /// names of the multiple-apply schema instances each composed prim's
    /// `apiSchemas` applies ([`SchemaRegistry::intern_instance_names`]), so
    /// the schema queries read the store without mutating it. A masked
    /// composition does this for the prims it composes only.
    pub fn compose(store: &mut dyn LayerStore, root: LayerId, options: StageOptions) -> Self {
        Self::compose_selected(store, root, options, false)
    }

    pub(crate) fn compose_selected(
        store: &mut dyn LayerStore,
        root: LayerId,
        mut options: StageOptions,
        exact_mask: bool,
    ) -> Self {
        options.muted_layers.remove(&root);
        let captured = options.clone();
        let schemas = options.schemas.clone();
        let mut controlled = loading::ControlledStore {
            inner: store,
            root,
            muted: &captured.muted_layers,
        };
        let store: &mut dyn LayerStore = &mut controlled;
        let mut stage =
            crate::compose::compose_stage_selected(store, root, options, None, exact_mask);
        stage.options = captured.clone();
        stage.root_layer = Some(root);
        stage.schemas = schemas;
        let mut offsets = HashMap::new();
        for index in stage.prims.values_mut() {
            index.graph.prepare_layer_offsets(store, &mut offsets);
        }
        stage.prepare_type_info(store);
        stage.prepare_clips(store);
        stage.prepare_prototypes(store);
        stage
    }

    pub(crate) fn compose_local_paths(
        store: &mut dyn LayerStore,
        root: LayerId,
        options: StageOptions,
        paths: alloc::collections::BTreeSet<PathId>,
    ) -> Self {
        let captured = options.clone();
        let schemas = options.schemas.clone();
        let mut stage = crate::compose::compose_stage_with_paths(store, root, options, Some(paths));
        stage.options = captured;
        stage.root_layer = Some(root);
        stage.schemas = schemas;
        let mut offsets = HashMap::new();
        for index in stage.prims.values_mut() {
            index.graph.prepare_layer_offsets(store, &mut offsets);
        }
        stage.prepare_type_info(store);
        stage.prepare_clips(store);
        stage.prepare_prototypes(store);
        stage
    }

    /// Replaces only complete local subtrees and their boundary child lists.
    /// Supporting ancestors are not merged. Boundary children reuse the
    /// retained indexes and the shared child-order fold.
    pub(crate) fn merge_local_subtrees(
        &mut self,
        store: &dyn LayerStore,
        mut partial: Self,
        affected: &[PathId],
        hierarchy: &[PathId],
        boundary_children: Vec<(PathId, Vec<PathId>)>,
    ) {
        for path in affected {
            if let Some(old) = self.prims.remove(path) {
                self.forget_type_info(old.type_info);
            }
        }
        for path in hierarchy {
            match partial.inactive_children.remove(path) {
                Some(children) => {
                    self.inactive_children.insert(*path, children);
                }
                None => {
                    self.inactive_children.remove(path);
                }
            }
            match partial.children.remove(path) {
                Some(children) => {
                    self.children.insert(*path, children);
                }
                None => {
                    self.children.remove(path);
                }
            }
        }
        self.merge_prims_from(store, partial, affected);
        for (parent, mut children) in boundary_children {
            let Some(index) = self.prims.get(&parent) else {
                self.children.remove(&parent);
                continue;
            };
            if children.is_empty() {
                self.children.remove(&parent);
                continue;
            }
            crate::compose::order_local_children(store, index, &mut children);
            // Ordinary composition orders before pruning inactive children:
            // invisible names can delimit reorder groups of visible names.
            children.retain(|child| self.prims.contains_key(child));
            if children.iter().any(|child| self.inactive.contains(child)) {
                self.inactive_children.insert(parent, children.clone());
                children.retain(|child| !self.inactive.contains(child));
            } else {
                self.inactive_children.remove(&parent);
            }
            if children.is_empty() {
                self.children.remove(&parent);
            } else {
                self.children.insert(parent, children);
            }
        }
        self.prepare_prototypes(store);
    }

    /// Resolve schema identity once with the composed snapshot. Ordinary value
    /// refreshes retain it; partial composition replaces affected identities.
    fn prepare_type_info(&mut self, store: &mut dyn LayerStore) {
        let empty = Arc::new(PrimDefinition::default());
        // Resolve only authored API lists through the normal metadata fold.
        // The common plain typed case needs no temporary entry or path list.
        let mut applied: HashMap<_, _> =
            match (self.schemas.as_ref(), store.tokens().lookup("apiSchemas")) {
                (Some(_), Some(field)) => self
                    .prims
                    .iter()
                    .filter(|(_, index)| index.metadata_opinions(field).is_some())
                    .map(|(&path, _)| (path, self.applied_schema_names(path, store)))
                    .collect(),
                _ => HashMap::new(),
            };
        for (&path, index) in &mut self.prims {
            let identity = SchemaIdentity {
                type_name: Self::source_type_name(index, store),
                applied: applied.remove(&path).unwrap_or_default(),
            };
            let info = self
                .type_infos
                .get(&identity)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let definition = match self.schemas.as_deref() {
                        None => empty.clone(),
                        Some(schemas) if identity.applied.is_empty() => schemas
                            .shared_typed(identity.type_name)
                            .unwrap_or_else(|| empty.clone()),
                        Some(schemas) => {
                            schemas.intern_instance_names(&identity.applied, store.tokens_mut());
                            Arc::new(schemas.prim_definition(
                                identity.type_name,
                                &identity.applied,
                                store.tokens(),
                            ))
                        }
                    };
                    let info = Arc::new(PrimTypeInfo {
                        identity: identity.clone(),
                        definition,
                    });
                    self.type_infos.insert(identity, Arc::downgrade(&info));
                    info
                });
            index.type_info = Some(info);
        }
    }

    // Retire only identities whose last prim was removed. A sweep of every
    // distinct schema combination would make a local edit scale with the stage.
    fn forget_type_info(&mut self, info: Option<Arc<PrimTypeInfo>>) {
        if let Some(info) = info
            && Arc::strong_count(&info) == 1
        {
            self.type_infos.remove(&info.identity);
        }
    }

    /// The root layer this stage was composed from, when it has one.
    #[must_use]
    pub fn root_layer(&self) -> Option<LayerId> {
        self.root_layer
    }

    /// Reads session/root metadata, using a registered layer default when absent.
    /// Sublayer metadata does not participate. Blocks and incompatible field
    /// representations do not resolve to a registered default.
    ///
    /// Spec: AOUSD Core §12.2.7 (layer metadata resolution).
    /// OpenUSD: `UsdStage::GetMetadata`, `SdfSchema::GetFallback`.
    #[must_use]
    pub fn layer_metadata(&self, key: TokenId, store: &dyn LayerStore) -> Option<Value> {
        let root = store.layer(self.root_layer?)?;
        let session = self
            .session_layer()
            .filter(|id| !self.is_layer_muted(*id))
            .and_then(|id| store.layer(id));
        if store.tokens().resolve(key) == "defaultPrim" {
            return session
                .and_then(|l| l.default_prim)
                .or(root.default_prim)
                .map(Value::Token);
        }
        if store.tokens().resolve(key) == "timeCodesPerSecond" {
            return Some(Value::Double(self.time_codes_per_second(store)));
        }
        let fields = session
            .and_then(|l| l.metadata(key))
            .into_iter()
            .chain(root.metadata(key));
        let mut dictionaries = Vec::new();
        for field in fields {
            match field {
                FieldValue::Value(Value::Dictionary(entries)) => {
                    dictionaries.push(entries.as_slice());
                }
                FieldValue::Value(Value::Blocked) if dictionaries.is_empty() => return None,
                FieldValue::Value(value) if dictionaries.is_empty() => return Some(value.clone()),
                _ if dictionaries.is_empty() => return None,
                _ => break,
            }
        }
        let fallback = self
            .schemas()
            .and_then(|s| s.metadata(key))
            .filter(|d| d.applies_to(crate::MetadataTarget::Layer))
            .and_then(|d| d.default.as_ref());
        if dictionaries.is_empty() {
            return fallback.cloned();
        }
        if let Some(Value::Dictionary(entries)) = fallback {
            dictionaries.push(entries.as_slice());
        }
        Some(Value::Dictionary(combine_dictionary_chain(dictionaries)))
    }

    /// The schemas the stage was composed with ([`StageOptions::schemas`]).
    #[must_use]
    pub fn schemas(&self) -> Option<&SchemaRegistry> {
        self.schemas.as_deref()
    }

    fn prepare_clips(&mut self, store: &mut dyn LayerStore) {
        let mut controlled = loading::ControlledStore {
            inner: store,
            root: self.root_layer.unwrap_or(LayerId(0)),
            muted: &self.options.muted_layers,
        };
        self.clips =
            crate::value_clips::Catalog::prepare(&mut controlled, &self.prims, &self.instances);
        if let Some(deps) = &mut self.deps {
            for prim in self.prims.keys().copied() {
                for (layer, _) in self.clips.source_sites(prim) {
                    deps.layer_to_prims.entry(layer).or_default().insert(prim);
                    deps.prim_to_layers.entry(prim).or_default().insert(layer);
                }
            }
        }
    }

    /// Assets consulted while preparing value clips, including unresolved assets.
    ///
    /// The host resolves and inserts these assets, registers their bindings with
    /// [`LayerStore`], and composes again. Numeric queries use owned snapshots
    /// and never perform I/O. Template holes require confirmed missing results.
    #[must_use]
    pub fn clip_asset_requests(&self) -> &[crate::value_clips::ClipAssetRequest] {
        self.clips.asset_requests()
    }

    /// Invalid or unsupported clip definitions found during preparation.
    #[must_use]
    pub fn clip_issues(&self) -> &[crate::value_clips::ClipIssue] {
        self.clips.issues()
    }

    pub(crate) fn clip_layers(&self) -> Vec<LayerId> {
        self.clips.layers()
    }

    /// The selected clip source and its mapped bracketing samples.
    ///
    /// Describes the selected numeric source chain. Returns `None` when ordinary
    /// source selection ends before clips or no manifest declares eligibility.
    /// Under held interpolation, a stronger dense lower sample may supply the
    /// entire result even when the selected chain reaches a clip at its upper
    /// bracket. Explanations and provenance identify actual contributions.
    /// Eligible gaps retain their clip source.
    #[must_use]
    pub fn property_clip_source(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
    ) -> Option<crate::value_clips::ClipValueSource> {
        let index = self.prims.get(&prim)?;
        self.clip_time_opinions(prim, field, index, time, interp)
            .source
    }

    /// A selected clip evaluator's query error, without exposing weaker values.
    ///
    /// Preparation errors are in [`Self::clip_issues`]. This separate diagnostic
    /// covers invalid query times and errors encountered while evaluating a
    /// selected source. It performs no loading or mutation.
    #[must_use]
    pub fn property_clip_evaluation_error(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
    ) -> Option<crate::value_clips::ClipEvalError> {
        let index = self.prims.get(&prim)?;
        let selected = self
            .clip_time_opinions(prim, field, index, time, interp)
            .selected?;
        self.clips
            .evaluation_error_for(prim, field, selected.entry, selected.query, interp)
    }

    // OpenUSD stage.cpp::_ResolveInfoResolver::ProcessLayerAtTime: a site
    // supplies one numeric value opinion, even when that opinion is sparse.
    fn clip_opinions_after_authored(
        &self,
        prim: PathId,
        field: TokenId,
        ordinary: &[Opinion],
        time: f64,
        interp: InterpolationType,
    ) -> Vec<ClipOpinion> {
        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        let initial = self.clips.opinions(prim, field, time, interp, &[]);
        if initial.is_empty() {
            return Vec::new();
        }
        let mut tagged: Vec<_> = ordinary.iter().cloned().map(|op| (op, None)).collect();
        tagged.extend(initial.into_iter().enumerate().filter_map(|(entry, clip)| {
            // OpenUSD stage.cpp::_GetResolveInfoWithClipsImpl: an authored
            // source and a clip source cannot both occupy the same site.
            let masked = ordinary.iter().any(|op| {
                (op.value.blocks_animation()
                    && index
                        .graph
                        .cmp_nodes(op.key.node, clip.key.node)
                        .then_with(|| op.key.layer_strength.cmp(&clip.key.layer_strength))
                        .is_le())
                    || op.key.node == clip.key.node
                        && op.key.layer_strength == clip.key.layer_strength
                        && (op.value.time_samples().is_some()
                            || op.value.spline().is_some()
                            || op.value.default_value().is_some())
            });
            (!masked).then_some((clip, Some(entry)))
        }));
        tagged.sort_by(|(a, a_entry), (b, b_entry)| {
            index
                .graph
                .cmp_nodes(a.key.node, b.key.node)
                .then_with(|| a.key.layer_strength.cmp(&b.key.layer_strength))
                .then_with(|| a_entry.cmp(b_entry))
        });
        let mut prefix = Vec::new();
        let mut prefix_kinds = ClipSampleKinds::default();
        let mut clips = Vec::new();
        for (opinion, entry) in tagged {
            let Some(entry) = entry else {
                prefix.push(opinion);
                continue;
            };
            // The existing kernel owns query advancement. Earlier finalized
            // clips belong to this prefix too, including mixed dense/sparse sets.
            let classify = |opinion: &Opinion, sample_time| {
                prefix_kinds.entry(opinion).and_then(|entry| {
                    self.clips
                        .sample_composes_for(prim, field, entry, sample_time)
                })
            };
            let planned = crate::value_resolution::query_time_for_weaker_source_with_composability(
                &prefix,
                time,
                interp,
                Some(&classify),
            );
            let sparse_at_query = prefix.iter().any(|op| {
                value_at_time(op, time, interp)
                    .flatten()
                    .is_some_and(|value| value.array_edit_ref().is_some())
            });
            let query = planned.unwrap_or(time);
            let queries = [query];
            let projection = if planned.is_some() && (sparse_at_query || query != time) {
                &queries[..]
            } else {
                &[]
            };
            let Some(opinion) = self
                .clips
                .opinion_for(prim, field, entry, time, interp, projection)
            else {
                continue;
            };
            prefix_kinds.insert(&opinion, entry);
            prefix.push(opinion.clone());
            clips.push(ClipOpinion {
                entry,
                opinion,
                query,
                contributes: planned.is_some(),
            });
        }
        clips
    }

    // AOUSD Core §12.3.2, §12.3.7; OpenUSD stage.cpp
    // _ProcessClipsForLayer: an anchor layer's authored values precede its clips.
    fn clip_time_opinions<'s>(
        &'s self,
        prim: PathId,
        field: TokenId,
        index: &'s PrimIndex,
        time: f64,
        interp: InterpolationType,
    ) -> ClipPlan<'s> {
        let ordinary = index.property_opinions(field).unwrap_or(&[]);
        if ordinary.is_empty() && self.property_definition_ref(prim, field).is_none() {
            return ClipPlan {
                opinions: Cow::Borrowed(ordinary),
                source: None,
                selected: None,
                kinds: ClipSampleKinds::default(),
            };
        }
        let clips = self.clip_opinions_after_authored(prim, field, ordinary, time, interp);
        if clips.is_empty() {
            return ClipPlan {
                opinions: Cow::Borrowed(ordinary),
                source: None,
                selected: None,
                kinds: ClipSampleKinds::default(),
            };
        }
        // Source participation follows the same temporal prefix plan as the
        // value. A dense ordinary lower sample can expose a clip beneath its
        // sparse upper sample, so inspecting only the original query is wrong.
        let selected = clips
            .iter()
            .find(|clip| clip.contributes)
            .map(|clip| SelectedClip {
                entry: clip.entry,
                key: clip.opinion.key.clone(),
                query: clip.query,
            });
        let mut tagged: Vec<_> = ordinary.iter().cloned().map(|op| (op, None)).collect();
        tagged.extend(clips.into_iter().map(|clip| {
            let selected = SelectedClip {
                entry: clip.entry,
                key: clip.opinion.key.clone(),
                query: clip.query,
            };
            (clip.opinion, Some(selected))
        }));
        tagged.sort_by(|(a, a_clip), (b, b_clip)| {
            index
                .graph
                .cmp_nodes(a.key.node, b.key.node)
                .then_with(|| a.key.layer_strength.cmp(&b.key.layer_strength))
                .then_with(|| {
                    a_clip
                        .as_ref()
                        .map(|s| s.entry)
                        .cmp(&b_clip.as_ref().map(|s| s.entry))
                })
        });
        let source = selected.as_ref().and_then(|selected| {
            self.clips
                .evaluation_for(prim, field, selected.entry, selected.query, interp)
        });
        let mut kinds = ClipSampleKinds::default();
        for (opinion, selected) in &tagged {
            if let Some(selected) = selected {
                kinds.insert(opinion, selected.entry);
            }
        }
        ClipPlan {
            opinions: Cow::Owned(tagged.into_iter().map(|(op, _)| op).collect()),
            source,
            selected,
            kinds,
        }
    }

    fn resolve_property_at_time(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        fallback: Option<&Value>,
        accepts: Option<&dyn Fn(&Value) -> bool>,
        with_source: bool,
    ) -> (Option<Resolved<Value>>, bool) {
        let Some(index) = self.prims.get(&prim) else {
            return (None, false);
        };
        let ClipPlan {
            opinions,
            source: clip,
            selected,
            kinds,
        } = self.clip_time_opinions(prim, field, index, time, interp);
        let classify = |opinion: &Opinion, sample_time| {
            kinds.entry(opinion).and_then(|entry| {
                self.clips
                    .sample_composes_for(prim, field, entry, sample_time)
            })
        };
        let composability =
            (!kinds.entries.is_empty()).then_some(&classify as &SampleComposability<'_>);
        let mut resolved = self.resolve_at_time_with_source(
            field,
            &opinions,
            index.property_type_for(&field),
            time,
            interp,
            fallback,
            accepts,
            with_source,
            composability,
        );
        if let (Some(resolved), Some(source)) = (&mut resolved, &clip) {
            // Sparse edits retain their own strongest provenance. Dense clip
            // values identify the raw data layer, not the metadata anchor.
            if with_source
                && resolved.provenance.as_ref().is_some_and(|p| {
                    selected.as_ref().is_some_and(|key| {
                        p.layer == key.key.layer_id && p.spec_path == key.key.spec_path
                    })
                })
            {
                resolved.provenance = source.lower.layer.map(|layer| Provenance {
                    layer,
                    spec_path: source.lower.spec_path.clone(),
                    field,
                });
            }
        }
        (resolved, selected.is_some())
    }

    /// Whether the effective property source might vary across numeric times.
    ///
    /// A contributing source with multiple samples, or a spline, counts as
    /// varying even when its values agree. Dense defaults and blocks mask weaker
    /// animation. Several contributing single-sample sources remain constant
    /// across numeric times, even when their combined sample grid has many knots.
    /// Numeric values may still differ from the default-time value.
    #[must_use]
    pub fn property_might_be_time_varying(&self, prim: PathId, field: TokenId) -> bool {
        self.property_time_info(prim, field).1
    }

    /// Sorted, unique stage-time samples of the effective property sources.
    ///
    /// Dense defaults and blocks mask weaker animation. Sparse array edits retain
    /// weaker grids. Clip times include activations and mapping discontinuities;
    /// splines have no discrete sample times. Default-time reads ignore clips.
    #[must_use]
    pub fn property_sample_times(&self, prim: PathId, field: TokenId) -> Vec<f64> {
        self.property_time_info(prim, field).0
    }

    // AOUSD Core §12.3.2: variability belongs to each contributing series;
    // merging differently timed single samples cannot create animation.
    fn property_time_info(&self, prim: PathId, field: TokenId) -> (Vec<f64>, bool) {
        let Some(index) = self.prims.get(&prim) else {
            return (Vec::new(), false);
        };
        let animation =
            stage_time::animation_opinions(index.property_opinions(field).unwrap_or(&[]));
        let ordinary = animation.as_ref();
        if ordinary.is_empty() && self.property_definition_ref(prim, field).is_none() {
            return (Vec::new(), false);
        }
        let clip_opinions =
            self.clip_opinions_after_authored(prim, field, ordinary, 0.0, InterpolationType::Held);
        let mut tagged: Vec<_> = ordinary.iter().map(|op| (op, None)).collect();
        tagged.extend(
            clip_opinions
                .iter()
                .map(|clip| (&clip.opinion, Some(clip.entry))),
        );
        if !clip_opinions.is_empty() {
            tagged.sort_by(|(a, a_clip), (b, b_clip)| {
                index
                    .graph
                    .cmp_nodes(a.key.node, b.key.node)
                    .then_with(|| a.key.layer_strength.cmp(&b.key.layer_strength))
                    .then_with(|| a_clip.cmp(b_clip))
            });
        }
        let mut times = Vec::new();
        let mut varying = false;
        let mut masking: Vec<Vec<(f64, bool)>> = Vec::new();
        for (op, clip) in tagged {
            let grid: Vec<_> = if let Some(entry) = clip {
                self.clips.sample_kinds_for(prim, field, entry)
            } else if let Some(samples) = op.value.time_samples() {
                samples
                    .iter()
                    .map(|(time, value)| {
                        (
                            op.layer_offset.offset + time * op.layer_offset.scale,
                            value.array_edit_ref().is_some(),
                        )
                    })
                    .collect()
            } else {
                if op.value.spline().is_some() {
                    varying = true;
                    break;
                }
                if op
                    .value
                    .default_value()
                    .is_some_and(|v| v.array_edit_ref().is_none())
                {
                    break;
                }
                continue;
            };
            // Prepared sample clips always have at least an activation knot;
            // only a manifest-selected spline has an empty discrete grid.
            if clip.is_some() && grid.is_empty() {
                varying = true;
                break;
            }
            let before = times.len();
            let mut grid = grid;
            grid.sort_by(|a, b| a.0.total_cmp(&b.0));
            times.extend(grid.iter().filter_map(|&(time, _)| {
                masking
                    .iter()
                    .all(|strong| {
                        let lower = strong
                            .partition_point(|&(t, _)| t <= time)
                            .saturating_sub(1);
                        strong[lower].1
                    })
                    .then_some(time)
            }));
            varying |= grid.len() > 1 && times.len() > before;
            if grid.iter().all(|&(_, sparse)| !sparse) {
                break;
            }
            masking.push(grid);
        }
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| *a == *b);
        (times, varying)
    }

    pub(crate) fn from_parts(
        prims: HashMap<PathId, PrimIndex>,
        children: HashMap<PathId, Vec<PathId>>,
        with_provenance: bool,
        deps: Option<CompositionDeps>,
    ) -> Self {
        #[cfg(test)]
        let prims = {
            let mut prims = prims;
            for index in prims.values_mut() {
                index.group_fields();
            }
            prims
        };
        Self {
            options: StageOptions::default(),
            loadable: HashSet::new(),
            local_layers: Vec::new(),
            used_layers: alloc::collections::BTreeSet::new(),
            used_layer_sites: HashMap::new(),
            inactive: HashSet::new(),
            inactive_children: HashMap::new(),
            prototypes: prototypes::PrototypeTable::default(),
            composition_work: CompositionWork::default(),
            root_layer: None,
            clips: crate::value_clips::Catalog::default(),
            prims,
            children,
            with_provenance,
            deps,
            errors: Vec::new(),
            instances: HashSet::new(),
            variant_fallbacks: VariantFallbacks::default(),
            schemas: None,
            type_infos: HashMap::new(),
        }
    }

    pub(crate) fn with_layer_inventory(
        mut self,
        local: Vec<LayerId>,
        used: alloc::collections::BTreeSet<LayerId>,
        sites: HashMap<PathId, HashSet<LayerId>>,
    ) -> Self {
        self.local_layers = local;
        self.used_layers = used;
        self.used_layer_sites = sites;
        self
    }

    pub(crate) fn with_inactive(
        mut self,
        inactive: HashSet<PathId>,
        children: HashMap<PathId, Vec<PathId>>,
    ) -> Self {
        self.inactive = inactive;
        self.inactive_children = children;
        self
    }

    pub(crate) fn with_loadable(mut self, loadable: HashSet<PathId>) -> Self {
        self.loadable = loadable;
        self
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
    pub(crate) fn merge_prims_from(
        &mut self,
        store: &dyn LayerStore,
        mut partial: Self,
        recomposed: &[PathId],
    ) {
        for path in recomposed {
            self.used_layer_sites.remove(path);
            if let Some(layers) = partial.used_layer_sites.remove(path) {
                self.used_layer_sites.insert(*path, layers);
            }
        }
        self.used_layers = self
            .local_layers
            .iter()
            .copied()
            .chain(self.used_layer_sites.values().flatten().copied())
            .collect();
        self.clips.merge_from(partial.clips, recomposed);
        for path in recomposed {
            if partial.inactive.contains(path) {
                self.inactive.insert(*path);
            } else {
                self.inactive.remove(path);
            }
            if let Some(mut index) = partial.prims.remove(path) {
                if let Some(info) = &mut index.type_info {
                    if let Some(shared) =
                        self.type_infos.get(&info.identity).and_then(Weak::upgrade)
                    {
                        *info = shared;
                    } else {
                        self.type_infos
                            .insert(info.identity.clone(), Arc::downgrade(info));
                    }
                }
                if let Some(old) = self.prims.insert(*path, index) {
                    self.forget_type_info(old.type_info);
                }
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
        self.prepare_prototypes(store);
    }

    /// Refreshes existing value slots without rebuilding prim graphs. The
    /// caller supplies only source dependents and verifies their generations.
    /// Ambiguous provenance takes the ordinary composition path. Preparation
    /// is atomic: no cached opinion changes unless every mapping is known.
    ///
    /// Spec: AOUSD Core §12.3 (attribute value resolution is distinct from
    /// prim composition). OpenUSD's `PcpChanges::DidChange` skips property-only
    /// changes in USD mode unless a dynamic file format depends on them.
    pub(crate) fn refresh_values(
        &mut self,
        store: &dyn LayerStore,
        edits: &[crate::edit::PropertyValueEdit],
        dependents: &[Vec<PathId>],
    ) -> bool {
        if edits
            .iter()
            .any(|edit| self.clips.layers().contains(&edit.layer))
        {
            return false;
        }
        let mut patches = Vec::new();
        for (edit_index, (edit, prims)) in edits.iter().zip(dependents).enumerate() {
            let Some(authored) = edit.property(store) else {
                return false;
            };
            for &prim in prims {
                let Some(index) = self.prims.get(&prim) else {
                    return false;
                };
                let Some(opinions) = index.property_opinions(edit.name) else {
                    return false;
                };
                let mut matched = false;
                for (position, opinion) in opinions.iter().enumerate() {
                    let key = &opinion.key;
                    if key.layer_id != edit.layer
                        || (key.lookup_path != edit.path.prim_path()
                            && key.spec_path.prim_path() != edit.path.prim_path())
                    {
                        continue;
                    }
                    // Remapped source paths and mixed variant contexts need
                    // composition to recover their exact authored locator.
                    if key.spec_path != edit.path
                        || !matches!(opinion.value, OpinionValue::Property(_))
                    {
                        return false;
                    }
                    matched = true;
                    let mut default = edit.default.then(|| authored.default.clone());
                    let mut samples = edit.samples.then(|| authored.time_samples.clone());
                    let shareable = !default
                        .iter()
                        .flatten()
                        .chain(
                            samples
                                .iter()
                                .flatten()
                                .flat_map(|samples| samples.iter().map(|(_, value)| value)),
                        )
                        .any(crate::path_expression::value::has_path_expression);
                    crate::path_expression::anchor_fresh_values(
                        store,
                        &index.graph,
                        prim,
                        key.node,
                        default.iter_mut().flatten().chain(
                            samples
                                .iter_mut()
                                .flatten()
                                .filter(|samples| {
                                    samples.iter().any(|(_, value)| {
                                        crate::path_expression::value::has_path_expression(value)
                                    })
                                })
                                .flat_map(|samples| samples.make_mut().iter_mut())
                                .map(|(_, value)| value),
                        ),
                    );
                    patches.push((
                        prim, edit.name, position, default, samples, edit_index, shareable,
                    ));
                }
                if !matched {
                    return false;
                }
            }
        }
        // Retain the old Arc alongside the replacement so pointer identity
        // cannot be reused during this refresh. An edit's unmapped slots are
        // identical across placements sharing the same old snapshot; mapped
        // expression values deliberately bypass this sharing.
        let mut refreshed = HashMap::new();
        for (prim, name, position, default, samples, edit_index, shareable) in patches {
            let opinion = &mut self
                .prims
                .get_mut(&prim)
                .expect("validated prim")
                .field_opinions_mut(crate::prim_index::FieldKey::Property(name))
                .expect("validated field")[position];
            let OpinionValue::Property(spec) = &mut opinion.value else {
                unreachable!("validated property");
            };
            let shared_key = (Arc::as_ptr(spec) as usize, edit_index);
            if shareable && let Some((_, updated)) = refreshed.get(&shared_key) {
                *spec = Arc::clone(updated);
                continue;
            }
            let original = (shareable && Arc::strong_count(spec) > 1).then(|| Arc::clone(spec));
            let record = Arc::make_mut(spec);
            if let Some(default) = default {
                record.default = default;
            }
            if let Some(samples) = samples {
                record.time_samples = samples;
            }
            if let Some(original) = original {
                refreshed.insert(shared_key, (original, Arc::clone(spec)));
            }
        }
        self.reshare_prototype_records();
        true
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

    /// Returns the composed descendants, outside `recomposed`, of each prim
    /// in `recomposed` whose contributing specs a partial composition
    /// changes, sorted by [`PathId`].
    ///
    /// A prim's arcs are ancestral to its descendants: selecting another
    /// branch of `/P`'s variant set changes which `/P{v=x}C` specs `/P/C`
    /// composes, though `/P/C` draws nothing from the spec that authors the
    /// selection. Those descendants must be recomposed too.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (variants), §10.2 (ancestral arcs).
    /// OpenUSD treats such an edit as a significant change that resyncs the
    /// prim's subtree (`PcpChanges::DidChange` in `pxr/usd/pcp/changes.cpp`).
    pub(crate) fn resynced_descendants(
        &self,
        partial: &Self,
        recomposed: &[PathId],
    ) -> Vec<PathId> {
        let recomposed_set: HashSet<PathId> = recomposed.iter().copied().collect();
        let mut out = Vec::new();
        for prim in recomposed {
            if !self.sources_changed(partial, *prim) {
                continue;
            }
            out.extend(
                self.traverse(*prim)
                    .filter(|descendant| !recomposed_set.contains(descendant)),
            );
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Compares complete ordered spec stacks, including variant context.
    /// The invalidation source-site lookup intentionally erases that context
    /// and is not sufficient to decide whether a prim needs a resync.
    pub(crate) fn sources_changed(&self, partial: &Self, prim: PathId) -> bool {
        self.prims.get(&prim).map(|index| &index.sources)
            != partial.prims.get(&prim).map(|index| &index.sources)
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
            .chain(index.opinions.iter().map(|op| &op.key));
        let mut sites = HashSet::new();
        for key in keys {
            sites.insert((key.layer_id, key.lookup_path));
            sites.insert((key.layer_id, key.spec_path.prim_path()));
        }
        sites.extend(self.clips.source_sites(prim));
        let mut sites: Vec<_> = sites.into_iter().collect();
        sites.sort_unstable();
        sites
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
        let ops = opinions.iter().filter_map(|op| op.value.targets());
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
        self.resolve_default_with_source(
            field,
            opinions,
            property_type,
            fallback,
            self.with_provenance,
        )
    }

    fn resolve_default_with_source(
        &self,
        field: TokenId,
        opinions: &[Opinion],
        property_type: Option<&PropertyType>,
        fallback: Option<&Value>,
        with_source: bool,
    ) -> Option<Resolved<ResolvedValue>> {
        // Spec: AOUSD Core §12.3.2.1 (`timecode` values are read in stage
        // time, through each opinion's layer offset).
        let opinions = stage_time::opinions_in_stage_time(opinions);
        let opinions: &[Opinion] = &opinions;
        let strongest_default = opinions
            .iter()
            .find(|opinion| opinion.value.default_value().is_some());

        // Spec: AOUSD Core §12.3 (a path expression's `%_` composes over
        // the next weaker one).
        if let Some(fold) = crate::path_expression::fold_default(opinions, fallback) {
            return Some(Resolved {
                value: ResolvedValue::Scalar(fold.value?),
                provenance: strongest_default
                    .and_then(|op| self.provenance_for_if(field, op, with_source)),
            });
        }

        if strongest_default.is_some_and(|opinion| {
            opinion
                .value
                .default_value()
                .is_some_and(|value| value.array_edit_type().is_some())
        }) {
            let (result, source) = crate::value_resolution::resolve_sparse_default_matching_source(
                opinions,
                property_type,
                fallback,
                |_| true,
            );
            return match result {
                SparseResolveResult::Resolved(value) => Some(Resolved {
                    value: ResolvedValue::Scalar(value),
                    provenance: source
                        .and_then(|i| self.provenance_for_if(field, &opinions[i], with_source)),
                }),
                _ => None,
            };
        }

        match resolve_sparse_value(opinions, SparseQuery::Default { fallback }, property_type) {
            SparseResolveResult::Resolved(value) => {
                return Some(Resolved {
                    value: ResolvedValue::Scalar(value),
                    provenance: strongest_default
                        .and_then(|op| self.provenance_for_if(field, op, with_source)),
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
                provenance: self.provenance_for_if(field, strongest_default, with_source),
            }),
            value => Some(Resolved {
                value: ResolvedValue::Scalar(value.clone()),
                provenance: self.provenance_for_if(field, strongest_default, with_source),
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
        if matches!(lookup, Lookup::Property) {
            return self
                .resolve_property_at_time(
                    prim,
                    field,
                    time,
                    interp,
                    fallback,
                    None,
                    self.with_provenance,
                )
                .0;
        }
        let (index, opinions) = self.opinions(prim, field, lookup)?;
        self.resolve_at_time_over(
            field,
            opinions,
            index.property_type_for(&field),
            time,
            interp,
            fallback,
            None,
        )
    }

    /// Resolves a chain of opinions at numeric `time`, as
    /// [`Stage::resolve_value_at_time`] does.
    fn resolve_at_time_over(
        &self,
        field: TokenId,
        opinions: &[Opinion],
        property_type: Option<&PropertyType>,
        time: f64,
        interp: InterpolationType,
        fallback: Option<&Value>,
        accepts: Option<&dyn Fn(&Value) -> bool>,
    ) -> Option<Resolved<Value>> {
        self.resolve_at_time_with_source(
            field,
            opinions,
            property_type,
            time,
            interp,
            fallback,
            accepts,
            self.with_provenance,
            None,
        )
    }

    fn resolve_at_time_with_source(
        &self,
        field: TokenId,
        opinions: &[Opinion],
        property_type: Option<&PropertyType>,
        time: f64,
        interp: InterpolationType,
        fallback: Option<&Value>,
        accepts: Option<&dyn Fn(&Value) -> bool>,
        with_source: bool,
        composability: Option<&SampleComposability<'_>>,
    ) -> Option<Resolved<Value>> {
        // Spec: AOUSD Core §12.3.2.1 (`timecode` values are read in stage
        // time, through each opinion's layer offset).
        let animation = stage_time::animation_opinions(opinions);
        let opinions = stage_time::opinions_in_stage_time(&animation);
        let opinions: &[Opinion] = &opinions;

        // Spec: AOUSD Core §12.3 (a path expression's `%_` composes over
        // the next weaker one at every time).
        if let Some(fold) = crate::path_expression::fold_at_time(opinions, time, interp, fallback) {
            let strongest = fold.contributors.first().copied().flatten();
            return Some(Resolved {
                value: fold.value?,
                provenance: strongest
                    .and_then(|i| self.provenance_for_if(field, &opinions[i], with_source)),
            });
        }

        let sparse = match accepts {
            Some(accepts) => {
                crate::value_resolution::resolve_sparse_at_time_matching_with_composability(
                    opinions,
                    property_type,
                    time,
                    interp,
                    fallback,
                    accepts,
                    composability,
                )
            }
            None => crate::value_resolution::resolve_sparse_value_with_composability(
                opinions,
                SparseQuery::AtTime {
                    time,
                    interp,
                    fallback,
                },
                property_type,
                composability,
            ),
        };
        match sparse {
            SparseResolveResult::Resolved(value) => {
                return Some(Resolved {
                    value,
                    provenance: opinions
                        .iter()
                        .find(|opinion| {
                            opinion.value.default_value().is_some()
                                || opinion
                                    .value
                                    .time_samples()
                                    .is_some_and(|samples| !samples.is_empty())
                                || opinion.value.spline().is_some()
                        })
                        .and_then(|opinion| self.provenance_for_if(field, opinion, with_source)),
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
            provenance: self.provenance_for_if(field, opinion, with_source),
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
        self.resolve_property_metadata_over(opinions, property, key)
    }

    /// Resolves the property metadata field `key` over a chain of property
    /// opinions, as [`Stage::resolve_property_metadata`] does.
    fn resolve_property_metadata_over(
        &self,
        opinions: &[Opinion],
        property: TokenId,
        key: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
        // Spec: AOUSD Core §12.3.2.1 (`timecode` values in stage time).
        let opinions = stage_time::opinions_in_stage_time(opinions);
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
        let index = self.prims.get(&prim)?;
        index.sources.iter().find_map(|source| {
            let spec = store.layer(source.layer_id)?.source_prim_spec(
                source.lookup_path,
                &source.spec_path,
                store.paths(),
            )?;
            let chain = source.spec_path.variant_chain();
            if chain.is_empty() {
                spec.property_order.clone()
            } else {
                spec.variant_spec(&chain)?.property_order.clone()
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

    /// Resolves a default and materializes its selected numeric array.
    /// Decode failures remain distinct from an absent value or value block.
    pub fn try_resolve_property_path(
        &self,
        path: PropertyPath,
    ) -> Result<Option<Resolved<ResolvedValue>>, crate::ArrayReadError> {
        let mut resolved = self.resolve_property_path(path);
        if let Some(Resolved {
            value: ResolvedValue::Scalar(Value::TypedArray(array)),
            ..
        }) = &mut resolved
        {
            *array = array.try_materialize().map_err(Clone::clone)?.clone();
        }
        Ok(resolved)
    }
    /// Resolves at time and materializes only selected numeric endpoints.
    /// A malformed selected endpoint is an error, never a weaker fallback.
    pub fn try_resolve_property_path_at_time(
        &self,
        path: PropertyPath,
        time: f64,
        interp: InterpolationType,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        let mut resolved = self.resolve_property_path_at_time(path, time, interp);
        if let Some(Resolved {
            value: Value::TypedArray(array),
            ..
        }) = &mut resolved
        {
            *array = array.try_materialize().map_err(Clone::clone)?.clone();
        }
        Ok(resolved)
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

    /// Returns the names of the properties of `prim`: those any opinion
    /// authors and those its schemas define ([`Stage::prim_definition`]),
    /// attributes and relationships together. Empty when `prim` is not on
    /// the stage.
    ///
    /// They are in dictionary order (letters ignoring case, runs of digits
    /// by value), then the names the prim's `reorder properties` lists
    /// ([`Stage::resolve_property_order`]) move to the front in its order
    /// ([`apply_property_order`]), as OpenUSD's `UsdPrim::GetPropertyNames`
    /// orders them.
    ///
    /// Reads the shared prim definition. Without schemas ([`StageOptions::schemas`]) these
    /// are the authored names alone ([`Stage::authored_property_names`]).
    ///
    /// Spec: AOUSD Core §7.3.3 (a prim's properties share one name space),
    /// §12 (the composed prim holds every property any opinion authors),
    /// §13.3.2.3 (and every property its prim definition defines).
    #[must_use]
    pub fn property_names(&self, prim: PathId, store: &dyn LayerStore) -> Vec<TokenId> {
        let mut names = self.authored_names(prim);
        if let Some(definition) = self.prim_definition_ref(prim) {
            let authored: HashSet<TokenId> = names.iter().copied().collect();
            names.extend(
                definition
                    .properties()
                    .iter()
                    .map(|property| property.name)
                    .filter(|name| !authored.contains(name)),
            );
        }
        self.order_property_names(prim, &mut names, store);
        names
    }

    /// Returns the names of the properties some opinion of `prim` authors,
    /// in the order of [`Stage::property_names`], leaving out those only its
    /// schemas define.
    ///
    /// OpenUSD: `UsdPrim::GetAuthoredPropertyNames`.
    ///
    /// Spec: AOUSD Core §12 (the composed prim holds every property any
    /// opinion authors).
    #[must_use]
    pub fn authored_property_names(&self, prim: PathId, store: &dyn LayerStore) -> Vec<TokenId> {
        let mut names = self.authored_names(prim);
        self.order_property_names(prim, &mut names, store);
        names
    }

    /// Sorts `names` in dictionary order, then applies the prim's
    /// `reorder properties`.
    fn order_property_names(&self, prim: PathId, names: &mut [TokenId], store: &dyn LayerStore) {
        sort_property_names(names, store.tokens());
        if let Some(order) = self.resolve_property_order(prim, store) {
            apply_property_order(&order, names);
        }
    }

    /// The names of the properties opinions of `prim` author, unsorted.
    fn authored_names(&self, prim: PathId) -> Vec<TokenId> {
        use crate::prim_index::FieldKey;

        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        index
            .fields
            .iter()
            .map(|(field, _)| field)
            .filter_map(|key| match key {
                FieldKey::Property(name) => Some(*name),
                FieldKey::Metadata(_) => None,
            })
            .collect()
    }

    /// Whether `prim` is a composed native instance root. Default USD
    /// traversal visits this root but does not descend into instance proxies.
    /// Spec: AOUSD Core §11.3.3 (scene-graph instancing).
    #[must_use]
    pub fn is_instance(&self, prim: PathId) -> bool {
        self.instances.contains(&prim)
    }

    /// Traverses prims in a deterministic preorder.
    /// Borrows child lists lazily; auxiliary storage grows with depth, not fan-out.
    pub fn traverse(&self, root: PathId) -> Traverse<'_> {
        Traverse::new(self, root, false)
    }

    /// Traverses the populated namespace including inactive prim roots.
    /// Inactive descendants are unpopulated, so this does not traverse them.
    /// Spec: AOUSD Core §11.3.1; OpenUSD `UsdStage::TraverseAll`.
    pub fn traverse_all(&self, root: PathId) -> Traverse<'_> {
        Traverse::new(self, root, true)
    }

    /// Whether a populated prim is active. Inactive prim roots remain
    /// inspectable; their descendants are absent from the snapshot.
    #[must_use]
    pub fn is_active(&self, prim: PathId) -> bool {
        self.has_prim(prim) && !self.inactive.contains(&prim)
    }

    /// Whether a populated active prim has every ancestor payload included.
    /// An unloaded payload prim can remain inspectable alongside local children;
    /// neither is loaded until their required payloads are included.
    /// Spec: AOUSD Core §10.3.2.7, §11.3; OpenUSD `UsdPrim::IsLoaded`.
    #[must_use]
    pub fn is_loaded(&self, prim: PathId, paths: &crate::PathInterner) -> bool {
        if !self.is_active(prim) {
            return false;
        }
        let mut path = Some(paths.resolve(prim).clone());
        while let Some(current) = path {
            if let Some(id) = paths.lookup(&current)
                && self.loadable.contains(&id)
                && !self.options.load_rules.is_loaded(&current)
            {
                return false;
            }
            path = current.parent();
        }
        true
    }

    /// Traversal with the USD default active, loaded, defined and non-abstract
    /// predicate. Includes the supplied root when it matches, as `traverse` does.
    /// Use `traverse_all` for inspection and ordinary iterator filtering for
    /// custom predicates. Spec: AOUSD Core §11; `UsdPrimDefaultPredicate`.
    pub fn traverse_default<'a>(
        &'a self,
        root: PathId,
        store: &'a dyn LayerStore,
    ) -> impl Iterator<Item = PathId> + 'a {
        self.traverse(root).filter(move |p| {
            self.is_loaded(*p, store.paths())
                && self.is_defined(*p, store)
                && !self.is_abstract(*p, store)
        })
    }

    /// All direct populated children, including inactive roots, in composed
    /// order. Instance-proxy traversal follows the same policy as `children_of`.
    #[must_use]
    pub fn all_children_of(&self, prim: PathId) -> Option<&[PathId]> {
        self.inactive_children
            .get(&prim)
            .map(|v| v.as_slice())
            .or_else(|| self.children_of(prim))
    }

    /// Borrows the direct children of `prim` in composed traversal order.
    /// Consumers can reuse this topology without maintaining a namespace copy.
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

    /// Returns the variant sets of the composed prim `prim`, each with the
    /// variant composition selected for it: the sets its specs declare,
    /// those nested in the selected variants included (a set declared only
    /// in a variant that is not selected is not one), strongest spec
    /// first, each spec's in `variantSets` order. A selection is the one
    /// [`Stage::variant_selections`] reports, which may name a variant no
    /// layer defines; `None` when nothing selects one. Empty when `prim` is
    /// not on the stage.
    ///
    /// OpenUSD: `UsdVariantSets::GetNames`, with
    /// `UsdVariantSet::GetVariantSelection` for each set (a variant arc of
    /// the prim index names the selection it composed).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §10.5 (variant selection).
    #[must_use]
    pub fn variant_sets(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Vec<(TokenId, Option<TokenId>)> {
        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        let selections = self.variant_selections(prim, store);
        let mut sets: Vec<TokenId> = Vec::new();
        for source in &index.sources {
            // A site inside one of the prim's own variants (`/P{v=x}`) is
            // walked from the prim's spec, through its selections.
            if matches!(
                source.spec_path.components().last(),
                Some(crate::spec_path::SpecComponent::VariantSelection { .. })
            ) {
                continue;
            }
            let Some(spec) = store.layer(source.layer_id).and_then(|layer| {
                layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
            }) else {
                continue;
            };
            for set in spec.selected_variant_set_order(&selections) {
                if !sets.contains(&set) {
                    sets.push(set);
                }
            }
        }
        sets.into_iter()
            .map(|set| (set, selections.get(&set).copied()))
            .collect()
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
    /// A `class` read through a direct inherit is weaker than every other
    /// defining opinion: inheriting a class does not make the inheriting
    /// prim a class, even when the class's opinion is stronger than the
    /// `def` of a reference. OpenUSD resolves specifiers the same way
    /// (`_GetPrimSpecifierImpl` in `pxr/usd/usd/stage.cpp`).
    ///
    /// Spec: AOUSD Core §12.2.1 (specifier resolution), §7.6.
    #[must_use]
    pub fn resolve_specifier(&self, prim: PathId, store: &dyn LayerStore) -> Option<Specifier> {
        let index = self.prims.get(&prim)?;
        let depth = store.paths().resolve(prim).depth();
        // The pseudo-root is always defining (OpenUSD `UsdPrim::GetSpecifier`).
        if depth == 0 {
            return Some(Specifier::Def);
        }
        // Whether `node` is reached through an inherit authored at this
        // prim rather than at one of its ancestors (OpenUSD's
        // `PcpIsInheritArc` and `!PcpNodeRef::IsDueToAncestor`).
        let direct_inherit = |node: NodeId| {
            let mut cursor = Some(node);
            while let Some(current) = cursor.and_then(|id| index.graph.node(id)) {
                if current.arc_kind() == ArcKind::Inherits
                    && usize::from(current.namespace_depth()) >= depth
                {
                    return true;
                }
                cursor = current.parent();
            }
            false
        };
        let mut resolved = Specifier::Over;

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
                Some(Specifier::Def) => return Some(Specifier::Def),
                Some(Specifier::Class) if !direct_inherit(key.node) => {
                    return Some(Specifier::Class);
                }
                Some(Specifier::Class) => resolved = Specifier::Class,
                Some(Specifier::Over) | None => {}
            }
        }

        Some(resolved)
    }

    /// Returns `true` if the prim is *defined* per §11.5.
    ///
    /// A prim is defined when it and all its ancestors have resolved specifiers
    /// `def` or `class`. A defined child under an undefining parent is undefining.
    /// OpenUSD `Usd_PrimData::IsDefined`; AOUSD Core §11.5.
    #[must_use]
    pub fn is_defined(&self, prim: PathId, store: &dyn LayerStore) -> bool {
        if !self.has_prim(prim) {
            return false;
        }
        let mut path = Some(store.paths().resolve(prim).clone());
        while let Some(current) = path {
            let Some(id) = store.paths().lookup(&current) else {
                return false;
            };
            if !matches!(
                self.resolve_specifier(id, store),
                Some(Specifier::Def | Specifier::Class)
            ) {
                return false;
            }
            path = current.parent();
        }
        true
    }

    /// Returns `true` when this prim or one of its ancestors is abstract
    /// (its resolved specifier is `class`). OpenUSD `Usd_PrimData::IsAbstract`;
    /// AOUSD Core §11.5.
    #[must_use]
    pub fn is_abstract(&self, prim: PathId, store: &dyn LayerStore) -> bool {
        if !self.has_prim(prim) {
            return false;
        }
        let mut path = Some(store.paths().resolve(prim).clone());
        while let Some(current) = path {
            if let Some(id) = store.paths().lookup(&current)
                && self.resolve_specifier(id, store) == Some(Specifier::Class)
            {
                return true;
            }
            path = current.parent();
        }
        false
    }

    /// Resolves the type name for a composed prim.
    ///
    /// Returns the strongest opinion's type name. If no contributing source
    /// has a type name, returns `None`. The result belongs to the composed
    /// snapshot; source type edits require recomposition. An empty type name and `__AnyType__`
    /// are no opinion, as in OpenUSD (`_ComposeTypeName` in
    /// `pxr/usd/usd/stage.cpp`).
    ///
    /// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
    #[must_use]
    pub fn resolve_type_name(&self, prim: PathId, _store: &dyn LayerStore) -> Option<TokenId> {
        self.prims
            .get(&prim)?
            .type_info
            .as_ref()?
            .identity
            .type_name
    }

    fn source_type_name(index: &PrimIndex, store: &dyn LayerStore) -> Option<TokenId> {
        for key in &index.sources {
            let Some(layer) = store.layer(key.layer_id) else {
                continue;
            };
            let Some(spec) = layer.source_prim_spec(key.lookup_path, &key.spec_path, store.paths())
            else {
                continue;
            };
            if let Some(tn) = spec.type_name {
                let name = store.tokens().resolve(tn);
                if !name.is_empty() && name != "__AnyType__" {
                    return Some(tn);
                }
            }
        }
        None
    }

    /// Resolves a property on a prim with schema fallback.
    ///
    /// Like [`Stage::resolve_property_path`], but when no authored opinion
    /// exists, resolves the fallback of the property's definition in the
    /// prim's definition ([`Stage::property_definition`]), from the stage's
    /// schemas ([`StageOptions::schemas`]).
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
        _store: &dyn LayerStore,
    ) -> Option<Resolved<ResolvedValue>> {
        let index = self.prims.get(&prim);
        let authored = index.and_then(|index| index.property_opinions(field));
        let fallback = self.schema_fallback(prim, field);

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
                if let Some(resolved) =
                    self.resolve_default(field, opinions, index.property_type_for(&field), fallback)
                {
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
                Value::Dictionary(d) => {
                    ResolvedValue::Dictionary(combine_dictionary_chain([d.as_slice()]))
                }
                v => ResolvedValue::Scalar(v.clone()),
            },
            provenance: None,
        })
    }

    /// Reads a composed attribute at `time` into the type accepted by `read`,
    /// including its captured schema fallback and optional source provenance.
    ///
    /// At [`Time::Default`], incompatible dense defaults are skipped while
    /// compatible stronger sparse edits are retained. At numeric times the
    /// source is selected before conversion: an incompatible selected dense
    /// value does not expose weaker sources. Untyped queries remain available
    /// through [`Stage::resolve_property_path`] and
    /// [`Stage::resolve_property_path_at_time`].
    ///
    /// `read` should reject only incompatible storage types. Validate shape
    /// or range after source selection so malformed authored data cannot
    /// silently turn into a fallback. Sparse folding can also invoke `read`
    /// to check candidate bases; shared-buffer conversions avoid temporary
    /// copies when these buffers are large.
    ///
    /// Schema identity and fallbacks belong to the composed stage; token or
    /// asset interpretation can be supplied by the conversion's own context.
    /// This method does not need access to the store the stage came from.
    ///
    /// OpenUSD: `UsdAttribute::Get<T>`. Spec: AOUSD Core §12.3 (values),
    /// §12.3.2.1 (layer offsets), §13.3.2.4 (schema fallbacks).
    #[must_use]
    pub fn read_property<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.read_property_by(property, time, read, self.with_provenance)
    }

    /// Reads the same typed composed value as `read_property`, always including
    /// the winning authored source. This opt-in query does not change the stage's
    /// provenance policy; schema fallbacks still have no authoring source.
    ///
    /// Useful for assets whose relative paths must be anchored to their authoring
    /// layer. Layer offsets, sparse composition and typed source selection use
    /// the ordinary resolver. AOUSD Core §9.4, §12.3, §13.3.2.4.
    #[must_use]
    pub fn read_property_with_provenance<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.read_property_by(property, time, read, true)
    }

    fn read_property_by<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
        with_source: bool,
    ) -> Option<Resolved<T>> {
        let (prim, field) = (property.prim_path(), property.property());
        match time {
            Time::Default => self.read_default(prim, field, read, with_source),
            Time::At {
                code,
                interpolation,
            } => {
                let fallback = self.schema_fallback(prim, field);
                let (resolved, clip_selected) = self.resolve_property_at_time(
                    prim,
                    field,
                    code,
                    interpolation,
                    fallback,
                    Some(&|value| read(value).is_some()),
                    with_source,
                );
                match resolved {
                    Some(resolved) => Some(Resolved {
                        value: read(&resolved.value)?,
                        provenance: resolved.provenance,
                    }),
                    None if clip_selected => None,
                    None => self.read_fallback(fallback, &read),
                }
            }
        }
    }

    /// Reads a default-time attribute into the type accepted by `read`.
    ///
    /// Dense opinions whose values `read` cannot accept are skipped in
    /// strength order; the schema fallback is tried last. This is the typed
    /// default-time contract of OpenUSD's `UsdAttribute::Get<T>` and
    /// `MetadataValueComposer`. Numeric-time reads select their source before
    /// conversion and must not use this method to retry weaker sources.
    ///
    /// Values are borrowed for conversion, avoiding an intermediate owned
    /// copy. Sparse folding also invokes `read` to check candidate dense
    /// bases; prefer shared-buffer conversions when those buffers are large.
    /// Layer offsets still map `timecode` values into stage time.
    /// Dictionaries and path expressions retain the composition rules of
    /// [`Stage::resolve_value_with_schema`]. Sparse array edits retain stronger
    /// edits while skipping incompatible dense bases. The documented
    /// block/fallback behavior of schema resolution applies here too.
    /// `read` is a conversion, not a filter on the resolved value. Validate the shape or range of a
    /// compatible value after source selection; returning `None` for malformed
    /// but correctly typed data would search weaker opinions instead.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §12.3.2.1 (layer offsets),
    /// §13.3.2.4 (schema fallbacks). The typed default-time retry is an
    /// OpenUSD API behavior beyond the untyped value-resolution contract.
    fn read_default<T>(
        &self,
        prim: PathId,
        field: TokenId,
        read: impl Fn(&Value) -> Option<T>,
        with_source: bool,
    ) -> Option<Resolved<T>> {
        let fallback = self.schema_fallback(prim, field);
        if let Some((index, opinions)) = self.opinions(prim, field, Lookup::Property) {
            for (position, opinion) in opinions.iter().enumerate() {
                let Some(value) = opinion.value.default_value() else {
                    continue;
                };
                if let Value::TypedArray(array) = value
                    && array.try_materialize().is_err()
                {
                    return None;
                }
                if matches!(value, Value::Blocked) {
                    break;
                }
                if matches!(value, Value::ArrayEdit(_) | Value::TypedArrayEdit(_)) {
                    let mapped = stage_time::opinions_in_stage_time(&opinions[position..]);
                    let (resolved, source) =
                        crate::value_resolution::resolve_sparse_default_matching_source(
                            &mapped,
                            index.property_type_for(&field),
                            fallback,
                            |value| read(value).is_some(),
                        );
                    if let SparseResolveResult::Resolved(value) = resolved
                        && let Some(value) = read(&value)
                    {
                        return Some(Resolved {
                            value,
                            provenance: source.and_then(|i| {
                                self.provenance_for_if(field, &mapped[i], with_source)
                            }),
                        });
                    }
                    return None;
                }
                // Compositional families must resolve as a family, never by
                // feeding one uncomposed authored edit to the conversion.
                if matches!(value, Value::Dictionary(_) | Value::PathExpression(_)) {
                    let Some(resolved) = self.resolve_default_with_source(
                        field,
                        &opinions[position..],
                        index.property_type_for(&field),
                        fallback,
                        with_source,
                    ) else {
                        break;
                    };
                    let value = match resolved.value {
                        ResolvedValue::Scalar(value) => value,
                        ResolvedValue::Dictionary(entries) => Value::Dictionary(entries),
                        _ => return None,
                    };
                    if let Some(value) = read(&value) {
                        return Some(Resolved {
                            value,
                            provenance: resolved.provenance,
                        });
                    }
                    continue;
                }
                let mapped = stage_time::retime_value(value, opinion.layer_offset);
                if let Some(value) = read(mapped.as_ref().unwrap_or(value)) {
                    return Some(Resolved {
                        value,
                        provenance: self.provenance_for_if(field, opinion, with_source),
                    });
                }
            }
        }
        self.read_fallback(fallback, &read)
    }

    fn read_fallback<T>(
        &self,
        fallback: Option<&Value>,
        read: &impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        let fallback = fallback?;
        let dictionary = match fallback {
            Value::Dictionary(entries) => Some(Value::Dictionary(combine_dictionary_chain([
                entries.as_slice(),
            ]))),
            _ => None,
        };
        Some(Resolved {
            value: read(dictionary.as_ref().unwrap_or(fallback))?,
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
    ) -> Option<Resolved<Value>> {
        let resolved = self.resolve_value_with_schema(prim, field, store)?;
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
        _store: &dyn LayerStore,
    ) -> Option<Resolved<Value>> {
        let fallback = self.schema_fallback(prim, field);
        let (resolved, clip_selected) = self.resolve_property_at_time(
            prim,
            field,
            time,
            interp,
            fallback,
            None,
            self.with_provenance,
        );
        if resolved.is_some() || clip_selected {
            return resolved;
        }
        let value = match fallback? {
            Value::Dictionary(entries) => {
                Value::Dictionary(combine_dictionary_chain([entries.as_slice()]))
            }
            value => value.clone(),
        };
        Some(Resolved {
            value,
            provenance: None,
        })
    }

    /// The schema fallback for `field` on `prim`.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    fn schema_fallback(&self, prim: PathId, field: TokenId) -> Option<&Value> {
        self.property_definition_ref(prim, field)?.fallback.as_ref()
    }

    /// The prim definition of `prim`: its typed schema (or none: it is
    /// typeless), its applied schemas in strength order, and the properties
    /// they define. Empty when the stage has no schemas
    /// ([`StageOptions::schemas`]); `None` when `prim` is not on the stage.
    ///
    /// Returns an owned copy of the shared definition captured at composition.
    /// Use [`Stage::prim_definition_ref`] to borrow it without cloning.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use layerstack::{
    ///     InMemoryStore, Layer, LayerId, PrimSpec, PropertyDefinition, SchemaDefinition,
    ///     SchemaRegistry, Stage, StageOptions,
    /// };
    ///
    /// let mut store = InMemoryStore::default();
    /// let (tile, width) = (store.tokens.intern("Tile"), store.tokens.intern("width"));
    /// let prim = store.path("/Floor");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(prim, PrimSpec::def().with_type_name(tile));
    /// store.insert_layer(layer);
    ///
    /// let mut builder = SchemaRegistry::builder();
    /// builder.register(SchemaDefinition::typed(tile).with_property(
    ///     PropertyDefinition::attribute(width).with_fallback(1.0_f32),
    /// ));
    /// let options = StageOptions {
    ///     schemas: Some(Arc::new(builder.build(&mut store.tokens))),
    ///     ..StageOptions::default()
    /// };
    /// let stage = Stage::compose(&mut store, LayerId(1), options);
    ///
    /// let definition = stage.prim_definition(prim, &store).expect("on the stage");
    /// assert!(definition.is_a(tile));
    /// assert!(definition.property(width).is_some());
    /// ```
    ///
    /// Spec: AOUSD Core §13.3.1 (typeless prims), §13.3.2.3 (the prim
    /// definition). OpenUSD: `UsdPrim::GetPrimDefinition`.
    #[must_use]
    pub fn prim_definition(&self, prim: PathId, _store: &dyn LayerStore) -> Option<PrimDefinition> {
        self.prim_definition_ref(prim).cloned()
    }

    /// Borrows the immutable schema definition captured when this prim was composed.
    /// Equivalent schema identities share one definition. Source schema edits
    /// become visible after recomposition, as with other composed opinions.
    /// Returns an empty definition without schemas, or `None` for an absent prim.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    #[must_use]
    pub fn prim_definition_ref(&self, prim: PathId) -> Option<&PrimDefinition> {
        Some(&self.prims.get(&prim)?.type_info.as_ref()?.definition)
    }

    /// The owned schema definition of a property. Use
    /// [`Self::property_definition_ref`] when an owned copy is unnecessary.
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4.
    #[must_use]
    pub fn property_definition(
        &self,
        prim: PathId,
        property: TokenId,
        _store: &dyn LayerStore,
    ) -> Option<PropertyDefinition> {
        self.property_definition_ref(prim, property).cloned()
    }

    /// Borrows a property's definition from the prim's composed schema identity.
    /// Returns `None` when the prim or schema property is absent.
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4.
    #[must_use]
    pub fn property_definition_ref(
        &self,
        prim: PathId,
        property: TokenId,
    ) -> Option<&PropertyDefinition> {
        self.prim_definition_ref(prim)?.property(property)
    }

    /// Whether the applied schema `schema`, with `instance` for a
    /// multiple-apply schema, may be applied to `prim`, given its resolved
    /// type name ([`SchemaRegistry::can_apply`]), and why not.
    ///
    /// # Errors
    ///
    /// [`CannotApply::NoSuchPrim`] when `prim` is not on the stage,
    /// [`CannotApply::NotAnAppliedSchema`] when the stage has no schemas,
    /// and otherwise what [`SchemaRegistry::can_apply`] reports.
    ///
    /// OpenUSD: `UsdPrim::CanApplyAPI`.
    pub fn can_apply(
        &self,
        prim: PathId,
        schema: TokenId,
        instance: Option<&str>,
        store: &dyn LayerStore,
    ) -> Result<(), CannotApply> {
        if !self.has_prim(prim) {
            return Err(CannotApply::NoSuchPrim);
        }
        let schemas = self
            .schemas
            .as_deref()
            .ok_or(CannotApply::NotAnAppliedSchema)?;
        let type_name = self.resolve_type_name(prim, store);
        schemas.can_apply(type_name, schema, instance, store.tokens())
    }

    /// The composed `apiSchemas` metadata of `prim`.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`), §13.3.2 (it composes as a
    /// list op).
    fn applied_schema_names(&self, prim: PathId, store: &dyn LayerStore) -> Vec<TokenId> {
        store
            .tokens()
            .lookup("apiSchemas")
            .and_then(|field| self.resolve_token_list(prim, field))
            .map(|resolved| resolved.value)
            .unwrap_or_default()
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
        self.provenance_for_if(field, strongest, self.with_provenance)
    }

    fn provenance_for_if(
        &self,
        field: TokenId,
        strongest: &Opinion,
        enabled: bool,
    ) -> Option<Provenance> {
        enabled.then_some(Provenance {
            layer: strongest.key.layer_id,
            spec_path: strongest.key.spec_path.clone(),
            field,
        })
    }
}

/// Moves the names `order` lists to the front of `names`, in `order`'s
/// order, keeping the rest in their order after them; names `order` lists
/// that `names` does not hold are skipped.
///
/// OpenUSD: `UsdPrim::ApplyPropertyOrder` (`pxr/usd/usd/prim.cpp`), which
/// `UsdPrim::GetPropertyNames` applies to the sorted names.
///
/// Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren` ordering).
pub fn apply_property_order(order: &[TokenId], names: &mut [TokenId]) {
    let mut rest = 0;
    for name in order {
        if let Some(found) = names[rest..].iter().position(|n| n == name) {
            names[rest..=rest + found].rotate_right(1);
            rest += 1;
        }
    }
}

/// Sorts property names as OpenUSD's `UsdPrim::GetPropertyNames` does
/// ([`dictionary_cmp`]).
fn sort_property_names(names: &mut [TokenId], tokens: &crate::interner::TokenInterner) {
    names.sort_by(|a, b| dictionary_cmp(tokens.resolve(*a), tokens.resolve(*b)));
}

/// Orders names as OpenUSD's `TfDictionaryLessThan`
/// (`pxr/base/tf/stringUtils.h`) does for the ASCII names of properties:
/// letters ignoring case, `_` before letters, runs of digits by value, and
/// ties by bytes.
pub(crate) fn dictionary_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let (x, y) = (a.as_bytes(), b.as_bytes());
    /// The end of the run of digits at `start`, and its digits without
    /// leading zeros.
    fn digits(s: &[u8], start: usize) -> (usize, &[u8]) {
        let end = s[start..]
            .iter()
            .position(|c| !c.is_ascii_digit())
            .map_or(s.len(), |n| start + n);
        let zeros = s[start..end].iter().take_while(|&&c| c == b'0').count();
        (end, &s[start + zeros..end])
    }
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        let (l, r) = (x[i], y[j]);
        if l.is_ascii_digit() && r.is_ascii_digit() {
            let (x_end, x_run) = digits(x, i);
            let (y_end, y_run) = digits(y, j);
            let order = x_run.len().cmp(&y_run.len()).then_with(|| x_run.cmp(y_run));
            if order != Ordering::Equal {
                return order;
            }
            (i, j) = (x_end, y_end);
            continue;
        }
        if l != r {
            let letter_zone = (0x40..0x80).contains(&l) && (0x40..0x80).contains(&r);
            if letter_zone && (l & !0x20) != (r & !0x20) {
                // `(c + 5) & 31` puts `_` before every letter.
                return (l.wrapping_add(5) & 31).cmp(&(r.wrapping_add(5) & 31));
            }
            if !(l.is_ascii_alphabetic() && r.is_ascii_alphabetic()) {
                return l.cmp(&r);
            }
        }
        i += 1;
        j += 1;
    }
    (x.len() - i).cmp(&(y.len() - j)).then_with(|| a.cmp(b))
}

/// An iterator for deterministic preorder stage traversal.
///
/// Yields each [`PathId`] starting from the root, visiting children in
/// authored order before moving to sibling subtrees. Created by
/// [`Stage::traverse`].
#[derive(Debug)]
pub struct Traverse<'a> {
    stage: &'a Stage,
    root: Option<PathId>,
    include_inactive: bool,
    stack: Vec<core::slice::Iter<'a, PathId>>,
}

impl<'a> Traverse<'a> {
    fn new(stage: &'a Stage, root: PathId, include_inactive: bool) -> Self {
        Self {
            stage,
            root: (stage.has_prim(root) && (include_inactive || stage.is_active(root)))
                .then_some(root),
            include_inactive,
            stack: Vec::new(),
        }
    }
}

impl Iterator for Traverse<'_> {
    type Item = PathId;

    fn next(&mut self) -> Option<Self::Item> {
        let next = if let Some(root) = self.root.take() {
            root
        } else {
            loop {
                if let Some(&path) = self.stack.last_mut()?.next() {
                    if self.stack.last()?.as_slice().is_empty() {
                        self.stack.pop();
                    }
                    break path;
                }
                self.stack.pop();
            }
        };
        let children = if self.include_inactive {
            self.stage.all_children_of(next)
        } else {
            self.stage.children_of(next)
        };
        if let Some(children) = children
            && !children.is_empty()
        {
            self.stack.push(children.iter());
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
    let value = if let Some(samples) = opinion
        .value
        .time_samples()
        .filter(|samples| !samples.is_empty())
    {
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
        SplineDataType::TimeCode => Value::TimeCode(val),
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

    #[test]
    fn traversal_borrows_wide_child_lists_and_preserves_preorder() {
        let root = PathId::from_raw(0);
        let children: Vec<_> = (1..=100_000).map(PathId::from_raw).collect();
        let stage = Stage::from_parts(
            HashMap::from([(root, PrimIndex::new(PrimIndexGraph::default()))]),
            HashMap::from([
                (root, children.clone()),
                (PathId::from_raw(1), vec![PathId::from_raw(100_001)]),
            ]),
            false,
            None,
        );
        let mut walk = stage.traverse(root);
        assert!(walk.stack.is_empty());
        assert_eq!(walk.next(), Some(root));
        assert_eq!(
            walk.stack.len(),
            1,
            "one borrowed iterator, not 100k queued children"
        );
        assert_eq!(walk.next(), Some(PathId::from_raw(1)));
        assert_eq!(walk.stack.len(), 2);
        assert_eq!(walk.next(), Some(PathId::from_raw(100_001)));
        assert_eq!(walk.collect::<Vec<_>>(), children[1..]);
    }

    #[test]
    fn shared_schema_identity_tracks_edits_and_undo_without_retaining_history() {
        use crate::{
            EditTarget, InMemoryStore, Layer, LiveStage, PrimSpec, SchemaDefinition, Transaction,
        };
        let mut store = InMemoryStore::default();
        let a = store.path("/A");
        let b = store.path("/B");
        let ty = store.tokens.intern("Thing");
        let api = store.tokens.intern("ExtraAPI");
        let field = store.tokens.intern("extra");
        let mut builder = SchemaRegistry::builder();
        builder.register(SchemaDefinition::typed(ty));
        builder.register(
            SchemaDefinition::api(api)
                .with_property(PropertyDefinition::attribute(field).with_fallback(7)),
        );
        let registry = Arc::new(builder.build(&mut store.tokens));
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(a, PrimSpec::def().with_type_name(ty));
        layer.insert_prim(b, PrimSpec::def().with_type_name(ty));
        store.insert_layer(layer);
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(registry.clone()),
                ..StageOptions::default()
            },
        );
        assert!(core::ptr::eq(
            live.stage().prim_definition_ref(a).unwrap(),
            registry.schema_definition(ty).unwrap()
        ));
        assert!(core::ptr::eq(
            live.stage().prim_definition_ref(a).unwrap(),
            live.stage().prim_definition_ref(b).unwrap()
        ));
        let at = EditTarget::for_layer(LayerId(1));
        let value = store.tokens.intern("value");
        let mut create_value = Transaction::new();
        create_value.create_property(
            at.property(PropertyPath::new(a, value)),
            PropertySpec::attribute()
                .with_type(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(1)),
        );
        live.apply(&mut store, &create_value).unwrap();
        let held = live.stage().prims[&a].type_info.clone().unwrap();
        let mut change_value = Transaction::new();
        change_value.set_default(at.property(PropertyPath::new(a, value)), Value::Int(2));
        let changed = live.apply(&mut store, &change_value).unwrap();
        assert!(changed.changes.resynced.is_empty());
        assert!(Arc::ptr_eq(
            &held,
            live.stage().prims[&a].type_info.as_ref().unwrap()
        ));
        live.apply(&mut store, &changed.inverse).unwrap();
        drop(held);
        let mut add = Transaction::new();
        add.add_applied_schema(at.prim(a), api);
        let undo = live.apply(&mut store, &add).unwrap().inverse;
        assert_eq!(
            live.stage()
                .resolve_field_with_schema(a, field, &store)
                .unwrap()
                .value,
            Value::Int(7)
        );
        assert!(live.stage().property_definition_ref(b, field).is_none());
        let mut add_b = Transaction::new();
        add_b.add_applied_schema(at.prim(b), api);
        let undo_b = live.apply(&mut store, &add_b).unwrap().inverse;
        assert!(core::ptr::eq(
            live.stage().prim_definition_ref(a).unwrap(),
            live.stage().prim_definition_ref(b).unwrap()
        ));
        live.apply(&mut store, &undo_b).unwrap();
        live.apply(&mut store, &undo).unwrap();
        for _ in 0..20 {
            let applied = live.apply(&mut store, &add).unwrap();
            let fresh = Stage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(registry.clone()),
                    ..StageOptions::default()
                },
            );
            assert_eq!(
                live.stage().prim_definition_ref(a),
                fresh.prim_definition_ref(a)
            );
            live.apply(&mut store, &applied.inverse).unwrap();
        }
        assert!(live.stage().property_definition_ref(a, field).is_none());
        // Includes the pseudo-root identity; old combinations do not accumulate.
        assert!(live.stage().type_infos.len() <= 2);
    }

    #[test]
    fn type_identity_is_a_composed_snapshot() {
        use crate::{InMemoryStore, Layer, LiveStage, PrimSpec, SchemaDefinition};
        let mut store = InMemoryStore::default();
        let path = store.path("/A");
        let a = store.tokens.intern("TypeA");
        let b = store.tokens.intern("TypeB");
        let field = store.tokens.intern("x");
        let mut builder = SchemaRegistry::builder();
        builder.register(
            SchemaDefinition::typed(a)
                .with_property(PropertyDefinition::attribute(field).with_fallback(1)),
        );
        builder.register(
            SchemaDefinition::typed(b)
                .with_property(PropertyDefinition::attribute(field).with_fallback(2)),
        );
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(path, PrimSpec::def().with_type_name(a));
        store.insert_layer(layer);
        let registry = Arc::new(builder.build(&mut store.tokens));
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(registry),
                ..StageOptions::default()
            },
        );
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&path)
            .unwrap()
            .type_name = Some(b);
        assert_eq!(live.stage().resolve_type_name(path, &store), Some(a));
        assert_eq!(
            live.stage()
                .resolve_field_with_schema(path, field, &store)
                .unwrap()
                .value,
            Value::Int(1)
        );
        live.notify_layer_edit(LayerId(1));
        live.recompose(&mut store);
        assert_eq!(live.stage().resolve_type_name(path, &store), Some(b));
        assert_eq!(
            live.stage()
                .resolve_field_with_schema(path, field, &store)
                .unwrap()
                .value,
            Value::Int(2)
        );
    }

    /// Layer 1 authors `class "C"` and `over "A"`, which references `/B` of
    /// layer 2; layer 2 authors `class "C"` and `def "B"`, which inherits
    /// `/C`. `/A`'s specifier opinions, strongest first: `over` (`/A`),
    /// `class` (`/C` of layer 1, implied), `def` (`/B`), `class` (`/C` of
    /// layer 2).
    fn inherited_class_scene(store: &mut crate::InMemoryStore) -> PathId {
        use crate::{Layer, PrimSpec, Reference};

        let (a, b, c) = (store.path("/A"), store.path("/B"), store.path("/C"));
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(c, PrimSpec::class());
        let mut over = PrimSpec::over();
        over.references.explicit = Some(vec![Reference::new(LayerId(2), b)]);
        root.insert_prim(a, over);
        store.insert_layer(root);
        let mut other = Layer::new(LayerId(2));
        other.insert_prim(c, PrimSpec::class());
        other.insert_prim(b, PrimSpec::def().with_inherit(c));
        store.insert_layer(other);
        a
    }

    #[test]
    fn inheriting_a_class_does_not_make_a_prim_a_class() {
        // Spec: AOUSD Core §12.2.1. The `class` of a direct inherit is
        // weaker than every other defining specifier, as in OpenUSD's
        // `_GetPrimSpecifierImpl`: `/A` is a `def`.
        let mut store = crate::InMemoryStore::default();
        let a = inherited_class_scene(&mut store);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let layers: Vec<LayerId> = stage
            .prim_stack(a)
            .expect("composed")
            .into_iter()
            .map(|(layer, _)| layer)
            .collect();
        assert_eq!(
            layers,
            [LayerId(1), LayerId(1), LayerId(2), LayerId(2)],
            "the implied class of layer 1 is stronger than the referenced def"
        );
        assert_eq!(stage.resolve_specifier(a, &store), Some(Specifier::Def));
        let c = store.path("/C");
        assert_eq!(stage.resolve_specifier(c, &store), Some(Specifier::Class));
    }

    #[test]
    fn a_prim_that_only_inherits_a_class_is_a_class() {
        // Spec: AOUSD Core §12.2.1. With no other defining opinion, the
        // inherited `class` still defines the prim.
        let mut store = crate::InMemoryStore::default();
        let (a, c) = (store.path("/A"), store.path("/C"));
        let mut root = crate::Layer::new(LayerId(1));
        root.insert_prim(c, crate::PrimSpec::class());
        root.insert_prim(a, crate::PrimSpec::over().with_inherit(c));
        store.insert_layer(root);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.resolve_specifier(a, &store), Some(Specifier::Class));
    }

    #[test]
    fn any_type_is_no_type_name_opinion() {
        // OpenUSD's `_ComposeTypeName` skips `__AnyType__`, so the
        // referenced prim's type wins.
        use crate::{Layer, PrimSpec, Reference};

        let mut store = crate::InMemoryStore::default();
        let (a, b) = (store.path("/A"), store.path("/B"));
        let any = store.tokens.intern("__AnyType__");
        let tree = store.tokens.intern("Tree");
        let mut root = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def().with_type_name(any);
        spec.references.explicit = Some(vec![Reference::new(LayerId(2), b)]);
        root.insert_prim(a, spec);
        store.insert_layer(root);
        let mut other = Layer::new(LayerId(2));
        other.insert_prim(b, PrimSpec::def().with_type_name(tree));
        store.insert_layer(other);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.resolve_type_name(a, &store), Some(tree));
    }

    #[test]
    fn property_names_sort_as_openusd_does() {
        // As `sorted(names, key=cmp_to_key(Tf.DictionaryStrcmp))` orders
        // them in OpenUSD 26.8.
        let mut names = vec!["b", "A", "a10", "a2", "_x", "a_b", "aB", "a:b", "a1b", "a"];
        names.sort_by(|a, b| dictionary_cmp(a, b));
        assert_eq!(
            names,
            ["_x", "A", "a", "a1b", "a2", "a10", "a:b", "a_b", "aB", "b"]
        );
    }

    /// Test-only opinion payload: a property authoring only time samples.
    fn samples(samples: Vec<(f64, Value)>) -> OpinionValue {
        OpinionValue::from(PropertySpec {
            time_samples: Some(samples.into()),
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
            node: NodeId::ROOT,
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
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: PropertySpec::typed_attribute(int_array_type()).into(),
            layer_offset: LayerOffset::IDENTITY,
        });
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
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: PropertySpec::typed_attribute(int_array_type()).into(),
            layer_offset: LayerOffset::IDENTITY,
        });
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
        index.add_opinion(Opinion {
            key: key.clone(),
            field,
            value: PropertySpec::typed_attribute(int_array_type()).into(),
            layer_offset: LayerOffset::IDENTITY,
        });
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
    ) -> (Stage, crate::doc::InMemoryStore, PathId, TokenId) {
        schema_fallback_fixture_of(opinions, array_value(&[5, 6]), int_array_type())
    }

    /// Like [`schema_fallback_fixture`], for a property of `property_type`
    /// whose schema fallback is `fallback`.
    fn schema_fallback_fixture_of(
        opinions: Vec<FieldValue>,
        fallback: Value,
        property_type: PropertyType,
    ) -> (Stage, crate::doc::InMemoryStore, PathId, TokenId) {
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

        let mut builder = SchemaRegistry::builder();
        builder.register(
            crate::schema::SchemaDefinition::typed(mesh)
                .with_property(PropertyDefinition::attribute(field).with_fallback(fallback)),
        );
        let registry = builder.build(&mut store.tokens);

        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_source(key.clone());
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
                        PropertySpec::typed_attribute(property_type.clone())
                            .with_default(default)
                            .into()
                    }
                    other => other.into(),
                },
                layer_offset: LayerOffset::IDENTITY,
            });
        }

        let mut stage =
            Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        stage.schemas = Some(Arc::new(registry));
        stage.prepare_type_info(&mut store);
        (stage, store, prim, field)
    }

    #[test]
    fn typed_default_read_borrows_the_winning_dense_array() {
        let (stage, _store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Double(2.0)),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        let opinions = stage.prims[&prim].property_opinions(field).unwrap();
        let Value::Array(original) = opinions[1].value.default_value().unwrap() else {
            panic!("array opinion");
        };
        let resolved = stage
            .read_property(
                PropertyPath::new(prim, field),
                Time::Default,
                |value| match value {
                    Value::Array(items) => Some(items.as_ptr()),
                    _ => None,
                },
            )
            .unwrap();
        assert_eq!(resolved.value, original.as_ptr());
        assert!(resolved.provenance.is_none());
    }

    #[test]
    fn typed_default_reads_preserve_compositional_families() {
        let edit = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(9)),
                index: ArrayIndex::Position(0),
            }],
        });
        let expression = |text: &str| Value::PathExpression(text.into());
        let cases = [
            (
                vec![edit, array_value(&[1, 2])],
                array_value(&[5, 6]),
                int_array_type(),
                array_value(&[9, 2]),
            ),
            (
                vec![
                    Value::Dictionary(vec![("a".into(), Value::Int(1))]),
                    Value::Dictionary(vec![("b".into(), Value::Int(2))]),
                ],
                Value::Dictionary(vec![("c".into(), Value::Int(3))]),
                PropertyType::new("dictionary", false, Value::Dictionary(Vec::new())),
                Value::Dictionary(vec![
                    ("a".into(), Value::Int(1)),
                    ("b".into(), Value::Int(2)),
                    ("c".into(), Value::Int(3)),
                ]),
            ),
            (
                vec![expression("/Strong %_")],
                expression("/Fallback"),
                PropertyType::new("pathExpression", false, expression("")),
                expression("/Strong /Fallback"),
            ),
        ];
        for (opinions, fallback, ty, expected) in cases {
            // An incompatible dense opinion is skipped before the existing
            // family resolver sees the remaining source stack.
            let mut chain = vec![FieldValue::Value(Value::Double(2.0))];
            chain.extend(opinions.into_iter().map(FieldValue::Value));
            let (stage, _store, prim, field) = schema_fallback_fixture_of(chain, fallback, ty);
            let resolved = stage
                .read_property(PropertyPath::new(prim, field), Time::Default, |value| {
                    (!matches!(value, Value::Double(_))).then(|| value.clone())
                })
                .unwrap();
            assert_eq!(resolved.value, expected);
            assert!(resolved.provenance.is_none());
        }
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
        let (stage, store, prim, field) = schema_fallback_fixture_of(
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
                .resolve_value_with_schema(prim, field, &store)
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
                )
                .map(|r| r.value),
            Some(composed.clone())
        );
        let explained = stage
            .explain_value_with_schema(prim, field, &store)
            .expect("authored");
        assert_eq!(
            explained.value,
            Some(ResolvedValue::Scalar(composed.clone()))
        );
        assert!(explained.seeded_by_fallback);
        assert_eq!(explained.contributors().count(), 2);
        let explained = stage
            .explain_value_at_time_with_schema(prim, field, 1.0, InterpolationType::Held, &store)
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
        let (stage, store, prim, field) = schema_fallback_fixture(vec![
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
            .resolve_value_with_schema(prim, field, &store)
            .expect("edit resolves");
        assert_eq!(
            with_schema.value,
            ResolvedValue::Scalar(array_value(&[5, 6, 7])),
            "the edit composes over the schema fallback, never over the blocked [1, 2]"
        );
    }

    #[test]
    fn strongest_array_block_resolves_to_schema_fallback() {
        let (stage, store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(array_value(&[1, 2])),
        ]);

        assert_eq!(
            stage.resolve_property_path(PropertyPath::new(prim, field)),
            None
        );
        let with_schema = stage
            .resolve_value_with_schema(prim, field, &store)
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
        let resolve = |stage: &Stage, store, prim, field| {
            [InterpolationType::Held, InterpolationType::Linear].map(|interp| {
                stage
                    .resolve_value_at_time_with_schema(prim, field, 1.0, interp, store)
                    .map(|resolved| resolved.value)
            })
        };
        let (stage, store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, &store, prim, field),
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

        let (stage, store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, &store, prim, field),
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

    /// `timecode` values read through a reference's offset (`offset = 5`,
    /// `scale = 2`) are in stage time wherever they are: an attribute
    /// default, time samples, prim metadata and property metadata, and the
    /// explanations report the same values.
    #[test]
    fn timecode_values_are_read_in_stage_time() {
        use crate::{InMemoryStore, Layer, PrimSpec, Reference, stage::StageOptions};

        let mut store = InMemoryStore::default();
        let (tree, asset) = (store.path("/Tree"), store.path("/Asset"));
        let (bloom, cues, custom_data) = (
            store.tokens.intern("bloom"),
            store.tokens.intern("cues"),
            store.tokens.intern("customData"),
        );
        let mut root = Layer::new(LayerId(1));
        let mut reference = Reference::new(LayerId(2), asset);
        reference.asset = Some("./asset.usda".into());
        reference.layer_offset = LayerOffset {
            offset: 5.0,
            scale: 2.0,
        };
        root.insert_prim(tree, PrimSpec::def().with_reference(reference));
        store.insert_layer(root);
        let timecode = PropertyType::new("timecode", false, Value::TimeCode(0.0));
        let timecodes = PropertyType::new("timecode", true, Value::TimeCode(0.0));
        let mut layer = Layer::new(LayerId(2));
        layer.insert_prim(
            asset,
            PrimSpec::def()
                .with_field(
                    custom_data,
                    Value::Dictionary(vec![("budded".into(), Value::TimeCode(2.0))]),
                )
                .with_property(
                    bloom,
                    PropertySpec::typed_attribute(timecode)
                        .with_default(Value::TimeCode(4.0))
                        .with_metadata(
                            custom_data,
                            Value::Dictionary(vec![("peak".into(), Value::TimeCode(6.0))]),
                        ),
                )
                .with_property(
                    cues,
                    PropertySpec::typed_attribute(timecodes).with_time_samples(vec![
                        (0.0, Value::Array(vec![Value::TimeCode(1.0)])),
                        (10.0, Value::Array(vec![Value::TimeCode(3.0)])),
                    ]),
                ),
        );
        store.insert_layer(layer);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

        let bloom = PropertyPath::new(tree, bloom);
        let default = Some(ResolvedValue::Scalar(Value::TimeCode(13.0)));
        assert_eq!(stage.resolve_property_path(bloom).map(|r| r.value), default);
        assert_eq!(
            stage.explain_property_value(bloom).and_then(|e| e.value),
            default
        );
        assert_eq!(
            stage
                .resolve_property_metadata(tree, bloom.property(), custom_data)
                .map(|r| r.value),
            Some(ResolvedValue::Dictionary(vec![(
                "peak".into(),
                Value::TimeCode(17.0)
            )]))
        );
        let budded = Some(ResolvedValue::Dictionary(vec![(
            "budded".into(),
            Value::TimeCode(9.0),
        )]));
        assert_eq!(
            stage.resolve_value(tree, custom_data).map(|r| r.value),
            budded
        );
        assert_eq!(
            stage.explain_value(tree, custom_data).and_then(|e| e.value),
            budded
        );

        // The sample at layer time 10 is at stage time 25.
        let cues = PropertyPath::new(tree, cues);
        let at_25 = Some(Value::Array(vec![Value::TimeCode(11.0)]));
        assert_eq!(
            stage
                .resolve_property_path_at_time(cues, 25.0, InterpolationType::Held)
                .map(|r| r.value),
            at_25
        );
        assert_eq!(
            stage
                .explain_property_value_at_time(cues, 25.0, InterpolationType::Held)
                .and_then(|e| e.value),
            at_25
        );
    }
}
