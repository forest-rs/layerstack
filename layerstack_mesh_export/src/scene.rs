// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene hierarchy and stage settings.

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::writer::{Document, Value};
use layerstack_usdz::{PackageFile, write_usdz};

use crate::{CustomAttribute, ExportError, Mesh, Transform};

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

/// A complete export: stage settings and one root prim, which becomes the
/// layer's `defaultPrim`.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene<'a> {
    /// Stage metadata.
    pub stage: StageSettings,
    /// The single root prim.
    pub root: Xform<'a>,
}

impl<'a> Scene<'a> {
    /// Creates a scene.
    pub fn new(stage: StageSettings, root: Xform<'a>) -> Self {
        Self { stage, root }
    }

    /// Validates the scene and maps it to an authored USDA document.
    ///
    /// Mesh buffers are copied into the document here.
    ///
    /// # Errors
    ///
    /// [`ExportError::InvalidMesh`] for inconsistent topology or primvar
    /// sizes, [`ExportError::InvalidStage`] for unusable stage settings, and
    /// [`ExportError::Usda`] for names or values USDA cannot represent.
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

    /// Serializes the scene and packages it as USDZ.
    ///
    /// The layer is stored as [`ROOT_LAYER_PATH`], first in the archive,
    /// followed by `assets` (e.g. textures) in order. Asset-valued
    /// attributes should refer to those files by their package path, e.g.
    /// `@textures/albedo.png@`.
    ///
    /// # Errors
    ///
    /// See [`Self::to_document`]; [`ExportError::Usdz`] for invalid or
    /// duplicate asset paths.
    pub fn to_usdz(&self, assets: &[PackageFile<'_>]) -> Result<Vec<u8>, ExportError> {
        let layer = self.to_usda()?;
        let mut files = Vec::with_capacity(assets.len() + 1);
        files.push(PackageFile::new(ROOT_LAYER_PATH, layer.as_bytes()));
        files.extend_from_slice(assets);
        Ok(write_usdz(&files)?)
    }
}
