//! `layerstack` is a small, domain-neutral composition kernel aligned with the `OpenUSD`
//! Core specification.
//!
//! For the normative reference used in this repo, see `specs/aousd_core_spec_1.0.1_2025-12-12.pdf`.
//!
//! It provides:
//! - Layer stacks (recursive sublayers, deterministic strength)
//! - Stage population (a composed prim tree)
//! - Value resolution (scalar + `ListOp`)
//! - Composition arcs: local, inherits, variants, references, payloads, specializes
//!
//! This crate is `no_std` by default.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "std"))]
extern crate std;

pub use hashbrown::{HashMap, HashSet};

pub mod arcs;
pub mod array_edit;
pub mod asset;
pub mod compose;
pub mod dependency_map;
pub mod doc;
pub mod interner;
pub mod layer_stack;
pub mod listop;
pub mod path;
pub mod population;
pub mod prim_index;
pub mod property;
pub mod schema;
pub mod spec_path;
pub mod spline;
pub mod stage;
mod value_resolution;

pub mod live_stage;

pub use array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
pub use asset::{AssetResolveError, AssetResolver, ResolvedAsset};
pub use dependency_map::ArcDependency;
pub use doc::{
    FieldEntry, FieldValue, InMemoryStore, InterpolationType, Layer, LayerId, LayerOffset,
    PrimSpec, Reference, ReferenceTarget, Specifier, SublayerEntry, Value, VariantSetSpec,
    VariantSpec, combine_dictionaries, combine_dictionary_chain, get_field, get_field_mut,
    insert_field_if_absent, insert_property_field_if_absent, set_field_vec, set_property_field_vec,
};
pub use interner::{TokenId, TokenInterner};
pub use layer_stack::LayerStack;
pub use listop::ListOp;
pub use path::{Path, PathId, PathInterner, PropertyPath, PropertyPathError};
pub use prim_index::{ArcKind, Opinion, OpinionKey};
pub use property::PropertyType;
pub use schema::{PropertyDefinition, SchemaDefinition, SchemaRegistry};
pub use spec_path::{SpecComponent, SpecPath, SpecPathError};
pub use spline::SplineData;
pub use stage::{Resolved, ResolvedValue, Stage, StageOptions};

pub use live_stage::LiveStage;
