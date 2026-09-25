// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instanced references to shared prototypes.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use layerstack_usda::writer::Value;

use crate::{CustomAttribute, CustomPrimvar, Primvar, PrimvarData, Transform};

/// One placement of a shared prototype ([`Scene::with_prototype`]),
/// written as an instanceable internal reference.
///
/// The prim has no type of its own: it is `def "<name>"` with
/// `instanceable = true` and a reference to
/// `/<root>/`[`Prototypes`](crate::PROTOTYPES_SCOPE)`/<prototype>`, so its
/// type, geometry, materials and children all come from the prototype,
/// and consumers that support scene graph instancing load that geometry
/// once. [`UsdzProfile::Arkit`](crate::UsdzProfile::Arkit) packages leave
/// `instanceable` out, because Apple's importer also draws each instancing
/// prototype where it is defined; the reference alone composes the same
/// prim. The prototype root's own transform still applies, before
/// [`Self::transform`]; since the instance's `xformOp:transform` replaces
/// the one the reference brings, the exporter authors their product.
///
/// An `Instance` can be a child of an [`Xform`](crate::Xform), a
/// prototype of a [`PointInstancer`](crate::PointInstancer) (so several
/// instancers share one prototype), or part of another shared prototype.
///
/// Spec: AOUSD Core §10.3.2.1 (references), §11 (instancing), §5.1.14
/// (`instanceable`); `UsdGeomXformable` for the transform
/// (<https://openusd.org/dev/api/class_usd_geom_xformable.html>).
///
/// [`Scene::with_prototype`]: crate::Scene::with_prototype
#[derive(Clone, Debug, PartialEq)]
pub struct Instance<'a> {
    /// Prim name; must be a USD identifier.
    pub name: Cow<'a, str>,
    /// Name of the shared prototype it places.
    pub prototype: Cow<'a, str>,
    /// Transform from the prototype root's space to the parent prim's,
    /// applied after the prototype root's own transform.
    pub transform: Option<Transform>,
    /// Primvars of this instance: constant, one value each. They are
    /// inherited by the prototype's geometry unless it authors the same
    /// primvar itself.
    pub primvars: Vec<CustomPrimvar<'a>>,
    /// Custom attributes.
    pub attributes: Vec<CustomAttribute<'a>>,
}

impl<'a> Instance<'a> {
    /// An untransformed instance of the shared prototype named
    /// `prototype`.
    pub fn new(name: impl Into<Cow<'a, str>>, prototype: impl Into<Cow<'a, str>>) -> Self {
        Self {
            name: name.into(),
            prototype: prototype.into(),
            transform: None,
            primvars: Vec::new(),
            attributes: Vec::new(),
        }
    }

    /// Sets the transform.
    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// Adds a constant primvar, written as `primvars:<name>`.
    #[must_use]
    pub fn with_primvar(
        mut self,
        name: impl Into<Cow<'a, str>>,
        primvar: Primvar<'a, PrimvarData<'a>>,
    ) -> Self {
        self.primvars.push(CustomPrimvar {
            name: name.into(),
            primvar,
        });
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: impl Into<Cow<'a, str>>, value: Value) -> Self {
        self.attributes.push(CustomAttribute::new(name, value));
        self
    }
}
