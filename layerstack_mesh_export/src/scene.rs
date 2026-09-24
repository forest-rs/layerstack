// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene hierarchy and stage settings.

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::writer::{Document, Value};
use layerstack_usdz::{PackageFile, write_usdz};

use crate::{CustomAttribute, ExportError, Material, Mesh, Transform};

/// Path of the root layer inside packages written by [`Scene::to_usdz`].
pub const ROOT_LAYER_PATH: &str = "scene.usda";

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
}

/// A transform group, written as an `Xform` prim.
#[derive(Clone, Debug, PartialEq)]
pub struct Xform<'a> {
    /// Prim name; must be a USD identifier.
    pub name: &'a str,
    /// Local transform relative to the parent prim.
    pub transform: Option<Transform>,
    /// Model kind (e.g. `component`, `assembly`, `group`), written as the
    /// `kind` prim metadata when set.
    pub kind: Option<&'a str>,
    /// Custom attributes.
    pub attributes: Vec<CustomAttribute<'a>>,
    /// Children, in order.
    pub children: Vec<Node<'a>>,
}

impl<'a> Xform<'a> {
    /// An empty group without a transform.
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
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
    pub fn with_kind(mut self, kind: &'a str) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: &'a str, value: Value) -> Self {
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
}

/// A complete export: stage settings, one root prim, which becomes the
/// layer's `defaultPrim`, and the materials meshes bind by name.
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
}

impl<'a> Scene<'a> {
    /// Creates a scene without materials.
    pub fn new(stage: StageSettings, root: Xform<'a>) -> Self {
        Self {
            stage,
            root,
            materials: Vec::new(),
        }
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
    /// [`ExportError::InvalidStage`] for unusable stage settings;
    /// [`ExportError::InvalidMaterial`] for unusable material inputs;
    /// [`ExportError::UnknownMaterial`] for a binding to an undefined
    /// material; and [`ExportError::Usda`] for names or values USDA cannot
    /// represent (including duplicate material names).
    pub fn to_document(&self) -> Result<Document, ExportError> {
        crate::build::document(self)
    }

    /// Serializes the scene as USDA text.
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`].
    pub fn to_usda(&self) -> Result<String, ExportError> {
        Ok(self.to_document()?.to_usda()?)
    }

    /// Serializes the scene and packages it as a generic-profile USDZ.
    ///
    /// The layer is stored as [`ROOT_LAYER_PATH`], first in the archive,
    /// followed by `assets` (e.g. textures) in order. Assets must be USD,
    /// image or audio files ([`layerstack_usdz::writer::MEMBER_EXTENSIONS`]).
    /// The package must be self-contained: every asset path the scene
    /// authors (material texture files, which become `UsdUVTexture`
    /// `inputs:file`, and asset-valued custom attributes, including asset
    /// arrays) must name one of `assets` by its package path, e.g.
    /// `@textures/albedo.png@` (a leading `./` is allowed). Those paths
    /// resolve relative to the root layer, i.e. inside the package,
    /// wherever the package is moved.
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`]; [`ExportError::UnpackagedAsset`] for an
    /// authored asset path with no matching file; [`ExportError::Usdz`] for
    /// invalid or duplicate package paths and unsupported member types.
    pub fn to_usdz(&self, assets: &[PackageFile<'_>]) -> Result<Vec<u8>, ExportError> {
        let mut authored = Vec::new();
        collect_xform_assets(&self.root, &mut authored);
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
        let layer = self.to_usda()?;
        let mut files = Vec::with_capacity(assets.len() + 1);
        files.push(PackageFile::new(ROOT_LAYER_PATH, layer.as_bytes()));
        files.extend_from_slice(assets);
        Ok(write_usdz(&files)?)
    }
}

fn collect_xform_assets<'s>(xform: &'s Xform<'_>, out: &mut Vec<&'s str>) {
    collect_attribute_assets(&xform.attributes, out);
    for child in &xform.children {
        match child {
            Node::Xform(x) => collect_xform_assets(x, out),
            Node::Mesh(m) => collect_attribute_assets(&m.attributes, out),
        }
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
