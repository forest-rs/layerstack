// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage facade and value resolution.
//!
//! Spec: AOUSD Core §11–§12 (stage population and value resolution).

mod authored_targets;
mod authoring;
mod dangling;
mod explain;
pub use authoring::{
    AuthoringInspection, AuthoringInspectionError, AuthoringQuery, AuthoringSource,
    AuthoringSourceKind, AuthoringSpan, AuthoringTimes, AuthoringVerdict, AuthoringWrite,
    CurrentSource, SampleBracket, SourceComposability, SourceStanding, SpanTimes,
};
mod objects;
mod relationships;
mod targets;
mod view;
pub use authored_targets::{
    AuthoredTarget, AuthoredTargets, LayerTargets, TargetListPosition, TargetRead, UnreadReason,
    UnreadTargetSpec,
};
pub use dangling::{
    DanglingByName, DanglingCounts, DanglingKind, DanglingTarget, DanglingTargetReport,
};
pub use objects::{
    Attribute, AttributeQuery, AttributeQueryWork, Object, Prim, PrimSnapshot, Property,
    Relationship,
};
pub use relationships::{RelationshipQuery, RelationshipQueryWork};
pub use targets::{DropReason, DroppedTarget, TargetOpinion, TargetsExplanation};
pub use view::{FromValue, NameArg, PrimPathArg, PropertyPathArg, StageView};
mod traversal;
pub use traversal::{PrimPredicate, PrimRange, PrimStatus, PrimVisit, PrimVisits};
pub(crate) mod metadata;
pub use metadata::StageMetadataError;
mod loading;
pub use loading::{LayerMuteError, LoadPolicy, PayloadLoadRules, PayloadRule};
pub(crate) mod preparation;
mod prototypes;
pub use prototypes::{CompositionStorage, CompositionWork, Prototype, PrototypeId, PrototypePrim};
pub(crate) use prototypes::{InstanceIndexKey, InstanceIndexKeys};
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
use core::hash::Hash;

use hashbrown::{HashMap, HashSet};

use invalidation::InvalidationGraph;

pub(crate) use crate::ident::dictionary_cmp;
use crate::prim_indexing::Boundaries;
use crate::variant_fallbacks::VariantFallbacks;
use crate::{
    composition_error::{CompositionError, order_errors},
    dependency_map::{ArcDependency, CompositionDeps},
    doc::{
        FieldValue, InterpolationType, LayerId, LayerStore, Specifier, Value,
        combine_dictionary_chain,
    },
    interner::TokenId,
    listop::{ListOp, resolve_list_chain},
    path::{Path, PathId, PropertyPath, TargetPath},
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
    mapped_type_name: Option<TokenId>,
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

impl Resolved<ResolvedValue> {
    /// The value of a scalar or dictionary, as a [`Value`]; `None` for a
    /// list.
    #[must_use]
    pub fn into_value(self) -> Option<Resolved<Value>> {
        let value = match self.value {
            ResolvedValue::Scalar(value) => value,
            ResolvedValue::Dictionary(entries) => Value::Dictionary(entries),
            ResolvedValue::TokenList(_)
            | ResolvedValue::PathList(_)
            | ResolvedValue::ValueList(_) => {
                return None;
            }
        };
        Some(Resolved {
            value,
            provenance: self.provenance,
        })
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
    fn chain<'a, T: Clone + Eq + Hash + 'a>(
        &mut self,
        values: impl Iterator<Item = &'a FieldValue>,
        pick: impl Fn(&'a FieldValue) -> Option<&'a ListOp<T>>,
    ) -> Vec<T>;
}

/// The resolution [`ListChainer`]: chains without reporting.
struct LeanLists;

impl ListChainer for LeanLists {
    #[inline(always)]
    fn chain<'a, T: Clone + Eq + Hash + 'a>(
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
/// See [`Property::declaration`].
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
#[doc(alias = "UsdStagePopulationMask")]
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
    /// the handles resolve ([`Attribute::get`], [`Prim::definition`]).
    /// `None`, the default, reads no schema: every prim is typeless and
    /// defines no property.
    ///
    /// The registry must be built on the tokens of the store the stage is
    /// composed from ([`SchemaRegistry::belongs_to`]); composition does not
    /// use one built on another interner and reports
    /// [`crate::CompositionError::ForeignSchemaRegistry`]. Composition itself
    /// never reads schemas.
    ///
    /// Spec: AOUSD Core §13 (schemas), §13.3.2.3 (the prim definition).
    pub schemas: Option<Arc<SchemaRegistry>>,
    /// The most threads composition indexes prims on, the calling thread
    /// included. `0` and `1`, the default, compose on the calling thread;
    /// above that, composition starts scoped worker threads for as long as
    /// it runs, and reads the store from all of them. Without the `std`
    /// feature composition always runs on the calling thread. A host that
    /// opens stages from storage may read their layers on as many threads
    /// too, as `layerstack_io::StageDocument` does: its loader and the
    /// readers it runs share one budget of `threads`
    /// ([`crate::parallel::Threads`]).
    ///
    /// The composed stage is the same for every count: its prims, values
    /// and the order of its composition errors. Only the order in which
    /// paths new to the store are interned varies, which no result is
    /// ordered by ([`crate::PathId`]).
    ///
    /// OpenUSD composes on as many threads as `WorkGetConcurrencyLimit`,
    /// every core unless the host lowers it.
    pub threads: usize,
    /// The dynamic file formats payloads compose arguments for: a payload
    /// to an asset with one of their extensions targets the layer its
    /// composed arguments name (see [`crate::dynamic_format`]). Empty by
    /// default. `layerstack_io::StageDocument` adds the dynamic sides of
    /// the file formats it reads.
    ///
    /// OpenUSD finds them among the registered `SdfFileFormat` plugins
    /// (`_GetDynamicFileFormat` in `pxr/usd/pcp/primIndex.cpp`).
    pub dynamic_formats: crate::dynamic_format::DynamicFormats,
}

/// A composed stage: read-only facade over composition results.
///
/// Build a `Stage` with [`Stage::compose`], bind it to its store with
/// [`Stage::view`], and read prims, attributes and relationships through
/// the handles a [`StageView`] hands out ([`Prim`], [`Attribute`],
/// [`Relationship`]). The stage itself answers stage-wide questions: its
/// root layer and metadata, composition errors and asset requests,
/// traversal ([`Stage::traverse`], [`Stage::prim_range`]), dependencies,
/// prototypes, flattening and reports.
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
/// let sphere = stage.view(&store).prim("/Sphere").unwrap();
///
/// let resolved = sphere.metadata("color").and_then(|r| r.into_value()).unwrap();
/// assert_eq!(resolved.value, Value::string("red"));
/// ```
#[derive(Debug)]
#[doc(alias = "UsdStage")]
pub struct Stage {
    options: StageOptions,
    store_identity: Option<crate::StoreIdentity>,
    loadable: HashSet<PathId>,
    local_layers: Vec<LayerId>,
    used_layers: alloc::collections::BTreeSet<LayerId>,
    used_layer_sites: HashMap<PathId, HashSet<LayerId>>,
    used_layer_counts: HashMap<LayerId, usize>,
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
    asset_requests: Vec<crate::AssetRequest>,
    /// The prims composed as instances, whose descendants hold only the
    /// opinions of the instance's own arcs.
    instances: HashSet<PathId>,
    /// The variant fallbacks the stage was composed with
    /// ([`StageOptions::variant_fallbacks`]).
    variant_fallbacks: VariantFallbacks,
    /// The schemas the stage was composed with ([`StageOptions::schemas`]).
    schemas: Option<Arc<SchemaRegistry>>,
    type_infos: HashMap<SchemaIdentity, Weak<PrimTypeInfo>>,
    /// For a partial composition a live stage merges, the whole child
    /// list of each boundary parent it was asked for and composed, in
    /// composition order with inactive children still in place.
    /// [`Self::merge_local_subtrees`] consumes it; every other composition
    /// leaves it empty, so a stage never keeps a stale list.
    boundary_children: HashMap<PathId, Vec<PathId>>,
    /// What a live stage retains for preparing namespace edits against
    /// this snapshot ([`preparation`]); `None` for a stage composed on
    /// its own.
    preparation: Option<alloc::boxed::Box<preparation::PreparationIndex>>,
}

/// How [`Stage::merge_local_subtrees`] ordered the boundary parents'
/// child lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BoundaryOrders {
    /// Lists taken whole from the partial composition, which composed the
    /// parent's child names once.
    pub(crate) composed: usize,
    /// Lists the partial composition did not index the parent for, ordered
    /// by composing the parent's child names again.
    pub(crate) reordered: usize,
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
        options: StageOptions,
        exact_mask: bool,
    ) -> Self {
        let child_index = crate::prim_indexing::ChildIndex::default();
        Self::compose_retained(
            store,
            root,
            options,
            exact_mask,
            &Boundaries::default(),
            &child_index,
        )
    }

    /// Composes as [`Self::compose`] does, reading the child specs of layers
    /// that do not list them from `child_index`, which a live stage keeps
    /// across compositions, and keeping the whole child list of each of
    /// `boundaries` it composes ([`Self::boundary_children`]).
    pub(crate) fn compose_retained(
        store: &mut dyn LayerStore,
        root: LayerId,
        options: StageOptions,
        exact_mask: bool,
        boundaries: &Boundaries,
        child_index: &crate::prim_indexing::ChildIndex,
    ) -> Self {
        Self::compose_retained_with(
            store,
            root,
            options,
            exact_mask,
            boundaries,
            child_index,
            true,
        )
    }

    /// Composes the whole stage as [`Self::compose_retained`] does for a live
    /// stage, which keeps the dependency maps but for the arc metadata
    /// ([`crate::dependency_map::CompositionDeps::arcs`]): its edits read the
    /// arcs of the partial compositions they merge instead.
    pub(crate) fn compose_live(
        store: &mut dyn LayerStore,
        root: LayerId,
        options: StageOptions,
        child_index: &crate::prim_indexing::ChildIndex,
    ) -> Self {
        Self::compose_retained_with(
            store,
            root,
            options,
            false,
            &Boundaries::default(),
            child_index,
            false,
        )
    }

    /// [`Self::compose_retained`], building the arc metadata of the
    /// dependency maps only when `arc_metadata`.
    fn compose_retained_with(
        store: &mut dyn LayerStore,
        root: LayerId,
        mut options: StageOptions,
        exact_mask: bool,
        boundaries: &Boundaries,
        child_index: &crate::prim_indexing::ChildIndex,
        arc_metadata: bool,
    ) -> Self {
        options.muted_layers.remove(&root);
        let captured = options.clone();
        let foreign_schemas = Self::drop_foreign_schemas(&mut options, store);
        let schemas = options.schemas.clone();
        let mut controlled = loading::ControlledStore {
            inner: store,
            root,
            muted: &captured.muted_layers,
        };
        let store: &mut dyn LayerStore = &mut controlled;
        let crate::compose::Composed {
            mut stage,
            instance_keys,
            dependencies,
        } = crate::compose::compose_stage_retained(
            store,
            root,
            options,
            None,
            exact_mask,
            boundaries,
            child_index,
        );
        stage.options = captured.clone();
        stage.store_identity = Some(store.identity());
        stage.root_layer = Some(root);
        stage.schemas = schemas;
        if foreign_schemas {
            // First: an error in no layer stack, of the first kind.
            stage
                .errors
                .insert(0, CompositionError::ForeignSchemaRegistry);
        }
        let dependencies = dependencies.map(|records| records.with_arc_metadata(arc_metadata));
        stage.prepare(store, instance_keys, dependencies);
        stage
    }

    /// Prepares the stage's type info, value clips and prototype inventory,
    /// building its dependency maps from `dependencies` meanwhile, on a
    /// thread the stage's budget spares ([`StageOptions::threads`]), when
    /// they are many enough to build on threads at all
    /// ([`DependencyRecords::builds_on_threads`]): the maps are built from
    /// what composition recorded alone, and preparing reads none of them.
    ///
    /// [`DependencyRecords::builds_on_threads`]: crate::dependency_map::DependencyRecords::builds_on_threads
    fn prepare(
        &mut self,
        store: &mut dyn LayerStore,
        instance_keys: InstanceIndexKeys,
        dependencies: Option<crate::dependency_map::DependencyRecords>,
    ) {
        let threaded = dependencies
            .as_ref()
            .is_some_and(crate::dependency_map::DependencyRecords::builds_on_threads);
        let threads = crate::parallel::Threads::new(self.options.threads);
        if threaded && threads.limit() > 1 {
            let ((), deps) = crate::parallel::join(
                &threads,
                || {
                    self.prepare_type_info(store);
                    self.prepare_clips(store);
                    self.prepare_prototypes(store, instance_keys);
                },
                || dependencies.map(|records| records.build(&threads)),
            );
            self.deps = deps;
        } else {
            // In turn, the maps first, as composition built them before:
            // the first edit after a one-thread open reads them faster when
            // they are allocated next to what composition built.
            self.deps = dependencies.map(|records| records.build(&threads));
            self.prepare_type_info(store);
            self.prepare_clips(store);
            self.prepare_prototypes(store, instance_keys);
        }
        self.record_clip_dependencies();
    }

    /// Removes a schema registry built on another token interner than the
    /// store's from `options` ([`SchemaRegistry::belongs_to`]), returning
    /// whether it did, so composition never reads its ids as the store's.
    fn drop_foreign_schemas(options: &mut StageOptions, store: &dyn LayerStore) -> bool {
        let foreign = options
            .schemas
            .as_deref()
            .is_some_and(|schemas| !schemas.belongs_to(store.tokens()));
        if foreign {
            options.schemas = None;
        }
        foreign
    }

    /// Composes exactly the prims `paths` (see
    /// [`crate::compose::compose_stage_with_paths`]), keeping the whole child
    /// list of each of `boundaries` it composes.
    pub(crate) fn compose_local_paths(
        store: &mut dyn LayerStore,
        root: LayerId,
        options: StageOptions,
        paths: alloc::collections::BTreeSet<PathId>,
        boundaries: &Boundaries,
        child_index: &crate::prim_indexing::ChildIndex,
    ) -> Self {
        let mut options = options;
        let captured = options.clone();
        // A partial composition's errors in no layer stack are not merged;
        // the full composition reported the foreign registry.
        Self::drop_foreign_schemas(&mut options, store);
        let schemas = options.schemas.clone();
        let crate::compose::Composed {
            mut stage,
            instance_keys,
            dependencies,
        } = crate::compose::compose_stage_with_paths(
            store,
            root,
            options,
            Some(paths),
            boundaries,
            child_index,
        );
        stage.options = captured;
        stage.store_identity = Some(store.identity());
        stage.root_layer = Some(root);
        stage.schemas = schemas;
        stage.prepare(store, instance_keys, dependencies);
        stage
    }

    /// The prim index of `prim`, if the stage holds it.
    #[cfg(test)]
    pub(crate) fn index_of(&self, prim: PathId) -> Option<&PrimIndex> {
        self.prims.get(&prim)
    }

    /// Whether this partial composition composed the whole child list of
    /// the boundary parent `parent`.
    pub(crate) fn has_boundary_children(&self, parent: PathId) -> bool {
        self.boundary_children.contains_key(&parent)
    }

    /// Replaces only complete local subtrees and their boundary child lists.
    /// Supporting ancestors are not merged.
    ///
    /// A boundary parent's children are the whole child list `partial`
    /// composed for it, as a full composition lists them, less the prims
    /// this stage does not hold. For a parent `partial` did not index, the
    /// list `boundary_children` gives is ordered by composing the parent's
    /// child names again against its retained index; a caller gives no
    /// list for a parent [`Self::has_boundary_children`] reports.
    pub(crate) fn merge_local_subtrees(
        &mut self,
        store: &dyn LayerStore,
        mut partial: Self,
        affected: &[PathId],
        hierarchy: &[PathId],
        boundary_children: Vec<(PathId, Vec<PathId>)>,
        composed_roots: &[PathId],
        child_index: &crate::prim_indexing::ChildIndex,
    ) -> BoundaryOrders {
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
        let mut composed = core::mem::take(&mut partial.boundary_children);
        self.merge_prims_from(store, partial, affected, composed_roots);
        let mut orders = BoundaryOrders::default();
        for (parent, given) in boundary_children {
            let Some(index) = self.prims.get(&parent) else {
                self.children.remove(&parent);
                continue;
            };
            let mut children = match composed.remove(&parent) {
                Some(children) => {
                    orders.composed += 1;
                    children
                }
                None => {
                    let mut children = given;
                    if !children.is_empty() {
                        orders.reordered += 1;
                        crate::prim_indexing::order_children(
                            store,
                            index,
                            child_index,
                            &mut children,
                        );
                    }
                    children
                }
            };
            if children.is_empty() {
                self.children.remove(&parent);
                self.inactive_children.remove(&parent);
                continue;
            }
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
        self.order_errors(store);
        self.prepare_prototypes(store, InstanceIndexKeys::new());
        orders
    }

    /// Resolve schema identity once with the composed snapshot. Ordinary value
    /// refreshes retain it; partial composition replaces affected identities.
    fn prepare_type_info(&mut self, store: &mut dyn LayerStore) {
        let empty = Arc::new(PrimDefinition::default());
        let fallback_types = self.fallback_type_map(store);
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
        let any_type = store.tokens().lookup("__AnyType__");
        // The info of the prim before: prims of one type often come
        // together, and their identity is then compared, not hashed.
        let mut last: Option<Arc<PrimTypeInfo>> = None;
        for (&path, index) in &mut self.prims {
            let type_name = index
                .type_name
                .unwrap_or_else(|| Self::source_type_name(index, store, any_type));
            let identity = SchemaIdentity {
                type_name,
                mapped_type_name: type_name.and_then(|name| fallback_types.get(&name).copied()),
                applied: applied.remove(&path).unwrap_or_default(),
            };
            if let Some(info) = last.as_ref().filter(|info| info.identity == identity) {
                index.type_info = Some(Arc::clone(info));
                continue;
            }
            let info = self
                .type_infos
                .get(&identity)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let schema_type = identity.mapped_type_name.or(identity.type_name);
                    let definition = match self.schemas.as_deref() {
                        None => empty.clone(),
                        Some(schemas) if identity.applied.is_empty() => schemas
                            .shared_typed(schema_type)
                            .unwrap_or_else(|| empty.clone()),
                        Some(schemas) => {
                            schemas.intern_instance_names(&identity.applied, store.tokens_mut());
                            Arc::new(schemas.prim_definition(
                                schema_type,
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
            index.type_info = Some(Arc::clone(&info));
            last = Some(info);
        }
    }

    // AOUSD Core §13.3.2.3; OpenUSD
    // Usd_PrimTypeInfoCache::ComputeInvalidPrimTypeToFallbackMap.
    // Candidates are direct substitutions, never recursively chased.
    fn fallback_type_map(&self, store: &dyn LayerStore) -> HashMap<TokenId, TokenId> {
        let Some(schemas) = self.schemas() else {
            return HashMap::new();
        };
        let concrete = |name| {
            schemas
                .schema(name)
                .is_some_and(|s| s.kind == crate::SchemaKind::ConcreteTyped)
        };
        let Some(key) = store.tokens().lookup("fallbackPrimTypes") else {
            return HashMap::new();
        };
        let Some(Value::Dictionary(entries)) = self.layer_metadata(key, store) else {
            return HashMap::new();
        };
        entries
            .into_iter()
            .filter_map(|(name, candidates)| {
                let name = store.tokens().lookup(&name)?;
                if concrete(name) {
                    return None;
                }
                let candidates = candidates.array_ref()?;
                // Sdf's field is a token array, not a string array.
                if candidates
                    .iter()
                    .any(|v| !matches!(v.as_ref(), Value::Token(_)))
                {
                    return None;
                }
                candidates.iter().find_map(|value| match value.as_ref() {
                    Value::Token(candidate) if concrete(*candidate) => Some((name, *candidate)),
                    _ => None,
                })
            })
            .collect()
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
    #[doc(alias = "UsdStage::GetRootLayer")]
    #[doc(alias = "GetRootLayer")]
    pub fn root_layer(&self) -> Option<LayerId> {
        self.root_layer
    }

    /// Reads session/root metadata, using the field's fallback when absent:
    /// its registered default, else its type's (`upAxis` reads `Y`, a
    /// plugin's `int` field without a default `0`). Sublayer metadata does
    /// not participate. Blocks and incompatible field representations do
    /// not resolve to a fallback.
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
        // The field's fallback, which an unauthored layer field reads as
        // (`UsdStage::GetMetadata`): built in or registered, for layers.
        let tokens = store.tokens();
        let fallback = crate::MetadataField::lookup(tokens.resolve(key), self.schemas(), tokens)
            .filter(|field| field.applies_to(crate::MetadataTarget::Layer))
            .and_then(|field| match field.fallback(tokens)? {
                FieldValue::Value(value) => Some(value),
                _ => None,
            });
        if dictionaries.is_empty() {
            return fallback;
        }
        if let Some(Value::Dictionary(entries)) = &fallback {
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
    }

    /// Records that each prim reading value clips depends on the layers of
    /// its clip sites ([`Self::prepare_clips`]).
    fn record_clip_dependencies(&mut self) {
        if let Some(deps) = &mut self.deps {
            for prim in self.prims.keys().copied() {
                for (layer, _) in self.clips.source_sites(prim) {
                    deps.add_layer_opinion(layer, prim);
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
    /// Plans residency for one numeric clip query without I/O or payload decoding.
    /// Replan after loading pending requests and synchronizing the stage. Explicit
    /// manifests gate property eligibility; automatic manifests and unresolved
    /// templates require broader discovery. `max_demands` limits returned uses,
    /// not metadata preparation work. A complete dense clip source masks weaker
    /// sets; stronger sparse or unresolved sets retain conservative discovery
    /// until their source composition can be established. Clips hold time
    /// samples only, so at [`Time::Default`] no clip is consulted and the
    /// plan is empty. AOUSD Core §12.3, §12.5.
    #[must_use]
    pub fn plan_clip_query(
        &self,
        property: PropertyPath,
        time: Time,
        max_demands: usize,
    ) -> crate::value_clips::ClipQueryPlan {
        let Time::At {
            code,
            interpolation,
        } = time
        else {
            return crate::value_clips::ClipQueryPlan::empty(property, time);
        };
        self.clips.query_plan(
            property,
            code,
            interpolation,
            max_demands,
            self.prims.get(&property.prim_path()),
        )
    }

    /// Invalid or unsupported clip definitions found during preparation.
    #[must_use]
    pub fn clip_issues(&self) -> &[crate::value_clips::ClipIssue] {
        self.clips.issues()
    }

    pub(crate) fn clip_layers(&self) -> Vec<LayerId> {
        self.clips.layers()
    }

    /// The layers the dynamic payloads of the composed prims target,
    /// which the store binds by identifiers no authored arc spells.
    pub(crate) fn dynamic_layers(&self) -> alloc::collections::BTreeSet<LayerId> {
        self.prims
            .values()
            .filter_map(|index| index.dynamic.as_deref())
            .flat_map(|dynamic| dynamic.targets.iter().copied())
            .collect()
    }

    /// The selected clip source and its mapped bracketing samples.
    ///
    /// Describes the selected numeric source chain. Returns `None` when ordinary
    /// source selection ends before clips or no manifest declares eligibility.
    /// Under held interpolation, a stronger dense lower sample may supply the
    /// entire result even when the selected chain reaches a clip at its upper
    /// bracket. Explanations and provenance identify actual contributions.
    /// Eligible gaps retain their clip source. `None` at [`Time::Default`]:
    /// clips hold time samples only.
    #[must_use]
    pub(crate) fn property_clip_source(
        &self,
        prim: PathId,
        field: TokenId,
        time: Time,
    ) -> Option<crate::value_clips::ClipValueSource> {
        let Time::At {
            code,
            interpolation,
        } = time
        else {
            return None;
        };
        let index = self.prims.get(&prim)?;
        self.clip_time_opinions(prim, field, index, code, interpolation)
            .source
    }

    /// A selected clip evaluator's query error, without exposing weaker values.
    ///
    /// Preparation errors are in [`Self::clip_issues`]. This separate diagnostic
    /// covers invalid query times and errors encountered while evaluating a
    /// selected source. It performs no loading or mutation. `None` at
    /// [`Time::Default`], where no clip is evaluated.
    #[must_use]
    pub(crate) fn property_clip_evaluation_error(
        &self,
        prim: PathId,
        field: TokenId,
        time: Time,
    ) -> Option<crate::value_clips::ClipEvalError> {
        let Time::At {
            code,
            interpolation,
        } = time
        else {
            return None;
        };
        let index = self.prims.get(&prim)?;
        let selected = self
            .clip_time_opinions(prim, field, index, code, interpolation)
            .selected?;
        self.clips
            .evaluation_error_for(prim, field, selected.entry, selected.query, interpolation)
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
                            || op.value.has_default_value())
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
        below: Option<LayerId>,
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
        let opinions = &opinions[self.below_start(&opinions, below)..];
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
            opinions,
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
    pub(crate) fn property_might_be_time_varying(&self, prim: PathId, field: TokenId) -> bool {
        self.property_time_info(prim, field).1
    }

    /// Sorted, unique stage-time samples of the effective property sources.
    ///
    /// Dense defaults and blocks mask weaker animation. Sparse array edits and
    /// path expressions retain weaker grids. Clip times include activations and mapping discontinuities;
    /// splines have no discrete sample times. Default-time reads ignore clips.
    #[must_use]
    pub(crate) fn property_sample_times(&self, prim: PathId, field: TokenId) -> Vec<f64> {
        self.property_time_info(prim, field).0
    }

    /// Effective property sample times within open, closed or unbounded stage-time bounds.
    ///
    /// Uses the same composed grid as [`Self::property_sample_times`], then
    /// filters it without adding evaluation times or interpolation brackets.
    /// NaN bounds and reversed intervals select no samples. Standard ranges
    /// and `(Bound<f64>, Bound<f64>)` tuples implement [`core::ops::RangeBounds`].
    /// AOUSD Core §12.3.2 (effective stage-time samples and layer offsets).
    #[must_use]
    pub(crate) fn property_sample_times_in_range(
        &self,
        prim: PathId,
        field: TokenId,
        interval: impl core::ops::RangeBounds<f64>,
    ) -> Vec<f64> {
        let mut times = self.property_sample_times(prim, field);
        times.retain(|time| interval.contains(time));
        times
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
                            shows_weaker_samples(value),
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
                    .is_some_and(|v| !shows_weaker_samples(v))
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
            store_identity: None,
            loadable: HashSet::new(),
            local_layers: Vec::new(),
            used_layers: alloc::collections::BTreeSet::new(),
            used_layer_sites: HashMap::new(),
            used_layer_counts: HashMap::new(),
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
            asset_requests: Vec::new(),
            instances: HashSet::new(),
            variant_fallbacks: VariantFallbacks::default(),
            schemas: None,
            type_infos: HashMap::new(),
            boundary_children: HashMap::new(),
            preparation: None,
        }
    }

    /// Keeps the whole child lists a partial composition composed for the
    /// boundary parents it was asked for.
    pub(crate) fn with_boundary_children(
        mut self,
        boundary_children: HashMap<PathId, Vec<PathId>>,
    ) -> Self {
        self.boundary_children = boundary_children;
        self
    }

    pub(crate) fn with_layer_inventory(
        mut self,
        local: Vec<LayerId>,
        used: alloc::collections::BTreeSet<LayerId>,
        sites: HashMap<PathId, HashSet<LayerId>>,
    ) -> Self {
        self.local_layers = local;
        self.used_layers = used;
        self.used_layer_counts.clear();
        for &layer in sites.values().flatten() {
            *self.used_layer_counts.entry(layer).or_default() += 1;
        }
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

    /// Attaches the composition errors found while building this stage,
    /// once its hierarchy is complete.
    pub(crate) fn with_composition_errors(
        mut self,
        store: &dyn LayerStore,
        errors: Vec<CompositionError>,
    ) -> Self {
        self.errors = errors;
        self.order_errors(store);
        self
    }

    /// Attaches the assets of deferred arcs composition asked for.
    pub(crate) fn with_asset_requests(mut self, mut requests: Vec<crate::AssetRequest>) -> Self {
        sort_asset_requests(&mut requests);
        self.asset_requests = requests;
        self
    }

    /// Puts the composition errors in the order
    /// [`composition_errors`](Self::composition_errors) reports them, by
    /// each prim's place in a traversal of every prim
    /// ([`all_children_of`](Self::all_children_of)).
    fn order_errors(&mut self, store: &dyn LayerStore) {
        if self.errors.len() < 2 {
            return;
        }
        let mut position = HashMap::new();
        let mut pending: Vec<PathId> = store.paths().lookup(&Path::root()).into_iter().collect();
        while let Some(prim) = pending.pop() {
            position.insert(prim, position.len());
            if let Some(children) = self.all_children_of(prim) {
                pending.extend(children.iter().rev());
            }
        }
        order_errors(&mut self.errors, &position, store.paths(), store.tokens());
    }

    /// Returns the composition errors found while composing this stage: the
    /// errors in layer stacks first, then each composed prim's in namespace
    /// order, each group by kind and then in the order found. A stage
    /// recomposed in part reports them in the same order as one composed
    /// anew.
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

    /// Returns the assets of the deferred references and payloads
    /// ([`Reference::deferred`](crate::Reference::deferred)) composition
    /// followed but the store does not resolve yet, by anchor layer, then
    /// asset path, then prim, then arc.
    ///
    /// Each such arc contributes nothing yet. A host loads the assets these
    /// name, records them in the store (for example
    /// [`InMemoryStore::insert_asset_layer`](crate::InMemoryStore::insert_asset_layer))
    /// and composes again, until none is left: the layers it loads may
    /// author deferred arcs of their own. Arcs composition does not follow,
    /// such as those in variant branches no selection names, ask for
    /// nothing.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads),
    /// §10.3.2.5 (only the selected variant contributes).
    #[must_use]
    pub fn asset_requests(&self) -> &[crate::AssetRequest] {
        &self.asset_requests
    }

    /// What the arguments of the payloads to dynamic file formats that the
    /// prim index of `prim` follows were composed from: the fields and
    /// attributes read, and each format's dependency data. `None` when the
    /// prim is not composed or its index composes no dynamic arguments; the
    /// payloads of its ancestors belong to their own indexes.
    ///
    /// See [`crate::dynamic_format`]. OpenUSD:
    /// `PcpCache::GetDynamicFileFormatArgumentDependencyData`.
    #[must_use]
    pub(crate) fn dynamic_dependency(&self, prim: PathId) -> Option<&crate::DynamicDependency> {
        self.prims.get(&prim)?.dynamic.as_deref()
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
    ///
    /// The layers a recomposed prim uses are replaced by the partial
    /// composition's when it lies beneath `composed_roots`, the subtrees
    /// the partial composition composed in full; a prim above them was
    /// composed only as far as the mask reaches, so its new uses are added
    /// to its existing ones.
    pub(crate) fn merge_prims_from(
        &mut self,
        store: &dyn LayerStore,
        mut partial: Self,
        recomposed: &[PathId],
        composed_roots: &[PathId],
    ) {
        let roots: HashSet<PathId> = composed_roots.iter().copied().collect();
        let complete = |path: PathId| {
            let mut at = Some(path);
            while let Some(id) = at {
                if roots.contains(&id) {
                    return true;
                }
                at = store.paths().parent(id);
            }
            false
        };
        for path in recomposed.iter().filter(|path| complete(**path)) {
            if let Some(layers) = self.used_layer_sites.remove(path) {
                for layer in layers {
                    let count = self.used_layer_counts.get_mut(&layer).expect("counted use");
                    *count -= 1;
                    if *count == 0 {
                        self.used_layer_counts.remove(&layer);
                    }
                }
            }
            if let Some(layers) = partial.used_layer_sites.remove(path) {
                for &layer in &layers {
                    *self.used_layer_counts.entry(layer).or_default() += 1;
                }
                self.used_layer_sites.insert(*path, layers);
            }
        }
        // A masked composition also composes the ancestors of the prims it
        // recomposes, and records there the layers the arcs it follows from
        // them reach, the recomposed prims' new layers among them. Those
        // uses are kept, beside the ancestors' existing ones, whether or not
        // the ancestors' indexes are replaced.
        for (path, layers) in partial.used_layer_sites.drain() {
            let site = self.used_layer_sites.entry(path).or_default();
            for layer in layers {
                if site.insert(layer) {
                    *self.used_layer_counts.entry(layer).or_default() += 1;
                }
            }
        }
        self.used_layers = self
            .local_layers
            .iter()
            .copied()
            .chain(self.used_layer_counts.keys().copied())
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
        self.order_errors(store);
        self.asset_requests
            .retain(|request| !recomposed.contains(&request.prim));
        self.asset_requests.extend(
            partial
                .asset_requests
                .into_iter()
                .filter(|request| recomposed.contains(&request.prim)),
        );
        sort_asset_requests(&mut self.asset_requests);
        self.prepare_prototypes(store, InstanceIndexKeys::new());
    }

    /// Refreshes existing value slots without rebuilding prim graphs. The
    /// caller supplies only source dependents and verifies their generations.
    /// Ambiguous provenance takes the ordinary composition path. Preparation
    /// is atomic: no cached opinion changes unless every mapping is known.
    /// Returns the prims whose value slots changed, sorted, or `None` when
    /// the edit needs composition.
    ///
    /// Source dependents are keyed by prim path without variant context, so a
    /// dependent may compose a sibling variant branch of the edited spec.
    /// Value edits only rewrite existing properties, so a prim whose opinions
    /// from that site all name another branch cannot gain the edited one:
    /// it keeps its slots. A prim with no opinion from the site at all stays
    /// on the composition path.
    ///
    /// Spec: AOUSD Core §12.3 (attribute value resolution is distinct from
    /// prim composition). OpenUSD's `PcpChanges::DidChange` skips property-only
    /// changes in USD mode unless a dynamic file format depends on them.
    pub(crate) fn refresh_values(
        &mut self,
        store: &dyn LayerStore,
        edits: &[crate::edit::PropertyValueEdit],
        dependents: &[Vec<PathId>],
    ) -> Option<Vec<PathId>> {
        // An empty edit has no records to detach or reshare. In particular, avoid
        // revisiting every native prototype member on unchanged publication.
        if edits.is_empty() {
            return Some(Vec::new());
        }
        if edits
            .iter()
            .any(|edit| self.clips.layers().contains(&edit.layer))
        {
            return None;
        }
        let mut patches = Vec::new();
        for (edit_index, (edit, prims)) in edits.iter().zip(dependents).enumerate() {
            let authored = edit.property(store)?;
            for &prim in prims {
                let index = self.prims.get(&prim)?;
                let opinions = index.property_opinions(edit.name)?;
                let mut matched = false;
                let mut sibling_branch = false;
                for (position, opinion) in opinions.iter().enumerate() {
                    let key = &opinion.key;
                    if key.layer_id != edit.layer
                        || (key.lookup_path != edit.path.prim_path()
                            && key.spec_path.prim_path() != edit.path.prim_path())
                    {
                        continue;
                    }
                    if key.spec_path != edit.path {
                        // An unremapped locator names its authored spec
                        // exactly; differing only in variant context, it is
                        // another branch's spec. Remapped source paths need
                        // composition to recover their authored locator.
                        if key.spec_path.prim_path() != key.lookup_path {
                            return None;
                        }
                        sibling_branch = true;
                        continue;
                    }
                    if !matches!(opinion.value, OpinionValue::Property(_)) {
                        return None;
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
                if !matched && !sibling_branch {
                    return None;
                }
            }
        }
        // Retain the old Arc alongside the replacement so pointer identity
        // cannot be reused during this refresh. An edit's unmapped slots are
        // identical across placements sharing the same old snapshot; mapped
        // expression values deliberately bypass this sharing.
        let changed: HashSet<_> = patches.iter().map(|(prim, ..)| *prim).collect();
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
        self.reshare_changed_prototype_records(&changed);
        let mut changed: Vec<PathId> = changed.into_iter().collect();
        changed.sort_unstable();
        Some(changed)
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

    /// Returns the source sites in `layer` that contribute specs or
    /// opinions to `prim`, as prim paths within that layer, sorted and
    /// deduplicated: what a source index of that layer records for it.
    ///
    /// Both the lookup path and the namespace path of each provenance spec
    /// path are reported: variant-branch opinions are looked up on the variant
    /// host but authored at the child path. Over-reporting is intended; this
    /// feeds invalidation, where a missed site is a correctness bug and an
    /// extra site only costs recomposition.
    ///
    /// Only the keys of `layer` are read past their layer, so the cost is
    /// that of comparing each key's layer, not of collecting and sorting
    /// every site of the prim.
    pub(crate) fn layer_source_sites(&self, prim: PathId, layer: LayerId) -> Vec<PathId> {
        self.collect_source_sites(prim, |found, path| (found == layer).then_some(path))
    }

    /// The [`Self::layer_source_sites`] of `prim` in every layer, as
    /// `(layer, prim path within that layer)` pairs, sorted.
    #[cfg(test)]
    pub(crate) fn source_sites(&self, prim: PathId) -> Vec<(LayerId, PathId)> {
        self.collect_source_sites(prim, |layer, path| Some((layer, path)))
    }

    /// The sites `site` makes of the `(layer, prim path)` source sites of
    /// `prim` ([`Self::layer_source_sites`]), skipping those it returns
    /// `None` for, sorted and deduplicated.
    fn collect_source_sites<T: Copy + Ord>(
        &self,
        prim: PathId,
        site: impl Fn(LayerId, PathId) -> Option<T>,
    ) -> Vec<T> {
        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        let push = |sites: &mut Vec<T>, lookup: T, key: &OpinionKey| {
            sites.push(lookup);
            let spec_prim = key.spec_path.prim_path();
            if spec_prim != key.lookup_path
                && let Some(spec) = site(key.layer_id, spec_prim)
            {
                sites.push(spec);
            }
        };
        // Sorted and deduplicated rather than hashed: a prim has few sites,
        // each named by many of its keys. Its opinions are read from its
        // sources' specs, so their keys nearly always name sites the
        // sources do; only one naming another is added.
        let mut sites = Vec::with_capacity(2 * index.sources.len() + 2);
        for key in &index.sources {
            if let Some(lookup) = site(key.layer_id, key.lookup_path) {
                push(&mut sites, lookup, key);
            }
        }
        sites.sort_unstable();
        sites.dedup();
        let of_sources = sites.len();
        for opinion in &index.opinions {
            let key = &opinion.key;
            let Some(lookup) = site(key.layer_id, key.lookup_path) else {
                continue;
            };
            let named = |found: Option<T>| {
                found.is_some_and(|found| sites[..of_sources].binary_search(&found).is_ok())
            };
            if !named(Some(lookup)) || !named(site(key.layer_id, key.spec_path.prim_path())) {
                push(&mut sites, lookup, key);
            }
        }
        sites.extend(
            self.clips
                .prim_sites(prim)
                .filter_map(|(layer, path)| site(layer, path)),
        );
        sites.sort_unstable();
        sites.dedup();
        sites
    }

    /// The `(layer, prim path)` sites the dynamic payload arguments of
    /// `prim`'s index were composed from, sorted.
    pub(crate) fn dynamic_sites(&self, prim: PathId) -> Vec<(LayerId, PathId)> {
        self.dynamic_dependency(prim)
            .map(|dynamic| dynamic.sites().collect())
            .unwrap_or_default()
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

    /// Physical contributor-set sharing when dependency recording is enabled.
    /// Excludes reverse indexes, invalidation topology and allocator overhead.
    #[must_use]
    pub fn layer_dependency_storage(&self) -> Option<crate::LayerDependencyStorage> {
        self.deps.as_ref().map(|deps| deps.prim_to_layers.storage())
    }

    /// Returns prims affected by the given layer: those that receive
    /// opinions from it, and those that a reference or payload it authors
    /// reaches, which its layer offset retimes. In namespace order
    /// ([`crate::PathInterner::cmp_paths`]).
    #[must_use]
    pub fn prims_affected_by_layer(&self, layer: LayerId, store: &dyn LayerStore) -> Vec<PathId> {
        let mut prims: Vec<PathId> = self
            .deps
            .as_ref()
            .and_then(|d| d.layer_to_prims.get(&layer))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        store.paths().sort_paths(&mut prims, store.tokens());
        prims
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
    /// [`Stage::resolve_token_list`].
    #[must_use]
    #[cfg(test)]
    pub(crate) fn resolve_field(&self, prim: PathId, field: TokenId) -> Option<Resolved<Value>> {
        self.resolve_field_by(prim, field, Lookup::Metadata)
    }

    fn resolve_field_by(
        &self,
        prim: PathId,
        field: TokenId,
        lookup: Lookup,
    ) -> Option<Resolved<Value>> {
        self.resolve_value_by(prim, field, lookup)
            .and_then(Resolved::into_value)
    }

    /// Resolves a token `ListOp` prim metadata field, such as `apiSchemas`.
    #[must_use]
    pub(crate) fn resolve_token_list(
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

    /// Resolves a prim metadata field.
    ///
    /// The name-based queries (this one, [`Stage::resolve_token_list`] and
    /// [`Stage::explain_field`], behind [`Prim::metadata`] and
    /// [`Prim::metadata_opinions`]) read prim metadata only. Properties are
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
    /// `Stage::resolve_value_at_time` for numeric times.
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
    pub(crate) fn resolve_value(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
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
            .find(|opinion| opinion.value.has_default_value());

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

    /// Resolves a time-varying field on a prim at `time`.
    ///
    /// At [`Time::Default`] this is the default value
    /// ([`Stage::resolve_value`], as a [`Value`]); time samples and splines
    /// are not consulted. At a time code, opinions are visited strongest first. For each, authored time samples
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
    #[cfg(test)]
    pub(crate) fn resolve_value_at_time(
        &self,
        prim: PathId,
        field: TokenId,
        time: Time,
    ) -> Option<Resolved<Value>> {
        match time {
            Time::Default => self.resolve_field_by(prim, field, Lookup::Metadata),
            Time::At {
                code,
                interpolation,
            } => self.resolve_value_at_time_by(
                prim,
                field,
                code,
                interpolation,
                Lookup::Metadata,
                None,
            ),
        }
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
                    None,
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
    /// `Stage::resolve_value_at_time` does.
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
                            opinion.value.has_default_value()
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

    /// Resolves a metadata field of a composed property, including its schema
    /// fallback. Authored scalar values (including empty tokens) win; blocks
    /// suppress weaker opinions and fallbacks. Dictionaries combine recursively
    /// across authored opinions and the schema fallback; list ops chain.
    ///
    /// Call [`Self::resolve_authored_property_metadata`] for the former
    /// authored-only behavior. Schema-only values have no layer provenance.
    ///
    /// Spec: AOUSD Core §12.2.5–§12.2.6, §13.3.2.3–§13.3.2.4.
    #[must_use]
    pub(crate) fn resolve_property_metadata(
        &self,
        prim: PathId,
        property: TokenId,
        key: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
        let index = self.prims.get(&prim)?;
        let opinions = index.property_opinions(property).unwrap_or(&[]);
        let fallback = self
            .property_definition_ref(prim, property)
            .and_then(|p| p.metadata(key));
        self.resolve_property_metadata_with_fallback(opinions, property, key, fallback)
    }

    /// Resolves only authored property metadata, excluding schema fallbacks.
    /// Empty values are opinions; a block returns `None` without exposing weaker
    /// values. Use this for APIs that explicitly inspect authored metadata.
    ///
    /// Spec: AOUSD Core §12.2.5–§12.2.6.
    #[must_use]
    pub(crate) fn resolve_authored_property_metadata(
        &self,
        prim: PathId,
        property: TokenId,
        key: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
        let opinions = self.prims.get(&prim)?.property_opinions(property)?;
        self.resolve_property_metadata_over(opinions, property, key)
    }

    /// Authored-only resolution, also used by flattening layer opinions.
    fn resolve_property_metadata_over(
        &self,
        opinions: &[Opinion],
        property: TokenId,
        key: TokenId,
    ) -> Option<Resolved<ResolvedValue>> {
        self.resolve_property_metadata_with_fallback(opinions, property, key, None)
    }

    fn resolve_property_metadata_with_fallback(
        &self,
        opinions: &[Opinion],
        property: TokenId,
        key: TokenId,
        fallback: Option<&FieldValue>,
    ) -> Option<Resolved<ResolvedValue>> {
        // Spec: AOUSD Core §12.3.2.1 (`timecode` values in stage time).
        let opinions = stage_time::opinions_in_stage_time(opinions);
        let authored: Vec<(&Opinion, &FieldValue)> = opinions
            .iter()
            .filter_map(|op| Some((op, op.value.as_property()?.metadata(key)?)))
            .collect();
        let provenance = authored
            .first()
            .and_then(|(opinion, _)| self.provenance_for(property, opinion));
        let fields = authored.iter().map(|(_, value)| *value).chain(fallback);
        let strongest = authored.first().map(|(_, value)| *value).or(fallback)?;
        let value = match strongest {
            FieldValue::Value(Value::Blocked) => return None,
            FieldValue::Value(Value::Dictionary(_)) => {
                let dictionaries = fields
                    .map_while(|value| match value {
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
            list => resolve_field_list(list, fields)?,
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
    pub(crate) fn resolve_property_declaration(
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
    pub(crate) fn resolve_property_order(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Option<Vec<TokenId>> {
        Self::property_order_of(self.prims.get(&prim)?, store)
    }

    /// [`Self::resolve_property_order`] of the prim whose index is `index`.
    fn property_order_of(index: &PrimIndex, store: &dyn LayerStore) -> Option<Vec<TokenId>> {
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
    pub(crate) fn explain_field(&self, prim: PathId, field: TokenId) -> Option<&[Opinion]> {
        self.opinions(prim, field, Lookup::Metadata)
            .map(|(_, opinions)| opinions)
    }

    /// Resolves a concrete property path.
    #[must_use]
    pub(crate) fn resolve_property_path(
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
    #[cfg(test)]
    pub(crate) fn resolve_field_path(
        &self,
        property_path: PropertyPath,
    ) -> Option<Resolved<Value>> {
        self.resolve_field_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Resolves a target-list field via concrete [`PropertyPath`].
    #[must_use]
    pub(crate) fn resolve_target_list_path(
        &self,
        property_path: PropertyPath,
    ) -> Option<Resolved<Vec<TargetPath>>> {
        self.resolve_targets_by(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
    }

    /// Resolves a concrete property path at `time`, as
    /// `Stage::resolve_value_at_time` resolves a field: at
    /// [`Time::Default`], its default value (`Stage::resolve_field_path`).
    #[must_use]
    pub(crate) fn resolve_property_path_at_time(
        &self,
        property_path: PropertyPath,
        time: Time,
    ) -> Option<Resolved<Value>> {
        let (prim, name) = (property_path.prim_path(), property_path.property());
        match time {
            Time::Default => self.resolve_field_by(prim, name, Lookup::Property),
            Time::At {
                code,
                interpolation,
            } => self.resolve_value_at_time_by(
                prim,
                name,
                code,
                interpolation,
                Lookup::Property,
                None,
            ),
        }
    }

    /// Resolves at `time` and materializes only selected numeric endpoints
    /// (at [`Time::Default`], the selected default).
    /// A malformed selected endpoint is an error, never a weaker fallback.
    pub(crate) fn try_resolve_property_path_at_time(
        &self,
        path: PropertyPath,
        time: Time,
    ) -> Result<Option<Resolved<Value>>, crate::ArrayReadError> {
        let mut resolved = self.resolve_property_path_at_time(path, time);
        if let Some(Resolved { value, .. }) = &mut resolved {
            materialize_selected(value)?;
        }
        Ok(resolved)
    }

    /// Returns the sorted opinion stack for a concrete property path.
    #[must_use]
    pub(crate) fn explain_property_path(&self, property_path: PropertyPath) -> Option<&[Opinion]> {
        self.opinions(
            property_path.prim_path(),
            property_path.property(),
            Lookup::Property,
        )
        .map(|(_, opinions)| opinions)
    }

    /// Returns `true` if the stage contains opinions for a concrete property path.
    #[must_use]
    pub(crate) fn has_property_path(&self, property_path: PropertyPath) -> bool {
        self.explain_property_path(property_path).is_some()
    }

    /// Returns the names of the properties of `prim`: those any opinion
    /// authors and those its schemas define ([`Stage::prim_definition_ref`]),
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
    pub(crate) fn property_names(&self, prim: PathId, store: &dyn LayerStore) -> Vec<TokenId> {
        self.prims
            .get(&prim)
            .map(|index| Self::property_names_of(index, store))
            .unwrap_or_default()
    }

    /// [`Self::property_names`] of the prim whose index is `index`, for a
    /// caller that holds the index already.
    pub(crate) fn property_names_of(index: &PrimIndex, store: &dyn LayerStore) -> Vec<TokenId> {
        let mut names = Self::authored_names_of(index);
        if let Some(info) = index.type_info.as_ref() {
            let definition = &info.definition;
            let authored: HashSet<TokenId> = names.iter().copied().collect();
            names.extend(
                definition
                    .properties()
                    .iter()
                    .map(|property| property.name)
                    .filter(|name| !authored.contains(name)),
            );
        }
        Self::order_names_of(index, &mut names, store);
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
    pub(crate) fn authored_property_names(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Vec<TokenId> {
        self.prims
            .get(&prim)
            .map(|index| Self::authored_property_names_of(index, store))
            .unwrap_or_default()
    }

    /// [`Self::authored_property_names`] of the prim whose index is `index`.
    pub(crate) fn authored_property_names_of(
        index: &PrimIndex,
        store: &dyn LayerStore,
    ) -> Vec<TokenId> {
        let mut names = Self::authored_names_of(index);
        Self::order_names_of(index, &mut names, store);
        names
    }

    /// Sorts `names` in dictionary order, then applies the `reorder
    /// properties` of the prim whose index is `index`.
    fn order_names_of(index: &PrimIndex, names: &mut [TokenId], store: &dyn LayerStore) {
        sort_property_names(names, store.tokens());
        if let Some(order) = Self::property_order_of(index, store) {
            apply_property_order(&order, names);
        }
    }

    /// The names of the authored properties of `prim` inside `namespace`
    /// (`material:binding` holds `material:binding:preview`, not itself),
    /// in the order of [`Stage::property_names`]. Only the matching names
    /// are sorted.
    ///
    /// OpenUSD: `UsdPrim::GetAuthoredPropertiesInNamespace`.
    pub(crate) fn authored_property_names_in(
        index: &PrimIndex,
        namespace: &str,
        store: &dyn LayerStore,
    ) -> Vec<TokenId> {
        let namespace = namespace.trim_end_matches(':');
        let tokens = store.tokens();
        let mut names = Self::authored_names_of(index);
        names.retain(|name| {
            tokens
                .resolve(*name)
                .strip_prefix(namespace)
                .is_some_and(|rest| rest.starts_with(':') || namespace.is_empty())
        });
        Self::order_names_of(index, &mut names, store);
        names
    }

    /// The names of the properties opinions of `prim` author, unsorted.
    fn authored_names(&self, prim: PathId) -> Vec<TokenId> {
        self.prims
            .get(&prim)
            .map(Self::authored_names_of)
            .unwrap_or_default()
    }

    /// [`Self::authored_names`] of the prim whose index is `index`.
    fn authored_names_of(index: &PrimIndex) -> Vec<TokenId> {
        use crate::prim_index::FieldKey;

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
    #[doc(alias = "UsdPrim::IsInstance")]
    #[doc(alias = "IsInstance")]
    pub(crate) fn is_instance(&self, prim: PathId) -> bool {
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
    #[doc(alias = "UsdPrim::IsActive")]
    #[doc(alias = "IsActive")]
    pub(crate) fn is_active(&self, prim: PathId) -> bool {
        self.has_prim(prim) && !self.inactive.contains(&prim)
    }

    /// Whether a populated active prim has every ancestor payload included.
    /// An unloaded payload prim can remain inspectable alongside local children;
    /// neither is loaded until their required payloads are included.
    /// Spec: AOUSD Core §10.3.2.7, §11.3; OpenUSD `UsdPrim::IsLoaded`.
    #[must_use]
    #[doc(alias = "UsdPrim::IsLoaded")]
    #[doc(alias = "IsLoaded")]
    pub(crate) fn is_loaded(&self, prim: PathId, paths: &crate::PathInterner) -> bool {
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

    /// All direct populated children, including inactive roots, in composed
    /// order. Instance-proxy traversal follows the same policy as `children_of`.
    #[must_use]
    pub(crate) fn all_children_of(&self, prim: PathId) -> Option<&[PathId]> {
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
    pub(crate) fn children_of(&self, prim: PathId) -> Option<&[PathId]> {
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
    pub(crate) fn explain_prim(&self, prim: PathId) -> Option<&[OpinionKey]> {
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
    pub(crate) fn explain_prim_graph(&self, prim: PathId) -> Option<&PrimIndexGraph> {
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
    #[doc(alias = "UsdPrim::GetPrimStack")]
    #[doc(alias = "GetPrimStack")]
    pub(crate) fn prim_stack(&self, prim: PathId) -> Option<Vec<(LayerId, SpecPath)>> {
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
    pub(crate) fn variant_selections(
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
    /// variant composition selected for it: the sets each node of its prim
    /// index lists, nodes in strength order, each node's in its composed
    /// `variantSetNames` order, a set listed by several nodes once. The
    /// node of a selected variant lists the sets nested in it (a set
    /// listed only in a variant that is not selected is not one). A variant
    /// set spec no list names is not one either. A selection is the one
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
    pub(crate) fn variant_sets(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Vec<(TokenId, Option<TokenId>)> {
        let Some(index) = self.prims.get(&prim) else {
            return Vec::new();
        };
        let selections = self.variant_selections(prim, store);
        let mut sets: Vec<TokenId> = Vec::new();
        for (_, names) in crate::compose::node_variant_set_names(store, &index.sources) {
            for set in names {
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
    pub(crate) fn has_prim(&self, path: PathId) -> bool {
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
    pub(crate) fn resolve_specifier(
        &self,
        prim: PathId,
        store: &dyn LayerStore,
    ) -> Option<Specifier> {
        let index = self.prims.get(&prim)?;
        Some(Self::specifier_of(prim, index, store))
    }

    /// [`Self::resolve_specifier`] of the prim `prim` whose index is
    /// `index`, for a caller that holds the index already.
    pub(crate) fn specifier_of(
        prim: PathId,
        index: &PrimIndex,
        store: &dyn LayerStore,
    ) -> Specifier {
        let depth = store.paths().resolve(prim).depth();
        // The pseudo-root is always defining (OpenUSD `UsdPrim::GetSpecifier`).
        if depth == 0 {
            return Specifier::Def;
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
                Some(Specifier::Def) => return Specifier::Def,
                Some(Specifier::Class) if !direct_inherit(key.node) => {
                    return Specifier::Class;
                }
                Some(Specifier::Class) => resolved = Specifier::Class,
                Some(Specifier::Over) | None => {}
            }
        }

        resolved
    }

    /// Returns `true` if the prim is *defined* per §11.5.
    ///
    /// A prim is defined when it and all its ancestors have resolved specifiers
    /// `def` or `class`. A defined child under an undefining parent is undefining.
    /// OpenUSD `Usd_PrimData::IsDefined`; AOUSD Core §11.5.
    #[must_use]
    #[doc(alias = "UsdPrim::IsDefined")]
    #[doc(alias = "IsDefined")]
    pub(crate) fn is_defined(&self, prim: PathId, store: &dyn LayerStore) -> bool {
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
    #[doc(alias = "UsdPrim::IsAbstract")]
    #[doc(alias = "IsAbstract")]
    pub(crate) fn is_abstract(&self, prim: PathId, store: &dyn LayerStore) -> bool {
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
    pub(crate) fn resolve_type_name(&self, prim: PathId) -> Option<TokenId> {
        self.prims
            .get(&prim)?
            .type_info
            .as_ref()?
            .identity
            .type_name
    }

    /// The strongest type name `index`'s sources author, other than the
    /// empty one and `any_type` (`__AnyType__`, when interned).
    pub(crate) fn source_type_name(
        index: &PrimIndex,
        store: &dyn LayerStore,
        any_type: Option<TokenId>,
    ) -> Option<TokenId> {
        for key in &index.sources {
            let Some(layer) = store.layer(key.layer_id) else {
                continue;
            };
            let Some(spec) = layer.source_prim_spec(key.lookup_path, &key.spec_path, store.paths())
            else {
                continue;
            };
            if let Some(tn) = spec.type_name
                && tn != TokenId::EMPTY
                && Some(tn) != any_type
            {
                return Some(tn);
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
    /// does at numeric times (`Stage::resolve_value_at_time_with_schema`).
    /// OpenUSD 26.08 resolves no value at the default time there: the named
    /// divergence `default-time-block-hides-fallback`
    /// (`docs/generic-sparse-composition.md`, "Divergences From OpenUSD").
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn resolve_value_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
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
    pub(crate) fn read_property<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.read_property_by(property, time, read, self.with_provenance, None)
    }

    /// Reads a typed value while preserving deferred numeric decode failures.
    /// Missing, blocked or incompatible values return `Ok(None)`. A malformed
    /// selected array returns an error rather than appearing absent or searching
    /// a weaker fallback. Conversion and source selection follow `read_property`.
    /// Spec: AOUSD Core §12.3 (value resolution), §13.3.2.4 (fallbacks).
    pub(crate) fn try_read_property<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Result<Option<Resolved<T>>, crate::ArrayReadError> {
        objects::check_decode(self, property, time)?;
        // Schema fallbacks are not authored property-path opinions. Preserve
        // their decode errors too, including conversions used by sparse folding.
        let error = core::cell::RefCell::new(None);
        let checked_read = |value: &Value| {
            let failure = match value {
                Value::TypedArray(array) => array.try_materialize().err(),
                Value::Deferred(_) => value.try_materialize().err(),
                _ => None,
            };
            if let Some(failure) = failure {
                *error.borrow_mut() = Some(failure.clone());
                return None;
            }
            read(value)
        };
        let result = match time {
            Time::Default => self.try_read_default(
                property.prim_path(),
                property.property(),
                checked_read,
                self.with_provenance,
                None,
            )?,
            Time::At { .. } => self.read_property(property, time, checked_read),
        };
        match error.into_inner() {
            Some(error) => Err(error),
            None => Ok(result),
        }
    }

    /// Reads the same typed composed value as `read_property`, always including
    /// the winning authored source. This opt-in query does not change the stage's
    /// provenance policy; schema fallbacks still have no authoring source.
    ///
    /// Useful for assets whose relative paths must be anchored to their authoring
    /// layer. Layer offsets, sparse composition and typed source selection use
    /// the ordinary resolver. AOUSD Core §9.4, §12.3, §13.3.2.4.
    #[must_use]
    pub(crate) fn read_property_with_provenance<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.read_property_by(property, time, read, true, None)
    }

    /// Reads the typed value `property` has beneath `layer`: from the
    /// opinions weaker than every opinion `layer` authors for it, or when
    /// `layer` authors none and belongs to the stage's own layer stack,
    /// weaker than every opinion of the stack's layers stronger than it.
    /// The schema fallback answers when nothing beneath holds a value, and
    /// the winning source is always included.
    ///
    /// This answers "what would this field be without this overlay" for one
    /// field at a time, without composing anything again. The cut is in the
    /// composed opinion stack, so the arcs `layer` authors still shape it:
    /// opinions a reference, payload, inherit or sublayer that `layer`
    /// authors brings in are weaker than `layer`'s own and count as beneath
    /// it. For a layer that authors only values this is the value the stage
    /// has with `layer` muted; for one that authors arcs it is not, and the
    /// layer's basis composed as its own stage answers structural questions.
    /// Source selection and conversion are those of [`Stage::read_property`].
    ///
    /// Spec: AOUSD Core §12.3 (value resolution in strength order).
    #[must_use]
    pub(crate) fn read_property_below<T>(
        &self,
        property: PropertyPath,
        layer: LayerId,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
    ) -> Option<Resolved<T>> {
        self.read_property_by(property, time, read, true, Some(layer))
    }

    /// Where the opinions beneath `below` start in `opinions`, strongest
    /// first (see [`Stage::read_property_below`]); `0` for no cut.
    fn below_start(&self, opinions: &[Opinion], below: Option<LayerId>) -> usize {
        let Some(layer) = below else {
            return 0;
        };
        let stack = self.layer_stack();
        if let Some(at) = stack.iter().position(|id| *id == layer) {
            // The stage's own stack opinions are its root node's; an arc
            // that brings the same layer in again is weaker and stays.
            let cut = &stack[..=at];
            opinions
                .iter()
                .rposition(|o| o.key.node == NodeId::ROOT && cut.contains(&o.key.layer_id))
                .map_or(0, |i| i + 1)
        } else {
            opinions
                .iter()
                .rposition(|o| o.key.layer_id == layer)
                .map_or(0, |i| i + 1)
        }
    }

    fn read_property_by<T>(
        &self,
        property: PropertyPath,
        time: Time,
        read: impl Fn(&Value) -> Option<T>,
        with_source: bool,
        below: Option<LayerId>,
    ) -> Option<Resolved<T>> {
        let (prim, field) = (property.prim_path(), property.property());
        match time {
            Time::Default => self.read_default(prim, field, read, with_source, below),
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
                    below,
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
    /// `Stage::resolve_value_with_schema`. Sparse array edits retain stronger
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
        below: Option<LayerId>,
    ) -> Option<Resolved<T>> {
        self.try_read_default(prim, field, read, with_source, below)
            .ok()
            .flatten()
    }

    // Keep decode failure in the actual typed selection path: a stronger
    // incompatible opinion can hide the failing array from untyped resolution.
    fn try_read_default<T>(
        &self,
        prim: PathId,
        field: TokenId,
        read: impl Fn(&Value) -> Option<T>,
        with_source: bool,
        below: Option<LayerId>,
    ) -> Result<Option<Resolved<T>>, crate::ArrayReadError> {
        let fallback = self.schema_fallback(prim, field);
        if let Some((index, opinions)) = self.opinions(prim, field, Lookup::Property) {
            let opinions = &opinions[self.below_start(opinions, below)..];
            for (position, opinion) in opinions.iter().enumerate() {
                let Some(value) = opinion.value.default_value() else {
                    continue;
                };
                // A selected value that does not decode answers the query
                // with its error, never a weaker opinion's value: a retained
                // array, or a deferred value (which `default_value` returns
                // still deferred when it fails), as at a time code.
                match value {
                    Value::TypedArray(array) => {
                        array.try_materialize().map_err(Clone::clone)?;
                    }
                    Value::Deferred(_) => {
                        value.try_materialize().map_err(Clone::clone)?;
                    }
                    _ => {}
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
                        return Ok(Some(Resolved {
                            value,
                            provenance: source.and_then(|i| {
                                self.provenance_for_if(field, &mapped[i], with_source)
                            }),
                        }));
                    }
                    return Ok(None);
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
                        _ => return Ok(None),
                    };
                    if let Some(value) = read(&value) {
                        return Ok(Some(Resolved {
                            value,
                            provenance: resolved.provenance,
                        }));
                    }
                    continue;
                }
                let mapped = stage_time::retime_value(value, opinion.layer_offset);
                if let Some(value) = read(mapped.as_ref().unwrap_or(value)) {
                    return Ok(Some(Resolved {
                        value,
                        provenance: self.provenance_for_if(field, opinion, with_source),
                    }));
                }
            }
        }
        Ok(self.read_fallback(fallback, &read))
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
    /// Like `Stage::resolve_field`, but falls back to the schema registry.
    ///
    /// Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn resolve_field_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Option<Resolved<Value>> {
        self.resolve_value_with_schema(prim, field)?.into_value()
    }

    /// Resolves a property on a prim at `time` with schema fallback; at
    /// [`Time::Default`], `Stage::resolve_field_with_schema`.
    ///
    /// At a time code, like [`Stage::resolve_property_path_at_time`], with the default-time
    /// fallback contract of `Stage::resolve_value_with_schema`:
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
    #[cfg(test)]
    pub(crate) fn resolve_value_at_time_with_schema(
        &self,
        prim: PathId,
        field: TokenId,
        time: Time,
    ) -> Option<Resolved<Value>> {
        let Time::At {
            code,
            interpolation,
        } = time
        else {
            return self.resolve_field_with_schema(prim, field);
        };
        let fallback = self.schema_fallback(prim, field);
        let (resolved, clip_selected) = self.resolve_property_at_time(
            prim,
            field,
            code,
            interpolation,
            fallback,
            None,
            self.with_provenance,
            None,
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

    /// Borrows the immutable schema definition captured when this prim was composed.
    /// Equivalent schema identities share one definition. Source schema edits
    /// become visible after recomposition, as with other composed opinions.
    /// Returns an empty definition without schemas, or `None` for an absent prim.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    #[must_use]
    pub(crate) fn prim_definition_ref(&self, prim: PathId) -> Option<&PrimDefinition> {
        Some(&self.prims.get(&prim)?.type_info.as_ref()?.definition)
    }

    /// The owned schema definition of a property. Use
    /// [`Self::property_definition_ref`] when an owned copy is unnecessary.
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4.
    #[must_use]
    pub(crate) fn property_definition(
        &self,
        prim: PathId,
        property: TokenId,
    ) -> Option<PropertyDefinition> {
        self.property_definition_ref(prim, property).cloned()
    }

    /// Borrows a property's definition from the prim's composed schema identity.
    /// Returns `None` when the prim or schema property is absent.
    ///
    /// Spec: AOUSD Core §13.3.2.3–§13.3.2.4.
    #[must_use]
    pub(crate) fn property_definition_ref(
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
    pub(crate) fn can_apply(
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
        let type_name = self
            .prims
            .get(&prim)
            .and_then(|p| p.type_info.as_ref())
            .and_then(|info| info.identity.mapped_type_name.or(info.identity.type_name));
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

/// Sorts asset requests by anchor layer, then asset path, then prim, then
/// arc.
fn sort_asset_requests(requests: &mut [crate::AssetRequest]) {
    fn key(request: &crate::AssetRequest) -> (LayerId, &str, PathId, u8) {
        (
            request.anchor,
            &request.asset_path,
            request.prim,
            request.arc.strength_rank(),
        )
    }
    requests.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
}

/// Decodes the selected value of a checked read in place: a retained numeric
/// array into its native buffer; a [`Value::Deferred`] value that failed to
/// decode into its error (value resolution has already read through one
/// that decodes).
pub(crate) fn materialize_selected(value: &mut Value) -> Result<(), crate::ArrayReadError> {
    match value {
        Value::TypedArray(array) => {
            *array = array.try_materialize().map_err(Clone::clone)?.clone();
        }
        Value::Deferred(_) => {
            *value = value.try_materialize().map_err(Clone::clone)?.clone();
        }
        _ => {}
    }
    Ok(())
}

/// Whether a sample or default holding `value` lets weaker samples show
/// through in the composed sample grid: a sparse array edit, or a path
/// expression, whose type composes over a weaker one even without a `%_`
/// (OpenUSD `VtValue::CanComposeOver`, `pxr/base/vt/value.cpp:450`, which
/// asks the type; `_SamplesInIntervalResolver` in `pxr/usd/usd/stage.cpp`
/// keeps weaker samples under a composing one).
///
/// Spec: AOUSD Core §12.3.2 (effective stage-time samples).
fn shows_weaker_samples(value: &Value) -> bool {
    value.array_edit_ref().is_some() || matches!(value, Value::PathExpression(_))
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
        let registry = Arc::new(builder.build(&store.tokens));
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
                .with_type(PropertyType::parse("int").unwrap())
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
                .resolve_field_with_schema(a, field)
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
        let registry = Arc::new(builder.build(&store.tokens));
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
        assert_eq!(live.stage().resolve_type_name(path), Some(a));
        assert_eq!(
            live.stage()
                .resolve_field_with_schema(path, field)
                .unwrap()
                .value,
            Value::Int(1)
        );
        live.notify_layer_edit(LayerId(1));
        live.recompose(&mut store);
        assert_eq!(live.stage().resolve_type_name(path), Some(b));
        assert_eq!(
            live.stage()
                .resolve_field_with_schema(path, field)
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
        over.references.make_mut().explicit = Some(vec![Reference::new(LayerId(2), b)]);
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
        spec.references.make_mut().explicit = Some(vec![Reference::new(LayerId(2), b)]);
        root.insert_prim(a, spec);
        store.insert_layer(root);
        let mut other = Layer::new(LayerId(2));
        other.insert_prim(b, PrimSpec::def().with_type_name(tree));
        store.insert_layer(other);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.resolve_type_name(a), Some(tree));
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
        PropertyType::parse("int[]").unwrap()
    }

    fn test_key(layer: LayerId, lookup_path: PathId) -> OpinionKey {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let spec_path = SpecPath::parse("/A", &tokens, &paths).expect("spec path");
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
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &tokens).expect("valid path"));
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
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &tokens).expect("valid path"));
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
                .resolve_property_path_at_time(PropertyPath::new(prim, field), Time::held(2.5))
                .expect("resolved override")
                .value,
            array_value(&[9, 2])
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(PropertyPath::new(prim, field), Time::held(3.5))
                .expect("resolved reset")
                .value,
            array_value(&[1, 2])
        );
    }

    #[test]
    fn sampled_block_hides_weaker_array_at_time() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let prim = paths.intern(Path::parse_absolute("/A", &tokens).expect("valid path"));
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
                stage.resolve_value_at_time(prim, field, Time::held(time)),
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
        let registry = builder.build(&store.tokens);

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
                PropertyType::parse("dictionary").unwrap_or_else(PropertyType::unregistered),
                Value::Dictionary(vec![
                    ("a".into(), Value::Int(1)),
                    ("b".into(), Value::Int(2)),
                    ("c".into(), Value::Int(3)),
                ]),
            ),
            (
                vec![expression("/Strong %_")],
                expression("/Fallback"),
                PropertyType::parse("pathExpression").unwrap(),
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
        let (stage, _store, prim, field) = schema_fallback_fixture_of(
            vec![
                FieldValue::Value(expression("/Strong %_")),
                FieldValue::Value(expression("%_ /Weak")),
            ],
            expression("/Fallback"),
            PropertyType::parse("pathExpression").unwrap(),
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
                .resolve_property_path_at_time(property, Time::held(1.0))
                .map(|r| r.value),
            authored
        );
        assert_eq!(
            stage
                .resolve_value_with_schema(prim, field)
                .map(|r| r.value),
            Some(ResolvedValue::Scalar(composed.clone()))
        );
        assert_eq!(
            stage
                .resolve_value_at_time_with_schema(prim, field, Time::held(1.0),)
                .map(|r| r.value),
            Some(composed.clone())
        );
        let explained = stage
            .explain_value_with_schema(prim, field, Time::Default)
            .expect("authored");
        assert_eq!(explained.value, Some(composed.clone()));
        assert!(explained.seeded_by_fallback);
        assert_eq!(explained.contributors().count(), 2);
        let explained = stage
            .explain_value_with_schema(prim, field, Time::held(1.0))
            .expect("authored");
        assert_eq!(explained.value, Some(composed));
        assert_eq!(explained.contributors().count(), 2);
    }

    /// A stage over one prim `/A` whose attribute `x` has `specs`, strongest
    /// first.
    fn property_stack_fixture(specs: Vec<PropertySpec>) -> (Stage, PathId, TokenId) {
        property_stack_fixture_in(&crate::doc::InMemoryStore::default(), specs)
    }

    /// [`property_stack_fixture`] over `store`'s interners.
    fn property_stack_fixture_in(
        store: &crate::doc::InMemoryStore,
        specs: Vec<PropertySpec>,
    ) -> (Stage, PathId, TokenId) {
        let prim = store.path("/A");
        let field = store.tokens.intern("x");
        let mut index = PrimIndex::default();
        let key = test_key(LayerId(1), prim);
        index.add_source(key.clone());
        for (strength, spec) in specs.into_iter().enumerate() {
            index.add_opinion(Opinion {
                key: OpinionKey {
                    layer_strength: u16::try_from(strength).expect("small fixture"),
                    ..key.clone()
                },
                field,
                value: spec.into(),
                layer_offset: LayerOffset::IDENTITY,
            });
        }
        let stage = Stage::from_parts(HashMap::from([(prim, index)]), HashMap::new(), false, None);
        (stage, prim, field)
    }

    /// A path expression's type composes over a weaker one, so the sample
    /// grid keeps weaker samples under a stronger expression, with or
    /// without `%_`, as OpenUSD's does (`VtValue::CanComposeOver`).
    ///
    /// Spec: AOUSD Core §12.3.2.
    #[test]
    fn path_expressions_keep_weaker_sample_grids() {
        let ty = PropertyType::parse("pathExpression").unwrap();
        let expression = |text: &str| Value::PathExpression(text.into());
        let weak = PropertySpec::typed_attribute(ty.clone()).with_time_samples(vec![
            (1.0, expression("/Weak1")),
            (3.0, expression("/Weak3")),
        ]);
        let strong_samples = PropertySpec::typed_attribute(ty.clone())
            .with_time_samples(vec![(2.0, expression("/Other"))]);
        let (stage, prim, field) = property_stack_fixture(vec![strong_samples, weak.clone()]);
        assert_eq!(
            stage.property_sample_times(prim, field),
            vec![1.0, 2.0, 3.0]
        );

        let strong_default =
            PropertySpec::typed_attribute(ty.clone()).with_default(expression("%_ /Other"));
        let (stage, prim, field) = property_stack_fixture(vec![strong_default, weak.clone()]);
        assert_eq!(stage.property_sample_times(prim, field), vec![1.0, 3.0]);
        assert!(stage.property_might_be_time_varying(prim, field));

        // A block still hides them.
        let blocked = PropertySpec::typed_attribute(ty).with_default(Value::Blocked);
        let (stage, prim, field) = property_stack_fixture(vec![blocked, weak]);
        assert!(stage.property_sample_times(prim, field).is_empty());
        assert!(!stage.property_might_be_time_varying(prim, field));
    }

    /// An animation block hides weaker time samples and splines, not their
    /// defaults, and the explanation says so (the value matrix's `absp_dbl`
    /// covers splines): the weaker default answers
    /// numeric times, as the read does (OpenUSD `UsdAttribute::BlockAnimation`;
    /// `GetResolveInfo` names the default).
    ///
    /// Spec: AOUSD Core §12.3.
    #[test]
    fn explanations_follow_animation_blocks() {
        let ty = PropertyType::parse("double").unwrap();
        let block = PropertySpec::typed_attribute(ty.clone()).with_default(Value::AnimationBlock);
        let sampled = PropertySpec::typed_attribute(ty.clone())
            .with_default(Value::Double(1.5))
            .with_time_samples(vec![(1.0, Value::Double(10.0)), (3.0, Value::Double(30.0))]);
        let (stage, prim, field) = property_stack_fixture(vec![block, sampled]);
        let property = PropertyPath::new(prim, field);
        for time in [
            Time::held(0.0),
            Time::held(2.0),
            Time::at(2.0),
            Time::held(5.0),
        ] {
            let read = stage.resolve_property_path_at_time(property, time);
            assert_eq!(read.map(|r| r.value), Some(Value::Double(1.5)));
            let explained = stage.explain_property_value(property, time).unwrap();
            assert_eq!(explained.value, Some(Value::Double(1.5)));
            assert_eq!(explained.source, ValueSource::Default);
            let contributors: Vec<_> = explained.contributors().collect();
            assert_eq!(contributors.len(), 1);
            assert_eq!(contributors[0].opinion.key.layer_strength, 1);
            assert!(
                contributors[0]
                    .samples
                    .iter()
                    .all(|sample| !sample.time.is_finite())
            );
        }
    }

    fn append_edit(value: i32) -> Value {
        Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(value)),
                index: ArrayIndex::End,
            }],
        })
    }

    /// Before the first composed sample, edits that compose over a weaker
    /// default hold their first sample, and the explanation names it as the
    /// time samples the value comes from, as OpenUSD's resolve info does.
    ///
    /// Spec: AOUSD Core §12.5.1.
    #[test]
    fn edits_held_before_their_first_sample_explain_as_samples() {
        let ty = PropertyType::parse("int[]").unwrap();
        let strong = PropertySpec::typed_attribute(ty.clone())
            .with_time_samples(vec![(2.0, append_edit(9)), (4.0, append_edit(8))]);
        let weak = PropertySpec::typed_attribute(ty).with_default(array_value(&[1, 2]));
        let (stage, prim, field) = property_stack_fixture(vec![strong, weak]);
        let property = PropertyPath::new(prim, field);
        for time in [Time::held(0.0), Time::at(1.0), Time::held(2.0)] {
            let explained = stage.explain_property_value(property, time).unwrap();
            assert_eq!(
                explained.value,
                stage
                    .resolve_property_path_at_time(property, time)
                    .map(|r| r.value)
            );
            assert!(
                matches!(
                    explained.source,
                    ValueSource::TimeSamples {
                        lower: 2.0,
                        upper: 2.0,
                        ..
                    }
                ),
                "{time:?}: {:?}",
                explained.source
            );
        }
    }

    /// A sampled block in effect says which of its samples blocks the value,
    /// before the first sample too, while a default block offers none.
    ///
    /// Spec: AOUSD Core §12.3.6.
    #[test]
    fn explanations_name_the_held_sampled_block() {
        let ty = PropertyType::parse("double").unwrap();
        let strong = PropertySpec::typed_attribute(ty.clone())
            .with_time_samples(vec![(2.0, Value::Blocked), (4.0, Value::Double(400.0))]);
        let weak = PropertySpec::typed_attribute(ty.clone())
            .with_time_samples(vec![(1.0, Value::Double(10.0)), (3.0, Value::Double(30.0))]);
        let (stage, prim, field) = property_stack_fixture(vec![strong, weak.clone()]);
        let property = PropertyPath::new(prim, field);
        for time in [Time::held(0.0), Time::held(3.0), Time::at(3.0)] {
            let explained = stage.explain_property_value(property, time).unwrap();
            assert_eq!(explained.value, None);
            assert_eq!(explained.source, ValueSource::None);
            let block = &explained.opinions[0];
            assert_eq!(block.role, OpinionRole::Block);
            assert_eq!(
                block.samples,
                vec![SampleUse {
                    time: 2.0,
                    role: OpinionRole::Block
                }]
            );
        }
        let blocked = PropertySpec::typed_attribute(ty).with_default(Value::Blocked);
        let (stage, prim, field) = property_stack_fixture(vec![blocked, weak]);
        let explained = stage
            .explain_property_value(PropertyPath::new(prim, field), Time::held(3.0))
            .unwrap();
        assert_eq!(explained.opinions[0].role, OpinionRole::Block);
        assert!(explained.opinions[0].samples.is_empty());
    }

    /// A property's metadata explanation reads as `Property::metadata` does,
    /// and names the opinion, and so the layer, that supplied each entry of
    /// a composed dictionary.
    ///
    /// Spec: AOUSD Core §12.2.5, §13.3.2.3.
    #[test]
    fn property_metadata_explanations_name_each_entry_supplier() {
        let store = crate::doc::InMemoryStore::default();
        let custom = store.tokens.intern("customData");
        let entry = |key: &str, value: Value| (Arc::<str>::from(key), value);
        let strong = PropertySpec::attribute().with_metadata(
            custom,
            Value::Dictionary(vec![entry("a", Value::String("strong".into()))]),
        );
        let weak = PropertySpec::attribute().with_metadata(
            custom,
            Value::Dictionary(vec![
                entry("a", Value::String("weak".into())),
                entry("p", Value::Asset("./weak.usda".into())),
            ]),
        );
        let (stage, prim, field) = property_stack_fixture_in(&store, vec![strong, weak]);
        let explained = stage
            .explain_property_metadata(prim, field, custom)
            .expect("authored");
        assert_eq!(
            explained.value,
            stage
                .resolve_property_metadata(prim, field, custom)
                .map(|resolved| resolved.value)
        );
        let supplied: Vec<(u16, Vec<KeyPath>)> = explained
            .contributors()
            .map(|opinion| match &opinion.role {
                OpinionRole::Contributed(Contribution::Dictionary(merge)) => {
                    (opinion.opinion.key.layer_strength, merge.supplied.clone())
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            supplied,
            vec![
                (0, vec![vec![Arc::from("a")]]),
                (1, vec![vec![Arc::from("p")]])
            ]
        );
        assert!(
            stage
                .explain_property_metadata(prim, field, store.tokens.intern("assetInfo"))
                .is_none()
        );
    }

    /// A block discards weaker authored opinions (AOUSD Core §12.3.6), but
    /// stronger sparse edits still compose over the weakest dense value that
    /// survives it, the schema fallback or the empty array (sparse-array-edits
    /// proposal, "Value Resolution").
    #[test]
    fn stronger_array_edit_over_block_materializes_over_fallback() {
        let (stage, _store, prim, field) = schema_fallback_fixture(vec![
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
            .resolve_value_with_schema(prim, field)
            .expect("edit resolves");
        assert_eq!(
            with_schema.value,
            ResolvedValue::Scalar(array_value(&[5, 6, 7])),
            "the edit composes over the schema fallback, never over the blocked [1, 2]"
        );
    }

    #[test]
    fn strongest_array_block_resolves_to_schema_fallback() {
        let (stage, _store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(array_value(&[1, 2])),
        ]);

        assert_eq!(
            stage.resolve_property_path(PropertyPath::new(prim, field)),
            None
        );
        let with_schema = stage
            .resolve_value_with_schema(prim, field)
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
        let resolve = |stage: &Stage, prim, field| {
            [InterpolationType::Held, InterpolationType::Linear].map(|interp| {
                stage
                    .resolve_value_at_time_with_schema(
                        prim,
                        field,
                        Time::At {
                            code: 1.0,
                            interpolation: interp,
                        },
                    )
                    .map(|resolved| resolved.value)
            })
        };
        let (stage, _store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(append_edit(7)),
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, prim, field),
            [Some(array_value(&[5, 6, 7])), Some(array_value(&[5, 6, 7]))],
            "the edit composes over the fallback, never over the blocked [1, 2]"
        );
        assert_eq!(
            stage
                .resolve_property_path_at_time(PropertyPath::new(prim, field), Time::held(1.0))
                .map(|resolved| resolved.value),
            Some(array_value(&[7])),
            "without a schema the edit composes over the empty array"
        );

        let (stage, _store, prim, field) = schema_fallback_fixture(vec![
            FieldValue::Value(Value::Blocked),
            FieldValue::Value(array_value(&[1, 2])),
        ]);
        assert_eq!(
            resolve(&stage, prim, field),
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
        let spec = SpecPath::from_prim_path(prim);
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
        let timecode = PropertyType::parse("timecode").unwrap();
        let timecodes = PropertyType::parse("timecode[]").unwrap();
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
            stage
                .explain_property_value(bloom, Time::Default)
                .and_then(|e| e.value),
            Some(Value::TimeCode(13.0))
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
                .resolve_property_path_at_time(cues, Time::held(25.0))
                .map(|r| r.value),
            at_25
        );
        assert_eq!(
            stage
                .explain_property_value(cues, Time::held(25.0))
                .and_then(|e| e.value),
            at_25
        );
    }
}
