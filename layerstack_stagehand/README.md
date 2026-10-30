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

Subdivision surfaces are drawn as their control cage. Instancing, cameras,
lights, materials, curves and points are not covered yet.

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
