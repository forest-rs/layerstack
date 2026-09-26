// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD's schemas for `layerstack`.
//!
//! The schemas OpenUSD defines, as [`SchemaDefinition`](layerstack::SchemaDefinition)s
//! for a [`SchemaRegistry`]: typed schemas such as `Mesh`, `Material` and
//! `SphereLight`, and applied schemas such as `CollectionAPI` and
//! `MaterialBindingAPI`, with their properties, fallbacks, built-ins and
//! auto-applies. They are generated from OpenUSD's own schema definitions
//! (see [`OPENUSD_VERSION`]), so nothing is parsed at run time.
//!
//! ```
//! use std::sync::Arc;
//!
//! use layerstack::{InMemoryStore, StageOptions};
//!
//! let mut store = InMemoryStore::default();
//! let schemas = layerstack_schemas::openusd(&mut store.tokens);
//! assert!(schemas.issues().is_empty());
//!
//! let mesh = store.tokens.intern("Mesh");
//! let gprim = store.tokens.intern("Gprim");
//! assert!(schemas.is_a(mesh, gprim));
//!
//! // Compose stages from `store` with them.
//! let options = StageOptions {
//!     schemas: Some(Arc::new(schemas)),
//!     ..StageOptions::default()
//! };
//! # let _ = options;
//! ```
//!
//! The registry's tokens are interned in the interner passed in, which must
//! be the one of the store whose stages use it.
//!
//! Pick domains with [`registry`], or add OpenUSD's schemas to a builder
//! that also registers your own with [`register`]. A domain always brings
//! the domains it depends on ([`Domain::dependencies`]): `UsdLux`'s lights
//! derive from `UsdGeom`'s `Boundable`.
//!
//! # License
//!
//! The generated tables are derived from OpenUSD's schema definitions,
//! licensed under the Tomorrow Open Source Technology License 1.0 (a
//! modified Apache License 2.0); see `LICENSE-TOST-1.0` and `NOTICE`. The
//! rest of the crate is under Apache-2.0 OR MIT.
//!
//! Spec: AOUSD Core §13 (schemas); §13.1 leaves how schemas are defined to
//! the implementation, and these are OpenUSD's.

#![no_std]

extern crate alloc;

mod generated;
mod table;

pub use generated::{Domain, OPENUSD_VERSION};

use alloc::vec::Vec;

use layerstack::{SchemaRegistry, SchemaRegistryBuilder, TokenInterner};

/// Registers the schemas of `domains`, and of the domains they depend on,
/// with `builder`, interning their names and fallback tokens in `tokens`.
///
/// Use this to build one registry of OpenUSD's schemas and your own.
pub fn register(
    builder: &mut SchemaRegistryBuilder,
    domains: &[Domain],
    tokens: &mut TokenInterner,
) {
    for domain in with_dependencies(domains) {
        let tables = domain.tables();
        for schema in tables.schemas {
            builder.register(schema.definition(tokens));
        }
        for (schema, target) in tables.auto_applies {
            builder.auto_apply(tokens.intern(schema), tokens.intern(target));
        }
    }
}

/// A registry of the schemas of `domains` and of the domains they depend
/// on.
#[must_use]
pub fn registry(domains: &[Domain], tokens: &mut TokenInterner) -> SchemaRegistry {
    let mut builder = SchemaRegistry::builder();
    register(&mut builder, domains, tokens);
    builder.build(tokens)
}

/// A registry of every OpenUSD schema this crate has ([`Domain::ALL`]).
#[must_use]
pub fn openusd(tokens: &mut TokenInterner) -> SchemaRegistry {
    registry(Domain::ALL, tokens)
}

/// `domains` and everything they depend on, in [`Domain::ALL`] order.
fn with_dependencies(domains: &[Domain]) -> Vec<Domain> {
    let mut wanted: Vec<Domain> = domains.to_vec();
    let mut i = 0;
    while i < wanted.len() {
        for &dependency in wanted[i].dependencies() {
            if !wanted.contains(&dependency) {
                wanted.push(dependency);
            }
        }
        i += 1;
    }
    Domain::ALL
        .iter()
        .copied()
        .filter(|domain| wanted.contains(domain))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_brings_its_dependencies() {
        let mut tokens = TokenInterner::default();
        let lights = registry(&[Domain::UsdLux], &mut tokens);
        assert!(lights.issues().is_empty(), "{:?}", lights.issues());
        let sphere_light = tokens.intern("SphereLight");
        let boundable = tokens.intern("Boundable");
        assert!(lights.is_a(sphere_light, boundable));
        assert!(with_dependencies(&[Domain::UsdLux]).contains(&Domain::UsdGeom));
    }

    #[test]
    fn every_domain_registers_without_issues() {
        let mut tokens = TokenInterner::default();
        let all = openusd(&mut tokens);
        assert!(all.issues().is_empty(), "{:?}", all.issues());
        assert!(all.schema(tokens.intern("Mesh")).is_some());
    }
}
