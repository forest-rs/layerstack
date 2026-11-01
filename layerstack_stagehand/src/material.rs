// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Renderer-ready `UsdPreviewSurface` materials: every input of the
//! surface as a constant, a primvar or a texture lookup, with the
//! textures' files anchored and their sampling spelled out.
//!
//! The inputs, their fallbacks and their meaning are those of OpenUSD
//! 26.11's shader definitions (`pxr/usd/plugin/usdShaders/shaders/
//! shaderDefs.usda`, the `UsdPreviewSurface` specification) and of its
//! reference GLSL (`previewSurface.glslfx`, `uvTexture.glslfx`,
//! `transform2d.glslfx`):
//!
//! - **Fallbacks.** An input nothing authors or connects takes the
//!   shader definition's default: `diffuseColor` (0.18, 0.18, 0.18),
//!   `emissiveColor` and `specularColor` black, `useSpecularWorkflow` 0,
//!   `metallic` 0, `roughness` 0.5, `clearcoat` 0, `clearcoatRoughness`
//!   0.01, `opacity` 1, `opacityMode` `transparent`, `opacityThreshold` 0,
//!   `ior` 1.5, `normal` (0, 0, 1), `displacement` 0, `occlusion` 1.
//! - **Alpha.** `opacityThreshold` above 0 masks: opacity below it is not
//!   drawn, and the rest is fully opaque, whatever `opacityMode` says
//!   (`previewSurface.glslfx`, `HD_MATERIAL_TAG_MASKED`). Otherwise an
//!   opacity that is not the constant 1 blends; `opacityMode`
//!   `transparent` scales only the diffuse response by it, `presence` the
//!   whole response ([`AlphaMode`]).
//! - **Textures.** A lookup is `texel * scale + bias`, per channel, after
//!   8-bit texels are mapped to [0, 1]; the output channel the input
//!   connects to (`r`, `g`, `b`, `a`, `rgb`) is the one used, except that
//!   a one-channel input on `rgb` reads its red channel and a color on a
//!   one-channel output is not read, as OpenUSD's reference renderer,
//!   Storm, does (`layerstack_schemas::shading::preview_surface::
//!   texture_channel` has the sources). A normal map
//!   carries its own decoding as `scale` (2, 2, 2, 1) and `bias`
//!   (-1, -1, -1, 0), and `sourceColorSpace` `raw`: the `normal` input
//!   expects tangent-space values in [-1, 1] and the renderer applies the
//!   texture's scale and bias as authored.
//! - **Coordinates.** `st` is read by a `UsdPrimvarReader_float2` (its
//!   `varname`, possibly from a material interface input) and optionally
//!   moved by a `UsdTransform2d`: `st * scale`, rotated by `rotation`
//!   degrees counter-clockwise, plus `translation`.
//!
//! The record is built from the material's captured network
//! (`layerstack_schemas::shading::MaterialNetwork`), which follows the
//! surface output through the render contexts asked for and the universal
//! one (`UsdShadeMaterial::ComputeSurfaceSource`), and every connection
//! through node graphs and interface inputs
//! (`UsdShadeConnectableAPI`, `UsdShadeInput::GetValueProducingAttributes`).
//! What the record cannot represent is reported, never guessed.

use alloc::string::String;
use alloc::vec::Vec;

use layerstack::asset::{UdimPattern, UdimTile, anchor_asset_path};
use layerstack::{LayerId, PathId, Time};
use layerstack_schemas::Scene;
use layerstack_schemas::shading::network::MaterialIssue;
use layerstack_schemas::shading::preview_surface::{
    PreviewConstant, PreviewConstantValue, PreviewExpression, PreviewIssueKind,
    PreviewSurfaceNetwork, PreviewTexture,
};
pub use layerstack_schemas::shading::preview_surface::{
    TextureChannel, TextureColorSpace, TextureWrap,
};
use layerstack_schemas::shading::{
    MaterialNetwork, MaterialTerminal, ShadingDependency, ShadingIssue,
};

/// How deep a surface input's expression may nest: texture, transform,
/// primvar reader and one spare.
const MAX_DEPTH: usize = 6;

/// Where a surface input's value comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum MaterialValue<T> {
    /// One value for the whole surface: authored, through an interface
    /// input, or the shader definition's fallback.
    Constant(T),
    /// A texture lookup: the [`MaterialItem::textures`] entry `texture`,
    /// the `channel` read: `rgb` for a color; for a float the one-channel
    /// output it connects to, or `r` when it connects to `rgb`.
    Texture {
        /// The index in [`MaterialItem::textures`].
        texture: usize,
        /// The channel used.
        channel: TextureChannel,
    },
    /// A primvar of the drawn prim, read by a `UsdPrimvarReader`; the
    /// reader's `fallback` where the prim has none.
    Primvar {
        /// The primvar's name (`displayColor`).
        name: String,
        /// The value where the prim has no such primvar.
        fallback: T,
    },
}

impl<T: Copy> MaterialValue<T> {
    /// The constant, if the input is one.
    #[must_use]
    pub fn constant(&self) -> Option<T> {
        match self {
            Self::Constant(value) => Some(*value),
            _ => None,
        }
    }
}

/// How a surface's opacity draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlphaMode {
    /// Fully opaque: `opacity` is the constant 1 and nothing masks.
    Opaque,
    /// Cut out: opacity below `threshold` is not drawn, the rest is fully
    /// opaque (`opacityThreshold` above 0; `opacityMode` is ignored).
    Mask {
        /// `opacityThreshold`.
        threshold: f32,
    },
    /// Blended by opacity. With `presence` false (`opacityMode`
    /// `transparent`) opacity scales only the diffuse response, as glass
    /// keeps its reflections; with `presence` true it scales the whole
    /// response, as a surface partly there.
    Blend {
        /// Whether `opacityMode` is `presence`.
        presence: bool,
    },
}

/// A `UsdTransform2d` moving texture coordinates: `st * scale`, rotated
/// `rotation` degrees counter-clockwise about the origin, plus
/// `translation` (`transform2d.glslfx`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureTransform {
    /// The transform shader.
    pub shader: PathId,
    /// Degrees, counter-clockwise.
    pub rotation: f32,
    /// Applied first.
    pub scale: [f32; 2],
    /// Applied last.
    pub translation: [f32; 2],
}

/// A texture's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TextureCoordinates {
    /// The primvar read for them (`st`), by a `UsdPrimvarReader_float2`;
    /// `None` when `st` is a constant.
    pub primvar: Option<String>,
    /// The constant `st`, or the reader's fallback where the prim has no
    /// such primvar.
    pub fallback: [f32; 2],
    /// The transform applied to them, if any.
    pub transform: Option<TextureTransform>,
}

/// A texture's file: the asset path as composed and its identifier, the
/// path anchored to the layer that authors it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureFile {
    /// The asset path as composed (an expression evaluated).
    pub asset_path: String,
    /// The layer whose opinion it is; `None` for a fallback.
    pub layer: Option<LayerId>,
    /// The path anchored to that layer's identifier, as OpenUSD's
    /// `SdfComputeAssetPathRelativeToLayer` anchors a `./` or `../` path;
    /// a search path or an absolute one is kept, normalized. `None` when
    /// it cannot be anchored (an unnamed layer, an unevaluated expression,
    /// an empty path). AOUSD Core §9.4.
    pub identifier: Option<String>,
    /// Whether the path names a UDIM tile set (`<UDIM>`;
    /// `UsdShadeUdimUtils::IsUdimIdentifier`): its tiles are listed by
    /// [`Self::udim_tiles`].
    pub udim: bool,
}

/// One tile of a UDIM texture that exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdimTileFile {
    /// The tile.
    pub tile: UdimTile,
    /// Its identifier: the anchored pattern with the tile's number.
    pub identifier: String,
}

impl TextureFile {
    /// The tiles of a UDIM texture that exist, in tile order: each of 1001
    /// through 1100 spelled into the anchored pattern and kept when
    /// `exists` says the host's storage or resolver has it, as
    /// `UsdShadeUdimUtils::ResolveUdimTilePaths` resolves each tile. Empty
    /// for a texture that is not a UDIM set or cannot be anchored. A host
    /// reading a `layerstack_io` document can use its
    /// `StageDocument::resolve_udim_tiles`, which lists directories instead
    /// of asking per tile.
    pub fn udim_tiles(&self, mut exists: impl FnMut(&str) -> bool) -> Vec<UdimTileFile> {
        let Some(pattern) = self.identifier.as_deref().and_then(UdimPattern::parse) else {
            return Vec::new();
        };
        UdimTile::all()
            .filter_map(|tile| {
                let identifier = pattern.tile_path(tile);
                exists(&identifier).then_some(UdimTileFile { tile, identifier })
            })
            .collect()
    }
}

/// A `UsdUVTexture` lookup, as the inputs reading it share it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialTexture {
    /// The texture shader.
    pub shader: PathId,
    /// Its file.
    pub file: TextureFile,
    /// Its coordinates.
    pub coordinates: TextureCoordinates,
    /// Wrapping along s.
    pub wrap_s: TextureWrap,
    /// Wrapping along t.
    pub wrap_t: TextureWrap,
    /// The value where the file cannot be read, before scale and bias.
    pub fallback: [f32; 4],
    /// Multiplies each channel.
    pub scale: [f32; 4],
    /// Added to each channel after the scale.
    pub bias: [f32; 4],
    /// How the file's texels are encoded: `sRGB` decodes through the sRGB
    /// curve; `raw` is linear; `auto` leaves it to the file (an 8-bit
    /// color texture read as sRGB, data as raw, as Hydra decides).
    pub color_space: TextureColorSpace,
}

/// What a material's record could not represent, or found wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaterialNoteKind {
    /// No surface output resolves to a shader in the contexts read.
    NoSurface,
    /// The surface shader is not `UsdPreviewSurface`; its `info:id`.
    UnsupportedSurface(Option<String>),
    /// An input connects to a shader this record does not represent
    /// (other than `UsdUVTexture`, `UsdPrimvarReader_*`,
    /// `UsdTransform2d`); its `info:id`. The input takes its fallback.
    UnsupportedShader(Option<String>),
    /// A connection's target is not a shading attribute.
    UnresolvedConnection,
    /// The input has no value at the time read and no source: the
    /// fallback is used (a value only in time samples, read at the default
    /// time).
    NoValue,
    /// A connection leads back to an attribute on its own path.
    Cycle,
    /// Several sources connect to one input; the fallback is used.
    MultipleSources,
    /// A value of the wrong type, or an unknown token (`opacityMode`,
    /// `wrapS`, `sourceColorSpace`); the fallback is used.
    InvalidValue,
    /// A texture's `file` is empty or cannot be anchored.
    MissingTexture,
    /// The input reads an output a standard shader does not define (`rgba`
    /// of a `UsdUVTexture`) or one of another shape (a
    /// `UsdPrimvarReader_float3` read by a float); the output's name. The
    /// fallback is used.
    UnsupportedOutput(String),
    /// A color or normal reads a one-channel texture output (`r`); the
    /// output's name. Storm, OpenUSD's reference renderer, does not draw
    /// that read either; the fallback is used.
    ChannelMismatch(String),
    /// The input's expression is not representable here (a constant-only
    /// input connected to a shader, nested transforms); the fallback is
    /// used.
    Unsupported,
}

/// A note on a material: what, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialNote {
    /// What.
    pub kind: MaterialNoteKind,
    /// The shader or prim it concerns, if one.
    pub prim: Option<PathId>,
    /// The input (`diffuseColor`) or port it concerns, if one.
    pub input: Option<String>,
}

/// A material as a renderer draws it: a `UsdPreviewSurface`'s inputs, each
/// a constant, a primvar or a texture lookup, and the textures read.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialItem {
    /// The material.
    pub path: PathId,
    /// The render context whose surface output was used (`""` for the
    /// universal one); `None` when no surface output resolved.
    pub context: Option<String>,
    /// The `UsdPreviewSurface` shader; `None` without one, every input
    /// then its fallback.
    pub surface: Option<PathId>,
    /// `diffuseColor`: the albedo in the metallic workflow.
    pub diffuse_color: MaterialValue<[f32; 3]>,
    /// `emissiveColor`.
    pub emissive_color: MaterialValue<[f32; 3]>,
    /// `useSpecularWorkflow` is 1: [`Self::specular_color`] applies,
    /// [`Self::metallic`] does not.
    pub use_specular_workflow: bool,
    /// `specularColor`, in the specular workflow.
    pub specular_color: MaterialValue<[f32; 3]>,
    /// `metallic`, in the metallic workflow.
    pub metallic: MaterialValue<f32>,
    /// `roughness`.
    pub roughness: MaterialValue<f32>,
    /// `clearcoat`.
    pub clearcoat: MaterialValue<f32>,
    /// `clearcoatRoughness`.
    pub clearcoat_roughness: MaterialValue<f32>,
    /// `opacity`.
    pub opacity: MaterialValue<f32>,
    /// `opacityThreshold`.
    pub opacity_threshold: f32,
    /// `opacityMode` is `presence` rather than `transparent`.
    pub opacity_presence: bool,
    /// How opacity draws, from `opacity`, `opacityThreshold` and
    /// `opacityMode`.
    pub alpha: AlphaMode,
    /// `ior`.
    pub ior: MaterialValue<f32>,
    /// `normal`: a tangent-space normal in [-1, 1].
    pub normal: MaterialValue<[f32; 3]>,
    /// `displacement`, along the normal.
    pub displacement: MaterialValue<f32>,
    /// `occlusion`: 1 unoccluded.
    pub occlusion: MaterialValue<f32>,
    /// The textures the inputs read, each once.
    pub textures: Vec<MaterialTexture>,
    /// The primvars the inputs and textures read, sorted.
    pub primvars: Vec<String>,
    /// What could not be represented: each such input takes its fallback.
    pub notes: Vec<MaterialNote>,
}

/// A material record, the prims it read and whether any input it read
/// might differ at another time.
pub(crate) struct ReadMaterial {
    pub item: MaterialItem,
    /// Every attribute inspected, absent ones included.
    pub dependencies: Vec<ShadingDependency>,
}

/// Reads the material at `path` at `time` through the render `contexts`
/// (then the universal one); `None` when it is not a `Material`.
pub(crate) fn read_material(
    scene: &Scene<'_>,
    path: PathId,
    time: Time,
    contexts: &[&str],
) -> Option<ReadMaterial> {
    let network =
        MaterialNetwork::capture(scene, path, MaterialTerminal::Surface, contexts, time).ok()?;
    let dependencies = network.dependencies.clone();
    let mut notes: Vec<MaterialNote> = Vec::new();
    for issue in &network.issues {
        let (kind, prim) = match issue {
            MaterialIssue::Connection(ShadingIssue::Cycle(at)) => {
                (MaterialNoteKind::Cycle, Some(at.prim_path()))
            }
            MaterialIssue::Connection(ShadingIssue::InvalidTarget { from, .. }) => (
                MaterialNoteKind::UnresolvedConnection,
                Some(from.prim_path()),
            ),
            MaterialIssue::Connection(ShadingIssue::NonContainerInput(at)) => {
                (MaterialNoteKind::UnresolvedConnection, Some(at.prim_path()))
            }
            MaterialIssue::ShaderCycle(chain) => (MaterialNoteKind::Cycle, chain.first().copied()),
            _ => continue,
        };
        push_note(&mut notes, kind, prim, None);
    }
    let context = network.source.context.clone();
    let preview = PreviewSurfaceNetwork::capture(alloc::sync::Arc::new(network), MAX_DEPTH);
    let mut builder = Builder {
        scene,
        preview: &preview,
        textures: Vec::new(),
        primvars: Vec::new(),
        notes,
    };
    if preview.surface.is_none() {
        let selected = preview.network.source.selected();
        match selected {
            None => builder.note(MaterialNoteKind::NoSurface, None, None),
            Some(source) => {
                let shader = source.output.prim_path();
                let id = preview
                    .network
                    .node(shader)
                    .and_then(|node| node.identifier.clone());
                builder.note(MaterialNoteKind::UnsupportedSurface(id), Some(shader), None);
            }
        }
    }
    let diffuse_color = builder.color("diffuseColor", [0.18; 3]);
    let emissive_color = builder.color("emissiveColor", [0.0; 3]);
    let use_specular_workflow = builder.int("useSpecularWorkflow", 0) == 1;
    let specular_color = builder.color("specularColor", [0.0; 3]);
    let metallic = builder.float("metallic", 0.0);
    let roughness = builder.float("roughness", 0.5);
    let clearcoat = builder.float("clearcoat", 0.0);
    let clearcoat_roughness = builder.float("clearcoatRoughness", 0.01);
    let opacity = builder.float("opacity", 1.0);
    let opacity_threshold = builder.constant_float("opacityThreshold", 0.0);
    let opacity_presence = builder.token("opacityMode", "transparent") == "presence";
    let ior = builder.float("ior", 1.5);
    let normal = builder.color("normal", [0.0, 0.0, 1.0]);
    let displacement = builder.float("displacement", 0.0);
    let occlusion = builder.float("occlusion", 1.0);
    // `previewSurface.glslfx`: a threshold above 0 masks whatever
    // `opacityMode` says; otherwise any opacity but the constant 1 blends.
    let alpha = if opacity_threshold > 0.0 {
        AlphaMode::Mask {
            threshold: opacity_threshold,
        }
    } else if opacity.constant() == Some(1.0) {
        AlphaMode::Opaque
    } else {
        AlphaMode::Blend {
            presence: opacity_presence,
        }
    };
    let Builder {
        textures,
        mut primvars,
        notes,
        ..
    } = builder;
    layerstack::sort::sort(&mut primvars);
    primvars.dedup();
    Some(ReadMaterial {
        item: MaterialItem {
            path,
            context,
            surface: preview.surface,
            diffuse_color,
            emissive_color,
            use_specular_workflow,
            specular_color,
            metallic,
            roughness,
            clearcoat,
            clearcoat_roughness,
            opacity,
            opacity_threshold,
            opacity_presence,
            alpha,
            ior,
            normal,
            displacement,
            occlusion,
            textures,
            primvars,
            notes,
        },
        dependencies,
    })
}

fn push_note(
    notes: &mut Vec<MaterialNote>,
    kind: MaterialNoteKind,
    prim: Option<PathId>,
    input: Option<&str>,
) {
    let note = MaterialNote {
        kind,
        prim,
        input: input.map(Into::into),
    };
    if !notes.contains(&note) {
        notes.push(note);
    }
}

struct Builder<'s, 'a> {
    scene: &'s Scene<'a>,
    preview: &'s PreviewSurfaceNetwork,
    textures: Vec<MaterialTexture>,
    primvars: Vec<String>,
    notes: Vec<MaterialNote>,
}

impl Builder<'_, '_> {
    fn note(&mut self, kind: MaterialNoteKind, prim: Option<PathId>, input: Option<&str>) {
        push_note(&mut self.notes, kind, prim, input);
    }

    /// The input's expression, or `None` (noted) when the projection
    /// failed: the caller uses the fallback.
    fn expression(&mut self, name: &str) -> Option<&PreviewExpression> {
        let preview = self.preview;
        preview.surface?;
        if let Some(expression) = preview
            .input(name)
            .and_then(|input| input.expression.as_ref())
        {
            return Some(expression);
        }
        // The projection records one issue per input it could not
        // represent, in input order, after those about the surface itself.
        let failed: Vec<&str> = preview
            .inputs
            .iter()
            .filter(|input| input.expression.is_none())
            .map(|input| input.name.as_str())
            .collect();
        let first = preview.issues.len().saturating_sub(failed.len());
        let issues: Vec<_> = failed
            .iter()
            .position(|failed| *failed == name)
            .and_then(|at| preview.issues.get(first + at))
            .cloned()
            .into_iter()
            .collect();
        for issue in issues {
            let kind = match issue.kind {
                PreviewIssueKind::UnknownNode => {
                    let id = issue
                        .node
                        .and_then(|node| preview.network.node(node))
                        .and_then(|node| node.identifier.clone());
                    MaterialNoteKind::UnsupportedShader(id)
                }
                PreviewIssueKind::MultipleProviders => MaterialNoteKind::MultipleSources,
                PreviewIssueKind::TypeMismatch | PreviewIssueKind::UnknownPolicy => {
                    MaterialNoteKind::InvalidValue
                }
                PreviewIssueKind::Cycle => MaterialNoteKind::Cycle,
                PreviewIssueKind::UnsupportedOutput => {
                    MaterialNoteKind::UnsupportedOutput(issue.port.clone().unwrap_or_default())
                }
                PreviewIssueKind::ChannelMismatch => {
                    MaterialNoteKind::ChannelMismatch(issue.port.clone().unwrap_or_default())
                }
                PreviewIssueKind::Unavailable => {
                    // Why nothing is there: a broken connection, a cycle, or
                    // no value at this time.
                    let port = issue
                        .node
                        .and_then(|node| preview.network.node(node))
                        .and_then(|node| node.input(issue.port.as_deref().unwrap_or(name)));
                    match port {
                        Some(port)
                            if port
                                .providers
                                .issues
                                .iter()
                                .any(|issue| matches!(issue, ShadingIssue::Cycle(_))) =>
                        {
                            MaterialNoteKind::Cycle
                        }
                        Some(port)
                            if !port.connected.invalid.is_empty()
                                || !port.providers.issues.is_empty() =>
                        {
                            MaterialNoteKind::UnresolvedConnection
                        }
                        _ => MaterialNoteKind::NoValue,
                    }
                }
                _ => MaterialNoteKind::Unsupported,
            };
            self.note(kind, issue.node, Some(name));
        }
        None
    }

    fn color(&mut self, name: &str, fallback: [f32; 3]) -> MaterialValue<[f32; 3]> {
        self.value(name, fallback, |value| match value {
            PreviewConstantValue::Float3(v) => Some(*v),
            _ => None,
        })
    }

    fn float(&mut self, name: &str, fallback: f32) -> MaterialValue<f32> {
        self.value(name, fallback, |value| match value {
            PreviewConstantValue::Float(v) => Some(*v),
            _ => None,
        })
    }

    fn constant_float(&mut self, name: &str, fallback: f32) -> f32 {
        match self.float(name, fallback) {
            MaterialValue::Constant(value) => value,
            _ => {
                self.note(
                    MaterialNoteKind::Unsupported,
                    self.preview.surface,
                    Some(name),
                );
                fallback
            }
        }
    }

    fn int(&mut self, name: &str, fallback: i32) -> i32 {
        match self.expression(name) {
            Some(PreviewExpression::Constant(PreviewConstant {
                value: PreviewConstantValue::Int(v),
                ..
            })) => *v,
            Some(_) => {
                self.note(
                    MaterialNoteKind::Unsupported,
                    self.preview.surface,
                    Some(name),
                );
                fallback
            }
            None => fallback,
        }
    }

    fn token(&mut self, name: &str, fallback: &str) -> String {
        match self.expression(name) {
            Some(PreviewExpression::Constant(PreviewConstant {
                value: PreviewConstantValue::Text(v),
                ..
            })) => v.clone(),
            Some(_) => {
                self.note(
                    MaterialNoteKind::Unsupported,
                    self.preview.surface,
                    Some(name),
                );
                fallback.into()
            }
            None => fallback.into(),
        }
    }

    fn value<T: Copy>(
        &mut self,
        name: &str,
        fallback: T,
        read: impl Fn(&PreviewConstantValue) -> Option<T>,
    ) -> MaterialValue<T> {
        let Some(expression) = self.expression(name).cloned() else {
            return MaterialValue::Constant(fallback);
        };
        match expression {
            PreviewExpression::Constant(constant) => {
                MaterialValue::Constant(read(&constant.value).unwrap_or(fallback))
            }
            PreviewExpression::Primvar {
                name: primvar,
                fallback: reader_fallback,
                ..
            } => {
                self.primvars.push(primvar.clone());
                MaterialValue::Primvar {
                    name: primvar,
                    fallback: read(&reader_fallback.value).unwrap_or(fallback),
                }
            }
            PreviewExpression::Texture(texture) => match self.texture(name, &texture) {
                Some(index) => MaterialValue::Texture {
                    texture: index,
                    channel: texture.channel,
                },
                None => MaterialValue::Constant(fallback),
            },
            PreviewExpression::Transform2d { node, .. } => {
                self.note(MaterialNoteKind::Unsupported, Some(node), Some(name));
                MaterialValue::Constant(fallback)
            }
        }
    }

    /// The index of `texture` in the record, added when new; `None`
    /// (noted) when its coordinates cannot be represented.
    fn texture(&mut self, input: &str, texture: &PreviewTexture) -> Option<usize> {
        if let Some(index) = self.textures.iter().position(|t| t.shader == texture.node) {
            return Some(index);
        }
        let coordinates = self.coordinates(input, &texture.coordinates)?;
        let asset_path = texture.file.asset_path.clone();
        let layer = texture.file.source.as_ref().map(|source| source.layer);
        let identifier = anchor(self.scene, &asset_path, layer);
        if identifier.is_none() {
            self.note(
                MaterialNoteKind::MissingTexture,
                Some(texture.node),
                Some(input),
            );
        }
        let udim = UdimPattern::parse(&asset_path).is_some();
        self.textures.push(MaterialTexture {
            shader: texture.node,
            file: TextureFile {
                asset_path,
                layer,
                identifier,
                udim,
            },
            coordinates,
            wrap_s: texture.wrap_s,
            wrap_t: texture.wrap_t,
            fallback: texture.fallback,
            scale: texture.scale,
            bias: texture.bias,
            color_space: texture.color_space,
        });
        Some(self.textures.len() - 1)
    }

    fn coordinates(
        &mut self,
        input: &str,
        expression: &PreviewExpression,
    ) -> Option<TextureCoordinates> {
        let (inner, transform) = match expression {
            PreviewExpression::Transform2d {
                node,
                input: inner,
                rotation,
                scale,
                translation,
            } => (
                &**inner,
                Some(TextureTransform {
                    shader: *node,
                    rotation: *rotation,
                    scale: *scale,
                    translation: *translation,
                }),
            ),
            other => (other, None),
        };
        let float2 = |constant: &PreviewConstant| match constant.value {
            PreviewConstantValue::Float2(v) => v,
            _ => [0.0; 2],
        };
        let (primvar, fallback) = match inner {
            PreviewExpression::Constant(constant) => (None, float2(constant)),
            PreviewExpression::Primvar { name, fallback, .. } => {
                self.primvars.push(name.clone());
                (Some(name.clone()), float2(fallback))
            }
            PreviewExpression::Texture(PreviewTexture { node, .. })
            | PreviewExpression::Transform2d { node, .. } => {
                self.note(MaterialNoteKind::Unsupported, Some(*node), Some(input));
                return None;
            }
        };
        Some(TextureCoordinates {
            primvar,
            fallback,
            transform,
        })
    }
}

/// `asset_path` anchored to the layer `layer` names, as
/// `SdfComputeAssetPathRelativeToLayer` anchors it (AOUSD Core §9.4).
/// The file of an asset attribute's value, anchored to the layer
/// authoring it, as a material's texture's is.
pub(crate) fn texture_file(
    scene: &Scene<'_>,
    asset: &layerstack_schemas::assets::AssetReference,
) -> TextureFile {
    let layer = asset.source.as_ref().map(|source| source.layer);
    TextureFile {
        identifier: anchor(scene, &asset.asset_path, layer),
        udim: UdimPattern::parse(&asset.asset_path).is_some(),
        asset_path: asset.asset_path.clone(),
        layer,
    }
}

fn anchor(scene: &Scene<'_>, asset_path: &str, layer: Option<LayerId>) -> Option<String> {
    if asset_path.is_empty() {
        return None;
    }
    let location = layer
        .and_then(|layer| scene.store().layer_identifier(layer))
        .unwrap_or("");
    anchor_asset_path(asset_path, location)
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the oracle writes OpenUSD's `float` values and tile numbers as JSON numbers"
)]
mod tests {
    extern crate std;

    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use layerstack::{InMemoryStore, LayerId, LiveStage, NoAssets, StageOptions, Time};
    use layerstack_schemas::Scene;
    use serde_json::Value as Json;

    use super::{MaterialItem, MaterialNoteKind, MaterialValue, TextureChannel};
    use crate::{DrawList, DrawOptions};

    // WASI reaches the host only through the directories its runner opens,
    // the crate's and its parent, so it names the fixture from the crate.
    #[cfg(not(target_os = "wasi"))]
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../layerstack_conformance/fixtures/stagehand_materials"
    );
    #[cfg(target_os = "wasi")]
    const FIXTURE: &str = "../layerstack_conformance/fixtures/stagehand_materials";

    fn open() -> (InMemoryStore, LiveStage) {
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            include_str!("../../layerstack_conformance/fixtures/stagehand_materials/scene.usda"),
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        store.insert_layer(read.emitted.layer);
        store.set_layer_identifier(LayerId(1), "scene.usda");
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        (store, live)
    }

    fn floats(value: &Json) -> Vec<f32> {
        match value {
            Json::Array(values) => values.iter().map(|v| v.as_f64().unwrap() as f32).collect(),
            other => alloc::vec![other.as_f64().unwrap() as f32],
        }
    }

    fn strip(path: &str) -> &str {
        path.strip_prefix("./").unwrap_or(path)
    }

    /// What OpenUSD gives a shader's input `name`: its one producing
    /// attribute's value at `key` (`value`, `value10`), else the shader
    /// registry's default for `id`.
    fn value_of<'j>(oracle: &'j Json, node: &'j Json, id: &str, name: &str, key: &str) -> &'j Json {
        match node["inputs"][name].as_array().map(Vec::as_slice) {
            Some([one]) if !one[key].is_null() => &one[key],
            _ => &oracle["defaults"][id][name],
        }
    }

    fn check_value<T: Copy + core::fmt::Debug>(
        at: &str,
        oracle: &Json,
        row: &Json,
        item: &MaterialItem,
        name: &str,
        key: &str,
        ours: &MaterialValue<T>,
        to_floats: impl Fn(T) -> Vec<f32>,
    ) {
        let surface = &row["surface"];
        let default = floats(&oracle["defaults"]["UsdPreviewSurface"][name]);
        let producing = surface["inputs"][name]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let at = alloc::format!("{at} {name}");
        match producing.as_slice() {
            [one] if one.get("shader").is_some() => {
                let shader = one["shader"].as_str().unwrap_or("");
                let node = &row["nodes"][one["attribute"]
                    .as_str()
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap()];
                match shader {
                    "UsdUVTexture" => {
                        // OpenUSD connects any types (`canConnect`); what is
                        // read is Storm's (`texture_channel`).
                        assert_eq!(surface["canConnect"][name], true, "{at}");
                        let output = one["output"].as_str().unwrap();
                        let expected = match output {
                            "r" => Some(TextureChannel::R),
                            "g" => Some(TextureChannel::G),
                            "b" => Some(TextureChannel::B),
                            "a" => Some(TextureChannel::A),
                            "rgb" => Some(TextureChannel::Rgb),
                            _ => None,
                        }
                        .map(|output| {
                            match surface["types"][name].as_str().unwrap() {
                                "float" => Some(match output {
                                    TextureChannel::Rgb => TextureChannel::R,
                                    one => one,
                                }),
                                _ => (output == TextureChannel::Rgb).then_some(output),
                            }
                        });
                        let noted =
                            |kind: MaterialNoteKind| {
                                assert_eq!(
                                    ours.constant().map(&to_floats),
                                    Some(default.clone()),
                                    "{at}"
                                );
                                assert!(
                                    item.notes.iter().any(|note| note.kind == kind
                                        && note.input.as_deref() == Some(name)),
                                    "{at}: {:?}",
                                    item.notes
                                );
                            };
                        match expected {
                            None => noted(MaterialNoteKind::UnsupportedOutput(output.into())),
                            Some(None) => noted(MaterialNoteKind::ChannelMismatch(output.into())),
                            Some(Some(expected)) => {
                                let MaterialValue::Texture { texture, channel } = ours else {
                                    panic!("{at}: {ours:?} is not a texture");
                                };
                                assert_eq!(*channel, expected, "{at}");
                                assert!(item.textures.len() > *texture, "{at}");
                            }
                        }
                    }
                    id if id.starts_with("UsdPrimvarReader_") => {
                        let MaterialValue::Primvar {
                            name: primvar,
                            fallback,
                        } = ours
                        else {
                            panic!("{at}: {ours:?} is not a primvar");
                        };
                        assert_eq!(
                            Some(primvar.as_str()),
                            value_of(oracle, node, id, "varname", key).as_str(),
                            "{at}"
                        );
                        assert_eq!(
                            to_floats(*fallback),
                            floats(value_of(oracle, node, id, "fallback", key)),
                            "{at}"
                        );
                    }
                    _ => {
                        assert_eq!(ours.constant().map(&to_floats), Some(default), "{at}");
                        assert!(
                            item.notes.iter().any(|note| matches!(
                                &note.kind,
                                MaterialNoteKind::UnsupportedShader(Some(id)) if id == shader
                            )),
                            "{at}: {:?}",
                            item.notes
                        );
                    }
                }
            }
            [one] if !one[key].is_null() => {
                assert_eq!(
                    ours.constant().map(&to_floats),
                    Some(floats(&one[key])),
                    "{at}"
                );
            }
            more => {
                assert_eq!(ours.constant().map(&to_floats), Some(default), "{at}");
                if more.len() > 1 {
                    assert!(
                        item.notes
                            .iter()
                            .any(|note| note.kind == MaterialNoteKind::MultipleSources),
                        "{at}: {:?}",
                        item.notes
                    );
                }
            }
        }
    }

    #[test]
    fn materials_match_openusd() {
        let oracle: Json = serde_json::from_str(include_str!(
            "../../layerstack_conformance/fixtures/stagehand_materials/oracle.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["version"].as_str(),
            Some(layerstack_schemas::OPENUSD_VERSION)
        );
        let (store, live) = open();
        let scene = Scene::new(live.stage(), &store);
        let rows = oracle["materials"].as_array().unwrap();
        assert_eq!(rows.len(), 20);
        for (contexts, key, time) in [
            (&[][..], "value", Time::Default),
            (&["mtlx"][..], "value", Time::Default),
            (&[][..], "value10", Time::at(10.0)),
        ] {
            let mut list = DrawList::new(DrawOptions {
                render_contexts: contexts,
                ..DrawOptions::default()
            });
            list.refresh(&scene);
            if time != Time::Default {
                list.set_time(&scene, time);
            }
            for row in rows
                .iter()
                .filter(|row| row["contexts"].as_array().unwrap().len() == contexts.len())
            {
                let path = row["path"].as_str().unwrap();
                let at = alloc::format!("{path} {contexts:?} {key}");
                let item = list
                    .material(store.path(path))
                    .unwrap_or_else(|| panic!("{at} is not held"));
                let shader = row["shader"].as_str();
                if row["id"] != "UsdPreviewSurface" {
                    assert_eq!(item.surface, None, "{at}");
                    let kind = match row["id"].as_str() {
                        None => MaterialNoteKind::NoSurface,
                        Some(id) => MaterialNoteKind::UnsupportedSurface(Some(id.to_string())),
                    };
                    assert!(
                        item.notes.iter().any(|note| note.kind == kind),
                        "{at}: {:?}",
                        item.notes
                    );
                    if let Some(shader) = shader {
                        let context = if shader.contains("Standard") {
                            "mtlx"
                        } else {
                            ""
                        };
                        assert_eq!(item.context.as_deref(), Some(context), "{at}");
                    }
                    continue;
                }
                assert_eq!(item.surface, shader.map(|s| store.path(s)), "{at}");
                let context = row["output"].as_str().unwrap();
                assert_eq!(item.context.as_deref(), Some(""), "{at} {context}");
                let three = |v: [f32; 3]| v.to_vec();
                let one = |v: f32| alloc::vec![v];
                for (name, ours) in [
                    ("diffuseColor", &item.diffuse_color),
                    ("emissiveColor", &item.emissive_color),
                    ("specularColor", &item.specular_color),
                    ("normal", &item.normal),
                ] {
                    check_value(&at, &oracle, row, item, name, key, ours, three);
                }
                for (name, ours) in [
                    ("metallic", &item.metallic),
                    ("roughness", &item.roughness),
                    ("clearcoat", &item.clearcoat),
                    ("clearcoatRoughness", &item.clearcoat_roughness),
                    ("opacity", &item.opacity),
                    ("ior", &item.ior),
                    ("displacement", &item.displacement),
                    ("occlusion", &item.occlusion),
                ] {
                    check_value(&at, &oracle, row, item, name, key, ours, one);
                }
                let surface = &row["surface"];
                let threshold = value_of(
                    &oracle,
                    surface,
                    "UsdPreviewSurface",
                    "opacityThreshold",
                    key,
                );
                assert_eq!(
                    item.opacity_threshold,
                    threshold.as_f64().unwrap() as f32,
                    "{at}"
                );
                let mode = value_of(&oracle, surface, "UsdPreviewSurface", "opacityMode", key);
                assert_eq!(item.opacity_presence, mode == "presence", "{at}");
                let workflow = value_of(
                    &oracle,
                    surface,
                    "UsdPreviewSurface",
                    "useSpecularWorkflow",
                    key,
                );
                assert_eq!(item.use_specular_workflow, workflow == 1, "{at}");

                // Every texture the record holds, against the node OpenUSD reaches.
                let nodes = row["nodes"].as_object().unwrap();
                for texture in &item.textures {
                    let shader = store.paths().display(texture.shader, store.tokens());
                    let at = alloc::format!("{at} {shader}");
                    let node = &nodes[&shader];
                    let tex = |name: &str| value_of(&oracle, node, "UsdUVTexture", name, key);
                    let file = &node["file"];
                    if file.is_null() {
                        assert_eq!(texture.file.asset_path, "", "{at}");
                        assert_eq!(texture.file.identifier, None, "{at}");
                        assert!(
                            item.notes
                                .iter()
                                .any(|note| note.kind == MaterialNoteKind::MissingTexture),
                            "{at}"
                        );
                    } else {
                        assert_eq!(texture.file.asset_path, file["authored"], "{at}");
                        assert_eq!(
                            texture.file.identifier.as_deref().map(strip),
                            file["anchored"].as_str(),
                            "{at}"
                        );
                        assert_eq!(texture.file.udim, file["udim"] == true, "{at}");
                        let tiles: Vec<(u16, String)> = texture
                            .file
                            .udim_tiles(|identifier| {
                                std::path::Path::new(FIXTURE)
                                    .join(strip(identifier))
                                    .exists()
                            })
                            .into_iter()
                            .map(|tile| (tile.tile.number(), strip(&tile.identifier).to_string()))
                            .collect();
                        let expected: Vec<(u16, String)> = file["tiles"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| (t[0].as_u64().unwrap() as u16, t[1].as_str().unwrap().into()))
                            .collect();
                        assert_eq!(tiles, expected, "{at}");
                    }
                    let token = |name: &str| tex(name).as_str().unwrap().to_string();
                    assert_eq!(
                        alloc::format!("{:?}", texture.wrap_s).to_lowercase(),
                        token("wrapS").to_lowercase(),
                        "{at}"
                    );
                    assert_eq!(
                        alloc::format!("{:?}", texture.wrap_t).to_lowercase(),
                        token("wrapT").to_lowercase(),
                        "{at}"
                    );
                    assert_eq!(
                        alloc::format!("{:?}", texture.color_space).to_lowercase(),
                        token("sourceColorSpace").to_lowercase(),
                        "{at}"
                    );
                    assert_eq!(texture.fallback.to_vec(), floats(tex("fallback")), "{at}");
                    assert_eq!(texture.scale.to_vec(), floats(tex("scale")), "{at}");
                    assert_eq!(texture.bias.to_vec(), floats(tex("bias")), "{at}");
                    // Coordinates: a transform's values, and the reader's.
                    let mut st = node["inputs"]["st"].as_array().cloned().unwrap_or_default();
                    if let Some(transform) = &texture.coordinates.transform {
                        let place = st[0]["attribute"]
                            .as_str()
                            .unwrap()
                            .split('.')
                            .next()
                            .unwrap();
                        assert_eq!(
                            store.paths().display(transform.shader, store.tokens()),
                            place,
                            "{at}"
                        );
                        let node = &nodes[place];
                        let t = |name: &str| {
                            floats(value_of(&oracle, node, "UsdTransform2d", name, key))
                        };
                        assert_eq!(alloc::vec![transform.rotation], t("rotation"), "{at}");
                        assert_eq!(transform.scale.to_vec(), t("scale"), "{at}");
                        assert_eq!(transform.translation.to_vec(), t("translation"), "{at}");
                        st = node["inputs"]["in"].as_array().cloned().unwrap_or_default();
                    }
                    match st.as_slice() {
                        [reader] if reader.get("shader").is_some() => {
                            let path = reader["attribute"]
                                .as_str()
                                .unwrap()
                                .split('.')
                                .next()
                                .unwrap();
                            let node = &nodes[path];
                            let id = "UsdPrimvarReader_float2";
                            assert_eq!(
                                texture.coordinates.primvar.as_deref(),
                                value_of(&oracle, node, id, "varname", key).as_str(),
                                "{at}"
                            );
                            assert_eq!(
                                texture.coordinates.fallback.to_vec(),
                                floats(value_of(&oracle, node, id, "fallback", key)),
                                "{at}"
                            );
                        }
                        _ => {
                            assert_eq!(texture.coordinates.primvar, None, "{at}");
                            assert_eq!(
                                texture.coordinates.fallback.to_vec(),
                                floats(tex("st")),
                                "{at}"
                            );
                        }
                    }
                }
            }
            // A light's connected inputs read what they produce.
            for row in oracle["lights"].as_array().unwrap() {
                let light = list
                    .light(store.path(row["path"].as_str().unwrap()))
                    .unwrap();
                let inputs = &row["inputs"];
                let produced = |name: &str| &inputs[name][0][key];
                assert_eq!(
                    light.emission.intensity.to_bits(),
                    (produced("intensity").as_f64().unwrap() as f32).to_bits()
                );
                assert_eq!(light.emission.color.to_vec(), floats(produced("color")));
                assert_eq!(
                    light.emission.exposure,
                    produced("exposure").as_f64().unwrap() as f32
                );
                let radius = produced("radius").as_f64().map_or(0.5, |v| v as f32);
                assert!(
                    matches!(light.geometry, crate::LightGeometry::Sphere { radius: r, .. } if r == radius),
                    "{key}: {:?}",
                    light.geometry
                );
            }
        }
    }

    #[test]
    fn texture_channels_are_read_as_storm_reads_them() {
        let (store, live) = open();
        let scene = Scene::new(live.stage(), &store);
        let mut list = DrawList::new(DrawOptions::default());
        list.refresh(&scene);
        let item = list.material(store.path("/Looks/Channels")).unwrap();
        let read = |value: &MaterialValue<f32>| match value {
            MaterialValue::Texture { texture, channel } => (
                store
                    .paths()
                    .display(item.textures[*texture].shader, store.tokens()),
                *channel,
            ),
            other => panic!("{other:?}"),
        };
        // Through a referenced node graph's `float3` output, as the public
        // production scenes connect metalness and roughness maps.
        assert_eq!(
            read(&item.metallic),
            ("/Looks/Channels/Metal/Texture".into(), TextureChannel::R)
        );
        assert_eq!(
            read(&item.roughness),
            ("/Looks/Channels/Rough".into(), TextureChannel::R)
        );
        assert_eq!(
            read(&item.opacity),
            ("/Looks/Channels/Mask".into(), TextureChannel::A)
        );
        assert!(matches!(
            item.normal,
            MaterialValue::Texture {
                channel: TextureChannel::Rgb,
                ..
            }
        ));
        assert_eq!(item.diffuse_color, MaterialValue::Constant([0.18; 3]));
        assert_eq!(item.occlusion, MaterialValue::Constant(1.0));
        let mask = Some(store.path("/Looks/Channels/Mask"));
        assert_eq!(
            item.notes,
            [
                super::MaterialNote {
                    kind: MaterialNoteKind::ChannelMismatch("r".into()),
                    prim: mask,
                    input: Some("diffuseColor".into()),
                },
                super::MaterialNote {
                    kind: MaterialNoteKind::UnsupportedOutput("rgba".into()),
                    prim: mask,
                    input: Some("occlusion".into()),
                },
            ]
        );
    }

    #[test]
    fn broken_inputs_are_noted_per_input() {
        let (store, live) = open();
        let scene = Scene::new(live.stage(), &store);
        let mut list = DrawList::new(DrawOptions::default());
        list.refresh(&scene);
        let broken = list.material(store.path("/Looks/Broken")).unwrap();
        let noted = |input: &str| -> Vec<MaterialNoteKind> {
            broken
                .notes
                .iter()
                .filter(|note| note.input.as_deref() == Some(input))
                .map(|note| note.kind.clone())
                .collect()
        };
        assert_eq!(
            noted("diffuseColor"),
            [MaterialNoteKind::UnresolvedConnection]
        );
        assert_eq!(noted("metallic"), [MaterialNoteKind::Cycle]);
        assert_eq!(
            noted("roughness"),
            [MaterialNoteKind::UnsupportedShader(Some("MyNoise".into()))]
        );
        assert_eq!(noted("clearcoat"), [MaterialNoteKind::MultipleSources]);
        assert_eq!(noted("emissiveColor"), [MaterialNoteKind::MissingTexture]);
        let animated = list.material(store.path("/Looks/Animated")).unwrap();
        assert!(
            animated
                .notes
                .iter()
                .any(|note| note.kind == MaterialNoteKind::NoValue
                    && note.input.as_deref() == Some("roughness"))
        );
        let bare = list.material(store.path("/Looks/Bare")).unwrap();
        assert_eq!(bare.context, None);
        assert_eq!(bare.diffuse_color, MaterialValue::Constant([0.18; 3]));
        assert_eq!(bare.alpha, super::AlphaMode::Opaque);
        let plain = list.material(store.path("/Looks/Plain")).unwrap();
        assert_eq!(plain.alpha, super::AlphaMode::Blend { presence: true });
        assert!(plain.notes.is_empty(), "{:?}", plain.notes);
    }

    #[test]
    fn a_material_is_read_again_only_when_what_it_reads_changes() {
        use layerstack::{EditTarget, Value};
        use layerstack_schemas::usd_shade::MaterialBindingApi;
        use layerstack_schemas::{MaterialPurpose, PrimEdit, SchemaEdit};

        let (mut store, mut live) = open();
        let mut list = DrawList::new(DrawOptions::default());
        let diff = list.refresh(&Scene::new(live.stage(), &store));
        assert_eq!(diff.materials_added.len(), 10);
        assert_eq!(diff.work.materials_read, 10);
        let edit = |store: &mut InMemoryStore,
                    live: &mut LiveStage,
                    list: &mut DrawList,
                    author: &dyn Fn(&mut SchemaEdit<'_>)| {
            let mut edit = SchemaEdit::new(live.stage(), store, EditTarget::for_layer(LayerId(1)));
            author(&mut edit);
            let transaction = edit.finish();
            let applied = live.apply(store, &transaction).unwrap();
            list.apply_changes(&Scene::new(live.stage(), store), &applied.changes)
        };
        let path = |text: &str| store.path(text);
        let (plain, graph, textured, lamp) = (
            path("/Looks/Plain"),
            path("/Looks/Graph"),
            path("/Looks/Textured"),
            path("/Lights/Lamp"),
        );
        let (surface, group, albedo, rig, mesh) = (
            path("/Looks/Plain/Surface"),
            path("/Looks/Graph"),
            path("/Looks/Textured/Albedo"),
            path("/Lights/Rig"),
            path("/World/Plain"),
        );

        // A surface input: that material alone, no item.
        let diff = edit(&mut store, &mut live, &mut list, &|edit| {
            PrimEdit::from_path_unchecked(surface).write_value(
                edit,
                "inputs:roughness",
                Value::Float(0.9),
            );
        });
        assert_eq!(diff.materials_changed, [plain]);
        assert_eq!(diff.work.materials_read, 1);
        assert_eq!(diff.work.items_read, 0);
        assert_eq!(
            list.material(plain).unwrap().roughness,
            MaterialValue::Constant(0.9)
        );

        // A material interface input a node graph forwards.
        let diff = edit(&mut store, &mut live, &mut list, &|edit| {
            PrimEdit::from_path_unchecked(group).write_value(
                edit,
                "inputs:tint",
                Value::Vec3f([1.0, 0.0, 0.0]),
            );
        });
        assert_eq!(diff.materials_changed, [graph]);
        assert_eq!(diff.work.materials_read, 1);
        assert_eq!(
            list.material(graph).unwrap().diffuse_color,
            MaterialValue::Constant([1.0, 0.0, 0.0])
        );

        // A texture's file.
        let diff = edit(&mut store, &mut live, &mut list, &|edit| {
            PrimEdit::from_path_unchecked(albedo).write_value(
                edit,
                "inputs:file",
                Value::Asset("./textures/other.png".into()),
            );
        });
        assert_eq!(diff.materials_changed, [textured]);
        assert_eq!(diff.work.materials_read, 1);
        let texture = &list.material(textured).unwrap().textures[0];
        assert_eq!(
            texture.file.identifier.as_deref(),
            Some("./textures/other.png")
        );

        // A light's connected input: the light alone.
        let diff = edit(&mut store, &mut live, &mut list, &|edit| {
            PrimEdit::from_path_unchecked(rig).write_value(edit, "inputs:power", Value::Float(9.0));
        });
        assert_eq!(diff.lights_changed, [crate::LightId::from(lamp)]);
        assert_eq!(diff.work.lights_read, 1);
        assert_eq!(diff.work.materials_read, 0);
        assert_eq!(list.light(lamp).unwrap().emission.intensity, 9.0);

        // Rebinding to a held material reads no material; the one no item
        // binds any more is dropped.
        let diff = edit(&mut store, &mut live, &mut list, &|edit| {
            MaterialBindingApi::apply(edit, mesh)
                .unwrap()
                .bind(edit, graph, None, &MaterialPurpose::All)
                .unwrap();
        });
        assert_eq!(diff.materials_removed, [plain]);
        assert_eq!(diff.work.materials_read, 0);
        assert!(list.material(plain).is_none());

        // A new time reads only the material whose inputs might vary.
        let diff = list.set_time(&Scene::new(live.stage(), &store), Time::at(10.0));
        assert_eq!(diff.work.materials_read, 1);
        let animated = store.path("/Looks/Animated");
        assert_eq!(diff.materials_changed, [animated]);
        assert_eq!(
            list.material(animated).unwrap().roughness,
            MaterialValue::Constant(0.5)
        );
    }
}
