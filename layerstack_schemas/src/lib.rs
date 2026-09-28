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
//! # Views
//!
//! Each domain's module ([`usd_geom`], [`usd_lux`], …) has a typed view of
//! every schema, which reads a composed prim through a [`Scene`] (a stage
//! composed with these schemas, and its store):
//!
//! - a typed schema's view (`Mesh`) is constructed with `new`, which checks
//!   `IsA`, and derefs to the view of the schema it inherits from, down to
//!   [`PrimView`]; an abstract schema's view (`Gprim`) reads any prim
//!   derived from it;
//! - a single-apply schema's view (`MaterialBindingApi`) is had with `get`,
//!   which checks `HasAPI`; a multiple-apply schema's (`CollectionApi`) with
//!   `get(scene, path, instance)`, or every applied instance with
//!   `instances`;
//! - getters return the resolved value, with the schema's fallback, as a
//!   Rust value, or `None` when there is none; `_at` getters read a varying
//!   attribute at a time code; relationship getters return their targets;
//! - a token attribute with `allowedTokens` reads as an enum with an
//!   `Other` variant for any other token;
//! - accessors are named for each property's `apiName`, snake-cased, else
//!   for its USD name without a multiple-apply schema's instance prefix.
//!
//! Vectors are arrays, quaternions `[i, j, k, r]`, halves `f32`, strings,
//! assets and path expressions `Arc<str>`, and matrices arrays of rows
//! (`m[row][column]`, translation in the last row, as USD stores them).
//! For anything a view does not offer, resolve the property by its USD name
//! on [`Scene::stage`]: `Stage::resolve_value_with_schema` returns the raw
//! resolved value with its provenance.
//!
//! Every view has an edit handle (`MeshEdit`) whose setters author through
//! a [`SchemaEdit`], which collects a `Transaction` for one explicit edit
//! target. A handle is had only for a prim that exists, on the stage or
//! defined earlier in the edit: from a view's `edit`, a concrete schema's
//! `define`, an applied schema's `apply` (which fails with
//! `CannotApply::NoSuchPrim` for a missing prim), or `new`, which returns
//! `None` for one. No edit manufactures an `over` for a missing prim.
//! Relationship targets are stage paths, mapped through the edit target
//! (through a reference, `/Instance/Light` is authored as `/Asset/Light`);
//! one it does not map rejects the transaction when it is applied.
//!
//! ```
//! use std::sync::Arc;
//!
//! use layerstack::edit::EditTarget;
//! use layerstack::{InMemoryStore, InterpolationType, Layer, LayerId, LiveStage, StageOptions};
//! use layerstack_schemas::usd_geom::{Mesh, MeshSubdivisionScheme};
//! use layerstack_schemas::{Scene, SchemaEdit};
//!
//! let mut store = InMemoryStore::default();
//! store.insert_layer(Layer::new(LayerId(1)));
//! let options = StageOptions {
//!     schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
//!     ..StageOptions::default()
//! };
//! let mut live = LiveStage::compose(&mut store, LayerId(1), options);
//! let path = store.path("/Ground");
//!
//! let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
//! Mesh::define(&mut edit, path)
//!     .set_face_vertex_counts(&mut edit, &[4])
//!     .set_points_at(&mut edit, 1.0, &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]]);
//! let transaction = edit.finish();
//! live.apply(&mut store, &transaction).expect("applies");
//!
//! let scene = Scene::new(live.stage(), &store);
//! let mesh = Mesh::new(&scene, path).expect("a mesh");
//! assert_eq!(mesh.face_vertex_counts(), Some(vec![4]));
//! assert_eq!(mesh.points_at(1.0, InterpolationType::Held).map(|p| p.len()), Some(4));
//! // Nothing authored: the schema's fallback.
//! assert_eq!(mesh.subdivision_scheme(), Some(MeshSubdivisionScheme::CatmullClark));
//! ```
//!
//! Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …) that enables
//! the domains it depends on; `all`, the default, enables every one.
//!
//! # Transforms, visibility and purpose
//!
//! With `usd-geom`, the views compute what `UsdGeom` defines to inherit
//! down namespace, as OpenUSD computes it, at a [`Time`] (the default
//! time, or a time code with linear or held interpolation):
//!
//! - `Xformable::local_transform`: the product of the prim's
//!   `xformOpOrder` ops (every op type and Euler order, in any precision,
//!   with inverse ops and `!resetXformStack!`), with the ops that could not
//!   contribute reported as [`XformProblem`]s rather than hidden;
//!   `Xformable::ordered_xform_ops` lists the ops;
//! - `Imageable::compute_local_to_world` and `compute_parent_to_world`;
//!   for many prims, an [`XformCache`] owned by the caller shares each
//!   ancestor's work, reports what it computed ([`XformCacheStats`]), and
//!   is invalidated explicitly after edits;
//! - `Imageable::compute_visibility`, `compute_effective_visibility` for a
//!   purpose (`VisibilityAPI`), and `compute_purpose_info`, which says
//!   which prim authors the inherited purpose.
//!
//! Matrices are `[[f64; 4]; 4]` rows that transform row vectors, as USD's
//! are: the translation is the last row, and a prim's local-to-world
//! transform is its local transform times its parent's.
//!
//! Transform ops are authored through `XformableEdit` (any `Xformable`'s
//! edit handle derefs to it), as `UsdGeomXformable` authors them:
//! `add_op` (and `add_translate_op`, `add_scale_op`, `add_rotate_xyz_op`,
//! `add_orient_op`, `add_transform_op`) creates the op's attribute in a
//! [`XformOpPrecision`] and appends the op to `xformOpOrder`, refusing an
//! op already listed ([`XformOpError`]); the [`XformOpEdit`] it returns
//! sets the op's value ([`XformOpValue`]) at the default time or a time
//! code. `clear_xform_op_order` and `set_reset_xform_stack` edit the order
//! itself.
//!
//! Each computation is a pure step over one prim's stage reads and its
//! parent's result: [`LocalTransformInputs`] (read once, then
//! `evaluate`d) with [`LocalTransform::local_to_world`];
//! [`VisibilityInputs`] with [`Visibility::inherit`] and
//! [`Visibility::effective`]; [`PurposeInputs`] with
//! [`PurposeInfo::inherit`]. Each inputs type gathers its reads in one
//! place and names them, so a caller that tracks dependencies can record
//! them; the views and [`XformCache`] are callers of the same steps.
//!
//! ```
//! use std::sync::Arc;
//!
//! use layerstack::edit::EditTarget;
//! use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions};
//! use layerstack_schemas::usd_geom::{ImageablePurpose, Scope, Sphere};
//! use layerstack_schemas::{Scene, SchemaEdit, Time, Visibility, XformCache};
//!
//! let mut store = InMemoryStore::default();
//! store.insert_layer(Layer::new(LayerId(1)));
//! let options = StageOptions {
//!     schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
//!     ..StageOptions::default()
//! };
//! let mut live = LiveStage::compose(&mut store, LayerId(1), options);
//! let (group, ball) = (store.path("/Guides"), store.path("/Guides/Ball"));
//!
//! let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
//! Scope::define(&mut edit, group).set_purpose(&mut edit, ImageablePurpose::Guide);
//! Sphere::define(&mut edit, ball);
//! let transaction = edit.finish();
//! live.apply(&mut store, &transaction).expect("applies");
//!
//! let scene = Scene::new(live.stage(), &store);
//! let sphere = Sphere::new(&scene, ball).expect("a sphere");
//! let info = sphere.compute_purpose_info();
//! assert_eq!(info.purpose, ImageablePurpose::Guide);
//! assert_eq!(info.authored_on, Some(group));
//! // Guides are hidden unless something makes them visible.
//! assert_eq!(
//!     sphere.compute_effective_visibility(&ImageablePurpose::Guide, Time::Default),
//!     Visibility::Invisible
//! );
//! let mut cache = XformCache::new(Time::at(1.0));
//! assert_eq!(cache.local_to_world(&scene, ball), Some(sphere.compute_local_to_world(Time::at(1.0))));
//! ```
//!
//! # Collections and material bindings
//!
//! With `usd`, `CollectionApi::membership_query` computes a collection's
//! membership as OpenUSD's `ComputeMembershipQuery` does (includes and
//! excludes, expansion rules, `includeRoot`, collections included by their
//! collection paths), and [`MembershipQuery::is_included`] answers for any
//! prim or property. A collection without includes or excludes is decided
//! by its `membershipExpression`: a path expression (see
//! `layerstack::path_expression`) whose references to other collections
//! resolve recursively, matched with OpenUSD's collection predicates
//! ([`CollectionPredicates`]: `isa`, `hasAPI`, `kind`, `model`, `group`,
//! `variant`, `specifier`, `abstract`, `defined`), with kinds from the
//! scene's [`KindRegistry`]. [`MembershipQuery::included_paths`] lists every
//! member, and [`ExpressionEvaluator`] matches any expression.
//!
//! With `usd-geom`, [`bounds::BoundsCache`] computes oriented local and world
//! bounds from authored extents and model extent hints, partitioned by purpose.
//! It indexes cached namespace dependencies so edits evict the affected subtree
//! and its ancestors. Procedural extents and point-instancer bounds are explicit
//! unsupported results in this first slice; see [`bounds`].
//!
//! With `usd-shade`, [`Scene::connected_sources`] reads composed shading
//! connections and [`Scene::shader_sources`] traces node-graph passthroughs.
//! [`usd_shade::Material::compute_surface_source`] selects a terminal through
//! ordered render contexts and universal fallback. Results retain candidate
//! endpoints, branch diagnostics and dependencies, including missing properties.
//!
//! With `usd-shade`, `PrimView::compute_bound_material` (every view derefs
//! to [`PrimView`]) resolves a prim's material for a [`MaterialPurpose`] as
//! `ComputeBoundMaterial` does: direct and collection bindings, binding
//! strength, the purpose's fallback to all-purpose and the walk up
//! namespace. It returns the material and the [`Binding`] that decided it,
//! including whether that binding's prim lacks `MaterialBindingAPI` (a
//! legacy binding, which [`BindingOptions`] allows by default as OpenUSD
//! 26.08 does).
//! [`BindingCache`] resolves many prims, sharing each ancestor's bindings and
//! each collection's membership; [`BindingInputs`] and
//! [`BoundMaterial::resolve`] are the pure steps it folds.
//!
//! # License
//!
//! The generated tables are derived from OpenUSD's schema definitions,
//! licensed under the Tomorrow Open Source Technology License 1.0 (a
//! modified Apache License 2.0); see `LICENSE-TOST-1.0` and `NOTICE`. The
//! rest of the crate is under Apache-2.0 OR MIT (`LICENSE-APACHE`,
//! `LICENSE-MIT`).
//!
//! Spec: AOUSD Core §13 (schemas); §13.1 leaves how schemas are defined to
//! the implementation, and these are OpenUSD's.

#![no_std]
// With fewer domains, fewer of the shared view helpers and value types are
// used; the full build (`all`) is linted strictly.
#![cfg_attr(
    not(feature = "all"),
    allow(
        dead_code,
        unused_imports,
        unused_macros,
        unreachable_pub,
        reason = "a build with fewer domains uses fewer of the shared helpers"
    )
)]

extern crate alloc;

#[macro_use]
mod view;
#[cfg(feature = "usd-shade")]
mod binding;
#[cfg(feature = "usd-geom")]
pub mod bounds;
#[cfg(feature = "usd")]
mod collection;
mod edit;
mod generated;
#[cfg(feature = "usd-geom")]
mod gf;
#[cfg(feature = "usd-geom")]
mod imageable;
pub mod kind;
#[cfg(feature = "usd")]
mod predicate;
#[cfg(feature = "usd")]
mod regex;
#[cfg(feature = "usd-shade")]
pub mod shading;
mod table;
mod value;
#[cfg(feature = "usd-geom")]
mod xform;
#[cfg(feature = "usd-geom")]
mod xform_edit;

#[cfg(feature = "usd-shade")]
pub use binding::{
    Binding, BindingCache, BindingCacheStats, BindingInputs, BindingKind, BindingOptions,
    BindingStrength, BoundMaterial, CollectionBinding, DirectBinding, MaterialPurpose,
};
#[cfg(feature = "usd")]
pub use collection::{
    ExpansionRule, ExpressionEvaluator, ExpressionSearch, Membership, MembershipProblem,
    MembershipQuery, MembershipRule,
};
pub use edit::SchemaEdit;
pub use generated::views::*;
pub use generated::{Domain, OPENUSD_VERSION};
#[cfg(feature = "usd-geom")]
pub use imageable::{PurposeInfo, PurposeInputs, Visibility, VisibilityInputs};
pub use kind::KindRegistry;
#[cfg(feature = "usd")]
pub use predicate::{CollectionPredicate, CollectionPredicates};
pub use view::{InstanceEdit, InstanceView, PrimEdit, PrimView, Scene, Time};
#[cfg(feature = "usd-geom")]
pub use xform::{
    INVERT_PREFIX, LocalTransform, LocalTransformInputs, RESET_XFORM_STACK, XformCache,
    XformCacheStats, XformOp, XformOpType, XformOps, XformProblem, XformProblemKind,
};
#[cfg(feature = "usd-geom")]
pub use xform_edit::{XformOpEdit, XformOpError, XformOpPrecision, XformOpValue};

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

// The tests name domains across the crate.
#[cfg(all(test, feature = "all"))]
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

    #[test]
    fn application_limits_come_from_plug_info() {
        let mut tokens = TokenInterner::default();
        let volumes = registry(&[Domain::UsdVol], &mut tokens);
        let [radius_api, field, mesh] =
            ["ParticleFieldOpacityAttributeAPI", "ParticleField", "Mesh"].map(|n| tokens.intern(n));
        assert_eq!(
            volumes.can_only_apply_to(radius_api, None, &tokens),
            [field]
        );
        assert!(
            volumes
                .can_apply(Some(field), radius_api, None, &tokens)
                .is_ok()
        );
        assert!(
            volumes
                .can_apply(Some(mesh), radius_api, None, &tokens)
                .is_err()
        );
    }
}
