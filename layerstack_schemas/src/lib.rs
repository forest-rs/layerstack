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
//! Matching numeric array getters retain `Arc<Vec<T>>` storage. Borrow with
//! `as_slice()` or explicitly copy with `as_ref().clone()` for a mutable vector.
//! Half and matrix representation conversions still materialize vectors.
//! Slice setters copy; `_owned` and `_shared` setters transfer numeric buffers.
//! With `usd-geom`, [`GeneratedMesh::prepare`] validates complete polygon-mesh
//! snapshots before collecting creation or update through an explicit edit target.
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
//! assert_eq!(mesh.face_vertex_counts(), Some(vec![4].into()));
//! assert_eq!(mesh.points_at(1.0, InterpolationType::Held).map(|p| p.len()), Some(4));
//! // Nothing authored: the schema's fallback.
//! assert_eq!(mesh.subdivision_scheme(), Some(MeshSubdivisionScheme::CatmullClark));
//! ```
//!
//! Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …) that enables
//! the domains it depends on; `all`, the default, enables every one.
//!
//! # Standard shader nodes
//!
//! With `usd-shade`, [`shading::nodes`] provides typed views and edit handles
//! for the standard preview surface, UV texture, primvar readers and 2D
//! transform nodes. Views check both `Shader` inheritance and the composed
//! identifier-based `info:id`. Input getters read USD values; associated
//! `_default` functions return the node definition's defaults explicitly.
//! Typed input/output creation returns ordinary shading ports for connections.
//!
//! ```
//! use std::sync::Arc;
//! use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};
//! use layerstack_schemas::{Scene, SchemaEdit, shading::nodes::PreviewSurface};
//!
//! let mut store = InMemoryStore::default();
//! store.insert_layer(Layer::new(LayerId(1)));
//! let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
//! let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions {
//!     schemas: Some(schemas), ..StageOptions::default()
//! });
//! let path = store.path("/Material/Surface");
//! let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
//! let surface = PreviewSurface::define(&mut edit, path);
//! surface.set_roughness(&mut edit, 0.25).expect("typed input");
//! let transaction = edit.finish();
//! live.apply(&mut store, &transaction).expect("applies");
//! let scene = Scene::new(live.stage(), &store);
//! assert_eq!(PreviewSurface::new(&scene, path).unwrap().roughness(), Some(0.25));
//! assert_eq!(PreviewSurface::roughness_default(), 0.5);
//! ```
//!
//! # Plugin metadata
//!
//! The registry also retains each enabled plugin's `SdfMetadata` declarations:
//! field types, applicable spec kinds, registered defaults and documentation.
//! Typed metadata readers are available on [`PrimView`], [`PropertyMetadata`]
//! (from [`PrimView::property_metadata`]) and [`StageMetadata`] (from
//! [`Scene::metadata`]). Property readers compose authored opinions without
//! synthesizing registered field defaults. Stage readers use the root layer
//! and registered layer defaults; sublayer metadata does not participate.
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
//!   `Xformable::ordered_xform_ops` lists the ops. `transform_time_samples`
//!   returns their composed sample times in stage time;
//!   `transform_might_be_time_varying` detects numeric-time variability,
//!   including splines;
//! - `Imageable::compute_local_to_world` and `compute_parent_to_world`;
//!   for many prims, an [`XformCache`] owned by the caller shares each
//!   ancestor's work, reports what it computed ([`XformCacheStats`]), and
//!   is invalidated explicitly after edits. `relative_transform` composes
//!   local ops toward an ancestor and reports any intervening reset;
//! - `bounds::BoundsCache` computes world, local, untransformed and relative
//!   bounds from authored extents or model extents hints, with purpose and
//!   visibility filtering. Relative bounds convert world coordinate frames,
//!   including across transform resets. Time changes preserve static results;
//!   built-in intrinsic extents and point-instancer prototype bounds are supported;
//!   other procedural extent providers return explicit errors;
//! - `Imageable::compute_visibility`, `compute_effective_visibility` for a
//!   purpose (`VisibilityAPI`), and `compute_purpose_info`, which says
//!   which prim authors the inherited purpose.
//! - `Scene::compute_motion_blur_scale`, `compute_nonlinear_sample_count`
//!   and `compute_velocity_scale` inherit readable authored `MotionAPI`
//!   settings through any prim type; the API view offers the same methods.
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
//! itself. `set_common_transform` and `set_common_transform_at` author a
//! compatible translation/pivot/Euler rotation/scale stack from
//! [`CommonTransform`], rejecting incompatible ops before writing. They retain
//! existing vector precision and the reset flag; they do not decompose matrices.
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
//! [`MembershipCache`] retains compiled queries and ordered candidate decisions.
//! Pass complete edit reports to `apply_changes`; separate query and decision
//! revisions, dependency scopes and work counters make reuse inspectable.
//!
//! With `usd-geom`, [`bounds::BoundsCache`] computes oriented local and world
//! bounds from authored extents and model extent hints, partitioned by purpose.
//! It indexes cached namespace dependencies so edits evict the affected subtree
//! and its ancestors, including external point-instancer prototypes. Built-in
//! extents are computed when unauthored; other plugins return errors; see [`bounds`].
//!
//! With `usd-shade`, [`Scene::connected_sources`] reads composed shading
//! connections and [`Scene::shader_sources`] traces node-graph passthroughs.
//! [`usd_shade::Material::compute_surface_source`] selects a terminal through
//! ordered render contexts and universal fallback. Results retain candidate
//! endpoints, branch diagnostics and dependencies, including missing properties.
//! [`Scene::value_sources`] also finds authored values behind material and
//! node-graph interface inputs. [`shading::Port`] reads a provider's value at a
//! chosen time. [`PrimEdit::create_input`] and [`PrimEdit::create_output`] create
//! typed ports; [`shading::PortEdit`] sets values and replaces connections through
//! normal source transactions. Disconnecting authors an empty list; clearing
//! removes the source opinion to reveal weaker connections.
//!
//! With `usd-lux`, [`light::LightInputs`] captures owned emitter inputs with
//! shader readiness, source evidence, stage units and typed shape/photometric
//! accessors. [`light::LightCache`] retains discovery and inputs across explicit
//! edits, with component revisions for engine uploads. [`light::LightLinkMembership`]
//! captures ordered CPU link decisions for engine masks. `MembershipCache`'s
//! `capture_light_links` and `capture_filter_links` retain those decisions.
//! `LightInputs::shaping`, `shadow` and `environment` expose checked USD groups,
//! preserving units, sentinels and unresolved asset inputs. [`assets::AssetReference`]
//! captures an asset's winning source even when general provenance is disabled,
//! then asks the host resolver to anchor its identifier explicitly.
//! [`affine::AffineFactors`] preserves full affine stretch, shear and reflection;
//! optional `to_trs` conversion checks a caller-specified reconstruction tolerance.
//! GPU layouts, shader execution, asset loading and color management remain
//! engine responsibilities.
//! See `layerstack_examples`' `lighting_inputs` binary for the complete flow.
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

#[cfg(feature = "usd-geom")]
mod mesh_publication;
#[cfg(feature = "usd-geom")]
pub use mesh_publication::{
    GeneratedMesh, MeshPrimvar, MeshPublication, MeshPublicationError, MeshSites,
    validate_mesh_primvar_cardinality, validate_mesh_primvar_indices,
};

#[macro_use]
mod view;
#[cfg(feature = "usd-geom")]
pub mod affine;
#[cfg(feature = "usd-shade")]
mod binding;
#[cfg(feature = "usd-geom")]
pub mod bounds;
#[cfg(feature = "usd-geom")]
pub mod camera;
#[cfg(feature = "usd")]
mod collection;
#[cfg(feature = "usd")]
pub mod color;
#[cfg(feature = "usd-geom")]
mod common_xform;
mod edit;
mod metadata;
#[cfg(feature = "usd")]
pub mod validation;
pub use metadata::{PropertyMetadata, StageMetadata};
pub mod assets;
#[cfg(feature = "usd-geom")]
mod extent;
mod generated;
#[cfg(feature = "usd-geom")]
pub mod geometry;
#[cfg(any(feature = "usd-geom", feature = "usd-lod"))]
mod gf;
#[cfg(feature = "usd-geom")]
mod imageable;
pub mod kind;
#[cfg(feature = "usd-semantics")]
mod labels;
#[cfg(feature = "usd-lux")]
pub mod light;
#[cfg(feature = "usd-lod")]
pub mod lod;
#[cfg(feature = "usd-physics")]
pub mod mass;
#[cfg(feature = "usd-geom")]
mod motion;
#[cfg(feature = "usd-geom")]
mod motion_sampling;
#[cfg(feature = "usd-physics")]
pub mod physics;
#[cfg(feature = "usd-physics")]
pub mod physics_scene;
#[cfg(feature = "usd-geom")]
pub mod point_instancer;
#[cfg(feature = "usd-geom")]
pub mod point_motion;
#[cfg(feature = "usd")]
mod predicate;
#[cfg(feature = "usd-geom")]
pub mod primvar;
#[cfg(feature = "usd")]
mod regex;
#[cfg(feature = "usd-render")]
pub mod render;
#[cfg(all(feature = "usd-geom", feature = "usd-shade"))]
pub mod retained;
#[cfg(feature = "usd-ri")]
pub mod ri_spline;
#[cfg(feature = "usd-shade")]
pub mod shading;
#[cfg(feature = "usd-skel")]
pub mod skel;
#[cfg(feature = "usd-geom")]
pub mod subset;
mod table;
#[cfg(feature = "usd-ui")]
pub mod ui_hints;
pub mod value;
#[cfg(feature = "usd-vol")]
pub mod volume;
#[cfg(feature = "usd-geom")]
mod xform;
#[cfg(feature = "usd-geom")]
mod xform_edit;
#[cfg(feature = "usd-geom")]
pub use common_xform::{CommonTransform, CommonTransformError, RotationOrder};

#[cfg(feature = "usd-shade")]
pub use binding::{
    Binding, BindingCache, BindingCacheStats, BindingInputs, BindingKind, BindingOptions,
    BindingStrength, BoundMaterial, CollectionBinding, DirectBinding, MaterialPurpose,
};
#[cfg(feature = "usd")]
pub use collection::{
    CollectionIdentity, ExpansionRule, ExpressionEvaluator, ExpressionSearch, Membership,
    MembershipCache, MembershipCacheError, MembershipCacheMemory, MembershipCacheStats,
    MembershipDependencies, MembershipProblem, MembershipQuery, MembershipRevisions,
    MembershipRule, MembershipSample,
};
pub use edit::SchemaEdit;
pub use generated::views::*;
pub use generated::{Domain, OPENUSD_VERSION};
#[cfg(feature = "usd-geom")]
pub use imageable::{PurposeInfo, PurposeInputs, Visibility, VisibilityInputs};
pub use kind::KindRegistry;
#[cfg(feature = "usd-semantics")]
pub use labels::{LabelInterval, LabelQueryError, LabelsQuery};
pub use layerstack::Time;
#[cfg(feature = "usd")]
pub use predicate::{CollectionPredicate, CollectionPredicates};
pub use view::{InstanceEdit, InstanceView, PrimEdit, PrimView, Scene};
#[cfg(feature = "usd-geom")]
pub use xform::{
    INVERT_PREFIX, LocalTransform, LocalTransformInputs, RESET_XFORM_STACK, RelativeTransform,
    XformCache, XformCacheStats, XformOp, XformOpType, XformOps, XformProblem, XformProblemKind,
};
#[cfg(feature = "usd-geom")]
pub use xform_edit::{XformOpEdit, XformOpError, XformOpPrecision, XformOpValue};

use alloc::vec::Vec;

use layerstack::{SchemaRegistry, SchemaRegistryBuilder, TokenInterner};

/// Registers the schemas of `domains`, and of the domains they depend on,
/// with `builder`, interning their names and fallback tokens in `tokens`.
///
/// Use this to build one registry of OpenUSD's schemas and your own.
/// Returns an error when an already registered metadata declaration conflicts.
/// Registrations preceding the conflict remain in the builder.
pub fn register(
    builder: &mut SchemaRegistryBuilder,
    domains: &[Domain],
    tokens: &mut TokenInterner,
) -> Result<(), layerstack::MetadataConflict> {
    for domain in with_dependencies(domains) {
        let tables = domain.tables();
        for metadata in tables.metadata {
            builder.register_metadata(metadata.definition(tokens))?;
        }
        for schema in tables.schemas {
            builder.register(schema.definition(tokens));
        }
        for (schema, target) in tables.auto_applies {
            builder.auto_apply(tokens.intern(schema), tokens.intern(target));
        }
    }
    Ok(())
}

/// A registry of the schemas of `domains` and of the domains they depend
/// on.
#[must_use]
pub fn registry(domains: &[Domain], tokens: &mut TokenInterner) -> SchemaRegistry {
    let mut builder = SchemaRegistry::builder();
    register(&mut builder, domains, tokens).expect("generated OpenUSD metadata declarations agree");
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
