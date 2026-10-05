// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed standard Preview Surface expressions, without shader execution.
//!
//! This projection owns a bounded handoff: constants, standard primvar readers,
//! UV textures and 2D coordinate transforms. Hosts own primvar interpolation,
//! image lookup/decoding, color conversion, filtering, lighting and GPU execution.
//! AOUSD Core §12.3–12.4; OpenUSD `usdShaders/shaders/*.glslfx` and `shaderDefs.usda`.
use super::network::{MaterialConstant, MaterialNode, MaterialPort, MaterialValueOrigin};
use super::{MaterialNetwork, ValueSourceKind};
use crate::assets::AssetReference;
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyPath, Provenance, Value};

/// Explicit supported value shape; no cross-shape or numeric precision coercion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewType {
    /// Single 32-bit float.
    Float,
    /// Two 32-bit floats.
    Float2,
    /// Three 32-bit floats, preserving the consumer's declared role.
    Float3,
    /// Four 32-bit floats.
    Float4,
    /// Signed integer, used for the specular workflow switch.
    Int,
    /// String/token spelling, used for categorical policies.
    Text,
}
/// Standard texture result channel selected by a connected output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureChannel {
    /// First channel.
    R,
    /// Second channel.
    G,
    /// Third channel.
    B,
    /// Alpha channel.
    A,
    /// Three color channels.
    Rgb,
}
/// Texture wrapping policy from the USD shader definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureWrap {
    /// Use file metadata/default host policy.
    UseMetadata,
    /// Black outside the normalized texture domain.
    Black,
    /// Clamp at the border.
    Clamp,
    /// Repeat normalized coordinates.
    Repeat,
    /// Mirror repeated normalized coordinates.
    Mirror,
}
/// Declared texture source color-space policy, without color conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureColorSpace {
    /// Host interprets texture metadata and channel usage.
    Auto,
    /// Linear/raw data.
    Raw,
    /// sRGB-encoded source.
    Srgb,
}
/// Captured texture request. Resource bytes and resulting colors remain unknown.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewTexture {
    /// Captured node identity.
    pub node: PathId,
    /// Source spelling and winning authoring layer, without host resolution.
    pub file: AssetReference,
    /// Selected output channel.
    pub channel: TextureChannel,
    /// Coordinate expression; transforms remain explicit and unevaluated.
    pub coordinates: Box<PreviewExpression>,
    /// Result when the image cannot be read, before scale/bias.
    pub fallback: [f32; 4],
    /// Per-channel multiply after texture lookup.
    pub scale: [f32; 4],
    /// Per-channel add after texture lookup.
    pub bias: [f32; 4],
    /// Horizontal wrapping.
    pub wrap_s: TextureWrap,
    /// Vertical wrapping.
    pub wrap_t: TextureWrap,
    /// Source color-space declaration.
    pub color_space: TextureColorSpace,
}
/// Exact typed constant storage; USD roles stay in the generic declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewConstantValue {
    /// Single float.
    Float(f32),
    /// Pair of floats.
    Float2([f32; 2]),
    /// Triple of floats.
    Float3([f32; 3]),
    /// Four floats.
    Float4([f32; 4]),
    /// Signed integer.
    Int(i32),
    /// Captured string/token spelling.
    Text(String),
}
/// Exact typed constant and its source category, without a live store lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewConstant {
    /// Exact validated storage, without precision or dimensional coercion.
    pub value: PreviewConstantValue,
    /// Authored, schema-fallback or shader-definition-default origin.
    pub origin: MaterialValueOrigin,
    /// Winning authored source; shader-definition defaults have none.
    pub provenance: Option<Provenance>,
}
fn typed_constant(value: MaterialConstant) -> PreviewConstant {
    let storage = match value.value.value.as_ref() {
        Some(Value::Float(v)) => PreviewConstantValue::Float(*v),
        Some(Value::Vec2f(v)) => PreviewConstantValue::Float2(*v),
        Some(Value::Vec3f(v)) => PreviewConstantValue::Float3(*v),
        Some(Value::Vec4f(v)) => PreviewConstantValue::Float4(*v),
        Some(Value::Int(v)) => PreviewConstantValue::Int(*v),
        _ => PreviewConstantValue::Text(value.value.text.unwrap_or_default()),
    };
    PreviewConstant {
        value: storage,
        origin: value.origin,
        provenance: value.value.provenance,
    }
}
/// Supported expression tree. Sharing remains visible by each node identity;
/// the full generic graph remains the canonical deduplicated representation.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewExpression {
    /// Exact typed constant and its authored/schema/node-default origin.
    Constant(PreviewConstant),
    /// Primvar lookup with the declared output type and fallback.
    Primvar {
        /// Captured reader identity.
        node: PathId,
        /// Required primvar spelling.
        name: String,
        /// Exact output shape.
        value_type: PreviewType,
        /// Captured fallback, not evaluated.
        fallback: PreviewConstant,
    },
    /// Standard UV image request with a selected channel.
    Texture(PreviewTexture),
    /// Standard rotation in degrees, scale and translation of a float2 input.
    Transform2d {
        /// Captured transform identity.
        node: PathId,
        /// Unevaluated incoming coordinates.
        input: Box<Self>,
        /// Rotation in degrees.
        rotation: f32,
        /// Component scale.
        scale: [f32; 2],
        /// Translation after rotation/scaling.
        translation: [f32; 2],
    },
}
/// Why one requested Preview expression cannot be represented faithfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewIssueKind {
    /// No selected typed Preview Surface terminal exists.
    MissingSurface,
    /// Multiple candidates/providers require a host policy.
    MultipleProviders,
    /// Unsupported shader identifier/implementation.
    UnknownNode,
    /// A connected output does not have a supported name/shape.
    UnsupportedOutput,
    /// Constant storage or declaration has the wrong exact type.
    TypeMismatch,
    /// Missing, blocked or failed decoding; definition defaults do not hide it.
    Unavailable,
    /// An input must be constant in this projection but is shader-driven.
    DynamicParameter,
    /// Cyclic shader graph.
    Cycle,
    /// Expression exceeds the explicit traversal-depth budget.
    DepthLimit,
    /// Unknown categorical texture/opacity policy.
    UnknownPolicy,
}
/// Structured projection failure tied to its captured node/input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewIssue {
    /// Captured prim involved, if any.
    pub node: Option<PathId>,
    /// Input base name or output name, if relevant.
    pub port: Option<String>,
    /// Machine-readable failed requirement.
    pub kind: PreviewIssueKind,
}
/// A complete or partial typed input; failures preserve the generic graph.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewInput {
    /// Standard Preview Surface input name.
    pub name: String,
    /// Exact expected type.
    pub value_type: PreviewType,
    /// Supported expression; None means diagnostics describe the unsupported input.
    pub expression: Option<PreviewExpression>,
}
/// Typed handoff plus immutable original graph. Unsupported inputs are explicit;
/// successful inputs do not imply that a renderer executed this material.
#[derive(Clone, Debug)]
pub struct PreviewSurfaceNetwork {
    /// Generic captured graph, including unknown upstream nodes and diagnostics.
    pub network: Arc<MaterialNetwork>,
    /// Selected Preview Surface node, when recognized.
    pub surface: Option<PathId>,
    /// All standard surface inputs in fixed definition order.
    pub inputs: Vec<PreviewInput>,
    /// Unsupported nodes, combinations, policies and missing data.
    pub issues: Vec<PreviewIssue>,
}
impl PreviewSurfaceNetwork {
    /// Finds a typed surface input by its standard base name.
    #[must_use]
    pub fn input(&self, name: &str) -> Option<&PreviewInput> {
        self.inputs.iter().find(|p| p.name == name)
    }
    /// Every standard surface input was represented without projection failures.
    /// Generic connection diagnostics remain available in `network.issues`.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.surface.is_some() && self.issues.is_empty()
    }
    /// Projects an immutable capture. `max_depth` bounds nested supported
    /// expressions; cycles terminate with diagnostics. No live store is queried.
    #[must_use]
    pub fn capture(network: Arc<MaterialNetwork>, max_depth: usize) -> Self {
        let mut result = Self {
            network: network.clone(),
            surface: None,
            inputs: Vec::new(),
            issues: Vec::new(),
        };
        let Some(selected) = network.source.selected() else {
            result
                .issues
                .push(issue(None, None, PreviewIssueKind::MissingSurface));
            return result;
        };
        let Some(node) = network.node(selected.output.prim_path()).filter(|n| {
            n.is_shader
                && n.identifier.as_deref() == Some("UsdPreviewSurface")
                && n.implementation_source.as_deref() == Some("id")
        }) else {
            result.issues.push(issue(
                Some(selected.output.prim_path()),
                None,
                PreviewIssueKind::UnknownNode,
            ));
            return result;
        };
        result.surface = Some(node.path);
        if network.source.trace.sources.len() > 1 {
            result.issues.push(issue(
                Some(node.path),
                None,
                PreviewIssueKind::MultipleProviders,
            ));
        }
        if output_name(&network, selected.output) != Some("surface") {
            result.issues.push(issue(
                Some(node.path),
                None,
                PreviewIssueKind::UnsupportedOutput,
            ));
        }
        for (name, ty) in [
            ("diffuseColor", PreviewType::Float3),
            ("emissiveColor", PreviewType::Float3),
            ("specularColor", PreviewType::Float3),
            ("normal", PreviewType::Float3),
            ("roughness", PreviewType::Float),
            ("metallic", PreviewType::Float),
            ("opacity", PreviewType::Float),
            ("opacityThreshold", PreviewType::Float),
            ("ior", PreviewType::Float),
            ("clearcoat", PreviewType::Float),
            ("clearcoatRoughness", PreviewType::Float),
            ("occlusion", PreviewType::Float),
            ("displacement", PreviewType::Float),
            ("useSpecularWorkflow", PreviewType::Int),
            ("opacityMode", PreviewType::Text),
        ] {
            let expression = expression(
                &network,
                node,
                name,
                ty,
                &mut alloc::vec![node.path],
                max_depth,
            )
            .map_err(|e| result.issues.push(e))
            .ok();
            result.inputs.push(PreviewInput {
                name: name.into(),
                value_type: ty,
                expression,
            });
        }
        result
    }
}
fn issue(node: Option<PathId>, port: Option<&str>, kind: PreviewIssueKind) -> PreviewIssue {
    PreviewIssue {
        node,
        port: port.map(Into::into),
        kind,
    }
}
fn output_name(network: &MaterialNetwork, path: PropertyPath) -> Option<&str> {
    network
        .node(path.prim_path())?
        .ports
        .iter()
        .find(|p| p.property == Some(path))
        .map(|p| p.name.as_str())
}
fn declaration_matches(port: &MaterialPort, ty: PreviewType) -> bool {
    let Some(declaration) = &port.property_type else {
        return false;
    };
    if declaration.is_array {
        return false;
    }
    match ty {
        PreviewType::Float => declaration.type_name.as_ref() == "float",
        PreviewType::Float2 => matches!(declaration.type_name.as_ref(), "float2" | "texCoord2f"),
        PreviewType::Float3 => matches!(
            declaration.type_name.as_ref(),
            "float3" | "color3f" | "point3f" | "normal3f" | "vector3f" | "texCoord3f"
        ),
        PreviewType::Float4 => matches!(declaration.type_name.as_ref(), "float4" | "color4f"),
        PreviewType::Int => declaration.type_name.as_ref() == "int",
        PreviewType::Text => matches!(declaration.type_name.as_ref(), "token" | "string"),
    }
}
fn valid(port: &MaterialPort, constant: &MaterialConstant, ty: PreviewType) -> bool {
    if !declaration_matches(port, ty) {
        return false;
    }
    matches!(
        (constant.value.value.as_ref(), ty),
        (Some(Value::Float(_)), PreviewType::Float)
            | (Some(Value::Vec2f(_)), PreviewType::Float2)
            | (Some(Value::Vec3f(_)), PreviewType::Float3)
            | (Some(Value::Vec4f(_)), PreviewType::Float4)
            | (Some(Value::Int(_)), PreviewType::Int)
    ) || (ty == PreviewType::Text && constant.value.text.is_some())
}
fn constant(
    node: &MaterialNode,
    name: &str,
    ty: PreviewType,
) -> Result<MaterialConstant, PreviewIssue> {
    let port = node
        .input(name)
        .ok_or_else(|| issue(Some(node.path), Some(name), PreviewIssueKind::Unavailable))?;
    let c = port.constant.as_ref().ok_or_else(|| {
        issue(
            Some(node.path),
            Some(name),
            if port
                .providers
                .sources
                .iter()
                .any(|s| s.kind == ValueSourceKind::ShaderOutput)
            {
                PreviewIssueKind::DynamicParameter
            } else {
                PreviewIssueKind::Unavailable
            },
        )
    })?;
    if !valid(port, c, ty) {
        return Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::TypeMismatch,
        ));
    }
    Ok(c.clone())
}
fn expression(
    network: &MaterialNetwork,
    node: &MaterialNode,
    name: &str,
    ty: PreviewType,
    chain: &mut Vec<PathId>,
    depth: usize,
) -> Result<PreviewExpression, PreviewIssue> {
    if depth == 0 {
        return Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::DepthLimit,
        ));
    }
    let port = node
        .input(name)
        .ok_or_else(|| issue(Some(node.path), Some(name), PreviewIssueKind::Unavailable))?;
    if !declaration_matches(port, ty) {
        return Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::TypeMismatch,
        ));
    }
    if let Some(c) = &port.constant {
        if !valid(port, c, ty) {
            return Err(issue(
                Some(node.path),
                Some(name),
                PreviewIssueKind::TypeMismatch,
            ));
        }
        if name == "opacityMode"
            && !matches!(c.value.text.as_deref(), Some("transparent" | "presence"))
        {
            return Err(issue(
                Some(node.path),
                Some(name),
                PreviewIssueKind::UnknownPolicy,
            ));
        }
        return Ok(PreviewExpression::Constant(typed_constant(c.clone())));
    }
    if port.providers.sources.len() > 1 {
        return Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::MultipleProviders,
        ));
    }
    let source = port
        .providers
        .sources
        .first()
        .filter(|s| s.kind == ValueSourceKind::ShaderOutput)
        .ok_or_else(|| issue(Some(node.path), Some(name), PreviewIssueKind::Unavailable))?;
    let source_node = network.node(source.attribute.prim_path()).ok_or_else(|| {
        issue(
            Some(source.attribute.prim_path()),
            None,
            PreviewIssueKind::UnknownNode,
        )
    })?;
    if chain.contains(&source_node.path) {
        return Err(issue(Some(source_node.path), None, PreviewIssueKind::Cycle));
    }
    if !source_node.is_shader || source_node.implementation_source.as_deref() != Some("id") {
        return Err(issue(
            Some(source_node.path),
            None,
            PreviewIssueKind::UnknownNode,
        ));
    }
    chain.push(source_node.path);
    let result = source_expression(network, source_node, source.attribute, ty, chain, depth - 1);
    chain.pop();
    result
}
fn source_expression(
    network: &MaterialNetwork,
    node: &MaterialNode,
    output: PropertyPath,
    ty: PreviewType,
    chain: &mut Vec<PathId>,
    depth: usize,
) -> Result<PreviewExpression, PreviewIssue> {
    if depth == 0 {
        return Err(issue(Some(node.path), None, PreviewIssueKind::DepthLimit));
    }
    let output_name = output_name(network, output).unwrap_or("");
    if !node
        .ports
        .iter()
        .find(|p| p.property == Some(output))
        .is_some_and(|p| declaration_matches(p, ty))
    {
        return Err(issue(
            Some(node.path),
            Some(output_name),
            PreviewIssueKind::TypeMismatch,
        ));
    }
    let unsupported = || {
        issue(
            Some(node.path),
            Some(output_name),
            PreviewIssueKind::UnsupportedOutput,
        )
    };
    match node.identifier.as_deref().unwrap_or("") {
        id if id.starts_with("UsdPrimvarReader_") => {
            let expected = match id {
                "UsdPrimvarReader_float" => PreviewType::Float,
                "UsdPrimvarReader_float2" => PreviewType::Float2,
                "UsdPrimvarReader_float3"
                | "UsdPrimvarReader_point"
                | "UsdPrimvarReader_normal"
                | "UsdPrimvarReader_vector" => PreviewType::Float3,
                "UsdPrimvarReader_float4" => PreviewType::Float4,
                "UsdPrimvarReader_int" => PreviewType::Int,
                "UsdPrimvarReader_string" => PreviewType::Text,
                _ => return Err(unsupported()),
            };
            if output_name != "result" || expected != ty {
                return Err(unsupported());
            }
            let name = constant(node, "varname", PreviewType::Text)?
                .value
                .text
                .ok_or_else(unsupported)?;
            if name.is_empty() {
                return Err(issue(
                    Some(node.path),
                    Some("varname"),
                    PreviewIssueKind::Unavailable,
                ));
            }
            let fallback = typed_constant(constant(node, "fallback", ty)?);
            Ok(PreviewExpression::Primvar {
                node: node.path,
                name,
                value_type: ty,
                fallback,
            })
        }
        "UsdTransform2d" => {
            if output_name != "result" || ty != PreviewType::Float2 {
                return Err(unsupported());
            }
            Ok(PreviewExpression::Transform2d {
                node: node.path,
                input: Box::new(expression(
                    network,
                    node,
                    "in",
                    PreviewType::Float2,
                    chain,
                    depth,
                )?),
                rotation: float(node, "rotation")?,
                scale: float2(node, "scale")?,
                translation: float2(node, "translation")?,
            })
        }
        "UsdUVTexture" => {
            let channel = match (output_name, ty) {
                ("r", PreviewType::Float) => TextureChannel::R,
                ("g", PreviewType::Float) => TextureChannel::G,
                ("b", PreviewType::Float) => TextureChannel::B,
                ("a", PreviewType::Float) => TextureChannel::A,
                ("rgb", PreviewType::Float3) => TextureChannel::Rgb,
                _ => return Err(unsupported()),
            };
            let file_port = node.input("file").ok_or_else(unsupported)?;
            if file_port
                .property_type
                .as_ref()
                .is_none_or(|t| t.is_array || t.type_name.as_ref() != "asset")
            {
                return Err(issue(
                    Some(node.path),
                    Some("file"),
                    PreviewIssueKind::TypeMismatch,
                ));
            }
            let file = file_port
                .constant
                .as_ref()
                .and_then(|c| c.value.asset_reference())
                .ok_or_else(|| {
                    issue(
                        Some(node.path),
                        Some("file"),
                        if file_port
                            .providers
                            .sources
                            .iter()
                            .any(|s| s.kind == ValueSourceKind::ShaderOutput)
                        {
                            PreviewIssueKind::DynamicParameter
                        } else {
                            PreviewIssueKind::TypeMismatch
                        },
                    )
                })?;
            let color_space = match constant(node, "sourceColorSpace", PreviewType::Text)?
                .value
                .text
                .as_deref()
            {
                Some("auto") => TextureColorSpace::Auto,
                Some("raw") => TextureColorSpace::Raw,
                Some("sRGB") => TextureColorSpace::Srgb,
                _ => {
                    return Err(issue(
                        Some(node.path),
                        Some("sourceColorSpace"),
                        PreviewIssueKind::UnknownPolicy,
                    ));
                }
            };
            Ok(PreviewExpression::Texture(PreviewTexture {
                node: node.path,
                file,
                channel,
                coordinates: Box::new(expression(
                    network,
                    node,
                    "st",
                    PreviewType::Float2,
                    chain,
                    depth,
                )?),
                fallback: float4(node, "fallback")?,
                scale: float4(node, "scale")?,
                bias: float4(node, "bias")?,
                wrap_s: wrap(node, "wrapS")?,
                wrap_t: wrap(node, "wrapT")?,
                color_space,
            }))
        }
        _ => Err(issue(Some(node.path), None, PreviewIssueKind::UnknownNode)),
    }
}
fn float(node: &MaterialNode, name: &str) -> Result<f32, PreviewIssue> {
    match constant(node, name, PreviewType::Float)?.value.value {
        Some(Value::Float(v)) => Ok(v),
        _ => Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::TypeMismatch,
        )),
    }
}
fn float2(node: &MaterialNode, name: &str) -> Result<[f32; 2], PreviewIssue> {
    match constant(node, name, PreviewType::Float2)?.value.value {
        Some(Value::Vec2f(v)) => Ok(v),
        _ => Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::TypeMismatch,
        )),
    }
}
fn float4(node: &MaterialNode, name: &str) -> Result<[f32; 4], PreviewIssue> {
    match constant(node, name, PreviewType::Float4)?.value.value {
        Some(Value::Vec4f(v)) => Ok(v),
        _ => Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::TypeMismatch,
        )),
    }
}
fn wrap(node: &MaterialNode, name: &str) -> Result<TextureWrap, PreviewIssue> {
    match constant(node, name, PreviewType::Text)?
        .value
        .text
        .as_deref()
    {
        Some("useMetadata") => Ok(TextureWrap::UseMetadata),
        Some("black") => Ok(TextureWrap::Black),
        Some("clamp") => Ok(TextureWrap::Clamp),
        Some("repeat") => Ok(TextureWrap::Repeat),
        Some("mirror") => Ok(TextureWrap::Mirror),
        _ => Err(issue(
            Some(node.path),
            Some(name),
            PreviewIssueKind::UnknownPolicy,
        )),
    }
}
