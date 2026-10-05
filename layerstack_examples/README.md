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
| `tutorial_stage_io` | Traverse with pruning, retain a checked query, undo edits, export USDC and reload while preserving a private session preview | [Traversing a Stage](https://openusd.org/release/tut_traversing_stage.html), [Layer Formats](https://openusd.org/release/tut_converting_between_layer_formats.html), [Stage session layers](https://openusd.org/release/api/class_usd_stage.html#Usd_SessionLayer) |

CI reopens these authored outputs with OpenUSD 26.8 and independently checks
schema values, reference overrides, variant selection and binary export.
The examples use `StageDocument<Filesystem>` for file workflows, `SchemaEdit`
for typed authoring and `Transaction` for guarded edits. Composition arcs use
layer APIs followed by explicit synchronization. Applications supply storage,
transport, conflict handling and rendering.

## Share edits and keep private previews

```sh
cargo run -p layerstack_examples --bin session_collaboration
cargo run -p layerstack_examples --example tutorial_stage_io -- ./tutorial-output/io
```

Start with `session_collaboration`: two clients read a published root through a
shared working layer and their own session layers. Client A previews exposure
`10`, while a guarded shared edit changes exposure from `2` to `3`. A keeps its
preview and B reads `3`; replaying the shared edit fails its guard. Creating and
undoing a shared prim shows when each client sees the change: after its explicit
synchronization. Finally, A undoes its private override and reads the current
shared value `3`. The published root still contains exposure `1`.

Then run `tutorial_stage_io` to persist authored content. It saves a sphere with
radius `2`, exports the root as `HelloWorld.usdc`, and attaches a private session
preview with radius `9`. Saving excludes the session; reloading source layers
preserves the preview. Undoing it reveals the saved radius `2`. Use
`ReloadPolicy::PreserveDirty` to reject reloads that would discard source edits,
or `DiscardDirty` when that replacement is intentional.

This follows OpenUSD's C++
[`UsdStage` session-layer, save and reload contracts](https://openusd.org/release/api/class_usd_stage.html).
Layerstack makes edit targets and synchronization explicit. The application
owns collaboration transport, permission checks and durable publication.

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
| `importer_inputs` | Checked geometry reads, validated material subsets, direct indexed primvars and bounded instance chunks |
| `lazy_io` | Opt-in retained USDC arrays, checked demand reads, decode statistics and query refresh after reload |
| `prepared_reload` | Validate checked geometry and packaged texture bytes before accepting a reload; rejected candidates leave the published document intact |
| `codeless_api` | Load a runtime schema library and apply single/multiple APIs by name, inspect fallback values and undo |
| `primvar_workflow` | Indexed primvar authoring, index animation, ID targets and reusable inheritance sets |
| `shading_values` | Material interface inputs and shader value providers |
| `lighting_inputs` | Light discovery, shaping, shadows, texture provenance and incremental engine inputs |
| `retained_queries` | Reusable transform, bounds and shading evaluation across changes |
| `extent_providers` | Custom Boundable geometry, tracked extent inputs, external revisions and authored extent precedence |
| `stage_notices` | Inspecting retained stage changes |
| `session_collaboration` | Two clients with independent session overrides over shared live edits, guarded conflicts and scoped synchronization |
| `namespace_and_layers` | Inspect an atomic dependent-stage rename, retain source geometry through relocates, consolidate local layers with external arcs, and undo |

GPU layouts, resource handles, color management, sampling, transport, permissions
and persistent publication remain application choices. The library target also
compiles a generated `shaderDefs.usda` interface with `#![no_std]`, covering
scalar and array inputs and outputs without the standard prelude.

For custom geometry, run `cargo run -p layerstack_examples --bin extent_providers`.
It registers a `PaddedBox` schema derived from `Boundable` and a provider that
reads its composed `size`. A repeated read and an unrelated edit reuse the
result; resizing recomputes it. Changing application-owned padding takes effect
after `set_extent_revision`. An authored `extent` overrides the provider.
The assertions check half-sizes `1`, `2`, `3` and `8`, with three provider calls.
This adapts OpenUSD's
[`UsdGeomRegisterComputeExtentFunction` pattern](https://openusd.org/release/api/boundable_compute_extent_8h.html)
to a caller-owned registry. Bounds caches still own transforms, purpose and
scene traversal.

To inspect binary arrays on demand, run:

```sh
cargo run -p layerstack_examples --bin lazy_io
cargo run -p layerstack_examples --bin lazy_io -- ./scene.usdc /World/Mesh.points
```

The first command generates a mesh and opens its USDC export with
`LoadOptions` selecting `UsdcArrayLoading::Retained`. It checks that opening
performs no array decodes, reading points decodes one source, and reading again
reuses that source. The second command inspects a property from your own asset;
USDZ packages with a USDC root work too. Both print retained-input bytes,
materialized arrays, decode attempts and the remaining decode budget.

Use `AttributeQuery::try_get`, generated schema `try_*` array getters or
`PrimView::try_read_value` to preserve decode errors. The existing `open` and
`open_in` calls use eager decoding; retained loading is an explicit option.
Reload creates fresh source pools, while captured values and inspection handles
keep their old pools alive. Decoding happens during demand reads; the library
does not schedule background I/O or evict retained arrays. This builds on the
[OpenUSD binary layer format workflow](https://openusd.org/release/tut_converting_between_layer_formats.html).

## Package export

| Command | What it writes |
| --- | --- |
| `cargo run -p layerstack_examples --example mesh_to_usdz -- cube.usdz [arkit\|generic]` | A cube with normals and UVs |
| `cargo run -p layerstack_examples --example mesh_materials_to_usdz -- cube.usdz [arkit\|generic]` | A cube with textured and constant `UsdPreviewSurface` face materials |
| `cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit\|generic]` | Material-bearing prototypes and point instances |

The default ARKit / AR Quick Look profile uses a USDC root and exports scatter
instances as references. The generic profile uses a USDA root and a
`PointInstancer`. Affine placements include negative scales for mirrors.
