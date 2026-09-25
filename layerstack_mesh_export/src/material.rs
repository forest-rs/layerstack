// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material description (`UsdPreviewSurface` inputs).

use alloc::borrow::Cow;

/// Name of the `Scope` under the root prim that holds every material; a
/// material `M` is written at `/<root>/Materials/M`.
pub const MATERIALS_SCOPE: &str = "Materials";

/// How a texture is sampled outside the unit square (`inputs:wrapS` /
/// `inputs:wrapT` of `UsdUVTexture`).
///
/// Always authored: the shader's fallback, `useMetadata`, reads wrap modes
/// from the image file and falls back to `black`, which common image
/// formats never override.
///
/// Spec: OpenUSD `docs/spec_usdpreviewsurface.rst`, `UsdUVTexture`
/// (`wrapS`, `wrapT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    /// Repeat the texture (glTF's default).
    #[default]
    Repeat,
    /// Mirror and repeat.
    Mirror,
    /// Extend edge values.
    Clamp,
    /// Transparent black outside the unit square.
    Black,
}

impl Wrap {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::Mirror => "mirror",
            Self::Clamp => "clamp",
            Self::Black => "black",
        }
    }
}

/// An image file read by a `UsdUVTexture` shader.
///
/// `file` is authored as the `inputs:file` asset path, exactly as given. In
/// a package written by [`Scene::to_usdz`](crate::Scene::to_usdz) it must
/// name one of the package's files (e.g. `textures/albedo.png`); for
/// [`Scene::to_usda`](crate::Scene::to_usda) it is resolved relative to
/// wherever the caller stores the layer.
///
/// Texture coordinates come from the mesh primvar named by `uv_set`
/// (`st`, the primary UV set, by default), read by a
/// `UsdPrimvarReader_float2`. Every mesh bound to the material must author
/// that primvar.
///
/// `st` (0, 0) samples the lower-left corner of the image as displayed
/// (`spec_usdpreviewsurface.rst`, "Texture Coordinate Orientation in
/// USD"); UVs with a top-left origin, such as glTF's, need `t` flipped to
/// `1 - t` before export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture<'a> {
    /// Asset path of the image.
    pub file: &'a str,
    /// Name of the texture-coordinate primvar, without the `primvars:`
    /// namespace.
    pub uv_set: &'a str,
    /// Wrap mode along `s`.
    pub wrap_s: Wrap,
    /// Wrap mode along `t`.
    pub wrap_t: Wrap,
}

impl<'a> Texture<'a> {
    /// A repeating texture read with the `st` UV set.
    pub fn new(file: &'a str) -> Self {
        Self {
            file,
            uv_set: "st",
            wrap_s: Wrap::Repeat,
            wrap_t: Wrap::Repeat,
        }
    }

    /// Reads texture coordinates from `primvars:<uv_set>` instead of
    /// `primvars:st`.
    #[must_use]
    pub fn with_uv_set(mut self, uv_set: &'a str) -> Self {
        self.uv_set = uv_set;
        self
    }

    /// Sets both wrap modes.
    #[must_use]
    pub fn with_wrap(mut self, wrap_s: Wrap, wrap_t: Wrap) -> Self {
        self.wrap_s = wrap_s;
        self.wrap_t = wrap_t;
        self
    }
}

/// One channel of a texture, i.e. a `UsdUVTexture` output (`outputs:r`,
/// `outputs:g`, `outputs:b` or `outputs:a`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
    /// Alpha.
    A,
}

impl Channel {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::R => 0,
            Self::G => 1,
            Self::B => 2,
            Self::A => 3,
        }
    }
}

/// A `color3f` shader input: a constant, or the `rgb` of a texture.
///
/// Color textures are read as sRGB-encoded (`inputs:sourceColorSpace =
/// "sRGB"`), as base-color and emissive images conventionally are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorInput<'a> {
    /// A linear RGB value.
    Constant([f32; 3]),
    /// A texture's `outputs:rgb`, multiplied per channel by `scale` (the
    /// texture's `inputs:scale`; e.g. a glTF `baseColorFactor`).
    Texture {
        /// The image.
        texture: Texture<'a>,
        /// Per-channel multiplier applied after decoding.
        scale: [f32; 3],
    },
}

impl<'a> ColorInput<'a> {
    /// A texture's color, unscaled.
    pub fn texture(texture: Texture<'a>) -> Self {
        Self::Texture {
            texture,
            scale: [1.0; 3],
        }
    }

    /// Multiplies the color by `scale`, per channel. A constant is
    /// multiplied directly; a texture's multiplier is multiplied, so
    /// repeated calls compose: `.scaled(a).scaled(b)` equals
    /// `.scaled(a * b)`.
    #[must_use]
    pub fn scaled(self, scale: [f32; 3]) -> Self {
        let mul = |v: [f32; 3]| [v[0] * scale[0], v[1] * scale[1], v[2] * scale[2]];
        match self {
            Self::Constant(c) => Self::Constant(mul(c)),
            Self::Texture {
                texture,
                scale: prior,
            } => Self::Texture {
                texture,
                scale: mul(prior),
            },
        }
    }
}

impl From<[f32; 3]> for ColorInput<'_> {
    fn from(color: [f32; 3]) -> Self {
        Self::Constant(color)
    }
}

impl<'a> From<Texture<'a>> for ColorInput<'a> {
    fn from(texture: Texture<'a>) -> Self {
        Self::texture(texture)
    }
}

/// A `float` shader input: a constant, or one channel of a texture.
///
/// Data textures are read without color decoding (`inputs:sourceColorSpace
/// = "raw"`). An alpha channel is linear in either color space, so an
/// [`Channel::A`] input shares the texture node of a color input reading
/// the same image (e.g. opacity from the base-color alpha).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FloatInput<'a> {
    /// A constant value.
    Constant(f32),
    /// `channel * scale + bias` of a texture (the texture's `inputs:scale`
    /// and `inputs:bias` for that channel).
    Texture {
        /// The image.
        texture: Texture<'a>,
        /// The channel read.
        channel: Channel,
        /// Multiplier.
        scale: f32,
        /// Offset added after scaling.
        bias: f32,
    },
}

impl<'a> FloatInput<'a> {
    /// One channel of a texture, unscaled.
    pub fn texture(texture: Texture<'a>, channel: Channel) -> Self {
        Self::Texture {
            texture,
            channel,
            scale: 1.0,
            bias: 0.0,
        }
    }

    /// Remaps the value to `value * scale + bias`, e.g. `(f, 0.0)` for a
    /// glTF roughness or metallic factor `f`, and `(s, 1.0 - s)` for a glTF
    /// occlusion strength `s`. A constant is remapped directly; for a
    /// texture the remap is applied after the existing one, so repeated
    /// calls compose: `(x * s1 + b1) * s2 + b2` is scale `s1 * s2` and bias
    /// `b1 * s2 + b2`.
    #[must_use]
    pub fn scaled(self, scale: f32, bias: f32) -> Self {
        match self {
            Self::Constant(v) => Self::Constant(v * scale + bias),
            Self::Texture {
                texture,
                channel,
                scale: prior_scale,
                bias: prior_bias,
            } => Self::Texture {
                texture,
                channel,
                scale: prior_scale * scale,
                bias: prior_bias * scale + bias,
            },
        }
    }
}

impl From<f32> for FloatInput<'_> {
    fn from(value: f32) -> Self {
        Self::Constant(value)
    }
}

/// A material, written as a `Material` prim whose surface is a
/// `UsdPreviewSurface` shader in the metallic workflow.
///
/// Inputs left as `None` are not authored, so renderers use the shader's
/// fallbacks: `diffuseColor` (0.18, 0.18, 0.18), `emissiveColor` black,
/// `metallic` 0, `roughness` 0.5, `opacity` 1, `opacityThreshold` 0,
/// `occlusion` 1, and an unperturbed normal. These differ from glTF's
/// defaults (metallic and roughness 1), so a glTF material should set
/// every factor explicitly.
///
/// Inside the `Material` prim the surface shader is `PreviewSurface`.
/// Each texture shader is named after the first input that reads it, in
/// the shader's input order (`DiffuseColorTexture`, `EmissiveColorTexture`,
/// `MetallicTexture`, `NormalTexture`, `OcclusionTexture`,
/// `OpacityTexture`, `RoughnessTexture`), so a packed image read for
/// metallic, occlusion and roughness is one `MetallicTexture`. UV sets are
/// read by `TexCoordReader`, `TexCoordReader1`, … in order of first use.
///
/// Spec: OpenUSD `docs/spec_usdpreviewsurface.rst` (inputs and
/// fallbacks); `pxr/usd/plugin/usdShaders/shaders/shaderDefs.usda` (input
/// types).
#[derive(Clone, Debug, PartialEq)]
pub struct Material<'a> {
    /// Prim name under [`MATERIALS_SCOPE`]; must be a USD identifier and
    /// unique among the scene's materials. Meshes bind the material by this
    /// name.
    pub name: Cow<'a, str>,
    /// Albedo (`inputs:diffuseColor`).
    pub diffuse_color: Option<ColorInput<'a>>,
    /// Emitted color (`inputs:emissiveColor`).
    pub emissive_color: Option<ColorInput<'a>>,
    /// Metallic value in \[0, 1\] (`inputs:metallic`).
    pub metallic: Option<FloatInput<'a>>,
    /// Specular roughness in \[0, 1\] (`inputs:roughness`).
    pub roughness: Option<FloatInput<'a>>,
    /// Coverage in \[0, 1\] (`inputs:opacity`).
    pub opacity: Option<FloatInput<'a>>,
    /// Cut-out threshold (`inputs:opacityThreshold`): with a value above
    /// 0, fragments whose opacity is below it are discarded and the rest
    /// are opaque (glTF `alphaMode = MASK`, `alphaCutoff`).
    pub opacity_threshold: Option<f32>,
    /// Tangent-space normal map (`inputs:normal`), with +Y up (the OpenGL
    /// and glTF convention). Read as `raw` data and remapped from \[0, 1\]
    /// to \[-1, 1\] with `inputs:scale = (2, 2, 2, 1)` and `inputs:bias =
    /// (-1, -1, -1, 0)`, as the specification requires for 8-bit maps.
    pub normal: Option<Texture<'a>>,
    /// Ambient occlusion in \[0, 1\] (`inputs:occlusion`). The
    /// specification calls it meaningful only as a surface-varying signal,
    /// so it is normally a texture; a constant is written as given, and
    /// Apple's `usdchecker --arkit` warns that it dims the whole surface.
    pub occlusion: Option<FloatInput<'a>>,
}

impl<'a> Material<'a> {
    /// A material with every input at the shader's fallback.
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self {
            name: name.into(),
            diffuse_color: None,
            emissive_color: None,
            metallic: None,
            roughness: None,
            opacity: None,
            opacity_threshold: None,
            normal: None,
            occlusion: None,
        }
    }

    /// Sets the albedo.
    #[must_use]
    pub fn with_diffuse_color(mut self, color: impl Into<ColorInput<'a>>) -> Self {
        self.diffuse_color = Some(color.into());
        self
    }

    /// Sets the emitted color.
    #[must_use]
    pub fn with_emissive_color(mut self, color: impl Into<ColorInput<'a>>) -> Self {
        self.emissive_color = Some(color.into());
        self
    }

    /// Sets the metallic value.
    #[must_use]
    pub fn with_metallic(mut self, metallic: impl Into<FloatInput<'a>>) -> Self {
        self.metallic = Some(metallic.into());
        self
    }

    /// Sets the roughness.
    #[must_use]
    pub fn with_roughness(mut self, roughness: impl Into<FloatInput<'a>>) -> Self {
        self.roughness = Some(roughness.into());
        self
    }

    /// Sets the opacity.
    #[must_use]
    pub fn with_opacity(mut self, opacity: impl Into<FloatInput<'a>>) -> Self {
        self.opacity = Some(opacity.into());
        self
    }

    /// Sets the cut-out threshold.
    #[must_use]
    pub fn with_opacity_threshold(mut self, threshold: f32) -> Self {
        self.opacity_threshold = Some(threshold);
        self
    }

    /// Sets the normal map.
    #[must_use]
    pub fn with_normal_map(mut self, texture: Texture<'a>) -> Self {
        self.normal = Some(texture);
        self
    }

    /// Sets the ambient occlusion.
    #[must_use]
    pub fn with_occlusion(mut self, occlusion: impl Into<FloatInput<'a>>) -> Self {
        self.occlusion = Some(occlusion.into());
        self
    }

    /// Every texture the material reads, in shader-input order.
    pub fn textures(&self) -> impl Iterator<Item = Texture<'a>> + '_ {
        let color = |input: &Option<ColorInput<'a>>| match input {
            Some(ColorInput::Texture { texture, .. }) => Some(*texture),
            _ => None,
        };
        let float = |input: &Option<FloatInput<'a>>| match input {
            Some(FloatInput::Texture { texture, .. }) => Some(*texture),
            _ => None,
        };
        [
            color(&self.diffuse_color),
            color(&self.emissive_color),
            float(&self.metallic),
            self.normal,
            float(&self.occlusion),
            float(&self.opacity),
            float(&self.roughness),
        ]
        .into_iter()
        .flatten()
    }
}
