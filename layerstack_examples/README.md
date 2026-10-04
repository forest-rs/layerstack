# layerstack_examples

Runnable workflows for Layerstack's composition, authoring, schema and I/O APIs.
Examples stay in this workspace crate so application dependencies do not enter
core crates. Existing example names remain stable.

## Start here: OpenUSD tutorials in Rust

These programs adapt the official OpenUSD tutorials (which use Python bindings
on the C++ implementation) to our explicit store, stage and edit-target APIs.
Each program creates its own input and checks its results. Pass an output directory
as the first argument, or use the temporary directory printed by the program.

```sh
cargo run -p layerstack_examples --example tutorial_hello_world -- ./tutorial-output/hello
cargo run -p layerstack_examples --example tutorial_references -- ./tutorial-output/references
cargo run -p layerstack_examples --example tutorial_variants -- ./tutorial-output/variants
cargo run -p layerstack_examples --example tutorial_stage_io -- ./tutorial-output/io
```

| Example | Workflow | OpenUSD tutorial |
| --- | --- | --- |
| `tutorial_hello_world` | Create, type and save a sphere; inspect generic properties | [Hello World](https://openusd.org/release/tut_helloworld.html), [Generic Prims](https://openusd.org/release/tut_helloworld_redux.html), [Properties](https://openusd.org/release/tut_inspect_and_author_props.html) |
| `tutorial_references` | Load one asset, reference it twice and override one occurrence | [Referencing Layers](https://openusd.org/release/tut_referencing_layers.html) |
| `tutorial_variants` | Clear a stronger opinion, author through variant edit targets and select a branch | [Authoring Variants](https://openusd.org/release/tut_authoring_variants.html) |
| `tutorial_stage_io` | Traverse with pruning, retain a checked query, undo edits, export USDC and reload | [Traversing a Stage](https://openusd.org/release/tut_traversing_stage.html), [Layer Formats](https://openusd.org/release/tut_converting_between_layer_formats.html) |

CI reopens these authored outputs with OpenUSD 26.8 and independently checks
schema values, reference overrides, variant selection and binary export.
The examples use `StageDocument<Filesystem>` for file workflows, `SchemaEdit`
for typed authoring and `Transaction` for guarded edits. Composition arcs use
layer APIs followed by explicit synchronization. Applications supply storage,
transport, conflict handling and rendering.

## Composition and editing

Run an example with `cargo run -p layerstack_examples --example NAME`.

| Example | What it demonstrates |
| --- | --- |
| `minimal` | A small composed layer stack |
| `asset_resolution` | Host-owned asset resolution |
| `asset_localization` | Explicit localization for portable assets |
| `instancing` | Shared composition prototypes and instance occurrences |
| `generated_assets` | Two `UsdProc` recipes, tracked evaluation, shared geometry, material texture references, native/point placements and dependent-bound invalidation |
| `producer_durability` | Store-bound retained producers, unchanged query/snapshot reuse, output repair, source-layer replacement, rejection of stale delayed work and explicit history recovery |
| `live_editing` | Retained stage edits and synchronization |
| `sparse_array_edits` | Incremental array changes |
| `explain_value` | Value provenance through references, overrides, sparse edits and time offsets |
| `flatten` | Flatten a referenced scene and verify its composed values |
| `timeline` | Time samples and interpolation |
| `value_clips` | Detached clip bundles, asset requests, interpolation and provenance |
| `validate_usda` | Validation of authored scene data |

`cargo run -p layerstack_examples` runs the introductory composition program.

## Geometry, shading and engine inputs

| Example | What it demonstrates |
| --- | --- |
| `typed_schema` | Typed mesh, light and collection authoring, fallbacks and inherited properties |
| `world_transforms` | Animated transforms, inherited visibility and purpose through `XformCache` |
| `camera_frustum` | Sampled projection and frustum culling of moving point instances |
| `morph_animation` | Retained morph definitions and weights with cached geometry outputs |
| `deformation_inputs` | Renderer-owned binding buffers and shared rig palettes for GPU deformation |
| `light_rig_slots` | Sparse slot edits using `opinionated` alone |

These additional workflows are binaries: use
`cargo run -p layerstack_examples --bin NAME`.

| Binary | What it demonstrates |
| --- | --- |
| `shading_values` | Material interface inputs and shader value providers |
| `lighting_inputs` | Light discovery, shaping, shadows, texture provenance and incremental engine inputs |
| `retained_queries` | Reusable transform, bounds and shading evaluation across changes |
| `stage_notices` | Inspecting retained stage changes |
| `session_collaboration` | Two clients with independent session overrides over shared live edits, guarded conflicts and scoped synchronization |

GPU layouts, resource handles, color management, sampling, transport, permissions
and persistent publication remain application choices. The library target also
compiles a generated `shaderDefs.usda` interface with `#![no_std]`, covering
scalar and array inputs and outputs without the standard prelude.

## Package export

| Command | What it writes |
| --- | --- |
| `cargo run -p layerstack_examples --example mesh_to_usdz -- cube.usdz [arkit\|generic]` | A cube with normals and UVs |
| `cargo run -p layerstack_examples --example mesh_materials_to_usdz -- cube.usdz [arkit\|generic]` | A cube with textured and constant `UsdPreviewSurface` face materials |
| `cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit\|generic]` | Material-bearing prototypes and point instances |

The default ARKit / AR Quick Look profile uses a USDC root and exports scatter
instances as references. The generic profile uses a USDA root and a
`PointInstancer`. Affine placements include negative scales for mirrors.
