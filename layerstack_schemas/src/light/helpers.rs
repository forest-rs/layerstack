// Copyright 2016 Pixar
// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: LicenseRef-TOST-1.0

//! Bounded `UsdLux` computations and transactional authoring helpers.
//! AOUSD Core §6.3, §7.6.4.1.2, §12.3–12.5; OpenUSD 26.8 `UsdLux`.

use crate::{
    PrimView, SchemaEdit,
    usd::CollectionApi,
    usd_geom::XformableEdit,
    usd_lux::{DomeLightEdit, LightApi, LightApiEdit, LightFilter, LightFilterEdit},
    xform::XformOpType,
    xform_edit::{XformOpError, XformOpPrecision},
};
use alloc::{format, string::String, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyType, Value};

/// Why blackbody conversion cannot be computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlackbodyError {
    /// Kelvin is NaN or infinite. Finite temperatures are clamped to 1000–10000 K.
    NonFiniteTemperature,
}
impl core::fmt::Display for BlackbodyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("blackbody temperature must be finite")
    }
}
impl core::error::Error for BlackbodyError {}

// OpenUSD `blackbody.cpp`, Walker spectrum table, padded for Catmull–Rom.
const BLACKBODY_RGB: [[f32; 3]; 22] = [
    [1.000000, 0.027490, 0.000000],
    [1.000000, 0.027490, 0.000000],
    [1.000000, 0.149664, 0.000000],
    [1.000000, 0.256644, 0.008095],
    [1.000000, 0.372033, 0.067450],
    [1.000000, 0.476725, 0.153601],
    [1.000000, 0.570376, 0.259196],
    [1.000000, 0.653480, 0.377155],
    [1.000000, 0.726878, 0.501606],
    [1.000000, 0.791543, 0.628050],
    [1.000000, 0.848462, 0.753228],
    [1.000000, 0.898581, 0.874905],
    [1.000000, 0.942771, 0.991642],
    [0.906947, 0.890456, 1.000000],
    [0.828247, 0.841838, 1.000000],
    [0.765791, 0.801896, 1.000000],
    [0.715255, 0.768579, 1.000000],
    [0.673683, 0.740423, 1.000000],
    [0.638992, 0.716359, 1.000000],
    [0.609681, 0.695588, 1.000000],
    [0.609681, 0.695588, 1.000000],
    [0.609681, 0.695588, 1.000000],
];

/// Converts Kelvin to nonnegative linear Rec.709/sRGB RGB with unit luminance.
///
/// Matches OpenUSD's `UsdLuxBlackbodyTemperatureAsRgb` table, Catmull–Rom
/// interpolation and float rounding. Finite temperatures clamp to 1000–10000 K.
/// The interpolated RGB is normalized before negative components are clamped,
/// so the final luminance can slightly exceed one. 6500 K is not pure white.
/// Engines must convert this color to their rendering color space if different.
/// Nonfinite temperatures return [`BlackbodyError::NonFiniteTemperature`].
#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded spline indexing and OpenUSD float rounding"
)]
pub fn blackbody_temperature_rgb(kelvin: f32) -> Result<[f32; 3], BlackbodyError> {
    if !kelvin.is_finite() {
        return Err(BlackbodyError::NonFiniteTemperature);
    }
    let x = ((kelvin - 1000.) / 9000.).clamp(0., 1.) * 18.;
    // x is finite, nonnegative, and at most 18; truncation is floor here.
    let segment = x as usize;
    let u = x - segment as f32;
    let [k0, k1, k2, k3] = core::array::from_fn::<_, 4, _>(|i| BLACKBODY_RGB[segment + i]);
    let rgb: [f32; 3] = core::array::from_fn(|i| {
        let a = ((-0.5 * k0[i] + 1.5 * k1[i]) + -1.5 * k2[i]) + 0.5 * k3[i];
        let b = ((k0[i] + -2.5 * k1[i]) + 2. * k2[i]) + -0.5 * k3[i];
        let c = ((-0.5 * k0[i] + 0. * k1[i]) + 0.5 * k2[i]) + 0. * k3[i];
        let d = ((0. * k0[i] + k1[i]) + 0. * k2[i]) + 0. * k3[i];
        ((a * u + b) * u + c) * u + d
    });
    let luma = (rgb[0] * 0.2126 + rgb[1] * 0.7152) + rgb[2] * 0.0722;
    // GfVec3f::operator/= computes a double reciprocal and rounds each product.
    let inverse = 1. / f64::from(luma);
    Ok(rgb.map(|c| ((f64::from(c) * inverse) as f32).max(0.)))
}

/// Selected uniform shader identifier and the property reads that chose it.
/// Identifiers are not executable shaders; engines resolve their own implementations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderIdSelection {
    /// Selected token; empty means no shader identifier was available.
    pub id: String,
    /// Winning renderer context; `None` means the default property was selected.
    pub context: Option<String>,
    /// Selected property name, including the default name when the ID is empty.
    pub property: String,
    /// Properties read in priority order, including missing/empty earlier contexts.
    /// These names provide dependencies for an engine's incremental graph.
    pub consulted_properties: Vec<String>,
}
fn shader_name(base: &str, context: &str) -> String {
    if context.is_empty() {
        base.into()
    } else {
        format!("{context}:{base}")
    }
}
// Typed default-time token reads skip incompatible dense opinions. A block
// encountered before a compatible token suppresses both weaker values and the
// schema fallback (`UsdAttribute::Get<TfToken>`, AOUSD Core §12.3.6).
fn shader_token<'a>(prim: &PrimView<'a>, property: &str) -> Option<&'a str> {
    let blocked = prim
        .property_path(property)
        .and_then(|path| prim.scene().stage().explain_property_path(path))
        .and_then(|opinions| {
            opinions
                .iter()
                .filter_map(|opinion| opinion.value.default_value())
                .find(|value| matches!(value, Value::Token(_) | Value::Blocked))
        })
        == Some(&Value::Blocked);
    if blocked {
        None
    } else {
        prim.read_value(property, crate::value::read_token)
    }
}
fn select_shader(prim: &PrimView<'_>, base: &str, contexts: &[&str]) -> ShaderIdSelection {
    let mut consulted_properties = Vec::new();
    for &context in contexts {
        let property = shader_name(base, context);
        consulted_properties.push(property.clone());
        if let Some(id) = shader_token(prim, &property).filter(|id| !id.is_empty()) {
            return ShaderIdSelection {
                id: id.into(),
                context: (!context.is_empty()).then(|| context.into()),
                property,
                consulted_properties,
            };
        }
    }
    if consulted_properties.last().is_none_or(|name| name != base) {
        consulted_properties.push(base.into());
    }
    ShaderIdSelection {
        id: shader_token(prim, base).unwrap_or_default().into(),
        context: None,
        property: base.into(),
        consulted_properties,
    }
}
impl<'a> LightApi<'a> {
    /// Selects the first nonempty renderer-context shader ID, then the default.
    /// Reads uniform attributes at default time, matching OpenUSD `GetShaderId`.
    /// Incompatible token opinions are skipped; authored blocks suppress fallback.
    #[must_use]
    pub fn select_shader_id(&self, contexts: &[&str]) -> ShaderIdSelection {
        select_shader(self, "light:shaderId", contexts)
    }
    /// Collection controlling which objects receive illumination from this light.
    /// The builtin collection includes the root by default.
    #[must_use]
    pub fn light_link_collection(&self) -> CollectionApi<'a> {
        CollectionApi::from_view(PrimView::new(self.scene(), self.path()), "lightLink")
    }
    /// Collection controlling which objects cast shadows for this light.
    #[must_use]
    pub fn shadow_link_collection(&self) -> CollectionApi<'a> {
        CollectionApi::from_view(PrimView::new(self.scene(), self.path()), "shadowLink")
    }
}
impl<'a> LightFilter<'a> {
    /// Selects the first nonempty renderer-context filter shader ID, then default.
    /// Reads uniform attributes at default time, matching OpenUSD `GetShaderId`.
    /// Incompatible token opinions are skipped; authored blocks suppress fallback.
    #[must_use]
    pub fn select_shader_id(&self, contexts: &[&str]) -> ShaderIdSelection {
        select_shader(self, "lightFilter:shaderId", contexts)
    }
    /// Collection controlling which objects receive this filter's effect.
    #[must_use]
    pub fn filter_link_collection(&self) -> CollectionApi<'a> {
        CollectionApi::from_view(PrimView::new(self.scene(), self.path()), "filterLink")
    }
}

/// Invalid light helper authoring; validation failures collect no operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightHelperError {
    /// The requested prim does not exist in the scene or this edit.
    MissingPrim(PathId),
    /// The renderer context is not empty or a valid namespaced identifier.
    InvalidContext(String),
    /// The property is a relationship or not a scalar token attribute.
    WrongPropertyType {
        /// Source light/filter path.
        prim: PathId,
        /// Requested shader-ID property.
        property: String,
    },
    /// The dome orientation op is missing or is not a scalar floating-point attribute.
    InvalidOrientationOp(PathId),
    /// Transform authoring failed.
    Transform(XformOpError),
}
impl core::fmt::Display for LightHelperError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid light helper edit: {self:?}")
    }
}
impl core::error::Error for LightHelperError {}
fn author_shader(
    edit: &mut SchemaEdit<'_>,
    prim: PathId,
    base: &str,
    context: &str,
    id: &str,
) -> Result<(), LightHelperError> {
    if !context.is_empty() && !context.split(':').all(layerstack::ident::is_identifier) {
        return Err(LightHelperError::InvalidContext(context.into()));
    }
    if !edit.exists(prim) || edit.is_pseudo_root(prim) {
        return Err(LightHelperError::MissingPrim(prim));
    }
    let property = shader_name(base, context);
    let kind = edit.property_kind(prim, &property);
    let ty = edit.attribute_type(prim, &property);
    if kind.is_some_and(|kind| kind != PropertyKind::Attribute)
        || ty
            .as_ref()
            .is_some_and(|ty| ty.is_array || ty.type_name.as_ref() != "token")
    {
        return Err(LightHelperError::WrongPropertyType { prim, property });
    }
    if ty.is_none() {
        let empty = edit.tokens().intern("");
        edit.create_uniform_attribute(
            prim,
            &property,
            PropertyType::new("token", false, Value::Token(empty)),
        );
    }
    let id = edit.tokens().intern(id);
    edit.set_value(prim, &property, None, Value::Token(id));
    Ok(())
}
impl LightApiEdit {
    /// Sets a renderer-context shader ID, using `light:shaderId` for empty context.
    /// Newly declared attributes are non-custom uniform scalar tokens. Existing
    /// compatible declarations retain metadata, custom qualifier and variability.
    /// Missing prims, invalid contexts and incompatible declarations collect no edits.
    pub fn set_shader_id_for_render_context(
        &self,
        edit: &mut SchemaEdit<'_>,
        context: &str,
        id: &str,
    ) -> Result<&Self, LightHelperError> {
        author_shader(edit, self.path(), "light:shaderId", context, id)?;
        Ok(self)
    }
}
impl LightFilterEdit {
    /// Sets a renderer-context filter shader ID, using default for empty context.
    /// Newly declared attributes are non-custom uniform scalar tokens. Existing
    /// compatible declarations retain metadata, custom qualifier and variability.
    /// Missing prims, invalid contexts and incompatible declarations collect no edits.
    pub fn set_shader_id_for_render_context(
        &self,
        edit: &mut SchemaEdit<'_>,
        context: &str,
        id: &str,
    ) -> Result<&Self, LightHelperError> {
        author_shader(edit, self.path(), "lightFilter:shaderId", context, id)?;
        Ok(self)
    }
}

/// Whether legacy dome orientation authoring changed the edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomeOrientation {
    /// Appended a float rotate-X op of 90 degrees, preserving other ops and reset.
    Authored,
    /// The orientation op is already ordered, including an inverse occurrence.
    AlreadyOriented,
    /// Stage up-axis is not Z; no operation is required or authored.
    NotZUp,
}
impl DomeLightEdit {
    /// Orients this legacy dome to stage Z-up, matching `OrientToStageUpAxis`.
    /// Appends `xformOp:rotateX:orientToStageUpAxis` at 90 degrees only when absent.
    /// Existing valid ordered occurrences are retained without inspecting their value.
    /// Y-up and other non-Z axes are explicit no-ops. Keeps reset/order unchanged.
    /// Errors for a missing prim or an missing or incompatible orientation property
    /// are returned before collecting edits. `DomeLight1` uses its pole-axis input.
    pub fn orient_to_stage_up_axis(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<DomeOrientation, LightHelperError> {
        if !edit.exists(self.path()) || edit.is_pseudo_root(self.path()) {
            return Err(LightHelperError::MissingPrim(self.path()));
        }
        if edit.stage_up_axis() != "Z" {
            return Ok(DomeOrientation::NotZUp);
        }
        let xform = XformableEdit::from_path(self.path());
        const NAME: &str = "xformOp:rotateX:orientToStageUpAxis";
        let ordered = xform
            .order(edit)
            .iter()
            .any(|op| op.strip_prefix("!invert!").unwrap_or(op) == NAME);
        if ordered && edit.attribute_type(self.path(), NAME).is_none() {
            return Err(LightHelperError::InvalidOrientationOp(self.path()));
        }
        if edit
            .property_kind(self.path(), NAME)
            .is_some_and(|kind| kind != PropertyKind::Attribute)
            || edit.attribute_type(self.path(), NAME).is_some_and(|ty| {
                ty.is_array || !matches!(ty.type_name.as_ref(), "half" | "float" | "double")
            })
        {
            return Err(LightHelperError::InvalidOrientationOp(self.path()));
        }
        if ordered {
            return Ok(DomeOrientation::AlreadyOriented);
        }
        xform
            .add_op(
                edit,
                XformOpType::RotateX,
                XformOpPrecision::Float,
                Some("orientToStageUpAxis"),
                false,
            )
            .map_err(LightHelperError::Transform)?
            .set(edit, 90.)
            .map_err(LightHelperError::Transform)?;
        Ok(DomeOrientation::Authored)
    }
}
