// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checked constant groups using USD units and semantics, with assets still inspectable.
use super::{LightInput, LightInputStatus, LightInputs, LightKind, LightParameterError};
use alloc::vec::Vec;
use layerstack::{TargetPath, Value};

/// Constant `ShapingAPI` controls. No range clamping or angular conversion is performed.
#[derive(Clone, Debug, PartialEq)]
pub struct LightShaping {
    /// Exponent controlling focus around the emission axis.
    pub focus: f32,
    /// Color tint in the scene's working color space.
    pub focus_tint: [f32; 3],
    /// Angular cutoff off the primary axis, in degrees (a half-angle).
    pub cone_angle: f32,
    /// Fraction of the cone over which emission fades inward from its edge.
    pub cone_softness: f32,
    /// IES asset input with readiness, connections and source evidence. An
    /// unassigned file remains `Unavailable`; it is not a failed scalar group.
    pub ies_file: LightInput,
    /// Authored `ies:angleScale`; interpretation follows `UsdLuxShapingAPI`.
    pub ies_angle_scale: f32,
    /// Whether to normalize the IES profile's emission.
    pub ies_normalize: bool,
}
/// Constant `ShadowAPI` controls, preserving USD's negative distance sentinels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightShadow {
    /// Whether the light casts shadows.
    pub enable: bool,
    /// Nonphysical shadow color, in the scene's working color space.
    pub color: [f32; 3],
    /// Surface-to-occluder distance in stage units; -1 means unlimited.
    pub distance: f32,
    /// Falloff zone in stage units; nonpositive values disable falloff.
    pub falloff: f32,
    /// Exponential falloff control, used with distance and falloff.
    pub falloff_gamma: f32,
}
/// USD environment texture parameterization; resource decoding belongs to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightTextureFormat {
    /// Determine the layout from the asset.
    Automatic,
    /// Latitude/longitude parameterization.
    Latlong,
    /// Orthographic reflection from a sphere.
    MirroredBall,
    /// Radial distance maps linearly to angle.
    Angular,
    /// Six cube faces in a vertical cross.
    CubeMapVerticalCross,
}
/// Starting pole alignment of `DomeLight_1`, before its transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomePoleAxis {
    /// Follow the stage's up axis. This correction is local to the dome,
    /// and is not inherited by namespace children.
    Scene,
    /// Align the top pole with +Y.
    Y,
    /// Align the top pole with +Z.
    Z,
}
/// Owned dome-specific inputs. The full transform and stage conventions remain
/// available on `LightInputs`; no coordinate-system or color conversion is baked in.
#[derive(Clone, Debug, PartialEq)]
pub struct LightEnvironment {
    /// Texture input, including missing values or shader execution requirements.
    pub texture_file: LightInput,
    /// Checked USD parameterization.
    pub texture_format: LightTextureFormat,
    /// `DomeLight_1` pole alignment. Legacy `DomeLight` has no such attribute;
    /// its orientation is represented solely by the captured transform.
    pub pole_axis: Option<DomePoleAxis>,
    /// Forwarded portal targets, preserving missing targets for diagnosis.
    pub portals: Vec<TargetPath>,
}
impl LightInputs {
    fn input_capture(&self, name: &str) -> Result<LightInput, LightParameterError> {
        self.input(name)
            .cloned()
            .ok_or_else(|| LightParameterError {
                light: self.light,
                input: name.into(),
                status: LightInputStatus::Unavailable,
                invalid_value: false,
            })
    }
    /// Extracts a finite color constant without converting its working color space.
    pub fn color(&self, name: &str) -> Result<[f32; 3], LightParameterError> {
        self.parameter(name, |v| match v {
            Value::Vec3f(c) if c.iter().all(|v| v.is_finite()) => Some(*c),
            _ => None,
        })
    }
    /// Extracts composed `ShapingAPI` constants when that API is applied or built in.
    /// Shader-driven scalar controls return a localized error; the IES asset
    /// remains an inspectable input regardless of its readiness.
    /// AOUSD Core §12.3–12.4, §13.3; OpenUSD `UsdLuxShapingAPI`.
    pub fn shaping(&self) -> Result<Option<LightShaping>, LightParameterError> {
        if !self.has_shaping_api {
            return Ok(None);
        }
        Ok(Some(LightShaping {
            focus: self.float("shaping:focus")?,
            focus_tint: self.color("shaping:focusTint")?,
            cone_angle: self.float("shaping:cone:angle")?,
            cone_softness: self.float("shaping:cone:softness")?,
            ies_file: self.input_capture("shaping:ies:file")?,
            ies_angle_scale: self.float("shaping:ies:angleScale")?,
            ies_normalize: self.boolean("shaping:ies:normalize")?,
        }))
    }
    /// Extracts composed `ShadowAPI` constants when that API is applied or built in.
    /// Negative sentinel values are preserved; no renderer shadow policy is chosen.
    /// AOUSD Core §12.3–12.4, §13.3; OpenUSD `UsdLuxShadowAPI`.
    pub fn shadow(&self) -> Result<Option<LightShadow>, LightParameterError> {
        if !self.has_shadow_api {
            return Ok(None);
        }
        Ok(Some(LightShadow {
            enable: self.boolean("shadow:enable")?,
            color: self.color("shadow:color")?,
            distance: self.float("shadow:distance")?,
            falloff: self.float("shadow:falloff")?,
            falloff_gamma: self.float("shadow:falloffGamma")?,
        }))
    }
    /// Extracts dome inputs. Other light kinds return `None`. Unknown format or
    /// pole tokens return errors while their original capture stays available.
    /// AOUSD Core §12.3–12.4; OpenUSD `UsdLuxDomeLight` / `UsdLuxDomeLight_1`.
    pub fn environment(&self) -> Result<Option<LightEnvironment>, LightParameterError> {
        if !matches!(self.kind, LightKind::Dome | LightKind::Dome1) {
            return Ok(None);
        }
        let input = self.input("texture:format");
        let value = input.and_then(LightInput::constant);
        let format = value
            .and_then(|v| {
                v.value
                    .as_ref()
                    .filter(|v| matches!(v, Value::Token(_)))
                    .and(v.text.as_deref())
            })
            .and_then(|v| match v {
                "automatic" => Some(LightTextureFormat::Automatic),
                "latlong" => Some(LightTextureFormat::Latlong),
                "mirroredBall" => Some(LightTextureFormat::MirroredBall),
                "angular" => Some(LightTextureFormat::Angular),
                "cubeMapVerticalCross" => Some(LightTextureFormat::CubeMapVerticalCross),
                _ => None,
            })
            .ok_or_else(|| LightParameterError {
                light: self.light,
                input: "texture:format".into(),
                status: input.map_or(LightInputStatus::Unavailable, |i| i.status),
                invalid_value: value.is_some(),
            })?;
        let pole_axis = if self.kind == LightKind::Dome1 {
            let value = self
                .attributes
                .iter()
                .find(|(n, _)| n == "poleAxis")
                .map(|(_, v)| v);
            Some(
                value
                    .and_then(|v| {
                        v.value
                            .as_ref()
                            .filter(|v| matches!(v, Value::Token(_)))
                            .and(v.text.as_deref())
                    })
                    .and_then(|v| match v {
                        "scene" => Some(DomePoleAxis::Scene),
                        "Y" => Some(DomePoleAxis::Y),
                        "Z" => Some(DomePoleAxis::Z),
                        _ => None,
                    })
                    .ok_or_else(|| LightParameterError {
                        light: self.light,
                        input: "poleAxis".into(),
                        status: if value.is_some_and(|v| v.value.is_some()) {
                            LightInputStatus::Constant
                        } else {
                            LightInputStatus::Unavailable
                        },
                        invalid_value: value.is_some_and(|v| v.value.is_some()),
                    })?,
            )
        } else {
            None
        };
        Ok(Some(LightEnvironment {
            texture_file: self.input_capture("texture:file")?,
            texture_format: format,
            pole_axis,
            portals: self
                .relationships
                .iter()
                .find(|(n, _)| n == "portals")
                .map_or_else(Vec::new, |(_, targets)| targets.clone()),
        }))
    }
}
