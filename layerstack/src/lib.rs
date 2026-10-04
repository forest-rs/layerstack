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
//!   return their inverses and check preconditions
//! - **Schemas** — prim definitions (type, applied schemas, defined
//!   properties and their fallbacks) from the [`SchemaRegistry`] a stage is
//!   composed with ([`StageOptions::schemas`], [`Stage::prim_definition`])
//! - **Value explanations** — [`Stage::explain_property_value`] and its
//!   siblings say why a value is what it is: every consulted opinion with its
//!   layer, spec, arc and layer offset, and whether it contributed a value, a
//!   sparse array edit, dictionary entries or a list edit, or was shadowed,
//!   cut off by a block or incompatible (compare OpenUSD's
//!   `UsdAttribute::GetResolveInfo`)
//!
//! # Quick start
//!
//! Add `layerstack = "0.1"` to your dependencies. This example authors a base
//! layer and a stronger local override, then reads their composed value:
//!
//! ```
//! use layerstack::{
//!     InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, SublayerEntry, Value,
//! };
//!
//! let mut store = InMemoryStore::default();
//! let title = store.tokens.intern("title");
//! let prim = store.path("/Doc");
//!
//! let mut base = Layer::new(LayerId(1));
//! base.insert_prim(prim, PrimSpec::def().with_field(title, Value::string("Untitled")));
//! store.insert_layer(base);
//!
//! let mut local = Layer::new(LayerId(2));
//! local.sublayers.push(SublayerEntry::new(LayerId(1)));
//! local.insert_prim(prim, PrimSpec::over().with_field(title, Value::string("Hello")));
//! store.insert_layer(local);
//!
//! let stage = Stage::compose(&mut store, LayerId(2), StageOptions::default());
//! assert!(stage.composition_errors().is_empty());
//! let resolved = stage.resolve_field(prim, title).unwrap();
//! assert_eq!(resolved.value, Value::string("Hello"));
//! ```
//!
//! Composition returns a stage even when some arcs cannot be resolved. Inspect
//! [`Stage::composition_errors`] before treating the result as complete. Enable
//! [`StageOptions::with_provenance`] to include the winning source in resolved values.
//!
//! # Key types
//!
//! | Type | Role |
//! |------|------|
//! | [`Layer`] / [`PrimSpec`] | Authored layer and prim opinions |
//! | [`Stage`] | Read-only composed values and prim hierarchy |
//! | [`LiveStage`] | Incremental edits, change reports, callbacks and change cursors |
//! | [`InMemoryStore`] / [`LayerStore`] | Built-in storage and a host storage interface |
//! | [`Value`] / [`FieldValue`] | Authored values and composition containers |
//! | [`Path`] / [`PropertyPath`] / [`TargetPath`] / [`SpecPath`] | Distinct prim, property, target, and source-spec paths |
//! | [`TokenInterner`] / [`PathInterner`] | Store-local token and path handles |
//! | [`SchemaRegistry`] | Schema definitions, prim definitions and fallback values |
//! | [`EditTarget`] / [`Transaction`] | Mapped, atomic authoring with preconditions and undo |
//!
//! [`PathId`] and [`TokenId`] belong to their interners; they are not durable
//! identities to persist or exchange between unrelated stores.
//!
//! # Scope and compatibility
//!
//! This crate owns in-memory composition, not file I/O, geometry evaluation, or
//! material interpretation. Companion crates in the [repository][repo] provide
//! USDA and USDC readers/writers, USDZ packaging, mesh export, and generated OpenUSD
//! schema views with transform, bounds and shading queries. Applications supply
//! asset resolution through [`AssetResolver`] and their own domain behavior.
//! For composition of already-ordered opinions without USD paths and arcs, see
//! [`opinionated`](https://docs.rs/opinionated).
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

pub use hashbrown::{HashMap, HashSet};

pub(crate) mod arc_cycle;
pub(crate) mod arcs;
pub mod array_edit;
pub mod asset;
pub mod asset_dependencies;
pub mod clip_authoring;
pub(crate) mod compose;
pub(crate) mod composition_checks;
pub mod composition_error;
pub mod dependency_map;
pub mod doc;
mod shared_vec;
mod value_array;
pub mod value_clips;
pub use value_array::{ArrayReadError, ArrayRef, DeferredArraySource, TypedArray};
pub mod edit;
pub(crate) mod expression_variables;
pub mod half;
pub mod ident;
pub mod interner;
pub mod layer_stack;
pub mod listop;
pub mod path;
pub mod path_expression;
pub(crate) mod population;
pub mod prim_index;
pub mod prim_index_graph;
pub mod property;
pub mod relocates;
pub mod schema;
pub mod sparse_writer;
pub mod spec_path;
pub mod spline;
pub mod stage;
pub mod stitch;
mod value_resolution;
pub mod variable_expression;
pub mod variant_fallbacks;

pub mod live_stage;

pub use array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, TypedArrayEdit};
pub use asset::{
    AssetResolveError, AssetResolver, ExpressionAssetPath, ResolvedAsset, expression_asset_paths,
};
pub use composition_error::{
    ArcCycle, ArcCycleSite, ArcToProhibitedChild, CompositionError, ExpressionContext,
    InconsistentPropertyType, InvalidAuthoredRelocation, InvalidConflictingRelocation,
    InvalidExternalTargetPath, InvalidInstanceTargetPath, InvalidRelocationReason,
    InvalidSameTargetRelocations, OpinionAtRelocationSource, RelocationConflict, SublayerCycle,
    UnresolvedAsset, UnresolvedDefaultPrim, UnresolvedPrimPath, UnresolvedSublayer,
    VariableExpressionError,
};
pub use dependency_map::ArcDependency;
pub use doc::{
    AssetAvailability, FieldEntry, FieldValue, InMemoryStore, InterpolationType, Layer, LayerId,
    LayerOffset, LayerStore, PrimSpec, Reference, ReferenceTarget, Relocate, Specifier,
    SublayerEntry, Value, VariantBranch, VariantBranches, VariantSetSpec, VariantSpec,
    combine_dictionaries, combine_dictionary_chain, get_field, get_field_mut,
    insert_field_if_absent, remove_field, set_field_vec,
};
pub use edit::{
    Address, Applied, Changes, EditError, EditTarget, NamespaceEdit, NamespaceError,
    PrimPropertyChanges, PropertyChange, PropertyField, Transaction,
};
pub use interner::{TokenId, TokenInterner};
pub use layer_stack::{LayerStack, LayerStackIdentifier};
pub use listop::ListOp;
pub use path::{
    Path, PathError, PathId, PathInterner, PropertyPath, PropertyPathError, TargetPath,
    TargetPathError,
};
pub use path_expression::PathExpression;
pub use prim_index::{ArcKind, Opinion, OpinionKey, OpinionValue};
pub use prim_index_graph::{NodeId, PrimIndexGraph, PrimNode};
pub use shared_vec::SharedVec;

pub use property::{
    PropertyEntry, PropertyKind, PropertySpec, PropertyType, Time, TimeSample, Variability,
};
pub use relocates::RelocationTable;
pub use schema::{
    AppliedSchema, CannotApply, MetadataConflict, MetadataDefinition, MetadataTarget,
    PrimDefinition, PropertyDefinition, SchemaDefinition, SchemaIssue, SchemaKind, SchemaRegistry,
    SchemaRegistryBuilder,
};
pub use spec_path::{SpecComponent, SpecPath, SpecPathError};
pub use spline::SplineData;
pub use stage::{
    Contribution, DictionaryMerge, ExplainedOpinion, FlattenError, FlattenReport,
    FlattenRequirements, FlattenVerification, Flattened, IgnoreCause, KeyPath, LayerMuteError,
    LoadPolicy, OpinionRole, PayloadLoadRules, PayloadRule, PopulationMask, PropertyDeclaration,
    Provenance, Resolved, ResolvedValue, SampleUse, Stage, StageOptions, Traverse,
    ValueExplanation, ValueSource,
};
pub use variant_fallbacks::VariantFallbacks;

pub use live_stage::{
    ChangeCursor, ChangeHistoryError, ChangeNotice, ChangeSubscription, LiveStage,
};
