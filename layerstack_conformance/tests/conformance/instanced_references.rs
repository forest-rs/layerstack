// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point instancers written as instanced references, checked by OpenUSD
//! and, on request, by Apple's `SceneKit`.
//!
//! Apple's USD stack does not draw `UsdGeomPointInstancer` instances, so
//! `UsdzProfile::Arkit` packages write each instance as an instanceable
//! internal reference ([`Instancing::References`]). The scene here (a
//! scattered field of three inline prototypes, a grove of a shared
//! prototype placed from affine transforms, including a mirror, and one
//! direct instance of it) is written in both forms. OpenUSD's Python
//! bindings (the Python named by `LAYERSTACK_USD_PYTHON`, or `python3`,
//! when it imports `pxr`) read both through `scripts/instancing_oracle.py`:
//! the reference form must have no `PointInstancer`, its instances' world
//! transforms must be the source transforms, their names, ids and tints
//! must be as given, and both forms must draw the same meshes, with the
//! same materials, at the same places. `usdchecker`, when on `PATH`, must
//! accept the `ARKit` package with `--arkit` where it has the option.
//!
//! With `LAYERSTACK_SCENEKIT=1` on macOS with `swift` installed,
//! `scripts/scenekit_check.swift` also loads the `ARKit` package through
//! `SceneKit`, renders it, and the number of geometries it draws must equal
//! the number of placed meshes. Everywhere else, and without the variable,
//! that test reports that it skipped and passes.

use std::path::{Path, PathBuf};
use std::process::Command;

use layerstack_conformance::instancer_fixtures::{BOULDER, Field, PINE, SHRUB, prototypes};
use layerstack_mesh_export::{
    Faces, Instance, Instancing, Mesh, Node, PointInstancer, Primvar, PrimvarData, Scene,
    StageSettings, Transform, UpAxis, UsdzProfile, Xform,
};
use serde::Deserialize;

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    let dir = base.join(format!("instanced-references-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Meshes in each prototype, by prototype index: the pine's trunk and
/// crown, the boulder, the shrub's leaves.
fn meshes_per_prototype(proto: u32) -> usize {
    match proto {
        PINE => 2,
        BOULDER | SHRUB => 1,
        _ => unreachable!(),
    }
}

/// The scene in every form, with what it should draw.
struct Fixture {
    field: Field,
    names: Vec<String>,
    tints: Vec<[f32; 3]>,
    /// Source transforms of the grove's instances.
    grove: Vec<Transform>,
    /// Source transform of the lone instance.
    lone: Transform,
}

impl Fixture {
    fn new() -> Self {
        let field = Field::new(6, 5);
        let names = (0..field.proto_indices.len())
            .map(|i| format!("plant_{i}"))
            .collect();
        let tints = (0..field.proto_indices.len())
            .map(|i| {
                let t = i as f32 / field.proto_indices.len() as f32;
                [t, 1.0 - t, 0.5]
            })
            .collect();
        // Column-vector rows `[R · S | t]`: a plain placement, a turn with a
        // stretch, and a mirror in Y.
        let grove = vec![
            Transform::from_affine_3x4([
                [1.0, 0.0, 0.0, 30.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ]),
            Transform::from_affine_3x4([
                [0.0, -1.5, 0.0, 34.0],
                [1.5, 0.0, 0.0, 2.0],
                [0.0, 0.0, 2.0, 0.25],
            ]),
            Transform::from_affine_3x4([
                [0.8, 0.0, 0.0, 38.0],
                [0.0, -0.8, 0.0, -2.0],
                [0.0, 0.0, 0.8, 0.0],
            ]),
        ];
        let lone = Transform::from_affine_3x4([
            [0.0, 1.0, 0.0, -30.0],
            [-1.0, 0.0, 0.0, 5.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        Self {
            field,
            names,
            tints,
            grove,
            lone,
        }
    }

    fn scene(&self, instancing: Instancing) -> Scene<'_> {
        let mut scene = self.field.scene();
        let Node::PointInstancer(field) = &mut scene.root.children[0] else {
            unreachable!("the field is one instancer");
        };
        field.names = Some(self.names.iter().map(|n| n.as_str().into()).collect());
        field.primvars.push(layerstack_mesh_export::CustomPrimvar {
            name: "displayColor".into(),
            primvar: Primvar::per_instance(PrimvarData::color3(&self.tints)),
        });
        let mut grove = PointInstancer::new("Grove", vec![], vec![])
            .with_prototype(Instance::new("Pine", "Pine"));
        for transform in &self.grove {
            grove.push_affine(0, transform).unwrap();
        }
        let pine = prototypes().swap_remove(PINE as usize);
        scene.root.children.push(grove.into());
        scene.root.children.push(
            Instance::new("Lone", "Pine")
                .with_transform(self.lone)
                .into(),
        );
        scene.with_prototype(pine).with_instancing(instancing)
    }

    /// Meshes the scene draws.
    fn placed(&self) -> usize {
        let field: usize = self
            .field
            .proto_indices
            .iter()
            .map(|&p| meshes_per_prototype(p))
            .sum();
        field + (self.grove.len() + 1) * meshes_per_prototype(PINE)
    }
}

fn usd_python() -> Option<String> {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .output()
        .ok()?;
    out.status.success().then_some(python)
}

#[derive(Deserialize)]
struct Report {
    version: String,
    layers: Vec<LayerReport>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayerReport {
    point_instancers: usize,
    instances: Vec<InstanceReport>,
    placed: Vec<Placed>,
    extents: Vec<ExtentReport>,
}

#[derive(Deserialize)]
struct ExtentReport {
    path: String,
    extent: Vec<[f64; 3]>,
    computed: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct InstanceReport {
    path: String,
    name: String,
    instanceable: bool,
    target: String,
    world: [f64; 16],
    id: Option<i64>,
    primvars: std::collections::BTreeMap<String, Vec<Vec<f64>>>,
    indices: std::collections::BTreeMap<String, Vec<i64>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Placed {
    material: String,
    triangles: usize,
    points: Vec<[f64; 3]>,
}

/// Writes the `PointInstancer` form as a generic package and the
/// reference form as an `ARKit` package.
fn write(fixture: &Fixture, dir: &Path) -> [PathBuf; 2] {
    let generic = dir.join("scene.usdz");
    let arkit = dir.join("scene_arkit.usdz");
    let scene = fixture.scene(Instancing::PointInstancers);
    std::fs::write(&generic, scene.to_usdz(UsdzProfile::Generic, &[]).unwrap()).unwrap();
    std::fs::write(&arkit, scene.to_usdz(UsdzProfile::Arkit, &[]).unwrap()).unwrap();
    [generic, arkit]
}

fn assert_matrix(got: &[f64; 16], want: &[[f64; 4]; 4], tolerance: f64, what: &str) {
    for (k, value) in got.iter().enumerate() {
        let want = want[k / 4][k % 4];
        assert!(
            (value - want).abs() <= tolerance * want.abs().max(1.0),
            "{what}: element {k}: OpenUSD {value}, expected {want}"
        );
    }
}

/// Placed meshes in a canonical order: by material, size and rounded
/// first point.
#[allow(
    clippy::cast_possible_truncation,
    reason = "coordinates are small; the key only needs to be stable"
)]
fn sorted(mut placed: Vec<Placed>) -> Vec<Placed> {
    let key = |p: &Placed| {
        let first = p.points.first().copied().unwrap_or_default();
        (
            p.material.clone(),
            p.triangles,
            first.map(|c| (c * 1e3).round() as i64),
        )
    };
    placed.sort_by_key(key);
    placed
}

#[test]
fn references_draw_what_the_instancers_draw() {
    let Some(python) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    let fixture = Fixture::new();
    let dir = scratch_dir("oracle");
    let [generic, arkit] = write(&fixture, &dir);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/instancing_oracle.py");
    let out = Command::new(&python)
        .arg(&script)
        .args([&generic, &arkit])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Report = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("OpenUSD {} via {python}", report.version);
    let [generic, arkit] = [&report.layers[0], &report.layers[1]];

    assert_eq!(generic.point_instancers, 2, "the generic form keeps both");
    assert_eq!(arkit.point_instancers, 0, "no PointInstancer for ARKit");
    assert_eq!(generic.placed.len(), fixture.placed(), "generic placements");
    assert_eq!(arkit.placed.len(), fixture.placed(), "ARKit placements");

    // Field instances: named, tinted, with ids, at the source transforms
    // (prototype root transforms included, as the schema composes them).
    let field = &fixture.field;
    let transforms = field.instance_transforms();
    let prototypes = ["Pine", "Boulder", "Shrub"];
    let in_field: Vec<&InstanceReport> = arkit
        .instances
        .iter()
        .filter(|i| i.path.starts_with("/Root/Field/"))
        .collect();
    assert_eq!(in_field.len(), field.proto_indices.len(), "field instances");
    for (i, got) in in_field.iter().enumerate() {
        assert_eq!(got.name, fixture.names[i], "instance {i} name");
        assert!(
            !got.instanceable,
            "instance {i} is a plain reference for ARKit"
        );
        assert_eq!(
            got.target,
            format!(
                "/Root/Field/Prototypes/{}",
                prototypes[field.proto_indices[i] as usize]
            ),
            "instance {i} prototype"
        );
        assert_eq!(got.id, Some(field.ids[i]), "instance {i} id");
        let tint: Vec<f64> = fixture.tints[i].iter().map(|&c| f64::from(c)).collect();
        assert_eq!(
            got.primvars.get("displayColor"),
            Some(&vec![tint]),
            "instance {i} tint"
        );
        assert_matrix(&got.world, &transforms[i], 1e-12, &got.path);
    }

    // The grove and the lone instance place the shared prototype, straight
    // from where it is defined, at their source transforms.
    let shared: Vec<&InstanceReport> = arkit
        .instances
        .iter()
        .filter(|i| !i.path.starts_with("/Root/Field/"))
        .collect();
    let sources: Vec<Transform> = fixture
        .grove
        .iter()
        .copied()
        .chain([fixture.lone])
        .collect();
    assert_eq!(shared.len(), sources.len(), "shared instances");
    for (got, source) in shared.iter().zip(&sources) {
        assert_eq!(got.target, "/Root/Prototypes/Pine", "{}", got.path);
        assert_matrix(&got.world, &source.usd_rows(), 1e-6, &got.path);
    }

    // Both forms draw the same meshes, bound to the same materials, at the
    // same places.
    let (want, got) = (sorted(generic.placed.clone()), sorted(arkit.placed.clone()));
    for (w, g) in want.iter().zip(&got) {
        assert_eq!(w.material, g.material, "materials");
        assert_eq!(w.triangles, g.triangles, "triangles");
        for (a, b) in w.points.iter().zip(&g.points) {
            for axis in 0..3 {
                assert!(
                    (a[axis] - b[axis]).abs() < 1e-5,
                    "point {a:?} vs {b:?} ({})",
                    w.material
                );
            }
        }
    }
    let bound = got.iter().filter(|p| !p.material.is_empty()).count();
    assert!(bound > 0 && bound < got.len(), "boulders are unbound");
}

#[test]
fn usdchecker_accepts_the_arkit_package() {
    let Some(version) = Command::new("usdchecker")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    else {
        eprintln!("skipped: usdchecker is not on PATH");
        return;
    };
    eprintln!("usdchecker: {version}");
    let dir = scratch_dir("checker");
    let [generic, arkit] = write(&Fixture::new(), &dir);
    let help = Command::new("usdchecker").arg("--help").output().unwrap();
    // OpenUSD 26.05 removed `--arkit`; from then on the USDZ validators
    // always run.
    let arkit_flag = String::from_utf8_lossy(&help.stdout).contains("--arkit");
    let mut runs: Vec<Vec<&Path>> = vec![vec![&generic], vec![&arkit]];
    if arkit_flag {
        runs.push(vec![Path::new("--arkit"), &arkit]);
    } else {
        eprintln!("skipped --arkit: usdchecker {version:?} has no such option");
    }
    let mut failures = Vec::new();
    for args in runs {
        let out = Command::new("usdchecker").args(&args).output().unwrap();
        if !out.status.success() {
            failures.push(format!(
                "usdchecker {args:?}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn scenekit_draws_every_instance() {
    if std::env::var_os("LAYERSTACK_SCENEKIT").is_none_or(|v| v != "1") {
        eprintln!("skipped: set LAYERSTACK_SCENEKIT=1 to render through SceneKit");
        return;
    }
    if !cfg!(target_os = "macos") {
        eprintln!("skipped: SceneKit is only on macOS");
        return;
    }
    if !Command::new("swift")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped: swift is not on PATH");
        return;
    }
    let fixture = Fixture::new();
    let dir = scratch_dir("scenekit");
    let [_, arkit] = write(&fixture, &dir);
    let png = dir.join("scene_arkit.png");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/scenekit_check.swift");
    let out = Command::new("swift")
        .arg(&script)
        .arg(&arkit)
        .arg(&png)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "SceneKit check failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let geometries: usize = stdout
        .lines()
        .find_map(|l| l.strip_prefix("geometries "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no geometry count in {stdout:?}"));
    eprintln!("SceneKit: {stdout}rendered {}", png.display());
    assert_eq!(geometries, fixture.placed(), "SceneKit draws every mesh");
}

/// Runs the oracle on `layers` and returns its report.
fn oracle(python: &str, layers: &[PathBuf]) -> Report {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/instancing_oracle.py");
    let out = Command::new(python)
        .arg(&script)
        .args(layers)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Writes `scene` in both instancing forms, each as USDA and USDC.
fn write_forms(scene: &Scene<'_>, dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (form, instancing) in [
        ("instancers", Instancing::PointInstancers),
        ("references", Instancing::References),
    ] {
        let scene = scene.clone().with_instancing(instancing);
        let usda = dir.join(format!("{form}.usda"));
        let usdc = dir.join(format!("{form}.usdc"));
        std::fs::write(&usda, scene.to_usda().unwrap()).unwrap();
        std::fs::write(&usdc, scene.to_usdc().unwrap()).unwrap();
        files.extend([usda, usdc]);
    }
    files
}

/// `a · b` for row-vector matrices.
fn mul(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}

/// The product of `transforms`, applied first to last.
fn chain(transforms: &[Transform]) -> [[f64; 4]; 4] {
    transforms
        .iter()
        .fold(Transform::IDENTITY.usd_rows(), |m, t| {
            mul(&m, &t.usd_rows())
        })
}

fn apply(m: &[[f64; 4]; 4], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|j| p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j])
}

const TRIANGLE: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

/// Column-vector rows `[R | t]` of a quarter turn about Z.
fn turn_z(t: [f64; 3]) -> Transform {
    Transform::from_affine_3x4([
        [0.0, -1.0, 0.0, t[0]],
        [1.0, 0.0, 0.0, t[1]],
        [0.0, 0.0, 1.0, t[2]],
    ])
}

/// A chain of shared prototypes whose roots are instances of each other,
/// every transform a rotation with a translation, so their order matters:
///
/// - `Tri`: a mesh, turned about Z and moved (`t0`);
/// - `Alias`: an instance of `Tri`, turned about X and moved (`t1`);
/// - `Bare`: an instance of `Alias` without a transform;
/// - `Top`: an instance of `Bare`, scaled, turned about Y and moved (`t2`).
///
/// The root places `Top` (`Direct`, at `t3`) and `Bare` (`Plain`, no
/// transform) directly, and a point instancer places `Top` (as `P`, no
/// transform) and `Alias` (as `Q`, at `t4`).
struct Chain {
    t0: Transform,
    t1: Transform,
    t2: Transform,
    t3: Transform,
    t4: Transform,
    placements: [Transform; 2],
}

impl Chain {
    fn new() -> Self {
        Self {
            t0: turn_z([10.0, 0.0, 0.0]),
            t1: Transform::from_affine_3x4([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, -1.0, 20.0],
                [0.0, 1.0, 0.0, 0.0],
            ]),
            t2: Transform::from_affine_3x4([
                [0.0, 0.0, 2.0, 0.0],
                [0.0, 2.0, 0.0, 0.0],
                [-2.0, 0.0, 0.0, 30.0],
            ]),
            t3: turn_z([100.0, 0.0, 0.0]),
            t4: turn_z([0.0, 5.0, 0.0]),
            placements: [
                Transform::from_affine_3x4([
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 0.0, -1.0, 0.0],
                    [0.0, 1.0, 0.0, 200.0],
                ]),
                Transform::from_translation([300.0, 0.0, 0.0]),
            ],
        }
    }

    fn scene(&self) -> Scene<'static> {
        let mut field = PointInstancer::new("Field", vec![], vec![])
            .with_prototype(Instance::new("P", "Top"))
            .with_prototype(Instance::new("Q", "Alias").with_transform(self.t4));
        field.push_affine(0, &self.placements[0]).unwrap();
        field.push_affine(1, &self.placements[1]).unwrap();
        Scene::new(
            StageSettings::new(UpAxis::Z, 1.0),
            Xform::new("Root")
                .with_instance(Instance::new("Direct", "Top").with_transform(self.t3))
                .with_instance(Instance::new("Plain", "Bare"))
                .with_point_instancer(field),
        )
        .with_prototype(
            Mesh::new("Tri", &TRIANGLE, Faces::triangles(&[0, 1, 2])).with_transform(self.t0),
        )
        .with_prototype(Instance::new("Alias", "Tri").with_transform(self.t1))
        .with_prototype(Instance::new("Bare", "Alias"))
        .with_prototype(Instance::new("Top", "Bare").with_transform(self.t2))
    }

    /// The world transform of each placed triangle: `Direct`, `Plain`,
    /// and the instancer's two instances.
    fn expected(&self) -> [[[f64; 4]; 4]; 4] {
        let top = [self.t0, self.t1, self.t2];
        [
            chain(&[top[0], top[1], top[2], self.t3]),
            chain(&[self.t0, self.t1]),
            chain(&[top[0], top[1], top[2], self.placements[0]]),
            chain(&[self.t0, self.t1, self.t4, self.placements[1]]),
        ]
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "coordinates are small; the sort key only needs to be stable"
)]
fn chains_of_shared_prototypes_place_their_geometry() {
    let Some(python) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    let chain = Chain::new();
    let dir = scratch_dir("chains");
    let files = write_forms(&chain.scene(), &dir);
    let report = oracle(&python, &files);
    eprintln!("OpenUSD {} via {python}", report.version);

    let mut want: Vec<Vec<[f64; 3]>> = chain
        .expected()
        .iter()
        .map(|m| {
            TRIANGLE
                .iter()
                .map(|p| apply(m, p.map(f64::from)))
                .collect()
        })
        .collect();
    let key = |points: &Vec<[f64; 3]>| points[0].map(|c| (c * 1e3).round() as i64);
    want.sort_by_key(key);
    for (file, layer) in files.iter().zip(&report.layers) {
        let name = file.display();
        let mut got: Vec<Vec<[f64; 3]>> = layer.placed.iter().map(|p| p.points.clone()).collect();
        got.sort_by_key(key);
        assert_eq!(got.len(), want.len(), "{name}: placed triangles");
        for (g, w) in got.iter().zip(&want) {
            for (a, b) in g.iter().zip(w) {
                for axis in 0..3 {
                    assert!(
                        (a[axis] - b[axis]).abs() < 1e-4,
                        "{name}: point {a:?}, expected {b:?}"
                    );
                }
            }
        }
        // The native instancer's extent is what OpenUSD computes from the
        // chain, and encloses its instances' points.
        let is_native = file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("instancers");
        assert_eq!(
            layer.extents.len(),
            usize::from(is_native),
            "{name}: instancers"
        );
        for extent in &layer.extents {
            for (a, c) in extent
                .extent
                .iter()
                .flatten()
                .zip(extent.computed.iter().flatten())
            {
                assert!(
                    (a - c).abs() < 1e-4,
                    "{name}: {} extent {:?}, OpenUSD computes {:?}",
                    extent.path,
                    extent.extent,
                    extent.computed
                );
            }
            let expected = chain.expected();
            for m in &expected[2..] {
                for p in TRIANGLE {
                    let p = apply(m, p.map(f64::from));
                    for axis in 0..3 {
                        assert!(
                            extent.extent[0][axis] - 1e-4 <= p[axis]
                                && p[axis] <= extent.extent[1][axis] + 1e-4,
                            "{name}: {p:?} outside {:?}",
                            extent.extent
                        );
                    }
                }
            }
        }
    }
}

/// A shared mesh with an indexed `tint` (`[1, 2, 3]` at index 2), placed
/// by instances that override it unindexed (`Over`, and `Deep` through
/// the shared instance `Alias`) or indexed (`Kept`), and by an instancer
/// whose per-instance `tint` is unindexed, over an inline copy of the mesh
/// and an instance of the shared one.
fn tinted_scene() -> Scene<'static> {
    let tinted = |name: &'static str| {
        Mesh::new(name, &TRIANGLE, Faces::triangles(&[0, 1, 2])).with_primvar(
            "tint",
            Primvar::constant(PrimvarData::float(vec![1.0, 2.0, 3.0])).with_indices(vec![2]),
        )
    };
    let unindexed = |value: f32| Primvar::constant(PrimvarData::float(vec![value]));
    let field = PointInstancer::new("Field", vec![0, 1], vec![[0.0; 3], [5.0, 0.0, 0.0]])
        .with_prototype(tinted("Inline"))
        .with_prototype(Instance::new("S", "Tinted"))
        .with_primvar(
            "tint",
            Primvar::per_instance(PrimvarData::float(vec![9.0, 10.0])),
        );
    Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root")
            .with_instance(Instance::new("Over", "Tinted").with_primvar("tint", unindexed(9.0)))
            .with_instance(Instance::new("Kept", "Tinted").with_primvar(
                "tint",
                Primvar::constant(PrimvarData::float(vec![7.0, 8.0])).with_indices(vec![1]),
            ))
            .with_instance(Instance::new("Deep", "Alias").with_primvar("tint", unindexed(5.0)))
            .with_point_instancer(field),
    )
    .with_prototype(tinted("Tinted"))
    .with_prototype(Instance::new("Alias", "Tinted"))
}

#[test]
fn unindexed_overrides_block_inherited_indices() {
    let Some(python) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    let dir = scratch_dir("indices");
    let files = write_forms(&tinted_scene(), &dir);
    let report = oracle(&python, &files);
    eprintln!("OpenUSD {} via {python}", report.version);
    for (file, layer) in files.iter().zip(&report.layers) {
        let name = file.display();
        let is_references = file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("references");
        let mut want: Vec<(&str, f64, Option<Vec<i64>>)> = vec![
            ("/Root/Over", 9.0, None),
            ("/Root/Kept", 8.0, Some(vec![1])),
            ("/Root/Deep", 5.0, None),
        ];
        if is_references {
            want.extend([
                ("/Root/Field/Inline_0", 9.0, None),
                ("/Root/Field/S_1", 10.0, None),
            ]);
        }
        for (path, value, indices) in want {
            let got = layer
                .instances
                .iter()
                .find(|i| i.path == path)
                .unwrap_or_else(|| panic!("{name}: no {path}"));
            assert_eq!(
                got.primvars.get("tint"),
                Some(&vec![vec![value]]),
                "{name}: {path} tint"
            );
            assert_eq!(
                got.indices.get("tint"),
                indices.as_ref(),
                "{name}: {path} tint indices"
            );
        }
    }
}
