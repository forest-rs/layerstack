# layerstack_clusters_examples

`cluster_lod`: builds a `layerstack_clusters` DAG for every mesh a USD
stage draws, reports each, and checks cuts against the full meshes. It
lives in its own crate so the C++ build of meshoptimizer stays out of
`layerstack_examples` and the core crates.

```sh
cargo run --release -p layerstack_clusters_examples --bin cluster_lod -- scene.usda --render out
```

- **Meshes**: `layerstack_stagehand`'s draw list triangulates what the
  stage draws (visible, default purpose), each shared buffer once however
  many instances draw it; `--prim <path>` reads one mesh alone, `--under
  <path>` keeps the meshes below a path. The adapter is a few lines:
  `MeshBuffers` positions, normals, `st` and indices, and a subset id per
  triangle from its draw ranges.
- **Report**: per mesh, triangles, vertices, subsets, levels, clusters,
  stuck and sloppy groups, build time and the set's size; the totals; the
  largest mesh's levels.
- **Cuts**: from 1.5, 4, 16 and 64 bounding radii on one diagonal, every
  instance takes its cut at 0.5, 1 and 4 px (the camera carried into the
  mesh's space by the inverse of the instance's matrix, so the cut is the
  one that instance would draw), and the triangles drawn are summed
  against the full count. `--threshold` sets the pixel error rendered.
- **Renders** (`--render <dir>`): a small software rasterizer draws the
  full meshes and their cuts from each distance (interpolated normals, a
  headlight, 4x4 supersampled and box-filtered to `--size`, 1024 px by
  default), compares them pixel by pixel, prints how many pixels differ
  by more than 8 and 32 grey levels and the mean difference, and writes
  `full-*.png`, `cut-*.png` and `diff-*.png` (dimmed where they agree, blue
  above 8 levels, red above 32).

Comparing filtered renders, rather than which triangle each pixel's center
hits, measures what a viewer sees; it needs no GPU and no image crate.
`--normal-weight`, `--unclamped` and `--simplify-root` change the build
configuration for experiments; `--triangulate-only` stops before building,
for measuring the build's memory against the stage's.
