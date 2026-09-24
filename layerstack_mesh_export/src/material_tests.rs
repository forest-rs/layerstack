// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::ast;
use layerstack_usda::parser::parse;
use layerstack_usda::writer::WriteError;

use crate::{
    Channel, ColorInput, ExportError, Faces, FloatInput, Material, MaterialProblem, Mesh,
    MeshProblem, PackageFile, Primvar, PrimvarData, Scene, StageSettings, Texture, UpAxis, Wrap,
    Xform,
};

const POINTS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
const ST: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];

fn tri() -> Mesh<'static> {
    Mesh::new("Tri", &POINTS, Faces::Triangles(&[0, 1, 2])).with_uvs(Primvar::vertex(&ST))
}

fn scene<'a>(mesh: Mesh<'a>, materials: Vec<Material<'a>>) -> Scene<'a> {
    let mut scene = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    );
    scene.materials = materials;
    scene
}

/// A glTF-shaped material: base color with alpha, packed
/// occlusion/roughness/metallic, and a normal map.
fn painted() -> Material<'static> {
    let base = Texture::new("textures/base.png");
    let orm = Texture::new("textures/orm.png").with_wrap(Wrap::Clamp, Wrap::Mirror);
    Material::new("Painted")
        .with_diffuse_color(ColorInput::texture(base).scaled([1.0, 0.5, 0.5]))
        .with_opacity(FloatInput::texture(base, Channel::A).scaled(0.5, 0.0))
        .with_opacity_threshold(0.25)
        .with_metallic(FloatInput::texture(orm, Channel::B))
        .with_roughness(FloatInput::texture(orm, Channel::G).scaled(0.8, 0.0))
        .with_occlusion(FloatInput::texture(orm, Channel::R).scaled(0.5, 0.5))
        .with_normal_map(Texture::new("textures/normal.png"))
        .with_emissive_color([0.1, 0.0, 0.0])
}

#[test]
fn golden_textured_material_and_binding() {
    let text = scene(tri().with_material("Painted"), alloc::vec![painted()])
        .to_usda()
        .unwrap();
    let expected = r#"#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "Root"
{
    def Mesh "Tri" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        float3[] extent = [(0, 0, 0), (1, 1, 0)]
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        uniform token orientation = "rightHanded"
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
        uniform token subdivisionScheme = "none"
        rel material:binding = </Root/Materials/Painted>
    }

    def Scope "Materials"
    {
        def Material "Painted"
        {
            token outputs:surface.connect = </Root/Materials/Painted/PreviewSurface.outputs:surface>

            def Shader "PreviewSurface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Root/Materials/Painted/DiffuseColorTexture.outputs:rgb>
                color3f inputs:emissiveColor = (0.1, 0, 0)
                float inputs:metallic.connect = </Root/Materials/Painted/MetallicTexture.outputs:b>
                normal3f inputs:normal.connect = </Root/Materials/Painted/NormalTexture.outputs:rgb>
                float inputs:occlusion.connect = </Root/Materials/Painted/MetallicTexture.outputs:r>
                float inputs:opacity.connect = </Root/Materials/Painted/DiffuseColorTexture.outputs:a>
                float inputs:opacityThreshold = 0.25
                float inputs:roughness.connect = </Root/Materials/Painted/MetallicTexture.outputs:g>
                int inputs:useSpecularWorkflow = 0
                token outputs:surface
            }

            def Shader "DiffuseColorTexture"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @textures/base.png@
                float2 inputs:st.connect = </Root/Materials/Painted/TexCoordReader.outputs:result>
                token inputs:sourceColorSpace = "sRGB"
                token inputs:wrapS = "repeat"
                token inputs:wrapT = "repeat"
                float4 inputs:scale = (1, 0.5, 0.5, 0.5)
                float3 outputs:rgb
                float outputs:a
            }

            def Shader "MetallicTexture"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @textures/orm.png@
                float2 inputs:st.connect = </Root/Materials/Painted/TexCoordReader.outputs:result>
                token inputs:sourceColorSpace = "raw"
                token inputs:wrapS = "clamp"
                token inputs:wrapT = "mirror"
                float4 inputs:scale = (0.5, 0.8, 1, 1)
                float4 inputs:bias = (0.5, 0, 0, 0)
                float outputs:r
                float outputs:g
                float outputs:b
            }

            def Shader "NormalTexture"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @textures/normal.png@
                float2 inputs:st.connect = </Root/Materials/Painted/TexCoordReader.outputs:result>
                token inputs:sourceColorSpace = "raw"
                token inputs:wrapS = "repeat"
                token inputs:wrapT = "repeat"
                float4 inputs:scale = (2, 2, 2, 1)
                float4 inputs:bias = (-1, -1, -1, 0)
                float3 outputs:rgb
            }

            def Shader "TexCoordReader"
            {
                uniform token info:id = "UsdPrimvarReader_float2"
                string inputs:varname = "st"
                float2 outputs:result
            }
        }
    }
}
"#;
    assert_eq!(text, expected, "USDA output");
    assert!(parse(&text).diagnostics.is_empty(), "re-parses");
}

/// `(shader name, file, sourceColorSpace)` for every texture node.
fn texture_nodes(text: &str) -> Vec<(String, String, String)> {
    let parsed = parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    fn prims<'p, 'a>(prim: &'p ast::Prim<'a>, out: &mut Vec<&'p ast::Prim<'a>>) {
        out.push(prim);
        for child in &prim.children {
            if let ast::PrimChild::Prim(p) = child {
                prims(p, out);
            }
        }
    }
    let mut all = Vec::new();
    for prim in &parsed.layer.prims {
        prims(prim, &mut all);
    }
    let value = |prim: &ast::Prim<'_>, name: &str| {
        prim.children.iter().find_map(|c| match c {
            ast::PrimChild::Attribute(a) if a.name == name => match &a.default {
                Some(ast::Value::String(s) | ast::Value::Asset(s)) => Some(String::from(*s)),
                _ => None,
            },
            _ => None,
        })
    };
    all.iter()
        .filter(|p| value(p, "info:id").as_deref() == Some("UsdUVTexture"))
        .map(|p| {
            (
                p.name.into(),
                value(p, "inputs:file").unwrap(),
                value(p, "inputs:sourceColorSpace").unwrap(),
            )
        })
        .collect()
}

#[test]
fn color_textures_are_srgb_and_data_textures_raw() {
    let shared = Texture::new("textures/shared.png");
    let material = Material::new("M")
        .with_diffuse_color(ColorInput::texture(Texture::new("textures/albedo.png")))
        .with_emissive_color(ColorInput::texture(Texture::new("textures/glow.png")))
        .with_metallic(FloatInput::texture(shared, Channel::B))
        .with_roughness(FloatInput::texture(shared, Channel::G))
        .with_occlusion(FloatInput::texture(
            Texture::new("textures/ao.png"),
            Channel::R,
        ))
        // Alpha alone is read raw.
        .with_opacity(FloatInput::texture(
            Texture::new("textures/mask.png"),
            Channel::A,
        ))
        .with_normal_map(Texture::new("textures/normal.png"));
    let text = scene(tri().with_material("M"), alloc::vec![material])
        .to_usda()
        .unwrap();
    let nodes = texture_nodes(&text);
    let expect = [
        ("DiffuseColorTexture", "textures/albedo.png", "sRGB"),
        ("EmissiveColorTexture", "textures/glow.png", "sRGB"),
        ("MetallicTexture", "textures/shared.png", "raw"),
        ("NormalTexture", "textures/normal.png", "raw"),
        ("OcclusionTexture", "textures/ao.png", "raw"),
        ("OpacityTexture", "textures/mask.png", "raw"),
    ];
    let nodes: Vec<(&str, &str, &str)> = nodes
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    assert_eq!(nodes, expect, "one node per image and color space");
}

#[test]
fn one_image_in_two_color_spaces_is_read_twice() {
    // The same file as a color (sRGB) and as data (raw) needs two nodes;
    // so do two reads of one channel with different remaps.
    let image = Texture::new("textures/a.png");
    let material = Material::new("M")
        .with_diffuse_color(ColorInput::texture(image))
        .with_metallic(FloatInput::texture(image, Channel::G))
        .with_roughness(FloatInput::texture(image, Channel::G).scaled(0.5, 0.0));
    let text = scene(tri().with_material("M"), alloc::vec![material])
        .to_usda()
        .unwrap();
    let nodes = texture_nodes(&text);
    let names: Vec<&str> = nodes.iter().map(|n| n.0.as_str()).collect();
    assert_eq!(
        names,
        ["DiffuseColorTexture", "MetallicTexture", "RoughnessTexture"],
        "{text}"
    );
    assert!(
        text.contains(
            "float inputs:roughness.connect = </Root/Materials/M/RoughnessTexture.outputs:g>"
        ),
        "{text}"
    );
}

#[test]
fn untextured_material_authors_constants_only() {
    let material = Material::new("Plain")
        .with_diffuse_color([0.8, 0.1, 0.1])
        .with_roughness(0.4)
        .with_metallic(FloatInput::Constant(1.0))
        .with_opacity(0.5);
    let text = scene(tri().with_material("Plain"), alloc::vec![material])
        .to_usda()
        .unwrap();
    assert!(texture_nodes(&text).is_empty(), "no texture nodes");
    assert!(!text.contains("UsdPrimvarReader"), "no primvar reader");
    for line in [
        "color3f inputs:diffuseColor = (0.8, 0.1, 0.1)",
        "float inputs:metallic = 1",
        "float inputs:opacity = 0.5",
        "float inputs:roughness = 0.4",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

#[test]
fn second_uv_set_gets_its_own_reader() {
    let uv1 = [[0.5, 0.5]; 3];
    let mesh = tri()
        .with_primvar("st1", Primvar::vertex(PrimvarData::TexCoord2(&uv1)))
        .with_material("M");
    let material = Material::new("M")
        .with_diffuse_color(ColorInput::texture(Texture::new("textures/a.png")))
        .with_occlusion(FloatInput::texture(
            Texture::new("textures/ao.png").with_uv_set("st1"),
            Channel::R,
        ));
    let text = scene(mesh, alloc::vec![material]).to_usda().unwrap();
    for line in [
        "</Root/Materials/M/TexCoordReader.outputs:result>",
        "</Root/Materials/M/TexCoordReader1.outputs:result>",
        "string inputs:varname = \"st1\"",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

#[test]
fn rejects_unusable_materials_and_bindings() {
    assert_eq!(
        scene(tri().with_material("Missing"), alloc::vec![painted()]).to_usda(),
        Err(ExportError::UnknownMaterial {
            path: "/Root/Tri".into(),
            material: "Missing".into()
        }),
        "binding to an undefined material"
    );
    let bare = Mesh::new("Tri", &POINTS, Faces::Triangles(&[0, 1, 2])).with_material("Painted");
    assert_eq!(
        scene(bare, alloc::vec![painted()]).to_usda(),
        Err(ExportError::InvalidMesh {
            path: "/Root/Tri".into(),
            problem: MeshProblem::MissingTexCoords {
                material: "Painted".into(),
                uv_set: "st".into()
            }
        }),
        "textures need the mesh's UV set"
    );
    let nan = Material::new("Nan").with_roughness(f32::NAN);
    assert_eq!(
        scene(tri(), alloc::vec![nan]).to_usda(),
        Err(ExportError::InvalidMaterial {
            path: "/Root/Materials/Nan".into(),
            problem: MaterialProblem::NonFinite {
                input: "inputs:roughness"
            }
        }),
        "non-finite constant"
    );
    let empty = Material::new("Empty").with_normal_map(Texture::new(""));
    assert_eq!(
        scene(tri(), alloc::vec![empty]).to_usda(),
        Err(ExportError::InvalidMaterial {
            path: "/Root/Materials/Empty".into(),
            problem: MaterialProblem::EmptyTexturePath
        }),
        "empty texture path"
    );
    assert_eq!(
        scene(tri(), alloc::vec![Material::new("M"), Material::new("M")]).to_usda(),
        Err(ExportError::Usda(WriteError::Duplicate {
            path: "/Root/Materials/M".into()
        })),
        "duplicate material names"
    );
}

#[test]
fn repeated_color_scaling_multiplies() {
    // Factors are powers of two, so products are exact.
    let a = [0.5, 0.25, 2.0];
    let b = [0.5, 4.0, 0.125];
    let ab = [0.25, 1.0, 0.25];
    let texture = ColorInput::texture(Texture::new("textures/c.png"));
    assert_eq!(
        texture.scaled(a).scaled(b),
        texture.scaled(ab),
        "texture multipliers compose"
    );
    assert_eq!(
        texture.scaled([0.5; 3]).scaled([0.5; 3]),
        ColorInput::Texture {
            texture: Texture::new("textures/c.png"),
            scale: [0.25; 3]
        },
        "two halvings are a quarter"
    );
    let constant = ColorInput::Constant([1.0, 0.5, 0.25]);
    assert_eq!(
        constant.scaled(a).scaled(b),
        constant.scaled(ab),
        "constants compose"
    );
}

#[test]
fn repeated_float_remaps_compose() {
    // (x * 0.5 + 0.25) * 2 + 0.125 = x * 1 + 0.625.
    let texture = FloatInput::texture(Texture::new("textures/f.png"), Channel::G);
    let twice = texture.scaled(0.5, 0.25).scaled(2.0, 0.125);
    assert_eq!(twice, texture.scaled(1.0, 0.625), "texture remaps compose");
    assert_eq!(
        twice,
        FloatInput::Texture {
            texture: Texture::new("textures/f.png"),
            channel: Channel::G,
            scale: 1.0,
            bias: 0.625
        },
        "scale s1 * s2, bias b1 * s2 + b2"
    );
    let constant = FloatInput::Constant(0.5);
    assert_eq!(
        constant.scaled(0.5, 0.25).scaled(2.0, 0.125),
        constant.scaled(1.0, 0.625),
        "constants compose"
    );
    assert_eq!(
        constant.scaled(0.5, 0.25).scaled(2.0, 0.125),
        FloatInput::Constant(1.125),
        "constant value"
    );
}

#[test]
fn usdz_requires_textures_to_be_packaged() {
    let png = b"\x89PNG\r\n\x1a\n";
    let packaged = [
        PackageFile::new("textures/base.png", png),
        PackageFile::new("textures/orm.png", png),
    ];
    assert_eq!(
        scene(tri().with_material("Painted"), alloc::vec![painted()]).to_usdz(&packaged),
        Err(ExportError::UnpackagedAsset {
            asset: "textures/normal.png".into()
        }),
        "the normal map is not in the package"
    );
    let mut all = packaged.to_vec();
    all.push(PackageFile::new("textures/normal.png", png));
    let bytes = scene(tri().with_material("Painted"), alloc::vec![painted()])
        .to_usdz(&all)
        .expect("self-contained package");
    assert_eq!(&bytes[..4], b"PK\x03\x04", "zip signature");
}
