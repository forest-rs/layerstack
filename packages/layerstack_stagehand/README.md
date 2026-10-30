# @forest-rs/layerstack_stagehand

Render-ready mesh buffers from a [Layerstack](https://github.com/forest-rs/layerstack)
stage, in the browser or Node: triangles as Hydra fans them (concave faces
ear-clipped), vertices split for face-varying normals, `st` and
`displayColor`, authored or smooth normals, one draw range per
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
from the stage's change records.

```ts
const list = stagehand.drawList(stage, { time, purposes: ["default", "proxy"] });
try {
  upload(list.refresh()); // every item and buffer added
  stage.onChange((changes) => upload(list.applyChanges(changes)));
  upload(list.setTime(next)); // animated meshes refilled, nothing triangulated again
} finally {
  list.dispose(); // frees every buffer's plan
}
```

Each `DrawDiff` lists the items `added`, `removed`, moved (`matrices`),
shown or hidden (`visibility`), rebound to `materials` or another buffer
(`rebound`), and the buffers added, `refilled` (same indices, new
positions, normals, `st` or colors), `rebuilt` or `freed`; its `work`
counts the items read and left untouched, the matrices computed, the
local transforms the stage recomputed, and the buffers reused, refilled,
triangulated and tessellated. Matrices are 16 numbers per instance, row by
row, as OpenUSD's.

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
false; `report.subdivisionScheme` says what was not applied). Cameras,
lights, materials (beyond their bindings), curves and points are not
covered yet.

Errors are `StagehandError`s with a stable `code` (`argument.invalid`,
`plan.mismatch`, `mesh.disposed`, `module.load`, `module.abi`) and a
message `${operation}: ${message}`.

## Building

`npm run build:wasm` builds the module (`wasm-bindgen` at the version
`Cargo.lock` holds, and `wasm-opt`), `npm run size` checks it against
`size-budget.json`, `npm run build` the wrapper, `npm test` the tests
(after building `../layerstack`).
