// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Polygon mesh export to USDA and USDZ.
//!
//! This crate is the adapter between a mesh kernel's buffers and the
//! `UsdGeom` schemas. The kernel describes its data with plain slices —
//! positions, face topology, normals, UVs, extra primvars, transforms — and
//! the adapter applies the USD conventions explicitly:
//!
//! - positions become `points` (`point3f[]`), topology becomes
//!   `faceVertexCounts` / `faceVertexIndices`, and every mesh authors
//!   `subdivisionScheme = "none"` so polygonal data is not subdivided by
//!   consumers (the schema's fallback is `catmullClark`);
//! - normals keep their interpolation. Unindexed normals are written to
//!   `normals`; indexed normals are written as the `primvars:normals`
//!   primvar, since only primvars can be indexed;
//! - UVs become the `primvars:st` primvar (`texCoord2f[]`), with optional
//!   `primvars:st:indices`;
//! - each prim's local transform is one `xformOp:transform` in
//!   `xformOpOrder`; `extent` is computed from the points;
//! - stage metadata `upAxis` and `metersPerUnit` are always authored, and
//!   the root prim is the layer's `defaultPrim`.
//!
//! Names, geometry buffers and per-instance arrays are [`Cow`]s, and every
//! constructor takes `impl Into<Cow<..>>`: pass `&[T]` (or `&[T; N]`,
//! `&Vec<T>`, `&str`) to borrow a kernel's data without copying, or `Vec<T>`
//! and `String` for data built only for the export, so a function can
//! return a `Scene<'static>` it assembled at runtime. Texture paths
//! ([`Texture::file`]) stay borrowed, like the [`PackageFile`]s they name.
//!
//! [`Cow`]: alloc::borrow::Cow
//!
//! Prim names must be USD identifiers and unique among siblings; the
//! exporter checks them but never rewrites them. Kernels whose keys are
//! arbitrary strings can derive names with [`sanitize_name`] and keep
//! siblings apart with [`SiblingNames`].
//!
//! Face-varying indices are written exactly as given. They encode the
//! primvar's own topology (seams and hard edges), so equal values at
//! distinct indices are never merged.
//!
//! # Outputs and profile
//!
//! [`Scene::to_usda`] writes one *authored* layer through
//! [`layerstack_usda::writer`]; nothing is composed or flattened, and the
//! composition kernel is not involved. [`Scene::to_usdc`] writes the same
//! layer in the binary crate format ([`layerstack_usdc::writer`]).
//! [`Scene::to_usdz`] is a separate packaging step
//! ([`layerstack_usdz::write_usdz`]): the layer becomes the package's first
//! file, followed by the caller's files (e.g. textures), and every asset
//! path the scene authors must name one of those files. Paths are used as
//! given; the packager does not discover or rewrite dependencies.
//!
//! Packages are written for an explicit [`UsdzProfile`] (OpenUSD
//! `docs/spec_usdz.rst`):
//!
//! - [`UsdzProfile::Generic`]: a USDA root layer (`scene.usda`) plus any
//!   member type the USDZ specification allows;
//! - [`UsdzProfile::Arkit`]: the profile AR Quick Look and `ARKit` expect, a
//!   single USDC root layer (`scene.usdc`) plus PNG/JPEG images and
//!   M4A/MP3/WAV audio (`spec_usdz.rst`, "File Types";
//!   `pxr/usd/usdUtils/usdzPackage.h`, `UsdUtilsCreateNewARKitUsdzPackage`).
//!
//! Passing `usdchecker --arkit` checks the package layout and stage rules;
//! it does not by itself establish that a given viewer renders the result.
//!
//! # Materials
//!
//! A [`Material`] is written under `/<root>/Materials` as a `Material` prim
//! whose surface is a `UsdPreviewSurface` shader in the metallic workflow.
//! Each input is a constant or a texture: textures become `UsdUVTexture`
//! shaders reading `primvars:st` (or another UV set) through a
//! `UsdPrimvarReader_float2`, with `sourceColorSpace` fixed by the input
//! (`sRGB` for base and emissive color, `raw` for data), explicit wrap
//! modes, and the `scale`/`bias` remap the specification prescribes for
//! normal maps. Channels of one image (e.g. packed occlusion, roughness
//! and metallic) share one texture shader. See [`Material`] for the
//! inputs and their fallbacks.
//!
//! A mesh binds a material by name ([`Mesh::with_material`]): the mesh gets
//! the `MaterialBindingAPI` schema and a direct `material:binding`
//! relationship. Per-face materials ([`Mesh::with_material_subset`]) are
//! `GeomSubset` children in the `materialBind` family, each with its own
//! binding, and the mesh authors the family's type
//! ([`FamilyType::Partition`] or [`FamilyType::NonOverlapping`]).
//! Bindings are checked: the material must exist, the mesh must author
//! every UV set its textures read, and the subsets must form the declared
//! family (indices in range, no face twice, every face covered by a
//! partition), as `UsdGeomSubset::ValidateFamily` requires.
//!
//! Known limits, by design of this profile: bindings are direct, of
//! default strength and for all purposes (no collection-based bindings,
//! `bindMaterialAs`, or purpose-specific `material:binding:<purpose>`);
//! the only shading model is `UsdPreviewSurface` (no `MaterialX` or other
//! render contexts, no specular workflow, clearcoat, `ior` or
//! displacement); and texture coordinates are not transformed
//! (`UsdTransform2d`).
//!
//! # Repeated geometry
//!
//! A [`PointInstancer`] places a few prototypes many times: each prototype
//! is an ordinary [`Mesh`] or [`Xform`] subtree, keeping its materials and
//! face subsets, and each instance picks one by index and gives its own
//! position, and optionally an orientation, a scale and a stable id. The
//! prototypes are written under a `Prototypes` scope below the
//! `PointInstancer` prim, in the order of its `prototypes` relationship;
//! the per-instance arrays become `protoIndices`, `positions`,
//! `orientationsf` (`quatf[]`; the half-precision `orientations` is
//! written instead or as well on request, see [`OrientationPrecision`]),
//! `scales` and `ids`, and the instancer's
//! `extent` is computed from the prototypes' points and the instance
//! transforms. Both layer formats, and therefore both USDZ profiles, carry
//! it.
//!
//! The arrays are checked before anything is written (indices in range,
//! one element per instance, finite values, unit orientations, unique
//! ids). Instancers are static: there are no time samples, no motion
//! (`velocities`, `accelerations`, `angularVelocities`) and no masking
//! (`invisibleIds`, `inactiveIds`).
//!
//! A prototype used in several places is a *shared prototype*
//! ([`Scene::with_prototype`]): it is written once, under the `class` prim
//! `/<root>/Prototypes`, and placed by [`Instance`]s, typeless
//! `instanceable` prims with an internal reference to it (AOUSD Core
//! §10.3.2.1, §11). An `Instance` can stand anywhere in the tree, or be a
//! prototype of any number of instancers, so their geometry is not copied
//! per instancer.
//!
//! ```
//! use layerstack_mesh_export::{
//!     Faces, Mesh, PointInstancer, Scene, StageSettings, UpAxis, Xform,
//! };
//!
//! let stone = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
//! let proto_indices = [0, 0, 0];
//! let positions = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
//! let turned = core::f32::consts::FRAC_1_SQRT_2;
//! // `[x, y, z, w]`: none, a quarter turn about Z, none.
//! let orientations = [[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, turned, turned], [0.0, 0.0, 0.0, 1.0]];
//! let field = PointInstancer::new("Stones", &proto_indices, &positions)
//!     .with_prototype(Mesh::new("Stone", &stone, Faces::triangles(&[0, 1, 2])))
//!     .with_orientations(&orientations)
//!     .with_ids(&[10, 11, 12]);
//! let scene = Scene::new(
//!     StageSettings::new(UpAxis::Z, 1.0),
//!     Xform::new("Root").with_point_instancer(field),
//! );
//! let usda = scene.to_usda()?;
//! assert!(usda.contains("rel prototypes = </Root/Stones/Prototypes/Stone>"));
//! assert!(usda.contains("int[] protoIndices = [0, 0, 0]"));
//! # Ok::<(), layerstack_mesh_export::ExportError>(())
//! ```
//!
//! # Example
//!
//! ```
//! use layerstack_mesh_export::{
//!     Channel, ColorInput, Faces, FloatInput, Material, Mesh, PackageFile, Primvar, Scene,
//!     StageSettings, Texture, Transform, UpAxis, UsdzProfile, Xform,
//! };
//!
//! let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
//! let normals = [[0.0, 0.0, 1.0]; 4];
//! let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
//! let quad = Mesh::new("Quad", &points, Faces::polygons(&[4], &[0, 1, 2, 3]))
//!     .with_normals(Primvar::vertex(&normals))
//!     .with_uvs(Primvar::vertex(&uvs))
//!     .with_material("Painted");
//!
//! // A glTF-style material: base color texture, packed metal/roughness.
//! let base = Texture::new("textures/base.png");
//! let metal_rough = Texture::new("textures/metal_rough.png");
//! let painted = Material::new("Painted")
//!     .with_diffuse_color(ColorInput::texture(base))
//!     .with_roughness(FloatInput::texture(metal_rough, Channel::G))
//!     .with_metallic(FloatInput::texture(metal_rough, Channel::B));
//!
//! let root = Xform::new("Root")
//!     .with_transform(Transform::from_translation([0.0, 0.0, 2.0]))
//!     .with_mesh(quad);
//! let scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root).with_material(painted);
//!
//! let usda = scene.to_usda()?;
//! assert!(usda.contains("uniform token subdivisionScheme = \"none\""));
//! assert!(usda.contains("rel material:binding = </Root/Materials/Painted>"));
//! let usdc = scene.to_usdc()?;
//! assert_eq!(&usdc[..8], b"PXR-USDC");
//! # let png: &[u8] = b"\x89PNG\r\n\x1a\n";
//! let usdz = scene.to_usdz(
//!     UsdzProfile::Arkit,
//!     &[
//!         PackageFile::new("textures/base.png", png),
//!         PackageFile::new("textures/metal_rough.png", png),
//!     ],
//! )?;
//! assert_eq!(&usdz[..4], b"PK\x03\x04");
//! # Ok::<(), layerstack_mesh_export::ExportError>(())
//! ```
//!
//! # References
//!
//! Conventions follow the OpenUSD reference implementation (v26.08 headers;
//! `pxr/usd/usdGeom/...`), which documents the `UsdGeom` domain schemas
//! that AOUSD Core (§2.1) leaves out of scope:
//! [`mesh.h`](https://openusd.org/dev/api/class_usd_geom_mesh.html) (topology,
//! explicit polygonal `subdivisionScheme`, primvar element counts),
//! [`pointBased.h`](https://openusd.org/dev/api/class_usd_geom_point_based.html)
//! (`normals` vs. `primvars:normals`),
//! [`primvar.h`](https://openusd.org/dev/api/class_usd_geom_primvar.html)
//! (interpolation, indexed primvars),
//! [`metrics.h`](https://openusd.org/dev/api/group___usd_geom_up_axis__group.html)
//! (`upAxis`, `metersPerUnit`),
//! [`xformable.h`](https://openusd.org/dev/api/class_usd_geom_xformable.html)
//! (`xformOpOrder`),
//! [`gprim.h`](https://openusd.org/dev/api/class_usd_geom_gprim.html)
//! (`orientation`, `doubleSided`),
//! [`boundable.h`](https://openusd.org/dev/api/class_usd_geom_boundable.html)
//! (`extent`), and
//! [`pointInstancer.h`](https://openusd.org/dev/api/class_usd_geom_point_instancer.html)
//! (prototypes, per-instance arrays, instance transforms).
//!
//! Materials follow the `UsdPreviewSurface` specification (OpenUSD
//! `docs/spec_usdpreviewsurface.rst`: inputs, fallbacks, color spaces,
//! normal-map remapping, texture orientation), the node definitions in
//! `pxr/usd/plugin/usdShaders/shaders/shaderDefs.usda` (input and output
//! types), and the `UsdShade` headers:
//! [`material.h`](https://openusd.org/dev/api/class_usd_shade_material.html)
//! (`outputs:surface`) and
//! [`materialBindingAPI.h`](https://openusd.org/dev/api/class_usd_shade_material_binding_a_p_i.html)
//! (direct bindings).

#![no_std]

extern crate alloc;

mod build;
mod error;
mod instance;
mod instancer;
mod material;
mod mesh;
mod names;
mod scene;
mod shading;
mod transform;

pub use error::{ExportError, InstancerProblem, MaterialProblem, MeshProblem};
pub use instance::Instance;
pub use instancer::{INSTANCE_NAMES, OrientationPrecision, PROTOTYPES_SCOPE, PointInstancer};
pub use layerstack_usda::writer::Value;
pub use layerstack_usdz::PackageFile;
pub use material::{Channel, ColorInput, FloatInput, MATERIALS_SCOPE, Material, Texture, Wrap};
pub use mesh::{
    CustomAttribute, CustomPrimvar, Faces, FamilyType, Interpolation, MaterialSubset, Mesh,
    Orientation, Primvar, PrimvarData,
};
pub use names::{SiblingNames, sanitize_name};
pub use scene::{Node, Scene, StageSettings, UpAxis, UsdzProfile, Xform};
pub use transform::{NotRigid, SHEAR_TOLERANCE, Transform};

#[cfg(test)]
mod instancer_tests;
#[cfg(test)]
mod material_tests;
#[cfg(test)]
mod tests;
