# Layerstack editor

A scene editor over [`@forest-rs/layerstack`](../layerstack) and
[`@forest-rs/layerstack_stagehand`](../layerstack_stagehand): USD stages
composed, inspected, explained and edited in the browser, drawn by three.js
from stagehand's buffers. No USD loader is involved; every value, matrix and
triangle comes from the stage.

```sh
npm ci
npm run dev      # http://localhost:5173/
npm run build    # type-check and bundle into dist/
npm test         # the model's tests, in Node
```

Build the two packages first (`npm run build:wasm && npm run build` in each):
the editor links them from `../layerstack` and `../layerstack_stagehand`.

## Requirements

A browser with WebGPU: the viewport and the material previews draw with
three.js's `WebGPURenderer` and nothing else (no WebGL fallback). As of
late 2026 that is Chrome and Edge (113 or later on Windows, macOS and
ChromeOS; 144 or later on Linux, Intel Gen12+ GPUs only; 121 or later on
Android), Safari 26 or later, and Firefox 141 or later on Windows and 147
or later on Apple silicon Macs. WebGPU is offered only to secure pages
(HTTPS, or `localhost`). Without it, the editor says so and why, and loads
nothing more.

## What it shows

- **Toolbar.** Open a scene: the sample, a folder on this computer (picked
  with `showDirectoryPicker`; its layers two levels deep are offered as the
  root), files or a folder dropped anywhere, or, in development, a local
  scene directory (below). Where edits go (the stage's edit layer, another
  layer of its stack, or an opinion's edit target chosen in the explain
  panel), undo and redo, and what the stage holds: its layers, the draws,
  the bytes fetched and the module's memory.
- **Prims.** The composed hierarchy, read as rows open (a prim's children
  are asked for only when it is expanded) and drawn by a virtualized React
  Aria tree, so a stage of tens of thousands of prims scrolls as one of ten.
  Find a prim by name. A prim picked in the viewport opens its ancestors.
  Each row's "⋯" (and the inspector's) adds a child of any concrete type
  the stage registers (`Stage.schemas()`, grouped by what the types are:
  transforms, geometry, lights, cameras, shading, physics), renames
  (`Stage.move`, which repairs the paths that name it) or deletes, each one
  undoable action where edits go.
- **Layers.** Every layer the stage holds, edited ones marked, the root
  stack's marked; a switch mutes or unmutes each (the stage's state, not an
  edit).
- **Cameras.** The free camera and the stage's own; choosing one looks
  through it, its view and projection as `GfFrustum` computes them.
- **Opening.** While a scene opens, the viewport says what the open is
  doing: fetching the root layer, reading it (its size), and each round of
  the asset loop fetching its assets and composing them. Each phase is
  painted before work that holds the page begins, so a large root layer's
  read is announced, not a frozen page. Below that are what has arrived
  and the time taken, and **Cancel** keeps the scene the editor had. Once
  the scene is drawn, a corner note counts the textures still loading and
  the decodes in flight. Textures are decoded and scaled in a few workers,
  off the page's thread, and the draws waiting on them are restyled a
  budgeted batch a frame as they arrive, so the viewport keeps drawing.
- **Draw scope and asset view.** The viewport's **Scene | Selection |
  Nothing** switch chooses what is drawn: the whole scene, the selection
  alone, or nothing (the prims, inspector, layers, shading and asset loop
  all work without draws). Either is drawn under the draw budget.
  Drawing the selection is the asset view: the selected prim's subtree in
  its own space, framed at the origin (a draw list scope), with what it
  is: a mesh's faces, points, triangles, vertices, primvars, subsets and
  what stagehand made of it; a subtree's draws, instances and bytes. A
  face subset draws its mesh with the subset highlighted and the rest
  ghosted; a point instancer its instances; a prototype inside one that
  prototype alone; a `UsdUVTexture` (or an asset-valued property chosen
  in the inspector) its image, with its size, channels, bits, color space
  and the material reading it; a material its shader ball. Clicking a
  part of what is drawn selects and draws that part.
- **Draw budget.** The viewport draws at most 1 GiB uploaded and 50
  million triangles drawn (instanced draws counted in full) unless the
  viewer raises it: the **Budget** switch offers 2 GiB, 4 GiB (bytes and
  triangles raised together) and **Unlimited**, and the choice is
  remembered for the viewer. The draw list weighs each item from counts
  before building it, gives every item a box of its extent, and then
  draws in full the items worth most per share of the budget (bound size
  times instances, over their share of bytes or triangles) while it
  holds, so any scene opens drawing, however large, and one a little over
  boxes only a few of its costliest items. A note says how many items were
  simplified, and the Draw cost tab's **Simplified** list names each:
  drawn as a box or skipped, the limits it would pass, what it asked
  for. Selecting a prim admits it first.
- **Payloads.** The **Open** menu chooses how the next scene opens its
  payloads: **Load all**, **Load none**, or (the default) load them unless
  over 1 GiB of layers. That automatic open composes the scene with no
  payload loaded, then loads them all with the asset loop weighing each
  round before fetching it, by the sizes the source reports (`HEAD`, or a
  file's size); a round that would take the payloads' layers past 1 GiB
  is not fetched, every payload is unloaded again, and the prim tree says
  why. The limit is the scene, not the draws: composing takes more memory
  than its layers' bytes (a large public scene's 3.1 GB of layers
  composes to 5.4 GB natively) and the module has 4 GiB, so a scene over
  it would not fit with its payloads loaded; the layers the rounds before
  it read stay held. In the prim tree a prim with payloads shows
  **loaded** or **unloaded**; pressing it loads them with those beneath it,
  or unloads them, as usdview's tree does, and the **⋯** menu of any prim
  loads its payloads (with or without those beneath) or unloads them. A
  load runs the asset loop for what it newly wants, with the opening
  overlay showing its rounds, then draws what it brought within the draw
  scope: the scope is decided from the estimate of what the open left
  loaded, so a scene opened with its payloads unloaded draws the scene,
  and drawing the selection draws a loaded prim's subtree alone.
- **Material view.** With a material selected (or a shader inside one),
  the viewport's **Scene | Material** switch shows the material on a
  shader ball (a ball with a raised band at its equator, on a pedestal and
  floor, built from three.js primitives), large enough to judge it; drag
  turns it. It is the default when the scene draws nothing, as a material
  library's does.
- **Viewport.** The draw list stagehand keeps from the stage's change
  records, applied to three.js in place and drawn with its
  `WebGPURenderer`; the stage's `upAxis` orients the
  free camera and the grid. Click to pick, drag to orbit, scroll to zoom,
  **F** to frame the selection, **Esc** to clear it.
- **Timeline.** The stage's `startTimeCode` to `endTimeCode`: scrub, step
  a frame (← →), play at `timeCodesPerSecond` (Space). The draw list moves
  to the time, rereading only what varies; the inspector and the explain
  panel read values there. A stage without a range is read at the default
  time.
- **Skinning.** A mesh a UsdSkel skeleton binds is skinned on the GPU: its
  joint influences (up to eight per vertex) are gathered into the draw
  list's buffer once, through stagehand's `vertexPoints`, and a time change
  uploads only the joint matrices (`skinningTransforms`), placed by the
  skeleton's world transform as UsdSkel places skinned points. A mesh with
  blend shapes skins its blend-shaped rest pose (`blendShapedPoints`,
  `blendShapedNormals`), refilled only when the animation's weights vary.
  Meshes with dual-quaternion skinning are deformed on the CPU
  (`deformedPoints`) and refilled each time instead; the stats say how many
  of each, and any point weighting more than eight joints. Normals a mesh
  does not author are its (blend-shaped) rest pose's, skinned, not computed
  again from the skinned points.
- **Draw modes.** A model whose `model:drawMode` applies is drawn by its
  stand-in, as Hydra draws it: axes for `origin`, its box's edges for
  `bounds`, its card textures on quads for `cards` (cut out at their alpha,
  in `model:drawModeColor` until or unless they load); a model's cards are
  one draw, its six textures in one material, as Hydra draws them. The
  inspector's **Draw mode** section, on prims with `GeomModelAPI`, picks a
  prim's mode (inherited, itself, origin, bounds or cards) and whether it
  applies it; a mode on an assembly reaches the components beneath. An
  asset-structure scene with card textures and no geometry draws once its
  set's model draws as cards.
- **Inspector.** The selected prim as the stage composes it: its type,
  specifier and kind; active and instanceable; its transform as
  translation, rotation, scale and pivot, each field authoring its one op as
  `UsdGeomXformCommonAPI` does (ops that are not a common stack shown as the
  stage factors them, read-only); its variant sets; its
  properties (those layers author, then, when shown, those only its schemas
  define, at their fallbacks), nested by namespace (`primvars:`, `xformOp:`,
  `inputs:`, `collection:lightLink:`; a namespace of one stays inline) and
  labelled by `displayName` where the schema gives one, each name's tooltip
  giving its full name, type, variability, source, `displayGroup` and
  `documentation`; its bound material; the arcs it composes from. Each
  section is a panel presentation of one of the prim's types
  ([Presentations](#presentations)), and each editor is the value's
  `accept` presentation, chosen by what the stage knows of the property
  (an animated one, marked with a key, is edited by a sample at the
  current time): a token the schema gives `allowedTokens` is a picker of
  them, a `bool` a switch, a number or tuple fields that commit on Enter,
  a colour (`color3f`, `color4d`, and their arrays) a swatch painted in
  sRGB from its linear value, with a picker. A colour array is a strip of
  its first elements (`primvars:displayOpacity` as their alpha) and its
  interpolation; one of one element (a constant `displayColor`) is edited
  by the picker. A gprim that authors a `color` no schema defines, and no
  `displayColor`, says that renderers draw `displayColor`. Paths shown
  anywhere (relationship targets, connections, the changes panel) and
  layers (the explain and history panels) are live: a click selects or
  explains, a context menu offers what else applies. Nothing has to be
  typed that the stage already knows.
- **Meshes.** The inspector's view of a `Mesh`: the mesh alone in a
  preview (drag to orbit, scroll to zoom, framed on its extent), what
  stagehand made of it (faces, points, triangles, vertices, concave faces
  and how many were ear-clipped, holes, degenerate faces, normals authored
  or computed), its subdivision scheme (drawn as its cage), orientation
  and `doubleSided`, its primvars with type, interpolation and element
  size, and its face subsets with their family and face count; hovering a
  subset highlights its triangles in the preview, the rest ghosted. The
  preview can draw a checker on `st`, the triangles' edges and the vertex
  normals. For a mesh the viewport draws, nothing is triangulated again:
  the facts are its draw list buffers' report and the preview draws the
  geometry the viewport uploaded (`Viewport.sharedGeometry`, the same GPU
  buffers on the shared renderer). A mesh the viewport does not draw
  (over the budget, out of the draw scope, or with Nothing drawn), and
  the edges and normals (read on the CPU, where the viewport released its
  arrays once uploaded), are built for this mesh alone with stagehand.
- **Point instancers.** The inspector's view of a `PointInstancer`: its
  prototypes, each with how many instances place it (the `protoIndices`
  histogram, and how many name no prototype), whether `ids` are authored,
  how many instances the mask hides (`invisibleIds`, `inactiveIds`), the
  range of the positions and scales, and a table of every instance (index,
  id, prototype, position, scale), read as typed columns once per change
  and time and virtualized, so half a million rows scroll as ten; a column
  header sorts by it (an index permutation). Choosing a row outlines the
  instance in the viewport (its prototype's bound through the instance's
  transform, composed as `ComputeInstanceTransformsAtTime` does) and shows
  it in full; double-clicking, or **Frame**, frames it; picking an instance
  in the viewport chooses its row. Each row hides its instance
  (`invisibleIds`) or deactivates it (`inactiveIds`), an edit where edits
  go.
- **Materials and shaders.** The inspector of a material or shader shows
  what it is (a shader's `info:id` and implementation; a material's
  outputs and what they read) and its inputs as their authors lay them
  out: grouped by `displayGroup`, labelled by `displayName`, ordered by
  `uiorder`; a number between `uimin` and `uimax` is a slider over its
  soft range (`uisoftmin`, `uisoftmax`) where one is given, a colour a
  swatch and picker. A connected input is a live link to its source, and
  one that reads a `UsdUVTexture` shows the image as a thumbnail with its
  file (a click shows it in the asset view), `sourceColorSpace` and wrap
  modes. A dot marks the inputs the prim sets itself, as opposed to those
  it inherits from a base material. Above them, the material (a shader's,
  its material's) on a shader ball, read again after each edit:
  `UsdPreviewSurface` as the viewport draws it, OpenPBR approximated by
  three.js's physical material (what is approximated is listed under the
  sphere); and, for a material, the prims the viewport draws with it bound
  (from the draws, not a search of the stage), each live.
- **Shading.** The network of the selected material (or of the material
  or node graph around a selected shader) as a node graph, left to right:
  the material's inputs, each shader with its inputs and outputs as rows,
  and the material's outputs last; each connection a curve from the row it
  reads to the row that reads it. Where the network's authors placed its
  nodes (`ui:nodegraph:node:pos`), the drawing follows their arrangement:
  columns in the order of the authored x, the authored y carried across
  columns at one scale; otherwise columns are by longest path to the
  outputs, and nodes in a column are placed by where their connections go
  ("Automatic layout" switches). Ports no connection touches fold away
  until shown. Opening the tab grows a short bottom panel. Drag to pan,
  scroll to zoom; clicking a node selects its prim, a port explains its
  value.
- **Draw cost.** What the draws hold, measured by the draw list
  (`DrawList.cost`): per item its instances, its buffer's triangles and
  vertices, the triangles it draws, the bytes it uploads (`f32`
  attributes, `u32` indices, 64 per instance matrix) and the JavaScript
  bytes it holds; sortable by any column, a row selecting its prim. The
  viewport's stats add the totals, each shared buffer counted once.
  **Whole scene, estimated** lists what drawing everything in full would
  cost, estimated from counts when first shown: its heaviest items a
  click from the asset view. **Simplified** lists the items the draw
  budget drew as boxes or skipped, in admission order.
- **Explain, Changes, History, Asset loop, Notices.** A property's value and
  every opinion consulted for it, strongest first, each with "Edit here";
  each recomposition and the action that caused it; the undo history; the
  asset loop; and what the stage refused, with its error code.
- **Asset loop.** Each run of the loop (the open, a variant selected, an
  undo) as a waterfall: a row per asset on one time axis, from the stage
  naming it (`unavailable()`) through the host's fetch and the wait for
  the rest of its round to the stage reading its layer (within the round's
  `loadSupplied()`, which reports each read and the composition after),
  colored by what uses it, with its bytes, prims and times on hover. A
  track above the rows shows each round's phases, composing among them,
  and the header where the time went: waiting on fetches, or in the
  stage, and of that reading and composing. Rows are virtualized, sorted
  by start, round, size, fetch or read time and filtered by name; selecting one
  shows where the asset is authored and the prims composed without it
  until it loaded. The session runs the loop itself (`unavailable`,
  `supply`, `loadSupplied`) rather than `Stage.resolve` so that every phase
  can be timed. The step-by-step log is a toggle away.

## Presentations

What the editor shows is chosen by what it shows, after CLIM's
presentations (`src/presentations/registry.ts`): every prim, value, path
and layer is presented as an object of a **presentation type**, and a
registration presents one type in the **views** it can do: `inline` (one
line: a property row, a table cell, the explain and changes panels),
`accept` (an inline editor), `panel` (an inspector section) and
`tooltip`.

- A prim's types are its schema type and that type's ancestors, most
  specific first (`Cube`, `Gprim`, `Boundable`, ..., `Typed`), then its
  applied API schemas (`Prim.appliedSchemas`: `MaterialBindingAPI`,
  `CollectionAPI:parts` then `CollectionAPI`), then `Prim`. A value's are
  its value type (`color3f[]`), its role (`color[]`), its kind (`token`,
  `asset`, `number`) and shape (`array`, `tuple`), then `value`; a token
  the schema restricts is `allowedToken` first. Paths are `primPath` or
  `propertyPath`, then `path`; a layer is `layer`.
- For `inline`, `accept` and `tooltip`, the registration of the most
  specific type that has the view wins (priority breaks a tie). Every
  `panel` that applies is shown, larger `priority` first: the built-ins
  use 100 for a prim's state, 90 its transform, 50 a view of what it is,
  0 its properties, below 0 what follows them.
- An `accept` view authors through the `accept` callback it is given: the
  host makes it one edit where edits go, undone, logged in Changes and
  History, and heard by every view as any edit is.
- What is presented stays live: a **command** registered for a type
  (`registerCommand`) is offered on every presented object of it, a click
  running the first that applies and a context menu (or Shift+F10)
  listing them: a prim path selects, frames, draws alone or selects its
  material; a property path explains; a layer takes edits or is muted; an
  asset is shown in the asset view.
- A panel keys its reads by `usePrimRevision(store, path)`, which changes
  only when a recomposition reaches its prim, so an edit elsewhere reads
  nothing of it again. A panel that throws is replaced by a line saying
  so; the others still show.

The built-ins (`src/app/presentations/builtins.ts`) register through the
same two calls a plugin makes. A panel for a schema of one's own, and a
command on prim paths:

```tsx
import { registerCommand, registerPresentation } from "./presentations/registry.ts";
import { Section } from "./app/Section.tsx";

registerPresentation({
  id: "example.label",
  kind: "prim",
  type: "SemanticsLabelsAPI",
  title: "Labels",
  priority: 50,
  panel: ({ object, context }) => (
    <Section title="Labels">{object.prim.attributeNames().filter((name) => name.startsWith("semantics:")).join(", ")}</Section>
  ),
});

registerCommand({
  id: "example.hide",
  kind: "primPath",
  type: "primPath",
  title: "Make invisible",
  applies: (object, { stage }) => stage.prim(object.path)?.isA("Imageable") ?? false,
  run: (object, { store }) => store.setValue(`${object.path}.visibility`, { type: "token", value: "invisible" }),
});
```

## Local scenes

In development the server serves a local scene directory under `/@scenes/`
(`vite.config.ts`): `LAYERSTACK_SCENES`, else `.local/scenes` beside the
repository's main checkout. **Open** lists its layers two levels deep, and
**Browse local scenes…** walks it folder by folder to open any layer (a
material library three folders down); `?scene=<path>` opens any layer
under it at load. Scenes there are never committed or
built: their licenses are their own.

## Prebuilt cluster sets

In development the server also serves `.local/cluster-cache` from the
main checkout (`LAYERSTACK_CLUSTER_CACHE` overrides it) under
`/@clusters/`: cluster level-of-detail sets built natively by
`cluster_lod --cache` (`layerstack_clusters_examples`), one file per mesh,
named by a hash of the mesh's content as the editor holds it
(`src/viewport/clusterFile.ts`). A static mesh whose
set is there is drawn on the GPU (`src/viewport/clusters.ts`): each frame
a compute pass culls its instances and clusters against the view and
against what the last frame drew in front of them (two phases over a depth
pyramid), chooses each instance's cut at a pixel error (1 px), draws
instances under 3 px across as proxies of a few of the mesh's triangles
(`src/viewport/clusterProxies.ts`) and drops those under a pixel, and the
clusters are drawn indirectly by vertex pulling, a draw per material
subset, with the mesh's own material (lights, light links, selection and
picking unchanged). The mesh's own geometry is never uploaded: its draws
wait for its set. The status line reports
what the clustered draws hold and the triangles the last cut drew. A mesh
without a set, or edited since, draws as before. The draw budget weighs a
clustered mesh by its set (every level, once), found by prim in the
stages' manifests before the mesh is built, so an element whose instances
draw far more triangles than the budget allows is still admitted.

On a production element of 594 k instances drawing 73 M triangles
(Apple silicon, headless Chrome, 1400 x 900), framed whole: the GPU's cut
and culling take 0.3 ms, the clustered draws 9 to 14 ms of GPU time for
the 1.0 M triangles they choose, against 35 ms for the full 73 M; from
four times farther, 1.8 ms (0.1 M triangles) against 21 ms. Close up, where
most instances are a few pixels across and their meshes' DAGs bottom out
at one cluster each, proxies and occlusion culling take a frame from 48 ms
to 22 ms (52 M triangles to 5.0 M; 50 ms for the full 73 M), with 1.1% of
its pixels 16 levels or more off the full render (0.4% from the cut
alone).
The CPU side is unchanged at about 12 ms a frame, three.js's per-object
updates of 2,302 bundled draws. Packing the element's sets and instances
takes 335 ms once. An architectural scene of 115 meshes (108 of several
materials, 3.7 M triangles) draws 0.6 to 1.1 M.

## Layout

- `src/model/`: no DOM and no React. `session.ts` (a stage, its undo
  history, where edits go, and logs of the asset loop and recompositions),
  `timeline.ts` (the asset loop's timings, run by run),
  `draws.ts` (the stagehand draw list kept from the change log), `sources.ts`
  (HTTP, a picked directory, dropped files).
- `src/store.ts`: the editor's state outside React; every action runs here,
  syncs the draw list, and bumps the revision components subscribe to.
- `src/viewport/`: three.js, in its own chunk: one `WebGPURenderer`
  (`gpu.ts`) shared by the viewport and the material previews, each canvas
  a target of it; node materials and TSL where the editor shades its own
  way (light links, texture channels, eight-influence skinning, picking).
- `src/app/`: the panels. `src/components/ui/`: shadcn components on React
  Aria.
