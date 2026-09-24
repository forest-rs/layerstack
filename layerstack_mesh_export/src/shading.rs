// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mapping from a [`Material`] to a `UsdShade` network.
//!
//! Each material becomes a `Material` prim holding one `UsdPreviewSurface`
//! shader, one `UsdUVTexture` shader per distinct texture read, and one
//! `UsdPrimvarReader_float2` per UV set, wired with connections:
//!
//! ```text
//! Material.outputs:surface  ->  PreviewSurface.outputs:surface
//! PreviewSurface.inputs:*   ->  <Slot>Texture.outputs:{rgb,r,g,b,a}
//! <Slot>Texture.inputs:st   ->  TexCoordReader.outputs:result
//! ```
//!
//! Spec: OpenUSD `docs/spec_usdpreviewsurface.rst` (node definitions and
//! the "USD Sample" network); `pxr/usd/usdShade/material.h:206`
//! (`outputs:surface`); `pxr/usd/plugin/usdShaders/shaders/shaderDefs.usda`
//! (input and output types, which `usdShadeValidators`'
//! `ShaderSdrCompliance` checks).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::writer::{Attribute, Prim, Value};

use crate::{ColorInput, ExportError, FloatInput, Material, MaterialProblem, Texture};

/// `inputs:sourceColorSpace` of a texture node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorSpace {
    /// Color data (base color, emissive): decoded with the sRGB transfer
    /// curve.
    Srgb,
    /// Data read as stored (normals, roughness, metallic, occlusion,
    /// opacity).
    Raw,
}

impl ColorSpace {
    fn token(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::Raw => "raw",
        }
    }
}

/// `UsdUVTexture` outputs, in the order they are declared.
const OUTPUTS: [(&str, &str); 5] = [
    ("outputs:rgb", "float3"),
    ("outputs:r", "float"),
    ("outputs:g", "float"),
    ("outputs:b", "float"),
    ("outputs:a", "float"),
];
const RGB: usize = 0;

/// One `UsdUVTexture` shader being assembled.
struct TextureNode<'a> {
    name: &'static str,
    texture: Texture<'a>,
    /// `None` while only alpha has been read: alpha is linear in both
    /// color spaces.
    color_space: Option<ColorSpace>,
    scale: [f32; 4],
    bias: [f32; 4],
    claimed: [bool; 4],
    outputs: [bool; 5],
    /// Author `scale` and `bias` even at their fallbacks: the normal-map
    /// convention requires them explicitly (`usdShadeValidators`,
    /// `NormalMapTextureValidator`).
    explicit_remap: bool,
}

/// A read of some texture channels.
struct Read<'a, 'c> {
    slot: &'static str,
    texture: Texture<'a>,
    color_space: Option<ColorSpace>,
    /// `(channel, scale, bias)` for each channel read.
    channels: &'c [(usize, f32, f32)],
    output: usize,
    explicit_remap: bool,
}

struct Network<'a> {
    path: String,
    nodes: Vec<TextureNode<'a>>,
    uv_sets: Vec<&'a str>,
}

impl<'a> Network<'a> {
    /// Returns the connection target for `read`, sharing a node with an
    /// earlier read of the same image when their color spaces and
    /// per-channel remaps agree, so a packed image (e.g. occlusion,
    /// roughness and metallic in R, G and B) is sampled once.
    fn read(&mut self, read: Read<'a, '_>) -> String {
        let compatible = |node: &TextureNode<'a>| {
            node.texture == read.texture
                && match (node.color_space, read.color_space) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
                && read.channels.iter().all(|&(c, scale, bias)| {
                    !node.claimed[c] || (node.scale[c] == scale && node.bias[c] == bias)
                })
        };
        let index = match self.nodes.iter().position(compatible) {
            Some(index) => index,
            None => {
                self.nodes.push(TextureNode {
                    name: read.slot,
                    texture: read.texture,
                    color_space: None,
                    scale: [1.0; 4],
                    bias: [0.0; 4],
                    claimed: [false; 4],
                    outputs: [false; 5],
                    explicit_remap: false,
                });
                self.nodes.len() - 1
            }
        };
        let node = &mut self.nodes[index];
        node.color_space = node.color_space.or(read.color_space);
        for &(c, scale, bias) in read.channels {
            node.claimed[c] = true;
            node.scale[c] = scale;
            node.bias[c] = bias;
        }
        node.outputs[read.output] = true;
        node.explicit_remap |= read.explicit_remap;
        if !self.uv_sets.contains(&read.texture.uv_set) {
            self.uv_sets.push(read.texture.uv_set);
        }
        format!("{}/{}.{}", self.path, node.name, OUTPUTS[read.output].0)
    }

    fn reader_name(&self, uv_set: &str) -> String {
        match self.uv_sets.iter().position(|&s| s == uv_set) {
            Some(0) | None => String::from("TexCoordReader"),
            Some(i) => format!("TexCoordReader{i}"),
        }
    }
}

const SURFACE: &str = "PreviewSurface";

/// Builds the `Material` prim for `material` at `<scope>/<name>`.
pub(crate) fn material_prim(material: &Material<'_>, scope: &str) -> Result<Prim, ExportError> {
    let path = format!("{scope}/{}", material.name);
    check(material).map_err(|problem| ExportError::InvalidMaterial {
        path: path.clone(),
        problem,
    })?;
    let mut network = Network {
        path: path.clone(),
        nodes: Vec::new(),
        uv_sets: Vec::new(),
    };

    let mut surface = Prim::def("Shader", SURFACE);
    let attrs = &mut surface.attributes;
    attrs.push(info_id("UsdPreviewSurface"));
    color(
        attrs,
        &mut network,
        "inputs:diffuseColor",
        "DiffuseColorTexture",
        material.diffuse_color,
    );
    color(
        attrs,
        &mut network,
        "inputs:emissiveColor",
        "EmissiveColorTexture",
        material.emissive_color,
    );
    float(
        attrs,
        &mut network,
        "inputs:metallic",
        "MetallicTexture",
        material.metallic,
    );
    if let Some(texture) = material.normal {
        // spec_usdpreviewsurface.rst, `normal`: tangent-space data in
        // [-1, 1]; 8-bit maps use scale (2, 2, 2, 1), bias (-1, -1, -1, 0)
        // and `raw` so no transfer curve is applied.
        let target = network.read(Read {
            slot: "NormalTexture",
            texture,
            color_space: Some(ColorSpace::Raw),
            channels: &[(0, 2.0, -1.0), (1, 2.0, -1.0), (2, 2.0, -1.0)],
            output: RGB,
            explicit_remap: true,
        });
        attrs.push(Attribute::declared("inputs:normal", "normal3f").with_connection(target));
    }
    float(
        attrs,
        &mut network,
        "inputs:occlusion",
        "OcclusionTexture",
        material.occlusion,
    );
    float(
        attrs,
        &mut network,
        "inputs:opacity",
        "OpacityTexture",
        material.opacity,
    );
    if let Some(threshold) = material.opacity_threshold {
        attrs.push(Attribute::new(
            "inputs:opacityThreshold",
            "float",
            Value::Float(threshold),
        ));
    }
    float(
        attrs,
        &mut network,
        "inputs:roughness",
        "RoughnessTexture",
        material.roughness,
    );
    // Metallic workflow, stated rather than left to the fallback.
    attrs.push(Attribute::new(
        "inputs:useSpecularWorkflow",
        "int",
        Value::Int(0),
    ));
    attrs.push(Attribute::declared("outputs:surface", "token"));

    let mut prim = Prim::def("Material", material.name);
    // `UsdShadeMaterial` `outputs:surface` (material.h:206), connected to
    // the shader's output as in the specification's sample.
    prim.attributes.push(
        Attribute::declared("outputs:surface", "token")
            .with_connection(format!("{path}/{SURFACE}.outputs:surface")),
    );
    prim.children.push(surface);
    for node in &network.nodes {
        prim.children.push(texture_prim(node, &network));
    }
    for uv_set in &network.uv_sets {
        let mut reader = Prim::def("Shader", network.reader_name(uv_set));
        reader.attributes.push(info_id("UsdPrimvarReader_float2"));
        reader.attributes.push(Attribute::new(
            "inputs:varname",
            "string",
            Value::String((*uv_set).into()),
        ));
        reader
            .attributes
            .push(Attribute::declared("outputs:result", "float2"));
        prim.children.push(reader);
    }
    Ok(prim)
}

fn info_id(id: &str) -> Attribute {
    Attribute::new("info:id", "token", Value::Token(id.into())).uniform()
}

fn color<'a>(
    attrs: &mut Vec<Attribute>,
    network: &mut Network<'a>,
    input: &str,
    slot: &'static str,
    value: Option<ColorInput<'a>>,
) {
    match value {
        None => {}
        Some(ColorInput::Constant(c)) => {
            attrs.push(Attribute::new(input, "color3f", Value::Float3(c)));
        }
        Some(ColorInput::Texture { texture, scale }) => {
            let channels = [(0, scale[0], 0.0), (1, scale[1], 0.0), (2, scale[2], 0.0)];
            let target = network.read(Read {
                slot,
                texture,
                color_space: Some(ColorSpace::Srgb),
                channels: &channels,
                output: RGB,
                explicit_remap: false,
            });
            attrs.push(Attribute::declared(input, "color3f").with_connection(target));
        }
    }
}

fn float<'a>(
    attrs: &mut Vec<Attribute>,
    network: &mut Network<'a>,
    input: &str,
    slot: &'static str,
    value: Option<FloatInput<'a>>,
) {
    match value {
        None => {}
        Some(FloatInput::Constant(v)) => {
            attrs.push(Attribute::new(input, "float", Value::Float(v)));
        }
        Some(FloatInput::Texture {
            texture,
            channel,
            scale,
            bias,
        }) => {
            let c = channel.index();
            let channels = [(c, scale, bias)];
            let target = network.read(Read {
                slot,
                texture,
                color_space: (c != 3).then_some(ColorSpace::Raw),
                channels: &channels,
                output: c + 1,
                explicit_remap: false,
            });
            attrs.push(Attribute::declared(input, "float").with_connection(target));
        }
    }
}

fn texture_prim(node: &TextureNode<'_>, network: &Network<'_>) -> Prim {
    let mut prim = Prim::def("Shader", node.name);
    let attrs = &mut prim.attributes;
    attrs.push(info_id("UsdUVTexture"));
    attrs.push(Attribute::new(
        "inputs:file",
        "asset",
        Value::Asset(node.texture.file.into()),
    ));
    attrs.push(
        Attribute::declared("inputs:st", "float2").with_connection(format!(
            "{}/{}.outputs:result",
            network.path,
            network.reader_name(node.texture.uv_set)
        )),
    );
    // Stated explicitly: the fallback, `auto`, guesses from the file.
    let color_space = node.color_space.unwrap_or(ColorSpace::Raw);
    attrs.push(Attribute::new(
        "inputs:sourceColorSpace",
        "token",
        Value::Token(color_space.token().into()),
    ));
    attrs.push(Attribute::new(
        "inputs:wrapS",
        "token",
        Value::Token(node.texture.wrap_s.token().into()),
    ));
    attrs.push(Attribute::new(
        "inputs:wrapT",
        "token",
        Value::Token(node.texture.wrap_t.token().into()),
    ));
    if node.explicit_remap || node.scale != [1.0; 4] {
        attrs.push(Attribute::new(
            "inputs:scale",
            "float4",
            Value::Float4(node.scale),
        ));
    }
    if node.explicit_remap || node.bias != [0.0; 4] {
        attrs.push(Attribute::new(
            "inputs:bias",
            "float4",
            Value::Float4(node.bias),
        ));
    }
    for (used, (name, type_name)) in node.outputs.iter().zip(OUTPUTS) {
        if *used {
            attrs.push(Attribute::declared(name, type_name));
        }
    }
    prim
}

/// Rejects values USD could store but no renderer can use.
fn check(material: &Material<'_>) -> Result<(), MaterialProblem> {
    let finite = |input: &'static str, values: &[f32]| {
        if values.iter().all(|v| v.is_finite()) {
            Ok(())
        } else {
            Err(MaterialProblem::NonFinite { input })
        }
    };
    for (input, value) in [
        ("inputs:diffuseColor", material.diffuse_color),
        ("inputs:emissiveColor", material.emissive_color),
    ] {
        match value {
            Some(ColorInput::Constant(c)) => finite(input, &c)?,
            Some(ColorInput::Texture { scale, .. }) => finite(input, &scale)?,
            None => {}
        }
    }
    for (input, value) in [
        ("inputs:metallic", material.metallic),
        ("inputs:occlusion", material.occlusion),
        ("inputs:opacity", material.opacity),
        ("inputs:roughness", material.roughness),
    ] {
        match value {
            Some(FloatInput::Constant(v)) => finite(input, &[v])?,
            Some(FloatInput::Texture { scale, bias, .. }) => finite(input, &[scale, bias])?,
            None => {}
        }
    }
    if let Some(threshold) = material.opacity_threshold {
        finite("inputs:opacityThreshold", &[threshold])?;
    }
    if material.textures().any(|t| t.file.is_empty()) {
        return Err(MaterialProblem::EmptyTexturePath);
    }
    Ok(())
}
