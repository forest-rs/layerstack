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
pub mod compose;
pub mod doc;
pub mod interner;
pub mod layer_stack;
pub mod listop;
pub mod path;
pub mod population;
pub mod prim_index;
pub mod stage;

pub use doc::{
    FieldValue, InMemoryStore, Layer, LayerId, PrimSpec, Reference, Specifier, Value,
    VariantSetSpec, VariantSpec,
};
pub use interner::{TokenId, TokenInterner};
pub use layer_stack::LayerStack;
pub use listop::ListOp;
pub use path::{Path, PathId, PathInterner};
pub use prim_index::{ArcKind, Opinion, OpinionKey};
pub use stage::{Resolved, ResolvedValue, Stage, StageOptions};
