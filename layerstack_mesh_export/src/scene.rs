// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene hierarchy and stage settings.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::writer::{Document, Value};
use layerstack_usdc::writer::write_document;
use layerstack_usdz::{PackageFile, write_usdz};

use crate::{CustomAttribute, ExportError, Instance, Material, Mesh, PointInstancer, Transform};

/// The consumers a USDZ package is written for.
///
/// Both profiles store every member uncompressed and 64-byte aligned, put
/// the root layer first, and require every asset path the scene authors to
/// name a packaged file (`docs/spec_usdz.rst`, "Layout" and "Default
/// Layer"). They differ in the root layer's format and the members allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsdzProfile {
    /// The generic USDZ profile: the root layer is USDA text
    /// (`scene.usda`), and members may be any file type the USDZ
    /// specification allows (USD layers, PNG, JPEG, EXR and AVIF images,
    /// M4A, MP3 and WAV audio; `spec_usdz.rst`, "File Types").
    Generic,
    /// The `ARKit` / AR Quick Look profile: the root layer is binary USDC
    /// (`scene.usdc`) and is the package's only USD layer, because Apple's
    /// implementation reads a single USDC file (`spec_usdz.rst`, "File
    /// Types"; `pxr/usd/usdUtils/usdzPackage.h`,
    /// `UsdUtilsCreateNewARKitUsdzPackage`, which flattens to one `.usdc`
    /// first layer). Other members are limited to PNG and JPEG images and
    /// M4A, MP3 and WAV audio. Point instancers are written as references
    /// ([`Instancing::References`]), whatever [`Scene::instancing`] says,
    /// and no prim is marked `instanceable`, since Apple's stack draws
    /// neither `PointInstancer` instances nor scene graph instancing
    /// correctly.
    Arkit,
}

impl UsdzProfile {
    /// Path of the root layer inside the package.
    pub const fn root_layer_path(self) -> &'static str {
        match self {
            Self::Generic => "scene.usda",
            Self::Arkit => "scene.usdc",
        }
    }

    /// Whether the profile allows a member at `path` besides the root
    /// layer. For [`Self::Generic`], the package writer's own member check
    /// ([`layerstack_usdz::writer::MEMBER_EXTENSIONS`]) applies.
    fn allows_member(self, path: &str) -> bool {
        match self {
            Self::Generic => true,
            Self::Arkit => path
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ["png", "jpg", "jpeg", "m4a", "mp3", "wav"].contains(&ext)),
        }
    }
}

/// How [`PointInstancer`]s are written.
///
/// Spec: `UsdGeomPointInstancer`
/// (<https://openusd.org/dev/api/class_usd_geom_point_instancer.html>);
/// scene graph instancing, AOUSD Core §11 and §5.1.14 (`instanceable`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Instancing {
    /// As `PointInstancer` prims: the most compact form, which OpenUSD and
    /// Hydra-based viewers draw.
    #[default]
    PointInstancers,
    /// As instanced references: each instancer becomes an `Xform` holding
    /// its prototypes in a `class` prim named
    /// [`Prototypes`](crate::PROTOTYPES_SCOPE) (abstract, so not drawn in
    /// place) and one typeless, `instanceable` prim per instance, with an
    /// internal reference to its prototype and a single
    /// `xformOp:transform`: the prototype root's transform followed by the
    /// instance's scale, rotation and translation. Instances are named by
    /// [`PointInstancer::names`], or `<prototype>_<index>` without them;
    /// ids become `int64 instancer:id` ([`INSTANCE_ID`](crate::INSTANCE_ID))
    /// and per-instance primvars become constant primvars on each
    /// instance, while constant ones stay on the group. Shared
    /// prototypes are referenced where they are defined.
    ///
    /// Apple's USD stack (AR Quick Look, `RealityKit`, `SceneKit` and
    /// `ModelIO`) does not implement `UsdGeomPointInstancer`: it draws
    /// each prototype once, where it is defined, and none of the
    /// instances, even though `usdchecker --arkit` accepts the file.
    /// References are what it draws, so [`UsdzProfile::Arkit`] packages
    /// always use this form. They also leave out `instanceable` (here and
    /// on every [`Instance`]): Apple's importer draws each scene graph
    /// instancing prototype (`/__Prototype_N`) once more, at the
    /// prototype's own origin, besides its instances. The references then
    /// compose into ordinary prims; the file is no larger.
    References,
}

/// The stage's up axis.
///
/// Spec: `UsdGeom` stage up axis
/// (<https://openusd.org/dev/api/group___usd_geom_up_axis__group.html>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpAxis {
    /// +Y is up (USD's fallback).
    Y,
    /// +Z is up.
    Z,
}

/// Stage-level metadata that tells consumers how to interpret coordinates.
///
/// Both fields are always written: USD's fallbacks (`Y` up, centimeters)
/// rarely match a kernel's conventions, so relying on them is a silent
/// scale or orientation error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageSettings {
    /// Up axis (`upAxis`).
    pub up_axis: UpAxis,
    /// Length of one scene unit in meters (`metersPerUnit`); `1.0` for
    /// meters, `0.001` for millimeters. Must be finite and positive.
    ///
    /// Spec: `UsdGeom` linear units
    /// (<https://openusd.org/dev/api/group___usd_geom_linear_units__group.html>).
    pub meters_per_unit: f64,
}

impl StageSettings {
    /// Creates stage settings.
    pub fn new(up_axis: UpAxis, meters_per_unit: f64) -> Self {
        Self {
            up_axis,
            meters_per_unit,
        }
    }
}

/// A child of an [`Xform`].
#[derive(Clone, Debug, PartialEq)]
pub enum Node<'a> {
    /// A nested transform group.
    Xform(Xform<'a>),
    /// A mesh.
    Mesh(Mesh<'a>),
    /// Repeated geometry: prototypes placed many times.
    PointInstancer(PointInstancer<'a>),
    /// One placement of a shared prototype, as an instanceable reference.
    Instance(Instance<'a>),
}

/// A transform group, written as an `Xform` prim.
#[derive(Clone, Debug, PartialEq)]
pub struct Xform<'a> {
    /// Prim name; must be a USD identifier.
    pub name: Cow<'a, str>,
    /// Local transform relative to the parent prim.
    pub transform: Option<Transform>,
    /// Model kind (e.g. `component`, `assembly`, `group`), written as the
    /// `kind` prim metadata when set.
    pub kind: Option<Cow<'a, str>>,
    /// Custom attributes.
    pub attributes: Vec<CustomAttribute<'a>>,
    /// Children, in order.
    pub children: Vec<Node<'a>>,
}

impl<'a> Xform<'a> {
    /// An empty group without a transform.
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self {
            name: name.into(),
            transform: None,
            kind: None,
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Sets the local transform.
    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// Sets the model kind.
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<Cow<'a, str>>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: impl Into<Cow<'a, str>>, value: Value) -> Self {
        self.attributes.push(CustomAttribute::new(name, value));
        self
    }

    /// Appends a mesh child.
    #[must_use]
    pub fn with_mesh(mut self, mesh: Mesh<'a>) -> Self {
        self.children.push(Node::Mesh(mesh));
        self
    }

    /// Appends a nested group.
    #[must_use]
    pub fn with_xform(mut self, xform: Self) -> Self {
        self.children.push(Node::Xform(xform));
        self
    }

    /// Appends a point instancer.
    #[must_use]
    pub fn with_point_instancer(mut self, instancer: PointInstancer<'a>) -> Self {
        self.children.push(Node::PointInstancer(instancer));
        self
    }

    /// Appends an instance of a shared prototype.
    #[must_use]
    pub fn with_instance(mut self, instance: Instance<'a>) -> Self {
        self.children.push(Node::Instance(instance));
        self
    }
}

/// A complete export: stage settings, one root prim, which becomes the
/// layer's `defaultPrim`, the materials meshes bind by name, and the
/// shared prototypes instances place by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene<'a> {
    /// Stage metadata.
    pub stage: StageSettings,
    /// The single root prim.
    pub root: Xform<'a>,
    /// Materials, written in order under
    /// `/<root>/`[`Materials`](crate::MATERIALS_SCOPE), inside the root
    /// prim so that a reference to the file brings its materials along.
    pub materials: Vec<Material<'a>>,
    /// Shared prototypes, written once, in order, under the `class` prim
    /// `/<root>/`[`Prototypes`](crate::PROTOTYPES_SCOPE) and placed by
    /// [`Instance`]s (directly, or as a [`PointInstancer`] prototype). A
    /// `class` prim and its descendants are abstract, so the prototypes
    /// are not drawn where they are defined (AOUSD Core §7.6, §12.2.1).
    /// Their names must be unique.
    pub prototypes: Vec<Node<'a>>,
    /// How point instancers are written by [`Self::to_document`],
    /// [`Self::to_usda`], [`Self::to_usdc`] and generic USDZ packages;
    /// [`UsdzProfile::Arkit`] packages always use
    /// [`Instancing::References`].
    pub instancing: Instancing,
}

impl<'a> Scene<'a> {
    /// Creates a scene without materials.
    pub fn new(stage: StageSettings, root: Xform<'a>) -> Self {
        Self {
            stage,
            root,
            materials: Vec::new(),
            prototypes: Vec::new(),
            instancing: Instancing::default(),
        }
    }

    /// Sets how point instancers are written.
    #[must_use]
    pub fn with_instancing(mut self, instancing: Instancing) -> Self {
        self.instancing = instancing;
        self
    }

    /// Adds a shared prototype, which [`Instance`]s name by its prim name.
    #[must_use]
    pub fn with_prototype(mut self, prototype: impl Into<Node<'a>>) -> Self {
        self.prototypes.push(prototype.into());
        self
    }

    /// Adds a material.
    #[must_use]
    pub fn with_material(mut self, material: Material<'a>) -> Self {
        self.materials.push(material);
        self
    }

    /// Validates the scene and maps it to an authored USDA document.
    ///
    /// Mesh buffers are copied into the document here.
    ///
    /// # Errors
    ///
    /// [`ExportError::InvalidMesh`] for inconsistent topology or primvar
    /// sizes, material subsets that do not form their family, or a bound
    /// material whose UV set the mesh lacks;
    /// [`ExportError::InvalidInstancer`] for point instancer prototypes or
    /// per-instance arrays that do not agree, or properties the static
    /// export does not support;
    /// [`ExportError::InvalidStage`] for unusable stage settings;
    /// [`ExportError::InvalidMaterial`] for unusable material inputs;
    /// [`ExportError::UnknownMaterial`] for a binding to an undefined
    /// material; and [`ExportError::Usda`] for names or values USDA cannot
    /// represent (including duplicate material names).
    pub fn to_document(&self) -> Result<Document, ExportError> {
        crate::build::document(self, crate::build::Target::stage(self.instancing))
    }

    /// Serializes the scene as USDA text.
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`].
    pub fn to_usda(&self) -> Result<String, ExportError> {
        Ok(self.to_document()?.to_usda()?)
    }

    /// Serializes the scene as a binary USDC layer.
    ///
    /// The layer holds the same specs and fields as [`Self::to_usda`]'s
    /// text (see [`layerstack_usdc::writer::document`]).
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`]; [`ExportError::Usdc`] if the crate writer
    /// rejects the document.
    pub fn to_usdc(&self) -> Result<Vec<u8>, ExportError> {
        Ok(write_document(&self.to_document()?)?)
    }

    /// Serializes the scene and packages it as a USDZ for `profile`.
    ///
    /// The layer is stored as [`UsdzProfile::root_layer_path`], first in
    /// the archive, followed by `assets` (e.g. textures) in order. Assets
    /// must be member types the profile allows. The package must be
    /// self-contained: every asset path the scene authors (material texture
    /// files, which become `UsdUVTexture` `inputs:file`, and asset-valued
    /// custom attributes, including asset arrays) must name one of `assets`
    /// by its package path, e.g. `@textures/albedo.png@` (a leading `./` is
    /// allowed). Those paths resolve relative to the root layer, i.e.
    /// inside the package, wherever the package is moved.
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`]; [`ExportError::UnpackagedAsset`] for an
    /// authored asset path with no matching file;
    /// [`ExportError::ProfileMember`] for an asset the profile excludes;
    /// [`ExportError::Usdc`] as for [`Self::to_usdc`]; [`ExportError::Usdz`]
    /// for invalid or duplicate package paths and unsupported member types.
    pub fn to_usdz(
        &self,
        profile: UsdzProfile,
        assets: &[PackageFile<'_>],
    ) -> Result<Vec<u8>, ExportError> {
        let mut authored = Vec::new();
        collect_xform_assets(&self.root, &mut authored);
        for prototype in &self.prototypes {
            collect_node_assets(prototype, &mut authored);
        }
        for material in &self.materials {
            authored.extend(material.textures().map(|t| t.file));
        }
        for asset in authored {
            let path = asset.strip_prefix("./").unwrap_or(asset);
            if !assets.iter().any(|file| file.path == path) {
                return Err(ExportError::UnpackagedAsset {
                    asset: asset.into(),
                });
            }
        }
        if let Some(file) = assets.iter().find(|f| !profile.allows_member(f.path)) {
            return Err(ExportError::ProfileMember {
                path: file.path.into(),
                profile,
            });
        }
        let layer = match profile {
            UsdzProfile::Generic => self.to_usda()?.into_bytes(),
            UsdzProfile::Arkit => {
                write_document(&crate::build::document(self, crate::build::Target::ARKIT)?)?
            }
        };
        let mut files = Vec::with_capacity(assets.len() + 1);
        files.push(PackageFile::new(profile.root_layer_path(), &layer));
        files.extend_from_slice(assets);
        Ok(write_usdz(&files)?)
    }
}

fn collect_xform_assets<'s>(xform: &'s Xform<'_>, out: &mut Vec<&'s str>) {
    collect_attribute_assets(&xform.attributes, out);
    for child in &xform.children {
        collect_node_assets(child, out);
    }
}

fn collect_node_assets<'s>(node: &'s Node<'_>, out: &mut Vec<&'s str>) {
    match node {
        Node::Xform(x) => collect_xform_assets(x, out),
        Node::Mesh(m) => collect_attribute_assets(&m.attributes, out),
        Node::PointInstancer(p) => {
            collect_attribute_assets(&p.attributes, out);
            for prototype in &p.prototypes {
                collect_node_assets(prototype, out);
            }
        }
        Node::Instance(i) => collect_attribute_assets(&i.attributes, out),
    }
}

fn collect_attribute_assets<'s>(attributes: &'s [CustomAttribute<'_>], out: &mut Vec<&'s str>) {
    for attribute in attributes {
        collect_value_assets(&attribute.value, out);
    }
}

fn collect_value_assets<'s>(value: &'s Value, out: &mut Vec<&'s str>) {
    match value {
        Value::Asset(path) => out.push(path),
        Value::AssetArray(paths) => out.extend(paths.iter().map(String::as_str)),
        _ => {}
    }
}
