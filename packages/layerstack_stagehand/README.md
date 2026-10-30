# @forest-rs/layerstack_stagehand

Render-ready mesh buffers from a [Layerstack](https://github.com/forest-rs/layerstack)
stage, in the browser or Node: triangles as Hydra fans them (concave faces
ear-clipped), vertices split for face-varying normals, `st`,
`displayColor` and the other texture coordinate sets the materials read
(`uvSets`: `st1`, `uv`, each a channel of its own), authored or smooth normals, one draw range per
`materialBind` subset, a report to check it all by, and tessellated implicit
shapes (the `_1` cylinder and capsule included), and a draw list kept up
to date from the stage's change records.

```ts
import { loadLayerstack } from "@forest-rs/layerstack";
import { loadStagehand, readMesh, readPoints, readShape } from "@forest-rs/layerstack_stagehand";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();
const stage = layerstack.open({ identifier: "scene.usda", bytes });

const input = readMesh(stage, "/World/Rock", { time }); // typed arrays, read through @forest-rs/layerstack
const mesh = stagehand.buildMesh(input!, { normals: "hydra" }); // or "faceOriented"
try {
  mesh.positions; mesh.normals; mesh.st; mesh.colors; // Float32Array | null
  mesh.indices;   // Uint32Array, three per triangle
  mesh.ranges;    // [{ subset: 0, first, count }, ..., { subset: null, first, count }]
  mesh.report;    // { triangles, vertices, concaveFaces, fallbackFaces, faceNotes, channels, subsets, ... }

  // Animation: new points, same topology, no new triangulation.
  mesh.refill(readPoints(stage, "/World/Rock", { time: next })!);
} finally {
  mesh.dispose(); // `using mesh = ...` does the same where the runtime supports it
}

const lamp = stagehand.tessellate(readShape(stage, "/World/Lamp")!, { segments: 32, rings: 16 });
```

## Draw lists

A `DrawList` is what a renderer draws, retained: instanced draws
(`items()`: a gprim drawn once, a native instancing prototype's gprim at
every instance, a point instancer's prototype at every unmasked
instance) over buffers the list owns (`buffers(key)`), kept up to date
from the stage's change records. Prims are classified by schema
(`prim.isA`), as the Rust list classifies them: a prim of a schema derived
from `Mesh` or from a shape (registered with the stage's `schemas` open
option) is drawn as one, and `readShape` reads it (`shapeSchema`).

```ts
const list = stagehand.drawList(stage, { time, purposes: ["default", "proxy"] });
try {
  upload(list.refresh()); // every item and buffer added
  stage.onChange((changes) => upload(list.applyChanges(changes)));
  upload(list.setTime(next)); // animated items read, meshes refilled, nothing triangulated again
} finally {
  list.dispose(); // frees every buffer's plan
}
```

Each `DrawDiff` lists the items `added`, `removed`, moved (`matrices`),
shown or hidden (`visibility`), rebound to `materials` or another buffer
(`rebound`), and the buffers added, `refilled` (same indices, new
positions, normals, `st` or colors), `rebuilt` or `freed`; its `work`
counts the items read and left untouched (and, for `setTime`, the items
left untouched because nothing they read is animated), the matrices
computed, the local transforms the stage recomputed, and the buffers
reused, refilled, triangulated and tessellated. `setTime` reads only the
items with time samples, splines or value clips on their geometry,
visibility or transform chain. Matrices are 16 numbers per instance, row by
row, as OpenUSD's.

The list also holds the stage's cameras and lights, by path:
`list.cameras()` (`CameraItem`: the `Camera` `stage.camera` computes, view
and projection matrices as `GfFrustum` computes them; conform it to a
viewport with `conformCamera(camera, policy, aspect)`, as
`CameraUtilConformWindow` fits it) and
`list.lights()` (`LightItem`: world `matrix`, `position` and `direction`,
`emission` with `radiance = color * blackbody * intensity * 2^exposure /
sizeFactor` in nits as `UsdLux` defines it, `geometry`, `shaping`,
`shadow` and the `lightLink` and `shadowLink` collections as data), a
`Mesh` with `MeshLightAPI` applied (`kind: "mesh"`, normalized by its
faces' world-space area) and a `GeometryLight` (its target as data).
Lights and cameras inside a native instance are listed once per instance,
by their instance proxies' paths; a light inside a point instancer's
prototype once per unmasked instance, with `instance` (the instancer and
the instance's position) and the key `${path}@${instancer}[${index}]`
(`lightKey`); `list.light(key)` finds it. A diff
lists `camerasAdded`, `camerasRemoved`, `camerasChanged` and the same for
lights (by key), and `work.camerasRead` and `work.lightsRead`: an edit reads only
the lights and cameras it reaches, and `setTime` only those whose inputs
or transform might vary. A light input connected to a node graph or
another prim's input reads what the connection leads to.

It also holds every material its items bind, by path: `list.materials()`
(`MaterialItem`: the `UsdPreviewSurface` the surface output resolves to
through `renderContexts`, then the universal context; each input a
`constant`, a `primvar` or a `texture` lookup, with the spec's fallbacks;
`textures` with their `file` anchored to the layer authoring it (`udim`
sets list their tiles with `udimTiles(file, exists)`), `wrapS`/`wrapT`,
`fallback`, `scale`, `bias`, `colorSpace` and `coordinates` (the
`UsdPrimvarReader_float2` primvar and a `UsdTransform2d`); the `alpha` mode
the opacity inputs come to, and `notes` for what could not be
represented). A diff lists `materialsAdded`, `materialsRemoved` and
`materialsChanged`, and `work.materialsRead`: a material is read again
only when a shading property of it or of a prim its network reads
changes, or an item comes to bind it, and `setTime` reads only those with
an input that might vary.

## Two modules, typed arrays between them

This package's WebAssembly module is its own (`layerstack_stagehand_wasm`)
and does not link Layerstack; `@forest-rs/layerstack` is a peer dependency
whose module stays as it is. WebAssembly modules cannot share Rust objects,
so data crosses as typed arrays: `readMesh` reads the topology
(`prim.meshTopology`), points, primvars and subsets into JS-owned typed
arrays, and `buildMesh` copies them into this module, plans the mesh, and
copies the buffers out. A native engine calls the Rust crate
(`layerstack_stagehand`) with a `Stage` directly.

Subdivision surfaces are drawn as their control cage (`report.refined` is
false; `report.subdivisionScheme` says what was not applied). Shaders other
than `UsdPreviewSurface` and its texture, primvar and transform nodes,
curves, points and light linking evaluation are not covered yet, nor
cameras inside a point instancer's prototypes.

Errors are `StagehandError`s with a stable `code` (`argument.invalid`,
`plan.mismatch`, `mesh.disposed`, `module.load`, `module.abi`) and a
message `${operation}: ${message}`.

## Building

`npm run build:wasm` builds the module (`wasm-bindgen` at the version
`Cargo.lock` holds, and `wasm-opt`), `npm run size` checks it against
`size-budget.json`, `npm run build` the wrapper, `npm test` the tests
(after building `../layerstack`).
