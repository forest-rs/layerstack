// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `cluster_lod`: builds the cluster level-of-detail DAG of each mesh a USD
//! stage draws, reports them, and checks cuts against the full meshes.
//!
//! ```text
//! cluster_lod <stage.usd> [--prim <path>] [--render <dir>] [--size <px>]
//! ```
//!
//! Every mesh the stage draws is triangulated by `layerstack_stagehand`'s
//! draw list, each shared buffer once however many instances draw it (with
//! `--prim`, only that mesh, read alone, at its own origin). Each mesh's
//! DAG is built in turn and reported: triangles, levels, clusters, stuck
//! and sloppy groups, build time and the set's size; then the largest
//! mesh's levels.
//!
//! The cuts: the camera stands at a few distances from the stage's bounds
//! (in bounding radii) on one diagonal, and every instance of every mesh
//! takes its cut at 0.5, 1 and 4 px (the camera carried into the mesh's
//! space by the inverse of the instance's matrix), and the drawn triangles
//! are summed against the full meshes'.
//!
//! With `--render`, a small software rasterizer draws the full meshes and
//! their 1 px cuts from each distance, compares the two pixel by pixel
//! (coverage, and shading of the interpolated normals), and writes both
//! and a difference image as PNG files.
//!
//! With `--cache <dir>`, each mesh's set is written there instead, as a
//! `layerstack_clusters` file named by the key the editor computes from the
//! buffers it draws (`cache.rs`); a mesh whose file is there already is not
//! built again. With `--cut-file <file> --camera x,y,z,proj_y,height,near`,
//! the cut of one cached set is printed, cluster ids in ascending order,
//! for checking a renderer's cut against the library's.
//!
//! ```text
//! cluster_lod <stage.usd> --cache <dir>
//! cluster_lod --cut-file <dir/key.lsclust> --camera <x,y,z,proj_y,height,near> [--threshold <px>]
//! ```

mod cache;
mod raster;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use layerstack::{InMemoryStore, StageOptions, Time};
use layerstack_clusters::{ClusterConfig, ClusterFile, ClusterInput, ClusterSet, CutCamera};
use layerstack_io::{Filesystem, OpenOptions, StageDocument};
use layerstack_schemas::Scene;
use layerstack_stagehand::{DrawList, DrawOptions, MeshBuffers, MeshOptions, MeshPlan, StageMesh};

use raster::{Image, View};

type Matrix = [[f64; 4]; 4];

/// Renders are made this many times larger in each direction and
/// box-filtered down, so the comparison sees what a viewer sees rather
/// than which sub-pixel triangle a pixel's center happens to hit.
const SUPERSAMPLING: usize = 4;

const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

struct Args {
    stage: PathBuf,
    prim: Option<String>,
    under: Option<String>,
    threshold: f32,
    simplify_root: bool,
    normal_weight: Option<f32>,
    unclamped: bool,
    triangulate_only: bool,
    render: Option<PathBuf>,
    size: usize,
    cache: Option<PathBuf>,
    cut_file: Option<PathBuf>,
    camera: Option<CutCamera>,
}

const USAGE: &str = "usage: cluster_lod <stage.usd> [--prim <path> | --under <path>] [--render <dir>] [--size <px>] [--threshold <px>] [--simplify-root] [--normal-weight <w>] [--unclamped] [--triangulate-only] [--cache <dir>]\n       cluster_lod --cut-file <file.lsclust> --camera <x,y,z,proj_y,height,near> [--threshold <px>]";

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        stage: PathBuf::new(),
        prim: None,
        under: None,
        threshold: 1.0,
        simplify_root: false,
        normal_weight: None,
        unclamped: false,
        triangulate_only: false,
        render: None,
        size: 1024,
        cache: None,
        cut_file: None,
        camera: None,
    };
    let mut stage = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--prim" => parsed.prim = Some(value("--prim")?),
            "--under" => parsed.under = Some(value("--under")?),
            "--threshold" => {
                parsed.threshold = value("--threshold")?
                    .parse()
                    .map_err(|e| format!("--threshold: {e}"))?;
            }
            "--simplify-root" => parsed.simplify_root = true,
            "--normal-weight" => {
                parsed.normal_weight = Some(
                    value("--normal-weight")?
                        .parse()
                        .map_err(|e| format!("--normal-weight: {e}"))?,
                );
            }
            "--unclamped" => parsed.unclamped = true,
            "--triangulate-only" => parsed.triangulate_only = true,
            "--render" => parsed.render = Some(value("--render")?.into()),
            "--cache" => parsed.cache = Some(value("--cache")?.into()),
            "--cut-file" => parsed.cut_file = Some(value("--cut-file")?.into()),
            "--camera" => parsed.camera = Some(parse_camera(&value("--camera")?)?),
            "--size" => {
                parsed.size = value("--size")?
                    .parse()
                    .map_err(|e| format!("--size: {e}"))?;
            }
            _ if stage.is_none() && !arg.starts_with("--") => stage = Some(arg),
            _ => return Err(format!("unexpected argument {arg}\n{USAGE}")),
        }
    }
    if parsed.cut_file.is_some() {
        return Ok(parsed);
    }
    parsed.stage = stage.ok_or(USAGE)?.into();
    Ok(parsed)
}

/// `x,y,z,proj_y,height,near` as a [`CutCamera`].
fn parse_camera(text: &str) -> Result<CutCamera, String> {
    let values: Vec<f32> = text
        .split(',')
        .map(|v| v.trim().parse().map_err(|e| format!("--camera: {e}")))
        .collect::<Result<_, _>>()?;
    let [x, y, z, projection_y, viewport_height, z_near] = values[..] else {
        return Err("--camera takes x,y,z,proj_y,height,near".into());
    };
    Ok(CutCamera {
        position: [x, y, z],
        projection_y,
        viewport_height,
        z_near,
    })
}

/// A triangulated mesh: its buffers and the matrices of the instances
/// drawing them.
struct Source<'a> {
    name: String,
    buffers: &'a MeshBuffers,
    matrices: Vec<Matrix>,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if let Err(message) = run(&args) {
        eprintln!("cluster_lod: {message}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    if let Some(file) = &args.cut_file {
        return print_cut(file, args);
    }
    let started = Instant::now();
    let path = args
        .stage
        .canonicalize()
        .map_err(|e| format!("{}: {e}", args.stage.display()))?;
    let dir = path.parent().ok_or("a stage file inside a directory")?;
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or("a UTF-8 stage file name")?;
    let storage = Filesystem::new(dir, []).map_err(|e| e.to_string())?;
    let store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let document = StageDocument::open(
        storage,
        file,
        OpenOptions {
            stage: StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
            store: Some(store),
            ..OpenOptions::default()
        },
    )
    .map_err(|e| format!("opening {}: {e}", path.display()))?;
    let store = document.store();
    let scene = Scene::new(document.stage().stage(), store);
    println!("opened {file} in {:.2} s", started.elapsed().as_secs_f64());

    let triangulating = Instant::now();
    let single;
    let mut list = DrawList::new(DrawOptions::default());
    let sources = match &args.prim {
        Some(prim) => {
            single = read_mesh(&scene, prim)?;
            vec![Source {
                name: prim.clone(),
                buffers: &single,
                matrices: vec![IDENTITY],
            }]
        }
        None => {
            list.refresh(&scene);
            let mut sources = draw_list_meshes(&list, store);
            if let Some(under) = &args.under {
                sources.retain(|s| s.name.starts_with(under.as_str()));
            }
            sources
        }
    };
    let unique: u64 = sources.iter().map(|s| triangles(s.buffers)).sum();
    let drawn: u64 = sources
        .iter()
        .map(|s| triangles(s.buffers) * s.matrices.len() as u64)
        .sum();
    println!(
        "triangulated {} meshes in {:.2} s: {unique} triangles, {drawn} drawn with instances",
        sources.len(),
        triangulating.elapsed().as_secs_f64(),
    );

    if args.triangulate_only {
        return Ok(());
    }
    let defaults = ClusterConfig::default();
    let config = ClusterConfig {
        simplify_root: args.simplify_root,
        normal_weight: args.normal_weight.unwrap_or(defaults.normal_weight),
        clamp_attribute_error: !args.unclamped,
        ..defaults
    };
    if let Some(dir) = &args.cache {
        if config != defaults {
            return Err(
                "--cache builds with the default configuration, which its key names".into(),
            );
        }
        return write_cache(dir, &path, &sources, &config);
    }
    let sets = build_sets(&sources, &config)?;
    if let Some(largest) = (0..sources.len()).max_by_key(|&i| triangles(sources[i].buffers)) {
        println!("largest: {}", sources[largest].name);
        println!("level\tgroups\tclusters\ttriangles\tstuck\tsloppy");
        for (level, l) in sets[largest].report().levels.iter().enumerate() {
            println!(
                "{level}\t{}\t{}\t{}\t{}\t{}",
                l.groups, l.clusters, l.triangles, l.stuck_groups, l.sloppy_groups
            );
        }
    }
    cuts(args, &sources, &sets, drawn)
}

fn triangles(buffers: &MeshBuffers) -> u64 {
    (buffers.indices.len() / 3) as u64
}

/// Builds the set of every source not cached in `dir` yet and writes it
/// there as `<key>.lsclust` ([`cache::cache_key`]), with its positions,
/// normals and `st`; then the stage's manifest ([`cache::Manifest`]).
fn write_cache(
    dir: &Path,
    stage: &Path,
    sources: &[Source<'_>],
    config: &ClusterConfig,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let started = Instant::now();
    let (mut written, mut cached, mut bytes) = (0, 0, 0);
    let mut seen = HashMap::new();
    let mut manifest = cache::Manifest::new(stage);
    for source in sources {
        let key = cache::cache_key(source.buffers);
        let path = dir.join(format!("{key}.lsclust"));
        if let Some(counts) = seen.get(&key) {
            manifest.add(&source.name, &key, counts);
            cached += 1;
            continue;
        }
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let file =
                ClusterFile::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            let counts = cache::SetCounts::of(&file.set);
            manifest.add(&source.name, &key, &counts);
            seen.insert(key, counts);
            cached += 1;
            continue;
        }
        let subsets = triangle_subsets(source.buffers);
        let input = ClusterInput {
            normals: Some(&source.buffers.normals),
            uvs: source.buffers.st.as_deref(),
            triangle_subsets: subsets.as_deref(),
            ..ClusterInput::new(&source.buffers.positions, &source.buffers.indices)
        };
        let set = ClusterSet::build(&input, config).map_err(|e| format!("{}: {e}", source.name))?;
        let file = ClusterFile {
            set,
            positions: source.buffers.positions.clone(),
            normals: Some(source.buffers.normals.clone()),
            uvs: source.buffers.st.clone(),
        };
        let encoded = file.encode();
        bytes += encoded.len();
        write_whole(&path, &encoded)?;
        let counts = cache::SetCounts::of(&file.set);
        manifest.add(&source.name, &key, &counts);
        seen.insert(key, counts);
        written += 1;
    }
    write_whole(
        &dir.join(manifest.file_name()),
        manifest.to_json().as_bytes(),
    )?;
    println!(
        "cache {}: wrote {written} sets ({:.1} MB) in {:.2} s, {cached} already there",
        dir.display(),
        mb(bytes),
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

/// Writes `bytes` to `path` whole: under another name, then renamed, so a
/// reader never sees a partial file.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Prints the cut of the cached set in `--cut-file` from `--camera` at
/// `--threshold`: the cluster ids, ascending, as one JSON array.
fn print_cut(file: &Path, args: &Args) -> Result<(), String> {
    let camera = args.camera.ok_or("--cut-file needs --camera")?;
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let file = ClusterFile::decode(&bytes).map_err(|e| e.to_string())?;
    let mut cut = Vec::new();
    file.set.select_cut(&camera, args.threshold, &mut cut);
    cut.sort_unstable();
    let ids: Vec<String> = cut.iter().map(u32::to_string).collect();
    println!("[{}]", ids.join(","));
    Ok(())
}

/// Builds every source's DAG, reporting each and the totals.
fn build_sets(sources: &[Source<'_>], config: &ClusterConfig) -> Result<Vec<ClusterSet>, String> {
    let mut sets = Vec::with_capacity(sources.len());
    let mut seconds = 0.0;
    let mut bytes = 0;
    let mut clusters = 0;
    let mut levels = 0;
    println!(
        "mesh\ttriangles\tvertices\tsubsets\tlevels\tclusters\tstuck\tsloppy\tbuild s\tset MB"
    );
    for source in sources {
        let subsets = triangle_subsets(source.buffers);
        let input = ClusterInput {
            normals: Some(&source.buffers.normals),
            uvs: source.buffers.st.as_deref(),
            triangle_subsets: subsets.as_deref(),
            ..ClusterInput::new(&source.buffers.positions, &source.buffers.indices)
        };
        let building = Instant::now();
        let set = ClusterSet::build(&input, config).map_err(|e| format!("{}: {e}", source.name))?;
        let elapsed = building.elapsed().as_secs_f64();
        let report = set.report();
        seconds += elapsed;
        bytes += set.byte_size();
        clusters += report.clusters();
        levels = levels.max(set.levels());
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{elapsed:.3}\t{:.2}",
            source.name,
            report.triangles,
            report.vertices,
            report.subsets,
            set.levels(),
            report.clusters(),
            report.levels.iter().map(|l| l.stuck_groups).sum::<u32>(),
            report.levels.iter().map(|l| l.sloppy_groups).sum::<u32>(),
            mb(set.byte_size()),
        );
        sets.push(set);
    }
    let unique: u64 = sources.iter().map(|s| triangles(s.buffers)).sum();
    println!(
        "built {} DAGs in {seconds:.2} s ({:.2} M triangles/s): {unique} triangles, {clusters} clusters, up to {levels} levels; sets {:.1} MB ({:.1} B per input triangle)",
        sources.len(),
        unique as f64 / seconds.max(1e-9) / 1e6,
        mb(bytes),
        bytes as f64 / unique.max(1) as f64,
    );
    Ok(sets)
}

/// Reads one mesh off the stage and triangulates it.
fn read_mesh(scene: &Scene<'_>, prim: &str) -> Result<MeshBuffers, String> {
    let mesh = StageMesh::read(scene, prim, Time::Default).ok_or(format!("no Mesh at {prim}"))?;
    let input = mesh.input();
    let plan = MeshPlan::new(&input, &MeshOptions::default());
    plan.buffers(&input).map_err(|e| format!("{prim}: {e:?}"))
}

/// Every triangulated buffer the stage draws, with its instances' matrices.
fn draw_list_meshes<'a>(list: &'a DrawList, store: &InMemoryStore) -> Vec<Source<'a>> {
    let mut sources: Vec<Source<'a>> = Vec::new();
    let mut seen = HashMap::new();
    for item in list.items() {
        let Some(buffers) = list.buffers(item.buffer) else {
            continue;
        };
        if buffers.buffers.indices.is_empty() || item.hidden() {
            continue;
        }
        let index = *seen.entry(item.buffer).or_insert_with(|| {
            sources.push(Source {
                name: store.paths().display(buffers.prim, store.tokens()),
                buffers: &buffers.buffers,
                matrices: Vec::new(),
            });
            sources.len() - 1
        });
        sources[index].matrices.extend_from_slice(&item.matrices);
    }
    sources
}

/// Each triangle's draw range, when the mesh has more than one.
fn triangle_subsets(buffers: &MeshBuffers) -> Option<Vec<u32>> {
    let ranges: Vec<_> = buffers.ranges.iter().filter(|r| r.count > 0).collect();
    if ranges.len() < 2 {
        return None;
    }
    let mut subsets = vec![0; buffers.indices.len() / 3];
    for (id, range) in (0..).zip(&ranges) {
        let first = range.first as usize / 3;
        subsets[first..first + range.count as usize / 3].fill(id);
    }
    Some(subsets)
}

/// `p * m` for a row-vector matrix.
fn transform(m: &Matrix, p: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|c| p[0] * m[0][c] + p[1] * m[1][c] + p[2] * m[2][c] + m[3][c])
}

/// The inverse of an affine row-vector matrix, and its largest axis scale.
fn invert_affine(m: &Matrix) -> (Matrix, f64) {
    let a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let mut inv = [[0.0; 3]; 3];
    for (r, row) in inv.iter_mut().enumerate() {
        for (c, value) in row.iter_mut().enumerate() {
            let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
            let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
            *value = (a[r1][c1] * a[r2][c2] - a[r1][c2] * a[r2][c1]) / det;
        }
    }
    let t = [m[3][0], m[3][1], m[3][2]];
    let it = [0, 1, 2].map(|c| -(t[0] * inv[0][c] + t[1] * inv[1][c] + t[2] * inv[2][c]));
    let scale = a
        .iter()
        .map(|row| (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt())
        .fold(0.0, f64::max);
    (
        [
            [inv[0][0], inv[0][1], inv[0][2], 0.0],
            [inv[1][0], inv[1][1], inv[1][2], 0.0],
            [inv[2][0], inv[2][1], inv[2][2], 0.0],
            [it[0], it[1], it[2], 1.0],
        ],
        scale,
    )
}

/// The world bounding sphere of every instance: the center of their
/// spheres' box and the farthest sphere edge from it.
fn world_bounds(sources: &[Source<'_>], sets: &[ClusterSet]) -> ([f64; 3], f64) {
    let mut spheres = Vec::new();
    for (source, set) in sources.iter().zip(sets) {
        // The root group's bounds contain the whole mesh.
        let Some(root) = set.groups().bounds.last() else {
            continue;
        };
        for m in &source.matrices {
            let (_, scale) = invert_affine(m);
            let center = transform(m, [0, 1, 2].map(|a| f64::from(root[a])));
            spheres.push((center, f64::from(root[3]) * scale));
        }
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (c, r) in &spheres {
        for a in 0..3 {
            lo[a] = lo[a].min(c[a] - r);
            hi[a] = hi[a].max(c[a] + r);
        }
    }
    let center = [0, 1, 2].map(|a| (lo[a] + hi[a]) * 0.5);
    let radius = spheres
        .iter()
        .map(|(c, r)| {
            (0..3)
                .map(|a| (c[a] - center[a]).powi(2))
                .sum::<f64>()
                .sqrt()
                + r
        })
        .fold(0.0, f64::max);
    (center, radius)
}

/// Each instance's cut from a few distances, summed; with `--render`,
/// renders of the full meshes and their 1 px cuts, compared.
fn cuts(
    args: &Args,
    sources: &[Source<'_>],
    sets: &[ClusterSet],
    drawn: u64,
) -> Result<(), String> {
    if sources.is_empty() {
        return Ok(());
    }
    let (center, radius) = world_bounds(sources, sets);
    let inverses: Vec<Vec<(Matrix, f64)>> = sources
        .iter()
        .map(|s| s.matrices.iter().map(invert_affine).collect())
        .collect();
    // 45 degrees vertically.
    let projection_y = 1.0 / std::f32::consts::FRAC_PI_8.tan();
    let direction = {
        let d = [0.6_f64, 0.45, 0.66];
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        d.map(|x| x / l)
    };
    if let Some(dir) = &args.render {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    println!(
        "stage radius {radius:.3}; cuts at {} px viewport height",
        args.size
    );
    println!("distance (radii)\tthreshold px\tclusters\ttriangles drawn\tof full\tselect ms");
    let mut cut = Vec::new();
    for distance in [1.5_f64, 4.0, 16.0, 64.0] {
        let eye = [0, 1, 2].map(|a| center[a] + direction[a] * radius * distance);
        let z_near = radius * 0.01;
        let camera_for = |inverse: &(Matrix, f64)| {
            let position = transform(&inverse.0, eye);
            CutCamera {
                position: position.map(narrow),
                projection_y,
                viewport_height: args.size as f32,
                z_near: narrow(z_near / inverse.1.max(1e-12)),
            }
        };
        for threshold in [0.5_f32, 1.0, 4.0] {
            let selecting = Instant::now();
            let (mut clusters, mut triangles) = (0_u64, 0_u64);
            for (set, inverses) in sets.iter().zip(&inverses) {
                for inverse in inverses {
                    set.select_cut(&camera_for(inverse), threshold, &mut cut);
                    clusters += cut.len() as u64;
                    triangles += cut
                        .iter()
                        .map(|&c| u64::from(set.clusters().triangle_count[c as usize]))
                        .sum::<u64>();
                }
            }
            let ms = selecting.elapsed().as_secs_f64() * 1e3;
            println!(
                "{distance}\t{threshold}\t{clusters}\t{triangles}\t{:.4}\t{ms:.1}",
                triangles as f64 / drawn.max(1) as f64
            );
        }
        let Some(dir) = &args.render else {
            continue;
        };
        let view = View {
            eye: eye.map(narrow),
            target: center.map(narrow),
            projection_y,
            z_near: narrow(z_near),
            size: args.size * SUPERSAMPLING,
        };
        let rendering = Instant::now();
        let mut full = Image::new(view.size);
        let mut lod = Image::new(view.size);
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut indices = Vec::new();
        for ((source, set), inverses) in sources.iter().zip(sets).zip(&inverses) {
            for (m, inverse) in source.matrices.iter().zip(inverses) {
                world_vertices(source.buffers, m, &mut positions, &mut normals);
                raster::render(
                    &view,
                    &mut full,
                    &positions,
                    Some(&normals),
                    &source.buffers.indices,
                );
                set.select_cut(&camera_for(inverse), args.threshold, &mut cut);
                indices.clear();
                indices.extend(
                    cut.iter()
                        .flat_map(|&c| set.cluster_indices(c as usize).iter().copied()),
                );
                raster::render(&view, &mut lod, &positions, Some(&normals), &indices);
            }
        }
        let full = raster::downsample(&full, SUPERSAMPLING);
        let lod = raster::downsample(&lod, SUPERSAMPLING);
        let (d, picture) = raster::compare(&full, &lod);
        let percent = |n: usize| 100.0 * n as f64 / d.covered.max(1) as f64;
        println!(
            "render {distance}: {} px covered; grey differs by more than 8/255 at {} px ({:.3}%), by more than 32/255 at {} px ({:.3}%); mean {:.2}/255 ({:.1} s)",
            d.covered,
            d.over_8,
            percent(d.over_8),
            d.over_32,
            percent(d.over_32),
            d.mean,
            rendering.elapsed().as_secs_f64(),
        );
        let write = |name: String, rgb: &[[u8; 3]]| {
            let path: PathBuf = Path::new(dir).join(name);
            raster::write_png(&path, args.size, rgb).map_err(|e| format!("{}: {e}", path.display()))
        };
        write(format!("full-{distance}.png"), &raster::grey(&full))?;
        write(format!("cut-{distance}.png"), &raster::grey(&lod))?;
        write(format!("diff-{distance}.png"), &picture)?;
    }
    Ok(())
}

/// One instance's vertices in world space.
fn world_vertices(
    buffers: &MeshBuffers,
    m: &Matrix,
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
) {
    positions.clear();
    positions.extend(
        buffers
            .positions
            .iter()
            .map(|p| transform(m, p.map(f64::from)).map(narrow)),
    );
    normals.clear();
    normals.extend(buffers.normals.iter().map(|n| {
        let n = n.map(f64::from);
        [0, 1, 2]
            .map(|c| n[0] * m[0][c] + n[1] * m[1][c] + n[2] * m[2][c])
            .map(narrow)
    }));
}

/// A world-space value as the `f32` meshes and cameras use.
#[expect(
    clippy::cast_possible_truncation,
    reason = "scene coordinates are f32 at heart"
)]
fn narrow(x: f64) -> f32 {
    x as f32
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
