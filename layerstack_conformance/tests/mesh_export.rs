// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Round-trip tests for `layerstack_mesh_export`.
//!
//! A mesh is written to USDA/USDZ, read back with the workspace's own
//! readers, composed into a [`Stage`], and compared with the input. Values
//! the composition model keeps (points, topology, primvar data, transforms,
//! `defaultPrim`, declared types) are checked on the composed stage;
//! authoring details it drops on ingest (primvar `interpolation`,
//! `uniform`/`custom`, `upAxis`, `metersPerUnit`) are checked on the parsed
//! AST, which records what the file authored.

use layerstack::doc::{FieldValue, LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, ResolvedAsset, Stage, StageOptions,
};
use layerstack_mesh_export::{
    Faces, Mesh, PackageFile, Primvar, PrimvarData, Scene, StageSettings, Transform, UpAxis,
    UsdzProfile, Value as AuthoredValue, Xform,
};
use layerstack_usda::ast;

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

// A unit cube: 8 shared points, 6 quads, flat (uniform) normals, and one
// UV island per face (unindexed faceVarying: every face corner has its own
// value, so every cube edge is a UV seam).
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
const COUNTS: [u32; 6] = [4; 6];
const INDICES: [u32; 24] = [
    0, 3, 2, 1, // -Z
    4, 5, 6, 7, // +Z
    0, 1, 5, 4, // -Y
    2, 3, 7, 6, // +Y
    1, 2, 6, 5, // +X
    3, 0, 4, 7, // -X
];
const NORMALS: [[f32; 3]; 6] = [
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
];
const UVS: [[f32; 2]; 24] = {
    let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut out = [[0.0; 2]; 24];
    let mut i = 0;
    while i < 24 {
        out[i] = square[i % 4];
        i += 1;
    }
    out
};
// A second, vertex-interpolated triangle mesh (render-buffer style).
const TRI_POINTS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0], [0.0, 0.125, 0.0]];
const TRI_NORMALS: [[f32; 3]; 3] = [[0.0, 0.0, 1.0]; 3];
const TRI_UVS: [[f32; 2]; 3] = [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]];
const TRI_WEIGHTS: [f32; 3] = [0.25, 0.5, 0.75];

fn placement() -> Transform {
    // Column-vector 3x4: rotate 90 degrees about Z, translate (1, 2, 3).
    Transform::from_affine_3x4([
        [0.0, -1.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 2.0],
        [0.0, 0.0, 1.0, 3.0],
    ])
}

fn scene() -> Scene<'static> {
    let cube = Mesh::new("Cube", &POINTS, Faces::polygons(&COUNTS, &INDICES))
        .with_normals(Primvar::uniform(&NORMALS[..]))
        .with_uvs(Primvar::face_varying(&UVS[..]))
        .with_attribute(
            "exedra:albedo",
            AuthoredValue::Asset("textures/checker.png".into()),
        );
    let tri = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2]))
        .with_normals(Primvar::vertex(&TRI_NORMALS[..]))
        .with_uvs(Primvar::vertex(&TRI_UVS[..]))
        .with_primvar("weight", Primvar::vertex(PrimvarData::float(&TRI_WEIGHTS)))
        .with_transform(Transform::from_translation([0.0, 0.0, 1.0]));
    let root = Xform::new("Root")
        .with_kind("component")
        .with_transform(placement())
        .with_attribute("exedra:source", AuthoredValue::String("roundtrip".into()))
        .with_mesh(cube)
        .with_mesh(tri);
    Scene::new(StageSettings::new(UpAxis::Z, 0.01), root)
}

fn vec3f(points: &[[f32; 3]]) -> Value {
    Value::Array(points.iter().map(|p| Value::Vec3f(*p)).collect())
}

fn vec2f(points: &[[f32; 2]]) -> Value {
    Value::Array(points.iter().map(|p| Value::Vec2f(*p)).collect())
}

fn ints(values: &[u32]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|&v| Value::Int(i32::try_from(v).unwrap()))
            .collect(),
    )
}

/// Resolves a prim's attribute default on the composed stage.
fn attr(stage: &Stage, store: &mut InMemoryStore, prim: &str, name: &str) -> Value {
    let prim_id = store.path(prim);
    let field = store.tokens.intern(name);
    stage
        .resolve_field_path(layerstack::PropertyPath::new(prim_id, field))
        .unwrap_or_else(|| panic!("{prim}.{name} resolves"))
        .value
}

/// Declared type name recorded for an attribute in the root layer.
fn declared_type(store: &mut InMemoryStore, prim: &str, name: &str) -> String {
    let prim_id = store.path(prim);
    let field = store.tokens.intern(name);
    let layer = store.layers.get(&LayerId(1)).expect("root layer");
    let entry = layer.prims[&prim_id]
        .property(field)
        .unwrap_or_else(|| panic!("{prim}.{name} authored"));
    let ty = entry
        .type_name
        .as_ref()
        .expect("attribute has a declared type");
    format!("{}{}", ty.type_name, if ty.is_array { "[]" } else { "" })
}

/// Composes the stage from an already-loaded store and checks every value
/// the scene authored.
fn assert_stage_matches_scene(store: &mut InMemoryStore) {
    let stage = Stage::compose(store, LayerId(1), StageOptions::default());

    let root_token = store.tokens.intern("Root");
    assert_eq!(
        store.layers[&LayerId(1)].default_prim,
        Some(root_token),
        "defaultPrim"
    );
    let cube_id = store.path("/Root/Cube");
    let mesh_type = store.tokens.intern("Mesh");
    assert_eq!(
        stage.resolve_type_name(cube_id, store),
        Some(mesh_type),
        "Cube is a Mesh"
    );

    // Topology and geometry.
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "points"),
        vec3f(&POINTS),
        "points"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "faceVertexCounts"),
        ints(&COUNTS),
        "faceVertexCounts"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "faceVertexIndices"),
        ints(&INDICES),
        "faceVertexIndices"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "extent"),
        vec3f(&[[-0.5; 3], [0.5; 3]]),
        "extent"
    );
    let none = store.tokens.intern("none");
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "subdivisionScheme"),
        Value::Token(none),
        "subdivisionScheme"
    );

    // Normals and UVs.
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "normals"),
        vec3f(&NORMALS),
        "normals"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "primvars:st"),
        vec2f(&UVS),
        "primvars:st"
    );
    assert_eq!(
        declared_type(store, "/Root/Cube", "normals"),
        "normal3f[]",
        "normals declared type"
    );
    assert_eq!(
        declared_type(store, "/Root/Cube", "primvars:st"),
        "texCoord2f[]",
        "primvars:st declared type"
    );
    assert_eq!(
        declared_type(store, "/Root/Cube", "points"),
        "point3f[]",
        "points declared type"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Tri", "normals"),
        vec3f(&TRI_NORMALS),
        "Tri normals"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Tri", "primvars:st"),
        vec2f(&TRI_UVS),
        "Tri primvars:st"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Tri", "primvars:weight"),
        Value::Array(TRI_WEIGHTS.iter().map(|&w| Value::Float(w)).collect()),
        "Tri primvars:weight"
    );

    // Transforms.
    let rows = placement().usd_rows();
    assert_eq!(
        attr(&stage, store, "/Root", "xformOp:transform"),
        Value::Matrix4d(Box::new(core::array::from_fn(|i| rows[i / 4][i % 4]))),
        "xformOp:transform"
    );
    let op = store.tokens.intern("xformOp:transform");
    assert_eq!(
        attr(&stage, store, "/Root", "xformOpOrder"),
        Value::Array(vec![Value::Token(op)]),
        "xformOpOrder"
    );
    assert_eq!(
        attr(&stage, store, "/Root", "exedra:source"),
        Value::String("roundtrip".into()),
        "exedra:source"
    );
    assert_eq!(
        attr(&stage, store, "/Root/Cube", "exedra:albedo"),
        Value::Asset("textures/checker.png".into()),
        "exedra:albedo"
    );
}

#[test]
fn usda_roundtrip_through_parser_and_stage() {
    let text = scene().to_usda().expect("export");
    assert_eq!(text, scene().to_usda().unwrap(), "deterministic output");

    let parsed = layerstack_usda::parser::parse(&text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    // Authored details the composition model drops: check them on the AST.
    let meta: Vec<String> = parsed
        .layer
        .metadata
        .iter()
        .filter_map(|m| match m {
            ast::LayerMeta::Custom(e) => Some(format!("{} = {:?}", e.key, e.value)),
            _ => None,
        })
        .collect();
    assert_eq!(
        meta,
        [
            "defaultPrim = Value(String(\"Root\"))",
            "metersPerUnit = Value(Number(0.01))",
            "upAxis = Value(String(\"Z\"))",
        ],
        "layer metadata"
    );
    let ast::PrimChild::Prim(cube) = parsed.layer.prims[0]
        .children
        .iter()
        .find(|c| matches!(c, ast::PrimChild::Prim(p) if p.name == "Cube"))
        .unwrap()
    else {
        unreachable!()
    };
    let attribute = |name: &str| {
        cube.children
            .iter()
            .find_map(|c| match c {
                ast::PrimChild::Attribute(a) if a.name == name => Some(a),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} authored"))
    };
    let interpolation = |name: &str| {
        let a = attribute(name);
        a.metadata
            .iter()
            .find(|m| m.key == "interpolation")
            .map(|m| format!("{:?}", m.value))
    };
    assert_eq!(
        interpolation("normals").as_deref(),
        Some("Value(String(\"uniform\"))"),
        "normals interpolation"
    );
    assert_eq!(
        interpolation("primvars:st").as_deref(),
        Some("Value(String(\"faceVarying\"))"),
        "st interpolation"
    );
    assert!(attribute("subdivisionScheme").uniform, "uniform scheme");
    assert!(attribute("orientation").uniform, "uniform orientation");
    assert!(attribute("exedra:albedo").custom, "custom attribute");
    assert!(!attribute("points").custom, "schema attribute");

    let mut store = InMemoryStore::default();
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    store.insert_layer(result.layer);
    assert_stage_matches_scene(&mut store);
}

#[test]
fn usdc_roundtrip_through_reader_and_stage() {
    let bytes = scene().to_usdc().expect("export");
    assert_eq!(bytes, scene().to_usdc().unwrap(), "deterministic output");
    assert_eq!(&bytes[..8], b"PXR-USDC", "crate file");
    let mut store = InMemoryStore::default();
    let result = layerstack_usdc::read_usdc(
        &bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .expect("the workspace reader reads the writer's output");
    store.insert_layer(result.layer);
    assert_stage_matches_scene(&mut store);
}

#[test]
fn usdz_roundtrip_through_package_reader() {
    let png = b"\x89PNG\r\n\x1a\n not a real image";
    for profile in [UsdzProfile::Generic, UsdzProfile::Arkit] {
        let bytes = scene()
            .to_usdz(profile, &[PackageFile::new("textures/checker.png", png)])
            .expect("package");

        let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).expect("valid USDZ");
        let names: Vec<&str> = archive.entries().iter().map(|e| &*e.name).collect();
        assert_eq!(
            names,
            [profile.root_layer_path(), "textures/checker.png"],
            "entries ({profile:?})"
        );
        for entry in archive.entries() {
            assert_eq!(entry.data_offset % 64, 0, "{} aligned", entry.name);
        }
        let texture = archive.find("textures/checker.png").expect("texture");
        assert_eq!(archive.entry_data(texture), png, "texture bytes");
        let layer = match profile {
            UsdzProfile::Generic => scene().to_usda().unwrap().into_bytes(),
            UsdzProfile::Arkit => scene().to_usdc().unwrap(),
        };
        assert_eq!(
            archive.entry_data(&archive.entries()[0]),
            layer,
            "root layer is the {profile:?} export"
        );

        let mut store = InMemoryStore::default();
        let result = layerstack_usdz::read_usdz(
            &bytes,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        )
        .expect("read_usdz verifies CRCs and layout");
        store.insert_layer(result.layer);
        assert_stage_matches_scene(&mut store);
    }
}

// ── Acceptance cases for the bounded mesh profile ──────────────────────

/// Parses and emits exported USDA into a fresh store (root layer 1).
fn load_usda(text: &str) -> InMemoryStore {
    let parsed = layerstack_usda::parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let mut store = InMemoryStore::default();
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    store.insert_layer(result.layer);
    store
}

fn as_u32s(value: &Value) -> Vec<u32> {
    let Value::Array(items) = value else {
        panic!("expected an int array, got {value:?}");
    };
    items
        .iter()
        .map(|v| match v {
            Value::Int(i) => u32::try_from(*i).expect("non-negative"),
            other => panic!("expected int, got {other:?}"),
        })
        .collect()
}

#[test]
fn quad_and_triangle_topology_round_trips() {
    // A quad and a triangle sharing the edge 1-2.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [2.0, 0.5, 0.0],
    ];
    let counts = [4, 3];
    let indices = [0, 1, 2, 3, 1, 4, 2];
    let mesh = Mesh::new("Panel", &points, Faces::polygons(&counts, &indices));
    let text = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    )
    .to_usda()
    .unwrap();

    let mut store = load_usda(&text);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let read_counts = as_u32s(&attr(&stage, &mut store, "/Root/Panel", "faceVertexCounts"));
    let read_indices = as_u32s(&attr(
        &stage,
        &mut store,
        "/Root/Panel",
        "faceVertexIndices",
    ));
    let Value::Array(read_points) = attr(&stage, &mut store, "/Root/Panel", "points") else {
        panic!("points array");
    };
    assert_eq!(read_counts, counts, "faceVertexCounts");
    assert_eq!(read_indices, indices, "faceVertexIndices");
    assert_eq!(
        read_counts.iter().sum::<u32>() as usize,
        read_indices.len(),
        "counts sum to the index count"
    );
    assert!(
        read_indices
            .iter()
            .all(|&i| (i as usize) < read_points.len()),
        "indices in bounds"
    );
    let none = store.tokens.intern("none");
    assert_eq!(
        attr(&stage, &mut store, "/Root/Panel", "subdivisionScheme"),
        Value::Token(none),
        "polygonal mesh opts out of subdivision"
    );
}

#[test]
fn uv_seam_and_hard_edge_keep_face_varying_topology() {
    // A floor quad and a wall quad folded 90 degrees along the shared edge
    // 1-2. Positions are shared (6 points, not 8).
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
    ];
    let indices = [0, 1, 2, 3, 1, 4, 5, 2];
    // Hard edge: each face keeps its own normal at the shared points.
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
    // UV seam: the wall's corners at points 1 and 2 use indices 4 and 5,
    // whose *values* equal indices 1 and 2. The distinct indices are the
    // seam and must survive; merging them by value would weld the islands.
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
    let uv_indices = [0, 1, 2, 3, 4, 6, 7, 5];
    let mesh = Mesh::new("Fold", &points, Faces::polygons(&[4, 4], &indices))
        .with_normals(Primvar::face_varying(&normals[..]))
        .with_uvs(Primvar::face_varying(&uvs[..]).with_indices(&uv_indices));
    let text = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    )
    .to_usda()
    .unwrap();

    let mut store = load_usda(&text);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        attr(&stage, &mut store, "/Root/Fold", "points"),
        vec3f(&points),
        "positions stay shared"
    );
    assert_eq!(
        as_u32s(&attr(&stage, &mut store, "/Root/Fold", "faceVertexIndices")),
        indices,
        "topology"
    );
    assert_eq!(
        attr(&stage, &mut store, "/Root/Fold", "normals"),
        vec3f(&normals),
        "per-corner normals keep the hard edge"
    );
    assert_eq!(
        attr(&stage, &mut store, "/Root/Fold", "primvars:st"),
        vec2f(&uvs),
        "UV values, including the equal-valued seam copies"
    );
    assert_eq!(
        as_u32s(&attr(
            &stage,
            &mut store,
            "/Root/Fold",
            "primvars:st:indices"
        )),
        uv_indices,
        "face-varying indices are not deduplicated"
    );
    assert!(
        text.contains("interpolation = \"faceVarying\""),
        "face-varying interpolation authored"
    );
}

#[test]
fn asymmetric_one_metre_object_keeps_units_axis_and_transform_order() {
    // A wedge 1 m long in X, 0.5 m deep in Y, 0.25 m tall in Z (Z up). Its
    // asymmetry makes axis swaps, unit scaling or transposed matrices
    // observable in the extent and transforms.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.5, 0.0],
        [0.0, 0.5, 0.0],
        [0.0, 0.0, 0.25],
        [0.0, 0.5, 0.25],
    ];
    let wedge = Mesh::new(
        "Wedge",
        &points,
        Faces::polygons(
            &[4, 4, 3, 3, 4],
            &[0, 3, 2, 1, 0, 1, 4, 3, 4, 5, 3, 0, 4, 1, 1, 2, 5, 4],
        ),
    );
    // Nested, non-commuting transforms: the parent rotates 90 degrees about
    // Z, the child translates 1 m along X and mirrors in X.
    let rotate = Transform::from_affine_3x4([
        [0.0, -1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);
    let mirror_and_move = Transform::from_affine_3x4([
        [-1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);
    let root = Xform::new("Root").with_transform(rotate).with_xform(
        Xform::new("Mirrored")
            .with_transform(mirror_and_move)
            .with_mesh(wedge),
    );
    let text = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root)
        .to_usda()
        .unwrap();

    let parsed = layerstack_usda::parser::parse(&text);
    let meta: Vec<String> = parsed
        .layer
        .metadata
        .iter()
        .filter_map(|m| match m {
            ast::LayerMeta::Custom(e) => Some(format!("{} = {:?}", e.key, e.value)),
            _ => None,
        })
        .collect();
    assert!(
        meta.contains(&"metersPerUnit = Value(Int(1))".to_string())
            && meta.contains(&"upAxis = Value(String(\"Z\"))".to_string()),
        "units and axis authored explicitly: {meta:?}"
    );

    let mut store = load_usda(&text);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        attr(&stage, &mut store, "/Root/Mirrored/Wedge", "extent"),
        vec3f(&[[0.0, 0.0, 0.0], [1.0, 0.5, 0.25]]),
        "extent in metres, Z up, local space"
    );
    let matrix = |t: Transform| {
        let rows = t.usd_rows();
        Value::Matrix4d(Box::new(core::array::from_fn(|i| rows[i / 4][i % 4])))
    };
    assert_eq!(
        attr(&stage, &mut store, "/Root", "xformOp:transform"),
        matrix(rotate),
        "parent rotation"
    );
    assert_eq!(
        attr(&stage, &mut store, "/Root/Mirrored", "xformOp:transform"),
        matrix(mirror_and_move),
        "child mirror + translation"
    );
    // Row-vector layout: translation in the last row, mirror in the first.
    assert_eq!(
        mirror_and_move.usd_rows(),
        [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0, 1.0],
        ],
        "USD matrix layout"
    );
}

/// Collects every asset path authored in a layer.
fn authored_assets(layer: &layerstack::Layer) -> Vec<String> {
    fn visit(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Asset(path) => out.push(path.to_string()),
            Value::Array(items) => items.iter().for_each(|v| visit(v, out)),
            Value::Dictionary(entries) => entries.iter().for_each(|(_, v)| visit(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for spec in layer.prims.values() {
        for field in &spec.fields {
            if let FieldValue::Value(value) = &field.value {
                visit(value, &mut out);
            }
        }
        for property in &spec.properties {
            let spec = &property.spec;
            let samples = spec.time_samples.iter().flatten().map(|(_, value)| value);
            for value in spec.default.iter().chain(samples) {
                visit(value, &mut out);
            }
        }
    }
    out.sort();
    out
}

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        std::path::Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    base.join(format!("mesh-export-{name}"))
}

#[test]
fn moved_usdz_resolves_every_internal_asset() {
    let base = scratch_dir("moved-usdz");
    // Start clean if an earlier run was interrupted.
    let _ = std::fs::remove_dir_all(&base);
    let sources = base.join("sources");
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();

    // Texture sources live in one directory...
    let albedo = b"\x89PNG\r\n\x1a\n albedo";
    let roughness = b"\x89PNG\r\n\x1a\n roughness";
    std::fs::write(sources.join("albedo.png"), albedo).unwrap();
    std::fs::write(sources.join("roughness.png"), roughness).unwrap();
    let albedo = std::fs::read(sources.join("albedo.png")).unwrap();
    let roughness = std::fs::read(sources.join("roughness.png")).unwrap();

    let mesh = Mesh::new("Tri", &TRI_POINTS, Faces::triangles(&[0, 1, 2]))
        .with_attribute(
            "exedra:albedo",
            AuthoredValue::Asset("./textures/albedo.png".into()),
        )
        .with_attribute(
            "exedra:maps",
            AuthoredValue::AssetArray(vec![
                "textures/albedo.png".into(),
                "textures/detail/roughness.png".into(),
            ]),
        );
    let bytes = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root").with_mesh(mesh),
    )
    .to_usdz(
        UsdzProfile::Generic,
        &[
            PackageFile::new("textures/albedo.png", &albedo),
            PackageFile::new("textures/detail/roughness.png", &roughness),
        ],
    )
    .unwrap();

    // ...the package is written somewhere else and the sources disappear.
    let package = elsewhere.join("model.usdz");
    std::fs::write(&package, &bytes).unwrap();
    std::fs::remove_dir_all(&sources).unwrap();
    let bytes = std::fs::read(&package).unwrap();
    std::fs::remove_dir_all(&base).unwrap();

    let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
    let mut store = InMemoryStore::default();
    let result = layerstack_usdz::read_usdz(
        &bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    let assets = authored_assets(&result.layer);
    assert_eq!(
        assets,
        [
            "./textures/albedo.png",
            "textures/albedo.png",
            "textures/detail/roughness.png"
        ],
        "authored asset paths"
    );
    for asset in &assets {
        // Asset paths anchor to the root layer, which is at the package
        // root, so they must name an entry of this archive.
        let entry = archive
            .find(asset.trim_start_matches("./"))
            .unwrap_or_else(|| panic!("{asset} resolves inside the package"));
        assert_eq!(entry.data_offset % 64, 0, "{asset} aligned");
    }
    let detail = archive.find("textures/detail/roughness.png").unwrap();
    assert_eq!(
        archive.entry_data(detail),
        roughness,
        "nested texture bytes"
    );
}

// ── Materials ──────────────────────────────────────────────────────────

/// Reads a USDZ package's root layer into a fresh store (root layer 1).
fn load_usdz(bytes: &[u8]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let result = layerstack_usdz::read_usdz(
        bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .expect("valid package");
    store.insert_layer(result.layer);
    store
}

fn load_usdc(bytes: &[u8]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let result = layerstack_usdc::read_usdc(
        bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .expect("valid crate file");
    store.insert_layer(result.layer);
    store
}

/// Targets of a relationship or connection, as path strings.
fn targets(stage: &Stage, store: &mut InMemoryStore, prim: &str, name: &str) -> Vec<String> {
    let prim_id = store.path(prim);
    let field = store.tokens.intern(name);
    stage
        .resolve_target_list_path(layerstack::PropertyPath::new(prim_id, field))
        .unwrap_or_else(|| panic!("{prim}.{name} has targets"))
        .value
        .into_iter()
        .map(|t| t.display(&store.paths, &store.tokens))
        .collect()
}

fn type_name(stage: &Stage, store: &mut InMemoryStore, prim: &str) -> String {
    let prim_id = store.path(prim);
    let ty = stage
        .resolve_type_name(prim_id, store)
        .unwrap_or_else(|| panic!("{prim} is typed"));
    store.tokens.resolve(ty).to_string()
}

fn token(stage: &Stage, store: &mut InMemoryStore, prim: &str, name: &str) -> String {
    match attr(stage, store, prim, name) {
        Value::Token(t) => store.tokens.resolve(t).to_string(),
        other => panic!("{prim}.{name}: expected a token, got {other:?}"),
    }
}

fn api_schemas(stage: &Stage, store: &mut InMemoryStore, prim: &str) -> Vec<String> {
    let prim_id = store.path(prim);
    let field = store.tokens.intern("apiSchemas");
    stage
        .resolve_token_list(prim_id, field)
        .unwrap_or_else(|| panic!("{prim} applies API schemas"))
        .value
        .into_iter()
        .map(|t| store.tokens.resolve(t).to_string())
        .collect()
}

fn assert_two_material_cube(store: &mut InMemoryStore) {
    use layerstack_conformance::export_fixtures::{BLUE_FACES, RED_FACES};
    let stage = Stage::compose(store, LayerId(1), StageOptions::default());
    assert_eq!(
        token(
            &stage,
            store,
            "/Root/Cube",
            "subsetFamily:materialBind:familyType"
        ),
        "partition",
        "family type on the mesh"
    );
    for (subset, material, faces) in [
        ("RedFaces", "Red", RED_FACES),
        ("BlueFaces", "Blue", BLUE_FACES),
    ] {
        let path = format!("/Root/Cube/{subset}");
        assert_eq!(type_name(&stage, store, &path), "GeomSubset", "{path} type");
        assert_eq!(
            api_schemas(&stage, store, &path),
            ["MaterialBindingAPI"],
            "{path} applies MaterialBindingAPI"
        );
        assert_eq!(
            targets(&stage, store, &path, "material:binding"),
            [format!("/Root/Materials/{material}")],
            "{path} binding"
        );
        assert_eq!(
            as_u32s(&attr(&stage, store, &path, "indices")),
            faces,
            "{path} indices"
        );
        assert_eq!(
            token(&stage, store, &path, "elementType"),
            "face",
            "{path} elementType"
        );
        assert_eq!(
            token(&stage, store, &path, "familyName"),
            "materialBind",
            "{path} familyName"
        );
        let material_path = format!("/Root/Materials/{material}");
        assert_eq!(
            type_name(&stage, store, &material_path),
            "Material",
            "{material_path} type"
        );
        assert_eq!(
            targets(&stage, store, &material_path, "outputs:surface"),
            [format!("{material_path}/PreviewSurface.outputs:surface")],
            "{material_path} surface output"
        );
        assert_eq!(
            token(
                &stage,
                store,
                &format!("{material_path}/PreviewSurface"),
                "info:id"
            ),
            "UsdPreviewSurface",
            "{material_path} shader id"
        );
    }
    assert_eq!(
        attr(
            &stage,
            store,
            "/Root/Materials/Red/PreviewSurface",
            "inputs:diffuseColor"
        ),
        Value::Vec3f([0.8, 0.05, 0.05]),
        "constant base color"
    );
}

fn assert_textured_cube(store: &mut InMemoryStore) {
    let stage = Stage::compose(store, LayerId(1), StageOptions::default());
    let m = "/Root/Materials/Textured";
    assert_eq!(
        api_schemas(&stage, store, "/Root/Cube"),
        ["MaterialBindingAPI"],
        "mesh applies MaterialBindingAPI"
    );
    assert_eq!(
        targets(&stage, store, "/Root/Cube", "material:binding"),
        [m],
        "direct binding"
    );
    let surface = format!("{m}/PreviewSurface");
    for (input, target) in [
        ("inputs:diffuseColor", "DiffuseColorTexture.outputs:rgb"),
        ("inputs:occlusion", "MetallicTexture.outputs:r"),
        ("inputs:roughness", "MetallicTexture.outputs:g"),
        ("inputs:metallic", "MetallicTexture.outputs:b"),
        ("inputs:normal", "NormalTexture.outputs:rgb"),
    ] {
        assert_eq!(
            targets(&stage, store, &surface, input),
            [format!("{m}/{target}")],
            "{input} connection"
        );
    }
    for (node, file, color_space) in [
        ("DiffuseColorTexture", "textures/albedo.png", "sRGB"),
        ("MetallicTexture", "textures/orm.png", "raw"),
        ("NormalTexture", "textures/ridges_normal.png", "raw"),
    ] {
        let path = format!("{m}/{node}");
        assert_eq!(
            token(&stage, store, &path, "info:id"),
            "UsdUVTexture",
            "{path} id"
        );
        assert_eq!(
            attr(&stage, store, &path, "inputs:file"),
            Value::Asset(file.into()),
            "{path} file"
        );
        assert_eq!(
            token(&stage, store, &path, "inputs:sourceColorSpace"),
            color_space,
            "{path} color space"
        );
        assert_eq!(
            targets(&stage, store, &path, "inputs:st"),
            [format!("{m}/TexCoordReader.outputs:result")],
            "{path} texture coordinates"
        );
    }
    let normal = format!("{m}/NormalTexture");
    assert_eq!(
        attr(&stage, store, &normal, "inputs:scale"),
        Value::Vec4f([2.0, 2.0, 2.0, 1.0]),
        "normal map scale"
    );
    assert_eq!(
        attr(&stage, store, &normal, "inputs:bias"),
        Value::Vec4f([-1.0, -1.0, -1.0, 0.0]),
        "normal map bias"
    );
    assert_eq!(
        attr(
            &stage,
            store,
            &format!("{m}/TexCoordReader"),
            "inputs:varname"
        ),
        Value::String("st".into()),
        "primvar reader reads st"
    );
}

#[test]
fn materials_round_trip_through_readers() {
    use layerstack_conformance::export_fixtures::{
        textured_cube, textured_cube_files, two_material_cube,
    };
    let partition = two_material_cube();
    assert_two_material_cube(&mut load_usda(&partition.to_usda().unwrap()));
    assert_two_material_cube(&mut load_usdc(&partition.to_usdc().unwrap()));
    for profile in [UsdzProfile::Generic, UsdzProfile::Arkit] {
        assert_two_material_cube(&mut load_usdz(&partition.to_usdz(profile, &[]).unwrap()));
    }

    let textured = textured_cube();
    assert_textured_cube(&mut load_usda(&textured.to_usda().unwrap()));
    let files = textured_cube_files();
    let files: Vec<PackageFile<'_>> = files
        .iter()
        .map(|(path, bytes)| PackageFile::new(path, bytes))
        .collect();
    assert_textured_cube(&mut load_usdc(&textured.to_usdc().unwrap()));
    let arkit = textured.to_usdz(UsdzProfile::Arkit, &files).unwrap();
    assert_textured_cube(&mut load_usdz(&arkit));
    let bytes = textured.to_usdz(UsdzProfile::Generic, &files).unwrap();
    assert_textured_cube(&mut load_usdz(&bytes));
    let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
    let names: Vec<&str> = archive.entries().iter().map(|e| &*e.name).collect();
    assert_eq!(
        names,
        [
            UsdzProfile::Generic.root_layer_path(),
            "textures/albedo.png",
            "textures/orm.png",
            "textures/ridges_normal.png"
        ],
        "textures are packaged after the layer"
    );
}
