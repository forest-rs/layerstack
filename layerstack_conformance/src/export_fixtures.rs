// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Exporter output for independent (non-Layerstack) validation.
//!
//! Round-trip tests read the writer's output with this workspace's own
//! parser, so a mistake shared by reader and writer goes unnoticed. These
//! fixtures are meant for external tools instead: `usdcat`, `usdchecker`,
//! Python's `zipfile` and, for the material fixtures, `usdrecord` renders,
//! driven by `layerstack_conformance/scripts/export_interop.sh` and, when
//! `usdcat` is on `PATH`, by the `export_interop` test.
//!
//! Every [`Fixture`] with [`Expect::Valid`] or [`Expect::ValidArkit`] must
//! be accepted by those tools. [`Expect::Invalid`] fixtures are negative
//! controls: they show that the selected validators really detect the
//! problem they name.
//!
//! [`documents`] are also written as USDC, so a USDA and a USDC file of the
//! same authored layer can be compared through OpenUSD (`usdcat`), and as
//! both USDZ profiles.

use std::path::{Path, PathBuf};

use layerstack_mesh_export::{
    Channel, ColorInput, Faces, FamilyType, FloatInput, Material, Mesh, PackageFile, Primvar,
    PrimvarData, Scene, StageSettings, Texture, Transform, UpAxis, UsdzProfile, Xform,
};
use layerstack_usda::writer::{Attribute, Document, Metadatum, Prim, Value};
use layerstack_usdc::writer::write_document;

/// What an external validator should conclude about a fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Exporter output; every tool must accept it.
    Valid,
    /// An `ARKit`-profile package: valid, and its only USD layer is a USDC
    /// root.
    ValidArkit,
    /// A negative control; the named validator must reject it.
    Invalid(&'static str),
}

/// One written fixture file.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// Path of the written file.
    pub path: PathBuf,
    /// Expected validator outcome.
    pub expect: Expect,
}

/// An 8x8 grey RGB checker PNG.
pub fn checker_png() -> Vec<u8> {
    rgb_png(8, 8, |x, y| {
        let v = if (x + y) % 2 == 0 { 0x20 } else { 0xff };
        [v, v, v]
    })
}

/// An 8-bit RGB PNG of `pixel(x, y)` (stored-deflate IDAT, valid CRCs).
///
/// # Panics
///
/// Panics if the image data exceeds one stored deflate block (64 KiB).
pub fn rgb_png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = layerstack_usdz::crc32::crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            raw.extend_from_slice(&pixel(x, y));
        }
    }
    // zlib stream with one stored block, then Adler-32.
    let mut zlib = vec![0x78, 0x01, 0x01];
    let len = u16::try_from(raw.len()).unwrap();
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&raw);
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// A 10 ms mono 8 kHz 16-bit PCM WAV of silence.
pub fn silence_wav() -> Vec<u8> {
    let samples = 80_u32;
    let data_len = samples * 2;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8000_u32.to_le_bytes());
    wav.extend_from_slice(&16000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    wav
}

fn stage_metadata(doc: &mut Document, root: &str) {
    doc.default_prim = Some(root.into());
    doc.metadata
        .push(Metadatum::new("metersPerUnit", Value::Double(1.0)));
    doc.metadata
        .push(Metadatum::new("upAxis", Value::Token("Z".into())));
}

/// Non-ASCII identifiers at the edge of the XID tables: a combining mark
/// (U+0301) continuing a name, CJK, and namespaced property names.
fn identifiers() -> Document {
    let mut root = Prim::def("Xform", "Root");
    let mut accented = Prim::def("Scope", "cafe\u{301}");
    accented.push_property(Attribute::new("ns:nai\u{308}ve", "int", Value::Int(1)).custom());
    let mut cjk = Prim::def("Scope", "\u{9802}\u{70b9}");
    cjk.push_property(Attribute::new("_x1:\u{3b1}\u{3b2}", "float", Value::Float(0.5)).custom());
    root.children.push(accented);
    root.children.push(cjk);
    let mut doc = Document::new();
    stage_metadata(&mut doc, "Root");
    doc.prims.push(root);
    doc
}

/// One attribute per supported value type and role alias, scalar and array,
/// plus declared-only registered types without a writer value.
fn types() -> Document {
    let mut p = Prim::def("Scope", "Root");
    let mut add = |name: &str, ty: &str, v: Value| {
        p.push_property(Attribute::new(name, ty, v).custom());
    };
    add("b", "bool", Value::Bool(true));
    add("i", "int", Value::Int(-7));
    add("u", "uint", Value::UInt(7));
    add("l", "int64", Value::Int64(-(1 << 40)));
    add("f", "float", Value::Float(0.1));
    add("d", "double", Value::Double(1e-300));
    add("t", "timecode", Value::Double(24.0));
    add(
        "s",
        "string",
        Value::String("quote \" tab \t newline \n back \\ end".into()),
    );
    add("k", "token", Value::Token("tok".into()));
    add("f2", "float2", Value::Float2([1.0, 2.0]));
    add("f3", "float3", Value::Float3([1.0, 2.0, 3.0]));
    add("f4", "float4", Value::Float4([1.0, 2.0, 3.0, 4.0]));
    add("d2", "double2", Value::Double2([1.0, 2.0]));
    add("d3", "double3", Value::Double3([1.0, 2.0, 3.0]));
    add("d4", "double4", Value::Double4([1.0, 2.0, 3.0, 4.0]));
    add("i2", "int2", Value::Int2([1, 2]));
    add("i3", "int3", Value::Int3([1, 2, 3]));
    add("i4", "int4", Value::Int4([1, 2, 3, 4]));
    add("p", "point3f", Value::Float3([0.0, 1.0, 2.0]));
    add("n", "normal3f", Value::Float3([0.0, 0.0, 1.0]));
    add("v", "vector3f", Value::Float3([1.0, 0.0, 0.0]));
    add("c3", "color3f", Value::Float3([1.0, 0.5, 0.25]));
    add("c4", "color4f", Value::Float4([1.0, 0.5, 0.25, 1.0]));
    add("st2", "texCoord2f", Value::Float2([0.5, 0.5]));
    add("st3", "texCoord3f", Value::Float3([0.5, 0.5, 0.5]));
    add("pd", "point3d", Value::Double3([0.0, 1.0, 2.0]));
    add("cd", "color3d", Value::Double3([1.0, 0.5, 0.25]));
    add(
        "m",
        "matrix4d",
        Value::Matrix4d(Transform::IDENTITY.usd_rows()),
    );
    add(
        "fr",
        "frame4d",
        Value::Matrix4d(Transform::from_translation([1.0, 2.0, 3.0]).usd_rows()),
    );
    add("bools", "bool[]", Value::BoolArray(vec![true, false]));
    add("ia", "int[]", Value::IntArray(vec![1, -2, 3]));
    add("ua", "uint[]", Value::UIntArray(vec![1, 2]));
    add("la", "int64[]", Value::Int64Array(vec![1 << 40]));
    add(
        "fa",
        "float[]",
        Value::FloatArray(vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN, -0.0]),
    );
    add("da", "double[]", Value::DoubleArray(vec![0.1, 2.5e10]));
    add(
        "sa",
        "string[]",
        Value::StringArray(vec!["a".into(), "b c".into()]),
    );
    add(
        "ka",
        "token[]",
        Value::TokenArray(vec!["x".into(), "y".into()]),
    );
    add(
        "pa",
        "point3f[]",
        Value::Float3Array(vec![[0.0; 3], [1.0; 3]]),
    );
    add(
        "na",
        "normal3f[]",
        Value::Float3Array(vec![[0.0, 0.0, 1.0]]),
    );
    add("ca", "color4f[]", Value::Float4Array(vec![[1.0; 4]]));
    add("sta", "texCoord2f[]", Value::Float2Array(vec![[0.0, 1.0]]));
    add(
        "d3a",
        "double3[]",
        Value::Double3Array(vec![[1.0, 2.0, 3.0]]),
    );
    add("i2a", "int2[]", Value::Int2Array(vec![[1, 2]]));
    // `[i, j, k, r]` bits: a 90° turn about Z, and a component (the half
    // nearest 1/3) with no short decimal spelling.
    add(
        "qa",
        "quath[]",
        Value::QuathArray(vec![[0, 0, 0x39a8, 0x39a8], [0x3555, 0, 0, 0x3c00]]),
    );
    for (name, ty) in [
        ("h", "half"),
        ("q", "quatf"),
        ("m2", "matrix2d"),
        ("hc", "color3h[]"),
    ] {
        p.push_property(Attribute {
            value: None,
            ..Attribute::new(name, ty, Value::Int(0)).custom()
        });
    }
    let mut doc = Document::new();
    stage_metadata(&mut doc, "Root");
    doc.prims.push(p);
    doc
}

/// Layer, prim and attribute metadata, including nested typed dictionaries
/// (dictionaries are metadata values, never attribute types).
fn metadata() -> Document {
    let data = Value::Dictionary(vec![
        ("exedra:path".into(), Value::String("assembly/part".into())),
        ("count".into(), Value::Int(3)),
        ("scale".into(), Value::Double(0.5)),
        (
            "tags".into(),
            Value::TokenArray(vec!["a".into(), "b".into()]),
        ),
        (
            "nested".into(),
            Value::Dictionary(vec![("deep".into(), Value::Float3([1.0, 2.0, 3.0]))]),
        ),
    ]);
    let mut root = Prim::def("Xform", "Root");
    root.metadata
        .push(Metadatum::new("kind", Value::Token("component".into())));
    root.metadata
        .push(Metadatum::new("customData", data.clone()));
    root.push_property(
        Attribute::new("exedra:note", "string", Value::String("n".into()))
            .custom()
            .with_metadata("doc", Value::String("An annotated attribute.".into()))
            .with_metadata("customData", data.clone()),
    );
    let mut doc = Document::new();
    stage_metadata(&mut doc, "Root");
    // A double that is not exactly a float: USDA and USDC must agree on it.
    doc.metadata[0].value = Value::Double(0.1);
    doc.metadata.push(Metadatum::new(
        "doc",
        Value::String("Exporter fixture.".into()),
    ));
    doc.metadata.push(Metadatum::new("customLayerData", data));
    doc.prims.push(root);
    doc
}

/// Dictionary-valued metadata on every owner: nested UI `limits`,
/// `uiHints`, `symmetryArguments`, `fallbackPrimTypes`, and profile data in
/// both spellings — the prim `profilesInfo` that `UsdProfilesClaimsAPI`
/// documents (unregistered in OpenUSD v26.08) and the `customData` entry
/// `ClaimsAPI` actually reads and writes. The claims are data only.
fn metadata_dictionaries() -> Document {
    let dict = |entries: Vec<(&str, Value)>| {
        Value::Dictionary(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    };
    let profiles = dict(vec![
        (
            "capabilityUsages",
            dict(vec![
                ("usd.geom.mesh", Value::String("hard".into())),
                ("usd.shading.mtlx", Value::String("soft".into())),
            ]),
        ),
        (
            "profileCompatibility",
            dict(vec![(
                "vnd.apple.visionos_v1",
                Value::StringArray(vec!["usd.geom.hairAndFur".into()]),
            )]),
        ),
    ]);
    let hints = dict(vec![("displayGroup", Value::String("Shape".into()))]);
    let mut root = Prim::def("Xform", "Root");
    root.metadata.push(Metadatum::new(
        "apiSchemas",
        Value::TokenListOp(layerstack_usda::writer::ListOp::prepend(vec![
            "ClaimsAPI".into(),
        ])),
    ));
    root.metadata
        .push(Metadatum::new("profilesInfo", profiles.clone()));
    root.metadata.push(Metadatum::new(
        "customData",
        dict(vec![("profilesInfo", profiles)]),
    ));
    root.metadata.push(Metadatum::new("uiHints", hints.clone()));
    root.metadata.push(Metadatum::new(
        "symmetryArguments",
        dict(vec![("axis", Value::Token("x".into()))]),
    ));
    root.push_property(
        Attribute::new("exedra:size", "float", Value::Float(1.0))
            .custom()
            .with_metadata(
                "limits",
                dict(vec![
                    (
                        "soft",
                        dict(vec![("min", Value::Float(0.0)), ("max", Value::Float(5.0))]),
                    ),
                    ("hard", dict(vec![("max", Value::Float(10.0))])),
                ]),
            )
            .with_metadata("uiHints", hints.clone()),
    );
    let mut rel = layerstack_usda::writer::Relationship::new("exedra:target", "/Root").custom();
    rel.metadata.push(Metadatum::new("uiHints", hints));
    root.push_property(rel);
    let mut doc = Document::new();
    stage_metadata(&mut doc, "Root");
    doc.metadata.push(Metadatum::new(
        "fallbackPrimTypes",
        dict(vec![(
            "ExedraShape",
            Value::TokenArray(vec!["Xform".into()]),
        )]),
    ));
    doc.prims.push(root);
    doc
}

/// Reorder statements, list-edited relationship targets and connections,
/// explicit empty lists and a value block default, in one property order
/// that interleaves attributes and relationships.
fn list_edits() -> Document {
    use layerstack_usda::writer::{ListOp, Relationship, Specifier};
    let mut prim = Prim::def("Xform", "A");
    prim.property_order = Some(vec!["y".into(), "x".into()]);
    prim.prim_order = Some(vec!["D".into(), "C".into()]);
    prim.push_property(Relationship {
        targets: Some(ListOp {
            deleted: vec!["/A/D".into()],
            prepended: vec!["/A/C".into(), "/A.x".into()],
            ..ListOp::default()
        }),
        ..Relationship::new("r", "/A")
    });
    prim.push_property(Attribute::new("x", "float", Value::Block));
    let mut y = Attribute::declared("y", "float");
    y.connections = Some(ListOp::explicit(Vec::new()));
    prim.push_property(y);
    let mut z = Attribute::new("z", "float", Value::Float(1.0)).uniform();
    z.connections = Some(ListOp {
        appended: vec!["/A.x".into()],
        ..ListOp::default()
    });
    prim.push_property(z);
    let mut s = Relationship {
        targets: Some(ListOp::prepend(vec!["/A/C".into()])),
        ..Relationship::new("s", "/A").custom()
    };
    s.metadata
        .push(Metadatum::new("doc", Value::String("s".into())));
    prim.push_property(s);
    prim.push_property(Relationship {
        targets: Some(ListOp::explicit(Vec::new())),
        ..Relationship::new("t", "/A")
    });
    prim.children.push(Prim::def("Scope", "C"));
    prim.children.push(Prim::def("Scope", "D"));
    let mut doc = Document::new();
    stage_metadata(&mut doc, "A");
    doc.prim_order = Some(vec!["B".into(), "A".into()]);
    doc.prims.push(prim);
    doc.prims.push(Prim::new(Specifier::Over, None, "B"));
    doc
}

/// Bare-string comments on every owner and the remaining registered
/// scalar metadata: frame range and render settings path on the layer,
/// display group order and naming hints on prims and properties, an
/// `int64` array size constraint, a `float` skinning weight and a
/// relationship output name.
fn metadata_scalars() -> Document {
    use layerstack_usda::writer::Relationship;
    let mut root = Prim::def("Xform", "Root");
    root.metadata
        .push(Metadatum::new("comment", Value::String("A prim.".into())));
    root.metadata.push(Metadatum::new(
        "displayGroupOrder",
        Value::StringArray(vec!["Shape".into(), "Look".into()]),
    ));
    root.metadata
        .push(Metadatum::new("prefix", Value::String("L_".into())));
    root.metadata
        .push(Metadatum::new("symmetricPeer", Value::String("R".into())));
    root.push_property(
        Attribute::new("exedra:sizes", "int[]", Value::IntArray(vec![1, 2]))
            .custom()
            .with_metadata("comment", Value::String("An attribute.".into()))
            .with_metadata("arraySizeConstraint", Value::Int64(5_000_000_000))
            .with_metadata("weight", Value::Float(0.25))
            .with_metadata("suffix", Value::String("_x".into())),
    );
    let mut rel = Relationship::new("exedra:out", "/Root").custom();
    rel.metadata.push(Metadatum::new(
        "comment",
        Value::String("A relationship.".into()),
    ));
    rel.metadata
        .push(Metadatum::new("outputName", Value::Token("result".into())));
    root.push_property(rel);
    let mut doc = Document::new();
    stage_metadata(&mut doc, "Root");
    doc.metadata
        .push(Metadatum::new("comment", Value::String("A layer.".into())));
    doc.metadata
        .push(Metadatum::new("startFrame", Value::Double(1.0)));
    doc.metadata
        .push(Metadatum::new("endFrame", Value::Double(48.5)));
    doc.metadata.push(Metadatum::new(
        "renderSettingsPrimPath",
        Value::String("/Render/Settings".into()),
    ));
    doc.prims.push(root);
    doc
}

const QUAD_TRI_POINTS: [[f32; 3]; 5] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [2.0, 0.5, 0.25],
];
const QUAD_TRI_COUNTS: [u32; 2] = [4, 3];
const QUAD_TRI_INDICES: [u32; 7] = [0, 1, 2, 3, 1, 4, 2];

/// Every primvar interpolation, indexed and unindexed, on a quad + triangle
/// under nested transforms.
fn primvars_scene<'a>(
    normal: &'a [[f32; 3]],
    normal_idx: &'a [u32],
    st: &'a [[f32; 2]],
    st_idx: &'a [u32],
    colors: &'a [[f32; 3]],
    opacity: &'a [f32],
    weights: &'a [f32],
    ids: &'a [i32],
    regions: &'a [u32],
    tint: &'a [[f32; 4]],
) -> Scene<'a> {
    let mesh = Mesh::new(
        "Panel",
        &QUAD_TRI_POINTS,
        Faces::Polygons {
            counts: &QUAD_TRI_COUNTS,
            indices: &QUAD_TRI_INDICES,
        },
    )
    .with_normals(Primvar::face_varying(normal).with_indices(normal_idx))
    .with_uvs(Primvar::face_varying(st).with_indices(st_idx))
    .with_primvar(
        "displayColor",
        Primvar::uniform(PrimvarData::Color3(colors)),
    )
    .with_primvar(
        "displayOpacity",
        Primvar::constant(PrimvarData::Float(opacity)),
    )
    .with_primvar(
        "exedra:weight",
        Primvar::vertex(PrimvarData::Float(weights)),
    )
    .with_primvar(
        "exedra:id",
        Primvar::new(
            PrimvarData::Int(ids),
            layerstack_mesh_export::Interpolation::Varying,
        ),
    )
    .with_primvar(
        "exedra:region",
        Primvar::uniform(PrimvarData::UInt(regions)),
    )
    .with_primvar("exedra:tint", Primvar::constant(PrimvarData::Color4(tint)))
    .with_transform(Transform::from_affine_3x4([
        [-1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]));
    let root = Xform::new("Root").with_kind("component").with_xform(
        Xform::new("Rotated")
            .with_transform(Transform::from_affine_3x4([
                [0.0, -1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ]))
            .with_mesh(mesh),
    );
    Scene::new(StageSettings::new(UpAxis::Z, 1.0), root)
}

// ── Materials ───────────────────────────────────────────────────────────

/// A unit cube centred on the origin: shared corners, flat per-face
/// normals and one UV island per face (face-varying).
const CUBE_POINTS: [[f32; 3]; 8] = [
    [-0.5, -0.5, -0.5],
    [0.5, -0.5, -0.5],
    [0.5, 0.5, -0.5],
    [-0.5, 0.5, -0.5],
    [-0.5, -0.5, 0.5],
    [0.5, -0.5, 0.5],
    [0.5, 0.5, 0.5],
    [-0.5, 0.5, 0.5],
];
const CUBE_COUNTS: [u32; 6] = [4; 6];
/// Faces in order -Z, +Z, -Y, +Y, +X, -X.
const CUBE_INDICES: [u32; 24] = [
    0, 3, 2, 1, 4, 5, 6, 7, 0, 1, 5, 4, 2, 3, 7, 6, 1, 2, 6, 5, 3, 0, 4, 7,
];
const CUBE_NORMALS: [[f32; 3]; 6] = [
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
];
const CUBE_UVS: [[f32; 2]; 24] = {
    let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut out = [[0.0; 2]; 24];
    let mut i = 0;
    while i < 24 {
        out[i] = square[i % 4];
        i += 1;
    }
    out
};

/// The `usdchecker` rule that rejects a normal map read as sRGB.
pub const SRGB_NORMAL_MAP_VALIDATOR: &str = "NormalMapTextureValidator.InvalidSourceColorSpace";

/// Faces of the red subset in [`two_material_cube`]: +Z, +X and -Z.
pub const RED_FACES: [u32; 3] = [1, 4, 0];
/// Faces of the blue subset in [`two_material_cube`]: -Y, +Y and -X.
pub const BLUE_FACES: [u32; 3] = [2, 3, 5];

fn cube() -> Mesh<'static> {
    Mesh::new(
        "Cube",
        &CUBE_POINTS,
        Faces::Polygons {
            counts: &CUBE_COUNTS,
            indices: &CUBE_INDICES,
        },
    )
    .with_normals(Primvar::uniform(&CUBE_NORMALS[..]))
    .with_uvs(Primvar::face_varying(&CUBE_UVS[..]))
}

fn cube_scene(
    mesh: Mesh<'static>,
    materials: impl IntoIterator<Item = Material<'static>>,
) -> Scene<'static> {
    let mut scene = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_kind("component").with_mesh(mesh),
    );
    scene.materials = materials.into_iter().collect();
    scene
}

/// A 64x64 checker of orange and teal 8x8 cells: a base-color texture
/// whose two hues are easy to find in a render.
pub fn albedo_png() -> Vec<u8> {
    rgb_png(64, 64, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            [230, 120, 20]
        } else {
            [20, 150, 160]
        }
    })
}

/// Packed occlusion (R = 1), roughness (G = 0.55) and metallic (B = 0),
/// read as raw data.
pub fn orm_png() -> Vec<u8> {
    rgb_png(4, 4, |_, _| [255, 140, 0])
}

/// A tangent-space normal map of vertical ridges: 8-pixel bands tilted
/// towards -X and +X, encoded as `n * 0.5 + 0.5`.
pub fn ridges_normal_png() -> Vec<u8> {
    rgb_png(64, 64, |x, _| {
        if (x / 8) % 2 == 0 {
            [74, 128, 238]
        } else {
            [182, 128, 238]
        }
    })
}

/// The textured material: sRGB base color, a packed raw ORM image and a
/// raw normal map.
fn textured_material() -> Material<'static> {
    let orm = Texture::new("textures/orm.png");
    Material::new("Textured")
        .with_diffuse_color(ColorInput::texture(Texture::new("textures/albedo.png")))
        .with_occlusion(FloatInput::texture(orm, Channel::R))
        .with_roughness(FloatInput::texture(orm, Channel::G))
        .with_metallic(FloatInput::texture(orm, Channel::B))
        .with_normal_map(Texture::new("textures/ridges_normal.png"))
}

/// A cube whose faces are split between two constant materials by a
/// `materialBind` partition: red on [`RED_FACES`], blue on
/// [`BLUE_FACES`].
pub fn two_material_cube() -> Scene<'static> {
    cube_scene(
        cube()
            .with_material_subset("RedFaces", &RED_FACES, "Red")
            .with_material_subset("BlueFaces", &BLUE_FACES, "Blue")
            .with_subset_family(FamilyType::Partition),
        [
            Material::new("Red")
                .with_diffuse_color([0.8, 0.05, 0.05])
                .with_roughness(0.5),
            Material::new("Blue")
                .with_diffuse_color([0.05, 0.1, 0.8])
                .with_roughness(0.5),
        ],
    )
}

/// A cube with one textured material ([`albedo_png`], [`orm_png`],
/// [`ridges_normal_png`]).
pub fn textured_cube() -> Scene<'static> {
    cube_scene(cube().with_material("Textured"), [textured_material()])
}

/// The texture files [`textured_cube`] authors, by package path.
pub fn textured_cube_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("textures/albedo.png", albedo_png()),
        ("textures/orm.png", orm_png()),
        ("textures/ridges_normal.png", ridges_normal_png()),
    ]
}

fn package(scene: &Scene<'_>, profile: UsdzProfile, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let files: Vec<PackageFile<'_>> = files
        .iter()
        .map(|(path, bytes)| PackageFile::new(path, bytes))
        .collect();
    scene.to_usdz(profile, &files).unwrap()
}

fn material_fixtures(dir: &Path, out: &mut Vec<Fixture>) {
    let untextured = cube_scene(
        cube().with_material("Plain"),
        [Material::new("Plain")
            .with_diffuse_color([0.7, 0.7, 0.2])
            .with_emissive_color([0.05, 0.0, 0.0])
            .with_roughness(0.3)
            .with_metallic(0.0)
            .with_opacity(1.0)],
    );
    let text = untextured.to_usda().unwrap();
    write(
        dir,
        "material_untextured.usda",
        text.as_bytes(),
        Expect::Valid,
        out,
    );
    write(
        dir,
        "material_untextured.usdz",
        &untextured.to_usdz(UsdzProfile::Generic, &[]).unwrap(),
        Expect::Valid,
        out,
    );
    for (suffix, profile, expect) in [
        ("", UsdzProfile::Generic, Expect::Valid),
        ("_arkit", UsdzProfile::Arkit, Expect::ValidArkit),
    ] {
        write(
            dir,
            &format!("material_textured{suffix}.usdz"),
            &package(&textured_cube(), profile, &textured_cube_files()),
            expect,
            out,
        );
        write(
            dir,
            &format!("material_partition{suffix}.usdz"),
            &two_material_cube().to_usdz(profile, &[]).unwrap(),
            expect,
            out,
        );
    }
    let normal_only = cube_scene(
        cube().with_material("Ridged"),
        [Material::new("Ridged")
            .with_diffuse_color([0.6, 0.6, 0.6])
            .with_normal_map(Texture::new("textures/ridges_normal.png"))],
    );
    write(
        dir,
        "material_normal_map.usdz",
        &package(
            &normal_only,
            UsdzProfile::Generic,
            &[("textures/ridges_normal.png", ridges_normal_png())],
        ),
        Expect::Valid,
        out,
    );

    // Negative control for the color-space rule: the same normal map read
    // as sRGB, which `NormalMapTextureValidator` must reject. The exporter
    // cannot produce this, so the layer is edited after export.
    let srgb = normal_only.to_usda().unwrap().replace(
        "token inputs:sourceColorSpace = \"raw\"",
        "token inputs:sourceColorSpace = \"sRGB\"",
    );
    assert!(srgb.contains("\"sRGB\""), "control edits the color space");
    let normal = ridges_normal_png();
    let control = layerstack_usdz::write_usdz(&[
        PackageFile::new("scene.usda", srgb.as_bytes()),
        PackageFile::new("textures/ridges_normal.png", &normal),
    ])
    .unwrap();
    write(
        dir,
        "control_srgb_normal_map.usdz",
        &control,
        Expect::Invalid(SRGB_NORMAL_MAP_VALIDATOR),
        out,
    );
}

/// The primvars scene with its buffers, as an owned document.
fn primvars() -> Document {
    let normal = [[0.0, 0.0, 1.0]];
    let normal_idx = [0; 7];
    let st = [
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
        [1.0, 0.0],
        [2.0, 0.5],
        [1.0, 1.0],
    ];
    let st_idx = [0, 1, 2, 3, 4, 5, 6];
    let colors = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let opacity = [1.0];
    let weights = [0.0, 0.25, 0.5, 0.75, 1.0];
    let ids = [10, 11, 12, 13, 14];
    let regions = [1, 2];
    let tint = [[1.0, 1.0, 1.0, 0.5]];
    primvars_scene(
        &normal,
        &normal_idx,
        &st,
        &st_idx,
        &colors,
        &opacity,
        &weights,
        &ids,
        &regions,
        &tint,
    )
    .to_document()
    .unwrap()
}

/// The unit cube of the `mesh_to_usdz` example: 8 shared points, six quads,
/// face-varying normals and UVs (every face its own UV island), resting on
/// the ground plane.
fn cube_document() -> Document {
    const POINTS: [[f32; 3]; 8] = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    let faces: [[u32; 4]; 6] = [
        [0, 3, 2, 1],
        [4, 5, 6, 7],
        [0, 1, 5, 4],
        [2, 3, 7, 6],
        [1, 2, 6, 5],
        [3, 0, 4, 7],
    ];
    let face_normals = [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
    ];
    let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let indices: Vec<u32> = faces.iter().flatten().copied().collect();
    let normals: Vec<[f32; 3]> = face_normals
        .iter()
        .flat_map(|n| std::iter::repeat_n(*n, 4))
        .collect();
    let uvs: Vec<[f32; 2]> = (0..6).flat_map(|_| square).collect();
    let cube = Mesh::new(
        "Cube",
        &POINTS,
        Faces::Polygons {
            counts: &[4; 6],
            indices: &indices,
        },
    )
    .with_normals(Primvar::face_varying(&normals[..]))
    .with_uvs(Primvar::face_varying(&uvs[..]));
    let root = Xform::new("Root")
        .with_kind("component")
        .with_transform(Transform::from_translation([0.0, 0.0, 0.5]))
        .with_mesh(cube);
    Scene::new(StageSettings::new(UpAxis::Z, 1.0), root)
        .to_document()
        .unwrap()
}

/// A quad and a triangle sharing an edge, without primvars.
fn quad_triangle() -> Document {
    let mesh = Mesh::new(
        "Panel",
        &QUAD_TRI_POINTS,
        Faces::Polygons {
            counts: &QUAD_TRI_COUNTS,
            indices: &QUAD_TRI_INDICES,
        },
    );
    Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    )
    .to_document()
    .unwrap()
}

/// Two quads folded along a shared edge: shared positions, a hard normal
/// edge, and a UV seam whose indices differ while their values repeat.
fn uv_seam() -> Document {
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
    ];
    let normals = [
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
    ];
    let uvs = [
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [2.0, 0.0],
        [2.0, 1.0],
    ];
    let mesh = Mesh::new(
        "Fold",
        &points,
        Faces::Polygons {
            counts: &[4, 4],
            indices: &[0, 1, 2, 3, 1, 4, 5, 2],
        },
    )
    .with_normals(Primvar::face_varying(&normals[..]))
    .with_uvs(Primvar::face_varying(&uvs[..]).with_indices(&[0, 1, 2, 3, 4, 6, 7, 5]));
    Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    )
    .to_document()
    .unwrap()
}

/// An asymmetric wedge under nested, non-commuting transforms (a rotation,
/// then a mirror and a move), in centimeters with Y up.
fn nested_transforms() -> Document {
    let points = [
        [0.0, 0.0, 0.0],
        [100.0, 0.0, 0.0],
        [100.0, 50.0, 0.0],
        [0.0, 50.0, 0.0],
        [0.0, 0.0, 25.0],
        [0.0, 50.0, 25.0],
    ];
    let wedge = Mesh::new(
        "Wedge",
        &points,
        Faces::Polygons {
            counts: &[4, 4, 3, 3, 4],
            indices: &[0, 3, 2, 1, 0, 1, 4, 3, 4, 5, 3, 0, 4, 1, 1, 2, 5, 4],
        },
    );
    let rotate = Transform::from_affine_3x4([
        [0.0, -1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);
    let mirror_and_move = Transform::from_affine_3x4([
        [-1.0, 0.0, 0.0, 100.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);
    let root = Xform::new("Root").with_transform(rotate).with_xform(
        Xform::new("Mirrored")
            .with_transform(mirror_and_move)
            .with_mesh(wedge),
    );
    Scene::new(StageSettings::new(UpAxis::Y, 0.01), root)
        .to_document()
        .unwrap()
}

/// Every authored document of the fixture set, by file stem: identifiers,
/// value types, metadata, and the mesh scenes (the cube, the two-material
/// and textured material cubes, a quad and a triangle, a UV seam, nested
/// transforms, and every primvar interpolation).
/// An OpenUSD release as `(year, month)`: `(25, 11)` is v25.11. Apple's
/// tools report the same numbering as `0.25.11`.
pub type OpenUsdRelease = (u32, u32);

/// The OpenUSD release the writers target: they store metadata with the
/// types its registry (`SdfSchema` and the plugin metadata) declares.
pub const TARGET_OPENUSD: OpenUsdRelease = (26, 8);

/// Reads an OpenUSD release from a tool's version output, such as `Apple
/// USD Tools (0.25.11)` or `0.26.8`: the last `0.YY.MM` or `YY.MM` number.
pub fn parse_openusd_release(text: &str) -> Option<OpenUsdRelease> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .rev()
        .find_map(|word| {
            let parts: Vec<u32> = word
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            match parts[..] {
                [0, year, month, ..] | [year, month] if year >= 20 => Some((year, month)),
                _ => None,
            }
        })
}

/// The earliest OpenUSD release whose metadata registry a document's
/// expected encoding assumes, with the reason, or `None` when it assumes
/// nothing newer than the releases the tools are known to have.
///
/// An older OpenUSD stores such metadata as `SdfUnregisteredValue`, so its
/// USDC of the same USDA differs from ours by design. Tests comparing
/// against an installed tool skip the document then, saying why.
pub fn minimum_openusd(name: &str) -> Option<(OpenUsdRelease, &'static str)> {
    match name {
        // UI hints proposal, implemented in 25.11
        // (`OpenUSD-proposals/proposals/ui-hints`).
        "metadata_dictionaries" => Some((
            (25, 11),
            "`uiHints` (usdUI) and `limits` are registered from OpenUSD 25.11",
        )),
        "metadata_scalars" => Some((
            (25, 11),
            "`arraySizeConstraint` is registered from OpenUSD 25.11",
        )),
        _ => None,
    }
}

pub fn documents() -> Vec<(&'static str, Document)> {
    vec![
        ("identifiers", identifiers()),
        ("types", types()),
        ("metadata", metadata()),
        ("metadata_dictionaries", metadata_dictionaries()),
        ("list_edits", list_edits()),
        ("metadata_scalars", metadata_scalars()),
        ("primvars", primvars()),
        ("cube", cube_document()),
        (
            "materials_partition",
            two_material_cube().to_document().unwrap(),
        ),
        ("materials_textured", textured_cube().to_document().unwrap()),
        ("quad_triangle", quad_triangle()),
        ("uv_seam", uv_seam()),
        ("nested_transforms", nested_transforms()),
    ]
}

fn write(dir: &Path, name: &str, bytes: &[u8], expect: Expect, out: &mut Vec<Fixture>) {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    out.push(Fixture { path, expect });
}

/// Writes every fixture into `dir` (created if needed) and returns them.
///
/// # Panics
///
/// Panics if the exporter rejects a fixture or a file cannot be written.
pub fn write_all(dir: &Path) -> Vec<Fixture> {
    std::fs::create_dir_all(dir).unwrap();
    let mut out = Vec::new();
    for (name, doc) in documents() {
        let text = doc.to_usda().unwrap_or_else(|e| panic!("{name}: {e}"));
        write(
            dir,
            &format!("{name}.usda"),
            text.as_bytes(),
            Expect::Valid,
            &mut out,
        );
        let usdc = write_document(&doc).unwrap_or_else(|e| panic!("{name}: {e}"));
        write(dir, &format!("{name}.usdc"), &usdc, Expect::Valid, &mut out);
        // Files the document's asset paths name, beside the layers and in
        // the packages.
        let assets = if name == "materials_textured" {
            textured_cube_files()
        } else {
            Vec::new()
        };
        for (path, bytes) in &assets {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        fn members<'a>(
            root: &'a str,
            layer: &'a [u8],
            assets: &'a [(&str, Vec<u8>)],
        ) -> Vec<PackageFile<'a>> {
            let mut files = vec![PackageFile::new(root, layer)];
            files.extend(
                assets
                    .iter()
                    .map(|(path, bytes)| PackageFile::new(path, bytes)),
            );
            files
        }
        let usdz = layerstack_usdz::write_usdz(&members(
            UsdzProfile::Generic.root_layer_path(),
            text.as_bytes(),
            &assets,
        ))
        .unwrap();
        write(dir, &format!("{name}.usdz"), &usdz, Expect::Valid, &mut out);
        let arkit = layerstack_usdz::write_usdz(&members(
            UsdzProfile::Arkit.root_layer_path(),
            &usdc,
            &assets,
        ))
        .unwrap();
        write(
            dir,
            &format!("{name}_arkit.usdz"),
            &arkit,
            Expect::ValidArkit,
            &mut out,
        );
    }

    // A package with nested media members referenced from the layer.
    let png = checker_png();
    let wav = silence_wav();
    let mesh = Mesh::new(
        "Panel",
        &QUAD_TRI_POINTS,
        Faces::Polygons {
            counts: &QUAD_TRI_COUNTS,
            indices: &QUAD_TRI_INDICES,
        },
    )
    .with_attribute(
        "exedra:albedo",
        Value::Asset("./textures/checker.png".into()),
    )
    .with_attribute(
        "exedra:media",
        Value::AssetArray(vec![
            "textures/checker.png".into(),
            "audio/silence.wav".into(),
        ]),
    );
    let scene = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    );
    let media = [
        PackageFile::new("textures/checker.png", &png),
        PackageFile::new("audio/silence.wav", &wav),
    ];
    for (name, profile, expect) in [
        ("package.usdz", UsdzProfile::Generic, Expect::Valid),
        ("package_arkit.usdz", UsdzProfile::Arkit, Expect::ValidArkit),
    ] {
        let package = scene.to_usdz(profile, &media).unwrap();
        write(dir, name, &package, expect, &mut out);
    }

    material_fixtures(dir, &mut out);

    // Negative control: correct layout, but the layer refers to a texture
    // that is not in the package. Built with the raw packager, since the
    // exporter refuses to write it.
    let layer = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n    metersPerUnit = 1\n    upAxis = \"Z\"\n)\n\ndef Xform \"Root\"\n{\n    custom asset tex = @textures/missing.png@\n}\n";
    let missing =
        layerstack_usdz::write_usdz(&[PackageFile::new("scene.usda", layer.as_bytes())]).unwrap();
    write(
        dir,
        "control_missing_asset.usdz",
        &missing,
        Expect::Invalid("MissingReferenceValidator"),
        &mut out,
    );
    out
}
