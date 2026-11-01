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
(`rebound`), the instances a point instancer's items gain and lose
(`instancesAdded`, `instancesRemoved`, by item and id), and the buffers
added, `refilled` (same indices, new positions, normals, `st` or colors),
`rebuilt` or `freed`; its `work`
counts the items read and left untouched (and, for `setTime`, the items
left untouched because nothing they read is animated), the matrices
computed, the local transforms the stage recomputed, and the buffers
reused, refilled, triangulated and tessellated. `setTime` reads only the
items with time samples, splines or value clips on their geometry,
visibility or transform chain, and derives again only the point
instancers whose `protoIndices`, `ids`, `positions` or `invisibleIds` might
vary (`work.instancersRederived`): their instances, and the lights they
place, follow the time. Matrices are 16 numbers per instance, row by
row, as OpenUSD's.

The list also holds the stage's cameras and lights, by path:
`list.cameras()` (`CameraItem`: the `Camera` `stage.camera` computes, view
and projection matrices as `GfFrustum` computes them; conform it to a
viewport with `conformCamera(camera, policy, aspect)`, as
`CameraUtilConformWindow` fits it) and
`list.lights()` (`LightItem`: world `matrix`, `position` and `direction`,
`emission` with `radiance = color * blackbody * intensity * 2^exposure /
sizeFactor` in nits as `UsdLux` defines it, `geometry`, `shaping`,
`shadow` and the `lightLink` and `shadowLink` collections with their
`category`), a
`Mesh` with `MeshLightAPI` applied (`kind: "mesh"`, normalized by its
faces' world-space area) and a `GeometryLight` (its target as data).
Lights and cameras inside a native instance are listed once per instance,
by their instance proxies' paths; a light inside a point instancer's
prototype once per unmasked instance, with `instance` (the instancer and
the instance's authored `ids` entry, `id`, or its position, `index`, when
the instancer authors no `ids`) and the key `${path}@${instancer}#${id}`
or `${path}@${instancer}[${index}]` (`lightKey`); `list.light(key)` finds
it. Keyed by id, reordering or compacting an instancer's arrays changes
its lights' matrices instead of removing and adding them. A diff
lists `camerasAdded`, `camerasRemoved`, `camerasChanged` and the same for
lights (by key), and `work.camerasRead` and `work.lightsRead`: an edit reads only
the lights and cameras it reaches, and `setTime` only those whose inputs
or transform might vary. A light input connected to a node graph or
another prim's input reads what the connection leads to.

Light linking is evaluated as Hydra does it, by light-link category:
each light's `lightLink` and `shadowLink` is a membership query
(`stage.collectionMembership`: `includeRoot` with the `UsdLux` fallback,
`expansionRule`, excludes, included collections, `membershipExpression`);
equal queries share one category (`list.linkSets()`, `list.linkSet(n)`),
and the fallback, `/` with `expandPrims` alone, has none
(`lightLink.category === null`) and links everything. Each item's
`linkPaths` are what is matched (a gprim's path, each instance proxy of a
prototype's item, a point instancer's path for its instances):
`list.isLinked(lightKey, "light" | "shadow", { item, path })` answers, and
`list.linkCategories(target)` gives a target its categories. A diff lists
`linksChanged` (per light and kind, the targets linked newly or no
longer, for lights and items it neither adds nor removes) and counts
`work.linksEvaluated` (queries computed: a new light's, an edited
`collection:lightLink:*` or `collection:shadowLink:*`, or one including or
referencing an edited collection) and `work.linksClassified`: a new
target is classified once against each category, a new category against
each target. Membership has no time.

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

## Per-point data and skinning

`mesh.vertexPoints` says which of the input's points each vertex came
from (a vertex split for a face-varying normal, `st` or color still names
one point; `null` for a shape), so data per point that the buffers do not
carry can be gathered into vertex order. `gatherInfluences(query,
mesh.vertexPoints, { slots: 4, time })` does it for a `SkinningQuery`
(`stage.skinning(path)`): each vertex's joints and weights, `slots` per
vertex, for GPU linear-blend skinning with the palette
`query.skinningTransforms(time)` gives each frame (per joint, the rest
position through the bind transform, then the joint's skinning transform;
the normal through each one's inverse transpose, as
`UsdSkelSkinNormals`). The result is in the skeleton's space: draw it with
the skeleton's world transform, not the mesh's. A rigid binding's one
block is copied to every vertex. A point weighting more joints than
`slots` keeps its largest weights, scaled to the block's sum, and is
counted (`reducedBlocks`, `droppedWeight`): skinning is then approximate
there, and exact everywhere else. Blend shapes come before the palette:
build the rest buffer from `query.blendShapedPoints(time)` instead of the
authored points. Dual-quaternion skinning is not in the palette: draw it
with `query.deformedPoints`.

The Rust crate gives the same map as `MeshPlan::vertex_points` and
`DrawBuffers::vertex_points`.

## Curves and points

`readCurves(stage, path, { time })` reads a `BasisCurves`, `NurbsCurves` or
`HermiteCurves` into typed arrays, and `stagehand.buildCurves(input,
{ style, samples, sides })` lays it out by OpenUSD's segment rules (`vstep`,
wrap, pinned ends through the schema's phantom points) in the module:
`positions`, `widths`, `colors`, `opacities` per sample, `lines` (two
sample indices per segment), `curves` (each curve's samples), and with
`style: "swept"` tubes of `sides` sides, or ribbons where normals are
authored, as `swept` triangles. A NURBS curve that jumps at an interior
knot of full multiplicity is sampled on both sides of the jump, and no line
segment or sweep crosses it. `report` gives segments per curve, the
evaluation and any token drawn as linear (`fallbacks`), samples, each
primvar's fate, widths below zero drawn as 0 (`clampedWidths`), notes (a
NURBS curve not drawn says why by its `code`) and the
`UsdGeomCurves::ComputeExtent` extent.
`basisSegmentCounts`, `basisDataSizes` and `interpolationForSize` answer as
`UsdGeomBasisCurves` does. `readPointSet` and `fillPoints` give a `Points`'
per-point positions, widths, colors and `ids`; `stagehand.pointShape` the
unit sphere or disc to instance at each point.

In a draw list (`curves` and `points` options), curves and points are
items with buffers (`buffers.curves`, `buffers.points`; `buffers.mesh` is
their swept triangles or instanced shape): new points, widths or colors
refill (`work.curvesRefilled`, `work.pointsRefilled`), a new
`curveVertexCounts`, `type`, `basis` or `wrap`, or a NURBS curve's `order`,
`knots`, `ranges` or `pointWeights`, lays curves out again
(`work.curvesLaidOut`), and a new point count reads points anew
(`work.pointsRebuilt`).

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
than `UsdPreviewSurface` and its texture, primvar and transform nodes
are not covered yet, nor
cameras inside a point instancer's prototypes.

Errors are `StagehandError`s with a stable `code` (`argument.invalid`,
`plan.mismatch`, `mesh.disposed`, `module.load`, `module.abi`) and a
message `${operation}: ${message}`.

## Building

`npm run build:wasm` builds the module (`wasm-bindgen` at the version
`Cargo.lock` holds, and `wasm-opt`), `npm run size` checks it against
`size-budget.json`, `npm run build` the wrapper, `npm test` the tests
(after building `../layerstack`).
