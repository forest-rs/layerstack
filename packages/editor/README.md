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
  skeleton's world transform as UsdSkel places skinned points. Meshes with
  blend shapes or dual-quaternion skinning are deformed on the CPU
  (`deformedPoints`) and refilled each time instead; the stats say how many
  of each, and any point weighting more than eight joints. Normals a mesh
  does not author are its rest pose's, skinned, not computed again from the
  skinned points.
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
- **Explain, Changes, History, Asset loop, Notices.** A property's value and
  every opinion consulted for it, strongest first, each with "Edit here";
  each recomposition and the action that caused it; the undo history; the
  asset loop's requests, fetches, supplies and reads; and what the stage
  refused, with its error code.

## Local scenes

In development the server serves a local scene directory under `/@scenes/`
(`vite.config.ts`): `LAYERSTACK_SCENES`, else `.local/scenes` beside the
repository's main checkout. **Open** lists its layers two levels deep, and
`?scene=<path>` opens one at load. Scenes there are never committed or
built: their licenses are their own.

## Layout

- `src/model/`: no DOM and no React. `session.ts` (a stage, its undo
  history, where edits go, and logs of the asset loop and recompositions),
  `draws.ts` (the stagehand draw list kept from the change log), `sources.ts`
  (HTTP, a picked directory, dropped files).
- `src/store.ts`: the editor's state outside React; every action runs here,
  syncs the draw list, and bumps the revision components subscribe to.
- `src/viewport/`: three.js, in its own chunk.
- `src/app/`: the panels. `src/components/ui/`: shadcn components on React
  Aria.
