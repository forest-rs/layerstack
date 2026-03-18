//! `layerstack` is a small, domain-neutral composition kernel aligned with the
//! [OpenUSD Core specification][spec].
//!
//! [spec]: https://openusd.org/release/spec_usdcore.html
//!
//! It provides:
//!
//! - **Layer stacks** — recursive sublayers with deterministic strength ordering
//! - **Stage population** — a composed prim tree from all contributing layers
//! - **Value resolution** — scalar (strongest-wins) and [`ListOp`] chaining
//! - **Composition arcs** — local, inherits, variants, references, payloads,
//!   specializes (LIVERPS)
//! - **Incremental recomposition** — via [`LiveStage`] and the `invalidation`
//!   dependency graph
//! - **Schema fallbacks** — via [`SchemaRegistry`]
//!
//! # Quick start
//!
//! ```ignore
//! use layerstack::{
//!     InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value,
//! };
//!
//! let mut store = InMemoryStore::default();
//! let title = store.tokens.intern("title");
//! let prim = store.path("/Doc");
//!
//! let mut layer = Layer::new(LayerId(1));
//! layer.insert_prim(prim, PrimSpec::def().with_field(title, Value::string("Hello")));
//! store.insert_layer(layer);
//!
//! let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
//! let resolved = stage.resolve_field(prim, title).unwrap();
//! assert_eq!(resolved.value, Value::string("Hello"));
//! ```
//!
//! # Key types
//!
//! | Type | Role |
//! |------|------|
//! | [`Stage`] | Immutable composed stage — query values, traverse prims |
//! | [`LiveStage`] | Mutable stage with incremental recomposition |
//! | [`InMemoryStore`] | Simple in-memory [`LayerStore`] implementation |
//! | [`LayerStore`] | Trait for pluggable layer storage |
//! | [`Value`] / [`FieldValue`] | Scalar values and field containers |
//! | [`TokenInterner`] / [`PathInterner`] | Interning for strings and paths |
//! | [`SchemaRegistry`] | Schema definitions and fallback value lookup |
//!
//! # `no_std` support
//!
//! This crate is `no_std` by default (uses `alloc`). Enable the **`std`** feature
//! for `std::error::Error` integration.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "std"))]
extern crate std;

pub use hashbrown::{HashMap, HashSet};

pub(crate) mod arcs;
pub mod array_edit;
pub mod asset;
pub(crate) mod compose;
pub mod dependency_map;
pub mod doc;
pub mod interner;
pub mod layer_stack;
pub mod listop;
pub mod path;
pub(crate) mod population;
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
    LayerStore, PrimSpec, Reference, ReferenceTarget, Specifier, SublayerEntry, Value,
    VariantSetSpec, VariantSpec, combine_dictionaries, combine_dictionary_chain, get_field,
    get_field_mut, insert_field_if_absent, insert_property_field_if_absent, set_field_vec,
    set_property_field_vec,
};
pub use interner::{TokenId, TokenInterner};
pub use layer_stack::LayerStack;
pub use listop::ListOp;
pub use path::{
    Path, PathError, PathId, PathInterner, PropertyPath, PropertyPathError, TargetPath,
    TargetPathError,
};
pub use prim_index::{ArcKind, Opinion, OpinionKey};
pub use property::PropertyType;
pub use schema::{PropertyDefinition, SchemaDefinition, SchemaRegistry};
pub use spec_path::{SpecComponent, SpecPath, SpecPathError};
pub use spline::SplineData;
pub use stage::{
    PopulationMask, Provenance, Resolved, ResolvedValue, Stage, StageOptions, Traverse,
};

pub use live_stage::LiveStage;
