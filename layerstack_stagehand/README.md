# layerstack_stagehand

Render-ready buffers from a composed Layerstack stage: the layer between
Layerstack and a renderer, for a 3D engine, procedural generation or the
browser.

- **Meshes** (`UsdGeomMesh`): faces triangulated as Hydra fans them, with
  concave faces ear-clipped instead (or everything fanned, for parity);
  vertices split for face-varying and indexed normals, `st` and
  `displayColor`; authored or smooth computed normals (Hydra's, or turned
  to the face at a concave face's reflex corners on request); one draw range per
  `materialBind` face subset; a retained plan that refills positions and
  normals for new points without triangulating again.
- **Implicit shapes**: `Cube`, `Sphere`, `Cylinder`, `Cone`, `Capsule` and
  `Plane`, and the `Cylinder_1` and `Capsule_1` revisions with a radius per
  end, tessellated at explicit segment counts.
- **Reports**: counts of faces, triangles and vertices, the faces skipped
  and why, concave faces and fallbacks, dropped primvars and subset
  overlaps.

- **Draw lists** (with `stage`): a retained `DrawList` of instanced
  draws (gprims, native instancing prototypes, point instancer
  prototypes) over buffers it owns, kept up to date from `Changes` and
  time changes by `DrawDiff`s that list what changed and count what was
  reused, refilled or rebuilt.

- **Cameras and lights** (with `stage`): the draw list also lists
  `UsdGeomCamera`s (view and projection matrices as `GfCamera` and
  `GfFrustum` compute them, fitted to a viewport by `conform_window`) and
  the built-in `UsdLux` lights with renderer-ready values (world placement
  and direction, emission with exposure, blackbody color and `normalize`,
  shape, shaping, shadow and link collections; inputs read through their
  connections), with diffs and time-variability like draw items.

- **Materials** (with `stage`): each material the items bind as a
  renderer-ready `UsdPreviewSurface` record: every input a constant, a
  primvar or a `UsdUVTexture` lookup (file anchored to its layer, UDIM
  tiles, wrap, scale and bias, color space, `UsdPrimvarReader_float2`
  coordinates and `UsdTransform2d`), connections followed through node
  graphs and interface inputs, spec fallbacks, the alpha mode, and notes
  for what cannot be represented; diffed, and read again only when
  something it reads changes.

Subdivision surfaces are drawn as their control cage. Other shaders, curves,
points and light linking evaluation are not covered yet.

```rust
use layerstack_stagehand::{MeshInput, MeshOptions, MeshPlan, MeshTopology};

let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
let input = MeshInput {
    topology: MeshTopology {
        face_vertex_counts: &[4],
        face_vertex_indices: &[0, 1, 2, 3],
        ..MeshTopology::default()
    },
    points: &points,
    ..MeshInput::default()
};
let plan = MeshPlan::new(&input, &MeshOptions::default());
let buffers = plan.buffers(&input).expect("the input the plan was built from");
assert_eq!(buffers.indices, [0, 1, 2, 0, 2, 3]);
```

With the `stage` feature (default), `StageMesh::read` and `read_shape` read
from a `layerstack_schemas::Scene`. Without it, the crate depends on neither
`layerstack` nor its schemas.

Requires Rust **1.92** or later (for `exedra_triangulate`). `no_std + alloc`.

The TypeScript package `@forest-rs/layerstack_stagehand` (`packages/layerstack_stagehand`)
wraps this crate's WebAssembly module (`layerstack_stagehand_wasm`).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
