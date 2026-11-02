# layerstack_clusters

Cluster level-of-detail DAGs for triangle meshes, built natively: a mesh
split into clusters of up to 128 triangles, merged into groups, each group
simplified with its borders locked and split into clusters again, level
after level. A renderer cuts through the DAG at a pixel error and draws a
crack-free mix of fine and coarse clusters.

**Experimental.** The crate serves the editor showcase's large-scene work
for now, is not published, and its home may change. It depends on no
Layerstack crate (plain arrays in, a `ClusterSet` out), so it can move as
it is.

```rust
use layerstack_clusters::{ClusterConfig, ClusterInput, ClusterSet, CutCamera};

// `positions: &[[f32; 3]]`, `indices: &[u32]`, three per triangle.
let input = ClusterInput::new(positions, indices);
let set = ClusterSet::build(&input, &ClusterConfig::default())?;
let camera = CutCamera {
    position: [0.0, 0.0, 10.0],
    projection_y: 2.414, // 1 / tan(fov_y / 2) for 45 degrees
    viewport_height: 1080.0,
    z_near: 0.1,
};
let mut cut = Vec::new();
set.select_cut(&camera, 1.0, &mut cut);
for &cluster in &cut {
    let indices = set.cluster_indices(cluster as usize);
    // draw `indices` over the input's vertices
}
```

## Design

- **Input** (`ClusterInput`): triangulated positions and indices; optional
  normals and texture coordinates, weighed into the simplification error;
  optional per-triangle subset ids (a material's draw range, say).
  Layerstack's `layerstack_stagehand` produces exactly this from a stage
  (`MeshBuffers`), and the crate depends on neither: the adapter is a few
  lines, shown in `layerstack_clusters_examples`.
- **Output** (`ClusterSet`): one per mesh, plain columns ready to become
  typed arrays. Every level indexes the caller's vertices; the build makes
  no new vertices and keeps no copy of them. A cluster table (index start,
  triangle and vertex counts, group, the group it was simplified from,
  subset, bounding sphere), a group table (depth, the bounds and error of
  the group's simplified replacement, its clusters) and a bounding
  hierarchy over each level's groups. Errors are monotonic: a group's
  error is at least that of every group its clusters came from, and its
  bounds contain theirs.
- **Cut** (`ClusterSet::select_cut`): meshoptimizer's `clusterlod` rule.
  A cluster is drawn when its own group's simplified error projects above
  the threshold (its replacement would show) and the group it was
  simplified from projects at or below it (it is fine enough); original
  clusters pass the second test. Projected error is `error / max(distance
  - radius, z_near) * proj[1][1] / 2 * viewport height`. The walk descends
  the hierarchy only into nodes whose largest error projects above the
  threshold. `select_cut_linear` tests every cluster, for checking.
- **Subsets**: a cluster holds one subset's triangles, and positions where
  subsets meet are locked at every level, so each subset's triangles in a
  cut keep their material and meet without gaps.
- **Seams**: a vertex split for a normal or texture coordinate is
  protected (meshoptimizer's permissive mode with protect flags), so seams
  stay put while simplification collapses across smooth attributes.
- **Report** (`ClusterReport`): groups, clusters and triangles per level,
  stuck groups (simplification kept more than 85% of their triangles, so
  their branch ends) and groups that needed the sloppy fallback.
- **Deterministic and single threaded.** Hosts supply threads. Within a
  level, every group is simplified and split from read-only state (the
  level's locks, the mesh) and appended in group order, so a host could
  simplify a level's groups in parallel and append in order with the same
  result.

The orchestration is a Rust port of meshoptimizer's `demo/clusterlod.h`
(version 1.3, MIT) over meshoptimizer's own primitives; the source files
carry the upstream notice. On a 2 M-triangle grid the port's levels match
upstream `clodBuild`'s within 1% of triangles per level, built in 2.27 s
against upstream's 2.09 s (it also computes every cluster's precise
bounds); a test checks the levels against upstream on every run.

## Dependency: `meshopt`

`meshopt` 0.7 (default features only; `bindgen` stays off, the bindings
ship pre-generated) wraps meshoptimizer 1.3, the C++ library by the
author of the algorithms, compiled by its `cc` build script. It provides
cluster building (`meshopt_buildMeshletsFlex`), cluster partitioning,
attribute-aware and sloppy simplification with per-vertex locks, cluster
and sphere bounds and spatial clustering: the reference implementations,
fast and maintained, which a pure Rust port would have to track. Only
this crate depends on it; `layerstack_stagehand` and the core crates stay
pure Rust. It brings `bitflags`, `thiserror` and `float-cmp` along (`cc`
is already in the workspace's lockfile). `meshopt` 0.7 declares Rust 1.87 and builds at the workspace's
1.89.

The crate ports `clusterlod.h` rather than calling `meshopt`'s
`clod_build` wrapper of it for three reasons: owning the orchestration
lets it keep subsets apart and lock their borders and report per level;
`meshopt` does not compile `clusterlod.cpp` for WebAssembly (it needs the
C++ standard library), while the primitives it uses are plain C; and the
port is where a parallel build would go.

It needs a C++ compiler (MSVC, GCC or Clang through `cc`). It is not
`no_std`. It does not build for `wasm32-unknown-unknown` yet:
`meshopt` 0.7 compiles meshoptimizer with its own minimal libc headers
there, which lack `floorf` and `log2f`, used by meshoptimizer 1.3's
opacity micromap code.

## Measurements

Release build, one thread, Apple silicon; build time excludes reading and
triangulating. Pixel differences compare renders of the full meshes and
their 1 px cuts (interpolated normals, headlight, 4x4 supersampled, 1024
px), from 1.5 and 4 bounding radii.

| Input | Meshes | Triangles | Build | Set size | Triangles drawn at 1 px (1.5 / 4 radii) | Mean grey difference (1.5 / 4 radii) |
| --- | --- | --- | --- | --- | --- | --- |
| A production element (its 594 k instances draw 73 M triangles) | 2,302 | 2.90 M | 3.4 s | 67 MB | 87% / 86% | 1.0 / 3.6 of 255 |
| The same element's 2,273 uninstanced meshes alone | 2,273 | 2.89 M | 3.3 s | 67 MB | 17% / 8% | 1.2 / 3.1 of 255 |
| A larger element: six variants of it (369 M triangles drawn) | 13,783 | 17.4 M | 19.4 s | 401 MB | | |
| An architectural scene | 115 | 3.75 M | 4.9 s | 89 MB | 39% / 24% | 0.08 / 0.42 of 255 |
| A foliage mesh (leaf cards, 11 M vertices, 3 subsets) | 1 | 4.94 M | 7.8 s | 115 MB | 73% / 32% | 2.9 / 22 of 255 |

- **Speed**: 0.6 to 0.9 M input triangles a second, as upstream.
- **Size**: 24 to 25 bytes per input triangle (every level's indices,
  about twice the input's, plus the tables).
- **Memory**: the foliage mesh peaks 563 MB above its triangulated buffers
  (114 bytes per input triangle, the retained set included); the largest
  transients are the interleaved attribute copy, the position remap and
  meshoptimizer's first clustering of the whole mesh. No step is
  quadratic: partitioning, boundary locking and the cut are linear in the
  clusters or indices they touch, and each level locks only the vertices
  it still references.

What the numbers show:

- A pixel of geometric error does not bound shading error: normals
  interpolated over larger triangles shade differently. At meshoptimizer's
  demo weight of 0.5 for normals, the uninstanced meshes above differ by a
  mean of 5.1 grey levels at 1.5 radii; at 2, by 2.1; at the default 4, by
  1.2, drawing 7%, 13% and 17% of the triangles.
- A DAG bottoms out at one cluster per mesh, so the element's 594 k
  instances of 29 small meshes (300 to 700 triangles) draw 90% of their
  triangles at 1.5 radii, and about as many at any distance.
  `ClusterConfig::simplify_root` keeps simplifying the last cluster and
  cuts that to 7%, but those instances are two or three
  pixels across there, and a pixel of mostly inward error removes a
  visible share of their coverage (mean difference 39 of 255). Whole
  instances that small want culling or merging above the mesh DAG.
- Leaf cards (thousands of disconnected quads) cannot be simplified while
  keeping topology, so every first-level group of the foliage mesh falls
  back to sloppy simplification, which merges leaves and opens holes
  inside groups; its cuts hold up close and lose the canopy's look from
  afar.

Not yet: a parallel build, a cache encoding (content hash plus
meshoptimizer's vertex and index codecs), and the WebAssembly build.
