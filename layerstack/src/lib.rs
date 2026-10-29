// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `layerstack` composes authored layers into a queryable scene description.
//! A stronger layer can override a field, edit a list, or select a variant while
//! preserving the weaker source data. Composition follows the
//! [OpenUSD Core specification][spec]; the value and schema APIs can also serve
//! applications outside graphics.
//!
//! [spec]: https://openusd.org/release/spec_usdcore.html
//!
//! It provides:
//!
//! - **Layer stacks** — recursive sublayers with deterministic strength ordering
//! - **Stage population** — a composed prim tree from all contributing layers
//! - **Value resolution** — scalars, [`ListOp`] chaining, recursive dictionaries,
//!   sparse array edits, time samples, splines and runtime value clips
//! - **Composition arcs** — local, inherits, variants, references, payloads,
//!   specializes, and namespace relocates (LIVERPS)
//! - **Path expressions** — sets of prim and property paths
//!   ([`PathExpression`]: globs, `//`, predicates, set operators and
//!   references to other expressions), composed as values and matched
//!   with a caller-supplied predicate library ([`path_expression`])
//! - **Variable expressions** — sublayer, reference and payload asset paths
//!   and variant selections authored as expressions, evaluated with each
//!   layer stack's `expressionVariables` ([`variable_expression`])
//! - **Incremental recomposition** — via [`LiveStage`] and the `invalidation`
//!   dependency graph
//! - **Authoring** — via [`edit`]: edit targets through any node of a
//!   prim's composition, and atomic transactions of spec edits that
//!   return their inverses and check preconditions, including composition
//!   arcs added and removed as OpenUSD's list editors do
//! - **Schemas** — prim definitions (type, applied schemas, defined
//!   properties and their fallbacks) from the [`SchemaRegistry`] a stage is
//!   composed with ([`StageOptions::schemas`], [`Prim::definition`])
//! - **Value explanations** — [`Attribute::explain`] and
//!   [`Prim::explain_metadata`] say why a value is what it is: every consulted opinion with its
//!   layer, spec, arc and layer offset, and whether it contributed a value, a
//!   sparse array edit, dictionary entries or a list edit, or was shadowed,
//!   cut off by a block or incompatible (compare OpenUSD's
//!   `UsdAttribute::GetResolveInfo`)
//!
//! # Quick start
//!
//! Add `layerstack = "0.1"` to your dependencies. This example authors a base
//! layer and a stronger local override, then reads the composed value through
//! [`Stage::view`] and asks why it is what it is:
//!
//! ```
//! use layerstack::{
//!     InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions,
//!     SublayerEntry, Time, Value,
//! };
//!
//! let mut store = InMemoryStore::default();
//! let prim = store.path("/Doc");
//! let title = store.tokens().intern("title");
//! let text = |value| {
//!     PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
//!         .with_default(Value::string(value))
//! };
//!
//! let mut base = Layer::new(LayerId(1));
//! base.insert_prim(prim, PrimSpec::def().with_property(title, text("Untitled")));
//! store.insert_layer(base);
//!
//! let mut local = Layer::new(LayerId(2));
//! local.sublayers_mut().push(SublayerEntry::new(LayerId(1)));
//! local.insert_prim(prim, PrimSpec::over().with_property(title, text("Hello")));
//! store.insert_layer(local);
//!
//! let stage = Stage::compose(&mut store, LayerId(2), StageOptions::default());
//! assert!(stage.composition_errors().is_empty());
//!
//! let view = stage.view(&store);
//! let title = view.attribute("/Doc.title").unwrap();
//! assert_eq!(title.type_name(), Some("string"));
//! assert_eq!(title.value::<String>(Time::Default).as_deref(), Some("Hello"));
//!
//! // The local layer's opinion wins; the base's is shadowed.
//! let why = title.explain(Time::Default).unwrap();
//! assert_eq!(why.contributors().map(|o| o.layer()).collect::<Vec<_>>(), [LayerId(2)]);
//! assert_eq!(why.opinions.len(), 2);
//! ```
//!
//! A [`StageView`] binds a stage to the store it was composed from: its
//! queries take paths as text or ids, and its handles ([`Prim`],
//! [`Attribute`], [`Relationship`]) read at a [`Time`] ([`Time::Default`],
//! [`Time::at`] or [`Time::held`]) and print their paths.
//!
//! Composition returns a stage even when some arcs cannot be resolved. Inspect
//! [`Stage::composition_errors`] before treating the result as complete. Enable
//! [`StageOptions::with_provenance`] to include the winning source in resolved values.
//!
//! # Key types
//!
//! | Type | Role |
//! |------|------|
//! | [`StageView`] / [`Prim`] / [`Attribute`] / [`Relationship`] | The way to read a stage: text paths, typed values at a [`Time`], types and explanations |
//! | [`Layer`] / [`PrimSpec`] | Authored layer and prim opinions |
//! | [`Stage`] | The composed snapshot: traversal, composition errors, asset requests, dependencies and stage-wide reports |
//! | [`LiveStage`] | Incremental edits, change reports, callbacks and change cursors |
//! | [`InMemoryStore`] / [`LayerStore`] | Built-in storage and a host storage interface |
//! | [`Value`] / [`FieldValue`] | Authored values and composition containers |
//! | [`Path`] / [`PropertyPath`] / [`TargetPath`] / [`SpecPath`] | Distinct prim, property, target, and source-spec paths |
//! | [`TokenInterner`] / [`PathInterner`] | Store-local token and path handles |
//! | [`SchemaRegistry`] | Schema definitions, prim definitions and fallback values |
//! | [`EditTarget`] / [`Transaction`] | Mapped, atomic authoring with preconditions and undo |
//! | [`NamespaceEdit`] | Prepared moves with explicit dependent stages and authored repairs |
//! | [`flatten_layer_stack`] | Local layer consolidation preserving external composition arcs |
//!
//! [`PathId`] and [`TokenId`] belong to their interners; they are not durable
//! identities to persist or exchange between unrelated stores.
//!
//! # Scope and compatibility
//!
//! This crate owns in-memory composition, not file I/O, geometry evaluation, or
//! material interpretation. Companion crates in the [repository][repo] provide
//! USDA and USDC readers/writers, USDZ packaging, mesh export, and generated OpenUSD
//! schema views with transform, bounds and shading queries.
//! [`layerstack_io`](https://docs.rs/layerstack_io) adds retained stage documents
//! with host-owned storage, explicit saving and reload policies. Applications supply
//! their own domain behavior.
//!
//! Assets reach composition through one of three seams, by when the host
//! answers for them:
//!
//! - at import, an [`AssetResolver`] resolves each asset path a reader meets
//!   to a layer ([`NoAssets`] resolves none);
//! - at composition, the store answers [`LayerStore::asset_layer`] for an arc
//!   or sublayer bound to an asset path, and an asset it has no answer for
//!   yet is asked for ([`Stage::asset_requests`]);
//! - with [`layerstack_io`](https://docs.rs/layerstack_io), a host implements
//!   its `Storage` (bytes by identifier), and the document does both of the
//!   others.
//!
//! The [`asset`] module's docs name the states of an asset that is not there.
//!
//! For composition of already-ordered opinions without USD paths and arcs, see
//! [`opinionated`](https://docs.rs/opinionated). `opinionated` is also a public
//! dependency: [`ListOp`], [`ArrayIndex`], [`ArrayEditOp`] and
//! [`listop::resolve_list_chain`] are its items, so a breaking `opinionated` release is a
//! breaking release of this crate.
//!
//! OpenUSD compatibility is bounded by the implemented and tested subset. Value
//! clips are evaluated from host-loaded raw layers; inspect
//! [`Stage::clip_asset_requests`] and [`Stage::clip_issues`] for preparation gaps.
//! Feature presence is not a guarantee of full OpenUSD
//! equivalence. The repository's [conformance harness][conformance] records exact
//! ordered stack and value checks against upstream fixtures and differential tests
//! for additional behavior.
//!
//! [`LiveStage`] applies transactions or refreshes explicitly reported host edits.
//! Recomposition can be scoped to affected prims; edits outside the supported local
//! paths can require a full rebuild. Change reports expose the work performed.
//! Callbacks and independent change cursors let consumers observe reported edits;
//! changes to host storage do not notify the stage automatically.
//! [`Layer::set_change_history_budget`] and [`LiveStage::set_change_history_budget`]
//! bound authored and composed evidence independently (default: 64 batches and
//! 1 MiB each). Their `change_history_stats` methods expose retained/allocated
//! bytes and cumulative record, eviction and discard work. Oversized reports
//! still reach callbacks, but cursor replay returns [`ChangeHistoryError::Expired`].
//! That read advances the cursor; rebuild notice-dependent derived state before
//! consuming later reports. Missing authored history automatically makes source
//! synchronization conservative. Retained queries check current composition
//! records independently of cursor replay.
//!
//! [`Stage::layer_dependency_storage`] reports contributor-set sharing when
//! `with_dependencies` is enabled; [`LiveStage::layer_dependency_storage`] reports
//! its retained dependency catalog. Equal sorted layer lists share immutable
//! storage across prims and incremental batches. Counts distinguish logical
//! references from physical references and exclude reverse indexes, arc topology
//! and allocator costs. Changing a prim's contributors preserves older snapshots;
//! weak cache entries do not retain obsolete contributor payloads.
//!
//! [repo]: https://github.com/forest-rs/layerstack
//! [conformance]: https://github.com/forest-rs/layerstack/tree/main/layerstack_conformance
//!
//! # Features and Rust version
//!
//! Requires **Rust 1.89** or later. The default feature set is empty, and the crate
//! uses `no_std` with `alloc` (a global allocator is required). The optional `std`
//! feature currently adds no composition capabilities.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "std"))]
extern crate std;

// Internal maps; the public API names no hash map type.
pub(crate) use hashbrown::{HashMap, HashSet};

pub(crate) mod arcs;
pub mod array_edit;
pub mod asset;
pub mod asset_dependencies;
mod change_history;
pub mod clip_authoring;
pub(crate) mod compose;
pub(crate) mod composition_checks;
mod diagnostic;
pub mod encoded_bytes;
pub use change_history::{ChangeHistoryBudget, ChangeHistoryStats};
pub mod composition_error;
pub mod composition_limits;
mod deferred_value;
pub mod dependency_map;
pub use deferred_value::{DeferredValue, DeferredValueSource};
pub mod doc;
pub mod dynamic_format;
#[cfg_attr(
    not(feature = "unstable-internals"),
    allow(
        unreachable_pub,
        dead_code,
        reason = "public only with the `unstable-internals` feature"
    )
)]
mod entries;
mod shared_vec;
mod store_identity;
pub use store_identity::StoreIdentity;
mod value_array;
pub mod value_clips;
pub use value_array::{ArrayReadError, ArrayRef, DeferredArraySource, StringArray, TypedArray};
pub mod edit;
pub(crate) mod expression_variables;
pub mod half;
pub mod ident;
pub mod interner;
mod interning;
pub mod layer_stack;
pub mod layer_stack_flatten;
pub mod parallel;
mod sync;
mod target_translation;
pub use layer_stack_flatten::{
    FlattenedLayerStack, LayerStackFlattenAssetPath, LayerStackFlattenError,
    LayerStackFlattenReport, flatten_layer_stack,
};
pub mod listop;
pub mod path;
pub mod path_expression;
pub mod prim_index;
pub mod prim_index_graph;
pub(crate) mod prim_indexing;
pub mod property;
pub mod relocates;
pub mod schema;
pub mod sort;
pub mod sparse_writer;
pub mod spec_path;
pub mod spline;
pub mod stage;
pub mod stitch;
mod value_resolution;
pub mod value_type;
pub mod variable_expression;
pub mod variant_fallbacks;

pub mod live_stage;

pub use array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, TypedArrayEdit};
pub use asset::{
    AssetRequest, AssetResolveError, AssetResolver, AssetUse, ExpressionAssetPath, FormatArgs,
    FormatArgsError, NoAssets, PendingPrim, ResolvedAsset, expression_asset_paths,
    expression_asset_paths_for_stack, resolve_selected_deferred_arcs,
};
pub use composition_error::{
    ArcCycle, ArcCycleSite, ArcToProhibitedChild, CompositionError, ExpressionContext,
    InconsistentPropertyType, InvalidAuthoredRelocation, InvalidConflictingRelocation,
    InvalidExternalTargetPath, InvalidInstanceTargetPath, InvalidRelocationReason,
    InvalidSameTargetRelocations, InvalidSublayerAndOffsetCount, LimitExceeded, MissingLayer,
    MutedAsset, OpinionAtRelocationSource, RelocationConflict, SublayerCycle, UnresolvedAsset,
    UnresolvedDefaultPrim, UnresolvedPrimPath, UnresolvedSublayer, VariableExpressionError,
};
pub use composition_limits::{CompositionLimit, CompositionLimits};
pub use dependency_map::{ArcDependency, LayerDependencyStorage};
pub use doc::{
    AssetAvailability, FieldEntry, FieldValue, InMemoryStore, InterpolationType, Layer, LayerId,
    LayerOffset, LayerPrims, LayerPrimsIntoIter, LayerPrimsIter, LayerPrimsIterMut, LayerStore,
    MaybeSync, PrimSpec, RarePrimFields, Reference, ReferenceSource, ReferenceTarget, Relocate,
    Specifier, SublayerEntry, Value, VariantBranch, VariantBranches, VariantPrims,
    VariantSelections, VariantSelectionsIter, VariantSetSpec, VariantSets, VariantSetsIter,
    VariantSetsIterMut, VariantSpec, Variants, VariantsIter, VariantsIterMut, combine_dictionaries,
    combine_dictionary_chain,
};
#[cfg(feature = "unstable-internals")]
pub use doc::{get_field, get_field_mut, insert_field_if_absent, remove_field, set_field_vec};
pub use dynamic_format::{
    DynamicArgs, DynamicDependency, DynamicFileFormat, DynamicFormatContext,
    DynamicFormatRegistrationError, DynamicFormats, UnregisteredField,
};
pub use edit::{
    Address, Applied, Changes, EditError, EditTarget, ListPosition, NamespaceEdit, NamespaceError,
    PrimField, PrimPropertyChanges, PropertyChange, PropertyField, Rejection, Slot, Transaction,
};
pub use interner::{TokenId, TokenInterner};
pub use layer_stack::{LayerStack, LayerStackIdentifier};
pub use listop::{ListOp, SparseListOp};
pub use path::{
    Path, PathError, PathId, PathInterner, PropertyPath, PropertyPathError, TargetPath,
    TargetPathError,
};
pub use path_expression::PathExpression;
pub use prim_index::{
    ArcKind, Opinion, OpinionIter, OpinionKey, OpinionRef, OpinionSource, OpinionValue, Opinions,
    SourceId,
};
pub use prim_index_graph::{NodeId, PrimIndexGraph, PrimNode};
pub use shared_vec::SharedVec;

pub use property::{
    PropertyEntry, PropertyKind, PropertySpec, PropertyType, Time, TimeSample, TimeSamples,
    Variability,
};
pub use relocates::RelocationTable;
pub use schema::{
    AppliedSchema, CannotApply, InvalidBaseReason, ListOpItem, MetadataConflict,
    MetadataDefinition, MetadataEditError, MetadataEditReason, MetadataField, MetadataOrigin,
    MetadataTarget, MetadataType, PrimDefinition, PropertyDefinition, SchemaDefinition,
    SchemaIssue, SchemaKind, SchemaRegistry, SchemaRegistryBuilder, is_spec_field,
};
pub use spec_path::{SpecComponent, SpecPath, SpecPathError};
pub use spline::SplineData;
pub use stage::{
    Attribute, AttributeQuery, AttributeQueryWork, AuthoringInspection, AuthoringInspectionError,
    AuthoringQuery, AuthoringSource, AuthoringSourceKind, AuthoringSpan, AuthoringTimes,
    AuthoringVerdict, AuthoringWrite, Contribution, CurrentSource, DictionaryMerge,
    ExplainedOpinion, FlattenError, FlattenReport, FlattenRequirements, FlattenVerification,
    Flattened, IgnoreCause, KeyPath, LayerMuteError, LoadPolicy, Object, OpinionRole,
    PayloadLoadRules, PayloadRule, PopulationMask, Prim, PrimPredicate, PrimRange, PrimSnapshot,
    PrimStatus, PrimVisit, PrimVisits, Property, PropertyDeclaration, Provenance, Relationship,
    RelationshipQuery, RelationshipQueryWork, Resolved, ResolvedValue, SampleBracket, SampleUse,
    SourceComposability, SourceStanding, SpanTimes, Stage, StageMetadataError, StageOptions,
    Traverse, ValueExplanation, ValueSource,
};
pub use stage::{
    AuthoredTarget, AuthoredTargets, DanglingByName, DanglingCounts, DanglingKind, DanglingTarget,
    DanglingTargetReport, DropReason, DroppedTarget, FromValue, LayerTargets, NameArg, PrimPathArg,
    PropertyPathArg, StageView, TargetListPosition, TargetOpinion, TargetRead, TargetsExplanation,
    UnreadReason, UnreadTargetSpec,
};
pub use value_type::{UnknownValueType, ValueRole, ValueType};
pub use variant_fallbacks::VariantFallbacks;

pub use live_stage::{
    ChangeCursor, ChangeHistoryError, ChangeNotice, ChangeSubscription, LiveStage,
    MaskExpansionReport, RecompositionWork,
};
