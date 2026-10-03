// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Effective color-space assignment, owned definitions and bounded RGB math.
//!
//! Implements OpenUSD `UsdColorSpaceAPI` ancestor/custom-definition lookup and
//! `GfColorSpace`'s native matrix and gamma/bias model. No OCIO configuration,
//! texture decoding, view/display transform or material execution is implied.
//! AOUSD Core §12.2.5, §12.3, §13.3.2; OpenUSD `usd/colorSpaceAPI.cpp`.

mod math;
use crate::{
    Scene,
    usd::{ColorSpaceApi, ColorSpaceDefinitionApi},
};
use alloc::{sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyPath, Time};

/// Built-in color-space names accepted by OpenUSD 26.08 `GfColorSpace`.
pub const BUILTIN_COLOR_SPACES: &[&str] = &[
    "lin_ap1_scene",
    "lin_ap0_scene",
    "lin_rec709_scene",
    "lin_p3d65_scene",
    "lin_rec2020_scene",
    "lin_adobergb_scene",
    "lin_ciexyzd65_scene",
    "srgb_rec709_scene",
    "g24_rec709_scene",
    "g22_rec709_scene",
    "g18_rec709_scene",
    "srgb_ap1_scene",
    "g22_ap1_scene",
    "srgb_p3d65_scene",
    "g22_adobergb_scene",
    "identity",
    "data",
    "raw",
    "unknown",
];
/// Where an effective color-space name originated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorSpaceSource {
    /// Authored `colorSpace` metadata on this attribute, even when empty/unknown.
    Attribute(PropertyPath),
    /// Nearest nonempty `ColorSpaceAPI` assignment on this prim or ancestor.
    Prim(PathId),
    /// No authored attribute metadata or ancestor assignment exists.
    Unassigned,
}
/// Owned effective assignment, preserving the source that decided it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorSpaceAssignment {
    /// The effective name. An authored empty token remains an empty string.
    pub name: Arc<str>,
    /// Source used to resolve this assignment.
    pub source: ColorSpaceSource,
}
/// Recovery errors for assignment, definition and numeric transformation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorError {
    /// The requested prim does not exist.
    MissingPrim(PathId),
    /// The requested property does not exist or is not an attribute.
    MissingAttribute(PropertyPath),
    /// An assigned name is neither built in nor defined in the applicable scope.
    UnknownColorSpace {
        /// Name that failed validation.
        name: Arc<str>,
        /// Prim from which lookup began.
        prim: PathId,
    },
    /// A definition attribute is missing, incompatible or nonfinite.
    InvalidAttribute {
        /// Definition prim.
        prim: PathId,
        /// Multiple-apply instance.
        instance: Arc<str>,
        /// Invalid parameter name.
        attribute: &'static str,
    },
    /// Chromaticities do not define a finite invertible RGB-to-XYZ matrix.
    InvalidChromaticities,
    /// Gamma/bias cannot define a finite continuous transfer function.
    InvalidTransferFunction,
    /// A matrix cannot be inverted or contains nonfinite entries.
    InvalidMatrix,
    /// An input or output color is nonfinite (including undefined power results).
    InvalidColor,
}
impl core::fmt::Display for ColorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid color-space input: {self:?}")
    }
}
impl core::error::Error for ColorError {}

/// Native RGB definition, captured independently of a stage.
/// Matrices act on RGB column vectors, as OpenUSD's nanocolor implementation;
/// this differs from USD geometry's row-vector convention. Built-in AP0/AP1
/// chromaticities already include OpenUSD's Bradford adaptation to D65.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSpaceDefinition {
    /// Color-space assignment name (custom names can differ from API instances).
    pub name: Arc<str>,
    /// Optional originating prim and multiple-apply API instance.
    pub source: Option<(PathId, Arc<str>)>,
    /// Red, green, blue and white-point xy chromaticities.
    chromaticities: [[f32; 2]; 4],
    /// Encoding exponent. One denotes linear encoding.
    gamma: f32,
    /// Bias of the nonlinear branch, zero for an ordinary gamma curve.
    linear_bias: f32,
    rgb_to_xyz: [[f32; 3]; 3],
}
impl ColorSpaceDefinition {
    /// Constructs a checked native definition using SMPTE RP 177-1993 primary
    /// normalization, matching `GfColorSpace`'s nanocolor implementation.
    pub fn from_chromaticities(
        name: impl Into<Arc<str>>,
        chromaticities: [[f32; 2]; 4],
        gamma: f32,
        linear_bias: f32,
    ) -> Result<Self, ColorError> {
        math::transfer(gamma, linear_bias)?;
        let rgb_to_xyz = math::matrix(chromaticities)?;
        Ok(Self {
            name: name.into(),
            source: None,
            chromaticities,
            gamma,
            linear_bias,
            rgb_to_xyz,
        })
    }
    /// Returns a named built-in, or `None` when the name is not registered.
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        math::builtin(name)
    }
    /// Red, green, blue and white-point xy chromaticities.
    #[must_use]
    pub fn chromaticities(&self) -> [[f32; 2]; 4] {
        self.chromaticities
    }
    /// Encoding exponent.
    #[must_use]
    pub fn gamma(&self) -> f32 {
        self.gamma
    }
    /// Bias of the nonlinear encoding branch.
    #[must_use]
    pub fn linear_bias(&self) -> f32 {
        self.linear_bias
    }
    /// RGB-to-XYZ matrix, before encoding/decoding transfer functions.
    #[must_use]
    pub fn rgb_to_xyz(&self) -> [[f32; 3]; 3] {
        self.rgb_to_xyz
    }
    /// Compiles an owned source-to-destination transform once for repeated use.
    pub fn transform_to(&self, destination: &Self) -> Result<ColorTransform, ColorError> {
        let matrix = math::multiply(math::inverse(destination.rgb_to_xyz)?, self.rgb_to_xyz);
        Ok(ColorTransform {
            matrix,
            source_curve: math::transfer(self.gamma, self.linear_bias)?,
            destination_curve: math::transfer(destination.gamma, destination.linear_bias)?,
        })
    }
}
/// Owned native color transform: source decode, matrix, destination encode.
/// Alpha is unassociated and preserved; gamut clipping and premultiplication
/// are explicit consumer operations. No implicit chromatic adaptation is added.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorTransform {
    matrix: [[f32; 3]; 3],
    source_curve: math::Curve,
    destination_curve: math::Curve,
}
impl ColorTransform {
    /// Linear RGB matrix. Multiply RGB column vectors after source decoding.
    #[must_use]
    pub fn matrix(&self) -> [[f32; 3]; 3] {
        self.matrix
    }
    /// Converts one RGB value. Negative gamma-encoded values follow OpenUSD's
    /// linear segment; undefined nonlinear powers return an explicit error.
    pub fn convert_rgb(&self, rgb: [f32; 3]) -> Result<[f32; 3], ColorError> {
        if rgb.iter().any(|v| !v.is_finite()) {
            return Err(ColorError::InvalidColor);
        }
        let linear = rgb.map(|v| self.source_curve.decode(v));
        let transformed: [f32; 3] = core::array::from_fn(|i| {
            self.matrix[i][0] * linear[0]
                + self.matrix[i][1] * linear[1]
                + self.matrix[i][2] * linear[2]
        });
        let out = transformed.map(|v| self.destination_curve.encode(v));
        if out.iter().any(|v| !v.is_finite()) {
            Err(ColorError::InvalidColor)
        } else {
            Ok(out)
        }
    }
    /// Converts unassociated RGBA and preserves alpha exactly.
    pub fn convert_rgba(&self, rgba: [f32; 4]) -> Result<[f32; 4], ColorError> {
        if !rgba[3].is_finite() {
            return Err(ColorError::InvalidColor);
        }
        let rgb = self.convert_rgb([rgba[0], rgba[1], rgba[2]])?;
        Ok([rgb[0], rgb[1], rgb[2], rgba[3]])
    }
    /// Converts a batch into a new owned buffer. Error leaves the input untouched.
    pub fn convert_rgb_slice(&self, rgb: &[[f32; 3]]) -> Result<Vec<[f32; 3]>, ColorError> {
        rgb.iter().map(|&v| self.convert_rgb(v)).collect()
    }
}
impl Scene<'_> {
    /// Resolves the nearest nonempty API assignment. An unknown nearest name
    /// stops inheritance and errors, as OpenUSD does (without its warning-only
    /// failure). Definitions are validated starting at the assigning ancestor,
    /// so descendant definitions cannot make an ancestor's name valid.
    pub fn compute_color_space_name(
        &self,
        path: PathId,
    ) -> Result<ColorSpaceAssignment, ColorError> {
        if !self.stage().has_prim(path) {
            return Err(ColorError::MissingPrim(path));
        }
        let mut current = Some(path);
        while let Some(prim) = current {
            if let Some(api) = ColorSpaceApi::get(self, prim)
                && let Some(name) = api.color_space_name()
                && !name.is_empty()
            {
                if !BUILTIN_COLOR_SPACES.contains(&name)
                    && find_definition(self, prim, name).is_none()
                {
                    return Err(ColorError::UnknownColorSpace {
                        name: Arc::from(name),
                        prim,
                    });
                }
                return Ok(ColorSpaceAssignment {
                    name: Arc::from(name),
                    source: ColorSpaceSource::Prim(prim),
                });
            }
            current = self.parent(prim);
        }
        Ok(ColorSpaceAssignment {
            name: Arc::from(""),
            source: ColorSpaceSource::Unassigned,
        })
    }
    /// Resolves authored attribute `colorSpace` metadata before ancestor APIs.
    /// Authored names bypass validation, matching `ComputeColorSpaceName(attr)`.
    /// Empty authored metadata also suppresses inheritance. This registry does
    /// not retain schema-property metadata, so OpenUSD's final prim-definition
    /// `colorSpace` fallback is unavailable; an unassigned result says so.
    pub fn compute_attribute_color_space_name(
        &self,
        path: PropertyPath,
    ) -> Result<ColorSpaceAssignment, ColorError> {
        let declaration = self
            .stage()
            .resolve_property_declaration(path.prim_path(), path.property())
            .map(|v| v.kind)
            .or_else(|| {
                self.stage()
                    .property_definition_ref(path.prim_path(), path.property())
                    .map(|v| v.kind)
            });
        if declaration != Some(PropertyKind::Attribute) {
            return Err(ColorError::MissingAttribute(path));
        }
        if let Some(key) = self.store().tokens().lookup("colorSpace")
            && let Some(value) =
                self.stage()
                    .resolve_property_metadata(path.prim_path(), path.property(), key)
            && let layerstack::ResolvedValue::Scalar(value) = value.value
            && let Some(name) = crate::value::read_token(&value, self.store().tokens())
        {
            return Ok(ColorSpaceAssignment {
                name: Arc::from(name),
                source: ColorSpaceSource::Attribute(path),
            });
        }
        self.compute_color_space_name(path.prim_path())
    }
    /// Finds a built-in or nearest matching named custom definition. Built-ins
    /// win even if an API illegally redefines one. Custom definition identity
    /// comes from its `name` attribute, not its multiple-apply instance name.
    /// OpenUSD reads these definition attributes at default time.
    pub fn color_space_definition(
        &self,
        path: PathId,
        name: &str,
    ) -> Result<ColorSpaceDefinition, ColorError> {
        if let Some(definition) = ColorSpaceDefinition::builtin(name) {
            return Ok(definition);
        }
        let api =
            find_definition(self, path, name).ok_or_else(|| ColorError::UnknownColorSpace {
                name: Arc::from(name),
                prim: path,
            })?;
        api.capture_definition(Time::Default)
    }
}
fn find_definition<'a>(
    scene: &Scene<'a>,
    path: PathId,
    name: &str,
) -> Option<ColorSpaceDefinitionApi<'a>> {
    let mut current = Some(path);
    while let Some(prim) = current {
        for api in ColorSpaceDefinitionApi::instances(scene, prim) {
            if api.name() == Some(name) {
                return Some(api);
            }
        }
        current = scene.parent(prim);
    }
    None
}
impl ColorSpaceApi<'_> {
    /// Effective assignment, including namespace ancestors.
    pub fn compute_color_space_name(&self) -> Result<ColorSpaceAssignment, ColorError> {
        self.scene().compute_color_space_name(self.path())
    }
    /// Effective owned definition, or `None` when no color space is assigned.
    pub fn compute_color_space(&self) -> Result<Option<ColorSpaceDefinition>, ColorError> {
        let assignment = self.compute_color_space_name()?;
        if assignment.name.is_empty() {
            Ok(None)
        } else {
            self.scene()
                .color_space_definition(self.path(), &assignment.name)
                .map(Some)
        }
    }
}
impl ColorSpaceDefinitionApi<'_> {
    /// Captures numeric definition attributes at an explicit time. Default-time
    /// capture matches `ComputeColorSpaceFromDefinitionAttributes`; explicit
    /// numeric time is also useful for caller-controlled animation sampling.
    pub fn capture_definition(&self, time: Time) -> Result<ColorSpaceDefinition, ColorError> {
        let error = |attribute| ColorError::InvalidAttribute {
            prim: self.path(),
            instance: Arc::from(self.instance()),
            attribute,
        };
        let chroma = |name, default: Option<[f32; 2]>, at: Option<[f32; 2]>| {
            let value = if time == Time::Default { default } else { at };
            value
                .filter(|v| v.iter().all(|v| v.is_finite()))
                .ok_or_else(|| error(name))
        };
        let (code, interpolation) = match time {
            Time::Default => (0., layerstack::InterpolationType::Held),
            Time::At {
                code,
                interpolation,
            } => {
                if !code.is_finite() {
                    return Err(error("time"));
                }
                (code, interpolation)
            }
        };
        let chromaticities = [
            chroma(
                "redChroma",
                self.red_chroma(),
                self.red_chroma_at(code, interpolation),
            )?,
            chroma(
                "greenChroma",
                self.green_chroma(),
                self.green_chroma_at(code, interpolation),
            )?,
            chroma(
                "blueChroma",
                self.blue_chroma(),
                self.blue_chroma_at(code, interpolation),
            )?,
            chroma(
                "whitePoint",
                self.white_point(),
                self.white_point_at(code, interpolation),
            )?,
        ];
        let gamma = if time == Time::Default {
            self.gamma()
        } else {
            self.gamma_at(code, interpolation)
        }
        .ok_or_else(|| error("gamma"))?;
        let bias = if time == Time::Default {
            self.linear_bias()
        } else {
            self.linear_bias_at(code, interpolation)
        }
        .ok_or_else(|| error("linearBias"))?;
        let mut definition = ColorSpaceDefinition::from_chromaticities(
            self.name().ok_or_else(|| error("name"))?,
            chromaticities,
            gamma,
            bias,
        )?;
        definition.source = Some((self.path(), Arc::from(self.instance())));
        Ok(definition)
    }
}
#[cfg(test)]
mod tests;
