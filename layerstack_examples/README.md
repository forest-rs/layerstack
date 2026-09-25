# layerstack_examples

Small runnable programs that demonstrate how to use the `layerstack` API.

## Run

- `cargo run -p layerstack_examples`
- `cargo run -p layerstack_examples --example minimal`
- `cargo run -p layerstack_examples --example sparse_array_edits`
- `cargo run -p layerstack_examples --example mesh_to_usdz -- cube.usdz [arkit|generic]`
  (writes a cube with normals and UVs as a USDZ package; the default ARKit / AR Quick Look
  profile uses a USDC root layer, `generic` a USDA one)
- `cargo run -p layerstack_examples --example mesh_materials_to_usdz -- cube.usdz [arkit|generic]`
  (writes a cube whose faces are split between a textured and a constant `UsdPreviewSurface`
  material, with its textures packaged, in the same two profiles)
- `cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit|generic]`
  (scatters trees and rocks over a field as one `PointInstancer`: each prototype, with its
  materials, is written once, and each placement is an index, a position, an orientation, a
  scale and an id)
