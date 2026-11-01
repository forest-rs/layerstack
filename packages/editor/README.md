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
- **Viewport.** The draw list stagehand keeps from the stage's change
  records, applied to three.js in place; the stage's `upAxis` orients the
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
  in `model:drawModeColor` until or unless they load). The inspector's
  **Draw mode** section, on prims with `GeomModelAPI`, picks a prim's mode
  (inherited, itself, origin, bounds or cards) and whether it applies it;
  a mode on an assembly reaches the components beneath. An asset-structure
  scene with card textures and no geometry (ALab's) shows as its lab once
  `/root/alab_set01` draws as cards.
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
  editor is chosen by what the stage knows of the property (an animated
  one, marked with a key, is edited by a sample at the current time): a
  token the schema gives `allowedTokens` is a picker of them, a `bool` a
  switch, a number or tuple fields that commit on Enter, a color a swatch.
  Nothing has to be typed that the stage already knows.
- **Materials and shaders.** The inspector of a material or shader shows
  what it is (a shader's `info:id` and implementation; a material's
  outputs and what they read) and its inputs as their authors lay them
  out: grouped by `displayGroup`, labelled by `displayName`, ordered by
  `uiorder`; a number between `uimin` and `uimax` is a slider over its
  soft range (`uisoftmin`, `uisoftmax`) where one is given. A connected
  input is a link to its source. A dot marks the inputs the prim sets
  itself, as opposed to those it inherits from a base material. Above
  them, the material (a shader's, its material's) on a sphere, read again
  after each edit: `UsdPreviewSurface` as the viewport draws it, OpenPBR
  approximated by three.js's physical material (what is approximated is
  listed under the sphere).
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
- **Explain, Changes, History, Asset loop, Notices.** A property's value and
  every opinion consulted for it, strongest first, each with "Edit here";
  each recomposition and the action that caused it; the undo history; the
  asset loop; and what the stage refused, with its error code.
- **Asset loop.** Each run of the loop (the open, a variant selected, an
  undo) as a waterfall: a row per asset on one time axis, from the stage
  naming it (`unavailable()`) through the host's fetch and the wait for
  the rest of its round to the stage reading the round (`loadSupplied()`),
  colored by what uses it, with its bytes and times on hover. A track
  above the rows shows each round's phases, and the header where the time
  went: waiting on fetches, or in the stage. Rows are virtualized, sorted
  by start, round, size or fetch time and filtered by name; selecting one
  shows where the asset is authored and the prims composed without it
  until it loaded. The session runs the loop itself (`unavailable`,
  `supply`, `loadSupplied`) rather than `Stage.resolve` so that every phase
  can be timed. The step-by-step log is a toggle away.

## Local scenes

In development the server serves a local scene directory under `/@scenes/`
(`vite.config.ts`): `LAYERSTACK_SCENES`, else `.local/scenes` beside the
repository's main checkout. **Open** lists its layers two levels deep, and
**Browse local scenes…** walks it folder by folder to open any layer (a
material library three folders down); `?scene=<path>` opens any layer
under it at load. Scenes there are never committed or
built: their licenses are their own.

## Layout

- `src/model/`: no DOM and no React. `session.ts` (a stage, its undo
  history, where edits go, and logs of the asset loop and recompositions),
  `timeline.ts` (the asset loop's timings, run by run),
  `draws.ts` (the stagehand draw list kept from the change log), `sources.ts`
  (HTTP, a picked directory, dropped files).
- `src/store.ts`: the editor's state outside React; every action runs here,
  syncs the draw list, and bumps the revision components subscribe to.
- `src/viewport/`: three.js, in its own chunk.
- `src/app/`: the panels. `src/components/ui/`: shadcn components on React
  Aria.
