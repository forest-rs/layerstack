# layerstack_mesh_export

Export mesh-kernel buffers as USD polygon meshes, with transforms, normals,
UVs, extra primvars, Preview Surface materials, and static point instancers.
Outputs are USDA text, USDC binary layers, and generic or ARKit-profile USDZ
packages. The exporter writes authored data; it does not flatten a composed stage.

```toml
[dependencies]
layerstack_mesh_export = "0.1"
```

Requires Rust **1.88** or later. The crate uses `no_std + alloc`, has no
feature flags, and returns strings or byte buffers. The caller owns filesystem
I/O, mesh generation, and rendering.

## Export a triangle

```rust
use layerstack_mesh_export::{Faces, Mesh, Scene, StageSettings, UpAxis, Xform};

let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
let mesh = Mesh::new("Triangle", &points, Faces::Triangles(&[0, 1, 2]));
let scene = Scene::new(
    StageSettings::new(UpAxis::Z, 1.0),
    Xform::new("Root").with_mesh(mesh),
);
let text = scene.to_usda().expect("valid scene");
let binary = scene.to_usdc().expect("valid scene");
assert!(text.contains("faceVertexIndices"));
assert!(!binary.is_empty());
```

The exporter explicitly authors units, up axis, default prim, transforms, and
`subdivisionScheme = "none"`. It validates mesh and primvar cardinality and
indices, material bindings and face subsets, and instancer arrays before
writing. Indexed face-varying data keeps its seams; equal values are not merged.

## Materials, instances, and packages

Materials use the metallic `UsdPreviewSurface` workflow with constants or
textures. Bindings are direct or per-face subsets; they are not collection-
based or purpose-specific. MaterialX/OpenPBR shader graphs are outside this
exporter's profile. Point instancers support static prototypes, positions,
orientations, scales, IDs, and per-instance primvars, without motion samples
or masking. Shared prototypes can also be placed by instanced references.

`Scene::to_usdz` requires an explicit profile and the package files it references.
The generic profile uses a USDA root; the ARKit profile uses a USDC root with
its narrower set of media types. The packager does not discover or rewrite
external dependencies. Passing layout validation is not a guarantee that every
viewer renders every feature identically.

See the [API guide and material/instancer examples](https://docs.rs/layerstack_mesh_export)
and the [workspace examples](https://github.com/forest-rs/layerstack/tree/main/layerstack_examples).
For direct authored-document control, use the USDA or USDC writer crates instead.

## Release notes and license

See [CHANGELOG.md](CHANGELOG.md). Licensed under either
[Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
