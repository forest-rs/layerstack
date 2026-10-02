// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Volume field bindings and particle attribute selection.
//!
//! OpenUSD: `UsdVolVolume` and `UsdVolParticleField3DGaussianSplat` in 26.8.
//! Field data decoding and rendering belong to the caller. Composition is
//! provided by the core (AOUSD Core §12.2–12.5).

use alloc::{collections::BTreeMap, format, string::String};
use layerstack::{PathId, PropertyKind, TargetPath};

use crate::{
    SchemaEdit,
    usd_vol::{Volume, VolumeEdit},
    view::{forwarded_targets, is_relationship},
};

/// An invalid field binding edit; rejected before collecting any operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeError {
    /// The volume's prim is missing from the stage and this edit.
    MissingPrim(PathId),
    /// The name is not a valid namespaced property identifier.
    InvalidName(String),
    /// An attribute already occupies the requested relationship name.
    WrongPropertyKind(String),
    /// The pseudo-root cannot be a field target.
    InvalidTarget(TargetPath),
}
impl core::fmt::Display for VolumeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid volume field binding: {self:?}")
    }
}
impl core::error::Error for VolumeError {}

fn field_name(name: &str) -> String {
    if name.starts_with("field:") {
        name.into()
    } else {
        format!("field:{name}")
    }
}
fn valid_name(name: &str) -> bool {
    name.split(':').all(layerstack::ident::is_identifier)
}

impl<'a> Volume<'a> {
    /// Whether a relationship named `field:name` exists, even if blocked or empty.
    /// Accepts either a bare name or an already namespaced name.
    ///
    /// OpenUSD: `UsdVolVolume::HasFieldRelationship`.
    #[must_use]
    pub fn has_field_relationship(&self, name: &str) -> bool {
        self.property_path(&field_name(name))
            .is_some_and(|path| is_relationship(&self.scene(), path))
    }

    /// The sole forwarded prim target of `field:name`, or `None` for an absent,
    /// empty, ambiguous or property-targeted relationship. The target need not
    /// exist or derive from a field schema. Accepts bare and namespaced names.
    ///
    /// OpenUSD: `UsdVolVolume::GetFieldPath`.
    #[must_use]
    pub fn field_path(&self, name: &str) -> Option<PathId> {
        let property = self.property_path(&field_name(name))?;
        match forwarded_targets(&self.scene(), property).as_slice() {
            [TargetPath::Prim(path)]
                if !self
                    .scene()
                    .store()
                    .paths()
                    .resolve(*path)
                    .segments()
                    .is_empty() =>
            {
                Some(*path)
            }
            _ => None,
        }
    }

    /// Field bindings sorted by the relationship's final namespace component.
    /// Only relationships with one forwarded prim target contribute. When two
    /// relationships share a basename, the first in composed property order wins.
    ///
    /// OpenUSD: `UsdVolVolume::GetFieldPaths` (`GetBaseName`, `std::map::emplace`).
    #[must_use]
    pub fn field_paths(&self) -> BTreeMap<&'a str, PathId> {
        let scene = self.scene();
        let mut fields = BTreeMap::new();
        for token in scene.stage().property_names(self.path(), scene.store()) {
            let name = scene.store().tokens().resolve(token);
            if name.starts_with("field:")
                && let Some(path) = self.field_path(name)
            {
                fields
                    .entry(name.rsplit(':').next().expect("nonempty field name"))
                    .or_insert(path);
            }
        }
        fields
    }
}
impl VolumeEdit {
    fn checked_field_name(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
    ) -> Result<String, VolumeError> {
        if !edit.exists(self.path()) {
            return Err(VolumeError::MissingPrim(self.path()));
        }
        let name = field_name(name);
        if !valid_name(&name) {
            return Err(VolumeError::InvalidName(name));
        }
        if edit.property_kind(self.path(), &name) == Some(PropertyKind::Attribute) {
            return Err(VolumeError::WrongPropertyKind(name));
        }
        Ok(name)
    }

    /// Creates a custom field relationship or replaces its targets with `target`.
    /// Existing declarations and metadata are preserved. A property target can
    /// forward through another relationship; target existence is not required.
    /// Both bare and already namespaced names are accepted.
    ///
    /// OpenUSD: `UsdVolVolume::CreateFieldRelationship`.
    pub fn create_field_relationship(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        target: TargetPath,
    ) -> Result<&Self, VolumeError> {
        let name = self.checked_field_name(edit, name)?;
        if !edit.valid_field_target(target) {
            return Err(VolumeError::InvalidTarget(target));
        }
        edit.ensure_relationship(self.path(), &name, true);
        edit.set_targets(self.path(), &name, &[target]);
        Ok(self)
    }

    /// Authors explicit empty targets, masking weaker bindings while retaining
    /// the relationship. Returns `false` and authors nothing if it is absent.
    /// Relationships created earlier in this edit can also be blocked.
    ///
    /// OpenUSD: `UsdVolVolume::BlockFieldRelationship`.
    pub fn block_field_relationship(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
    ) -> Result<bool, VolumeError> {
        let name = self.checked_field_name(edit, name)?;
        if edit.property_kind(self.path(), &name).is_none() {
            return Ok(false);
        }
        edit.ensure_relationship(self.path(), &name, true);
        edit.set_targets(self.path(), &name, &[]);
        Ok(true)
    }
}

/// A Gaussian splat channel whose float and half attributes are alternatives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplatData {
    /// Particle centers (`positions` / `positionsh`).
    Positions,
    /// Particle rotations (`orientations` / `orientationsh`).
    Orientations,
    /// Ellipsoid scales (`scales` / `scalesh`).
    Scales,
    /// Particle opacity (`opacities` / `opacitiesh`).
    Opacities,
    /// Spherical harmonics radiance coefficients.
    RadianceCoefficients,
}
impl SplatData {
    /// Every channel, in position, orientation, scale, opacity and radiance order.
    pub const ALL: [Self; 5] = [
        Self::Positions,
        Self::Orientations,
        Self::Scales,
        Self::Opacities,
        Self::RadianceCoefficients,
    ];

    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Positions => ("positions", "positionsh"),
            Self::Orientations => ("orientations", "orientationsh"),
            Self::Scales => ("scales", "scalesh"),
            Self::Opacities => ("opacities", "opacitiesh"),
            Self::RadianceCoefficients => (
                "radiance:sphericalHarmonicsCoefficients",
                "radiance:sphericalHarmonicsCoefficientsh",
            ),
        }
    }
}

/// The attribute selected by OpenUSD's float-versus-half splat convention.
/// Selection does not assert that the chosen attribute contains usable data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplatAttribute {
    /// Full USD attribute name, including any namespace.
    pub name: &'static str,
    /// Composed attribute path, suitable for caller-owned reads and dependencies.
    pub property: layerstack::PropertyPath,
    /// Whether the float alternative has a nonempty array at earliest time.
    pub uses_float: bool,
}

impl crate::usd_vol::ParticleField3DGaussianSplat<'_> {
    /// Selects float when its value at earliest numeric time is a nonempty
    /// array; otherwise selects half, even when neither alternative has data.
    /// Each channel is selected independently, and float wins when both have data.
    /// Later populated samples do not override an empty first float sample.
    ///
    /// This unifies the token and attribute overloads of OpenUSD 26.8's
    /// `UsesFloatPositions`, `UsesFloatOrientations`, `UsesFloatScales`,
    /// `UsesFloatOpacities` and `UsesFloatRadianceCoefficients`. Value resolution
    /// remains the stage's (AOUSD Core §12.3, §12.5).
    #[must_use]
    pub fn attribute_in_use(&self, data: SplatData) -> SplatAttribute {
        let (float_name, half_name) = data.names();
        let uses_float = self
            .read_value_at(
                float_name,
                f64::MIN,
                layerstack::InterpolationType::Held,
                |value, _| Some(value.array_ref().is_some_and(|array| !array.is_empty())),
            )
            .unwrap_or(false);
        let name = if uses_float { float_name } else { half_name };
        SplatAttribute {
            name,
            property: self
                .property_path(name)
                .expect("registered built-in splat attribute"),
            uses_float,
        }
    }
}
