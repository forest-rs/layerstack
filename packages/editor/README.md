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
  transforms, geometry, lights, cameras, shading, physics) as a `def`,
  `over` or `class`, duplicates it, moves it under another prim or up and
  down among its siblings, binds a material, renames (`Stage.move`, which
  repairs the paths that name it) or deletes, each one undoable action
  where edits go. A row dragged onto another moves its prim there, and
  between two of its siblings reorders it (`reorder nameChildren`). An
  imageable prim's eye, shown on hover and always when it is hidden, makes
  it invisible or visible again; one an ancestor hides shows it dimmed,
  and pressing it selects that ancestor. A purpose other than `default`
  is a badge. Visibility is read for every row at once, when the rows or
  the paused time change, not per row nor while playing.
- **Saving and exporting.** The **File** menu counts the layers with
  unsaved edits, and the page asks before it is left while there are any.
  A layer whose edits are undone back to where it was saved or read no
  longer counts, though the stage still reads it dirty, as OpenUSD does.
  **Save** (⌘S) lists every edited layer and what a save does with it,
  then, on one confirmation, writes each to the identifier it was read
  from (`Stage.save`): back into the folder it was opened from when the
  person allows the page to write there (asked at that moment), else as
  downloads. A layer exports as `.usda` or `.usdc` (`Stage.exportLayer`;
  also its own commands, wherever a layer is shown), the composed stage
  as one flattened layer (`Stage.exportFlattened`; a flatten that loses
  something lists what before it is downloaded), or as a `.usdz` package
  (`Stage.exportPackage`), for which the editor fetches what the stage
  never read (a variant's layers, textures, every tile of a UDIM set) and
  lists what it still could not. The **Open** menu can put the next
  scene's edits in a session layer in front of its root; a `.usdz` root,
  which is read-only, is always edited so, and the edit target says why.
  A save leaves the session layer, which is never part of the root: it is
  kept by exporting it, and a package lists it as left out.
- **Layers.** The stage's own layer stack as a tree in strength order,
  stronger above weaker: the session layer, then the root, each layer's
  sublayers beneath it in the order it lists them, numbered, each with
  the asset path and offset its parent authors (`Stage.sublayers`). A
  sublayer the stage could not read is marked missing. On a layer of the
  stack that takes edits, **Add a sublayer** picks a layer the source
  holds (or a typed path), a position among its sublayers and an offset,
  and shows the `subLayers` it authors; each sublayer moves stronger or
  weaker (`setSublayers`), takes a new offset, or is removed. Each is one
  undoable edit of that layer, whatever layer other edits go to, after
  which the asset loop reads what it names. Below the stack, the layers
  reached through references and payloads, then muted layers the stage
  released. A switch mutes or unmutes a layer (the stage's state, not an
  edit), and every layer is live: a click shows its text, a context menu
  offers the rest, **Reload from source** among it. **Reload** reads every
  layer again from the source (`Stage.reloadLayers`), so files changed on
  disk come in and only what changed recomposes; a layer with unsaved
  edits is left as it is, and one asked for alone is refused.
- **Stage.** The stage's metadata: up axis (the viewport turns with it),
  meters per unit (by unit name, or any length), default prim (one of the
  root prims), start and end time codes, time codes and frames per
  second, and documentation. Each says which layer holds its opinion (the
  session layer over the root; no sublayer counts), or that it is the
  field's fallback, and clears it. An edit is one undoable action in the
  root, or in the session layer when edits go there; with edits going to
  another layer, the tab says the stage's metadata is authored in the
  root instead.
- **Health.** What is wrong with the stage, counted on its tab:
  composition errors grouped by prim (each prim live, and marked in the
  prim tree), missing assets with the layer that authors each and the
  prims composed without it, assets not supplied yet, and the paths the
  last move or delete left naming the old one. Read once per change of
  the stage, never as time moves; with nothing wrong, it says what it
  checked.
- **Text.** A layer as USDA, as `Stage.exportLayer` writes it with its
  unsaved edits ("Show text" on any layer, or the edit layer from the
  File menu), read again after each edit, undo and redo. The lines the
  last change added or changed are marked, where lines went is marked,
  and the view scrolls to the change, so the opinion an edit authored is
  there to read. Lines are drawn as they scroll into view; a layer over
  4 MB of text shows its first 4 MB and says so, and one whose read is
  slow is read again on **Refresh** rather than after every edit.
- **Cameras.** The free camera and the stage's own; choosing one looks
  through it, its view and projection as `GfFrustum` computes them.
- **Opening.** While a scene opens, the viewport says what the open is
  doing: fetching the root layer, reading it (its size), and each round of
  the asset loop fetching its assets and composing them. Each phase is
  painted before work that holds the page begins, so a large root layer's
  read is announced, not a frozen page. Below that are what has arrived
  and the time taken, and **Cancel** stops it: the fetches in flight are
  aborted, nothing more is fetched or read, what the open had made is
  freed, and the editor keeps the scene it had. Once
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
  overlay showing its rounds (its **Cancel** aborts the fetches, puts the
  payloads back as they were loaded and releases the layers it read),
  then draws what it brought within the draw
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
  In the prim tree, **F** frames the focused row (a name starting with F is
  found with the Find field, or by typing a second letter within a second),
  **Delete** (or ⌘⌫) deletes it, **F2** or **Enter** renames it, and Shift+F10 opens
  its context menu.
- **Look.** A stage looks as usdview shows it by default: lit by
  usdview's default camera light (the **Camera light** switch, as View >
  Lights) and the stage's own lights as Storm approximates them, at the
  camera's exposure, and encoded as sRGB with no tone curve, so light past
  white clips (usdview's default `sRGB` colour correction). **Tone mapping:
  sRGB (usdview) | AgX** shows the viewport and the material and mesh
  previews through AgX instead, whose curve rolls highlights off, for
  scenes lit brighter than white; the choice is remembered in this
  browser, and switching rebuilds only the renderer's output pass.
- **Timeline.** The stage's `startTimeCode` to `endTimeCode`: scrub, step
  a frame (← →), play at `timeCodesPerSecond` (Space), or type a time code
  (Enter goes there, Escape cancels; one outside the range is moved into
  it). The draw list moves to the time, rereading only what varies; the
  inspector and the explain panel read values there. Under the track are
  the selected prim's keys: a tick at each time any of its attributes has
  a time sample (`unionedTimeSamples`), and, taller and in the accent
  colour, the samples of the property being explained. Clicking a tick
  goes to its key; the previous and next key buttons (`,` and `.`) jump
  between them. The times are read once per selection and composition,
  and thinned to one tick per pixel, so playing does not read them
  again. The loop button chooses whether playing starts over at the end or
  stops there; it is remembered in this browser. A stage without a range
  is read at the default time. The explain panel says where the time falls
  among the property's samples (`bracketingSamples`): on one, before the
  first, after the last, or between two, each time a link that moves the
  timeline there; the stage does not report whether it interpolates, so
  it says so only for types USD always holds (tokens, strings, integers).
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
  (one with time samples, marked with a key, hollow for a single sample,
  is edited by a sample at the current time): a token the schema gives `allowedTokens` is a picker of
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
- **Authoring prims.** The inspector's **State** retypes the prim (the
  type badge too: the Add dialog's grouped types, or none, which clears
  the edit target's `typeName`), sets its specifier and its kind (the
  model hierarchy's kinds, each with what it means: the stage exposes no
  kind registry), and switches active and instanceable, with **Clear** to
  take the edit target's opinion of either away. **Applied schemas** lists
  `Prim.appliedSchemas`, those its type builds in told from those layers
  author, each authored one removable; **Apply** lists the API schemas
  that can apply to the prim (by `canOnlyApplyTo`, grouped, with a line
  on what each is for), asks a multiple-apply schema's instance name
  (checked against `allowedInstanceNames` and those applied), and shows
  the properties the schema brought, at their fallbacks. **Material**
  shows the bound material and the binding that decides it, live; **Bind…**
  picks a `Material` (those the scene's draws bind first) with a purpose
  and strength, and **Unbind** authors an empty binding, which also blocks
  what ancestors bind. **Metadata** edits `documentation` and shows
  `customData` and `assetInfo` key by key (read, not edited: a dictionary
  composes key by key across layers). "Bind a material…", "Duplicate" and
  "Move to…" are commands on every prim path. A duplicate copies the
  prim's spec within the edit target's layer (`copySpec`); when that layer
  has none, the editor names the layers that do, with **Edit there** and
  **Duplicate there**. A move or delete that leaves paths naming the old
  place (in a variant no prim selects) lists them in a notice, each live,
  as the Health tab does for the last one. While edits go through an arc
  or inside a variant, moving (by **Move to…** or a drag) is not offered,
  as rename and delete are not, and neither is a duplicate whose copy the
  target does not map; the menu says why. Every edit says the layer it
  goes to.
- **Authoring properties.** Each property's name in the inspector is
  its path, live: a click explains it, and its context menu (or its
  "⋯", or Shift+F10), there and wherever the path is shown (the explain
  and changes panels), offers what would change something where edits
  go, each saying in which layer: clear its value (`ClearAtTime` of its
  default, and of the samples the stage reads when they are the edit
  target's, so weaker opinions show), block it (`UsdAttribute::Block`),
  key it at the current frame or delete the key there, or remove its
  spec. An edit a stronger opinion hides (or one through a variant not
  selected) is said under its field, the winning layer live, with
  **Edit there**, which makes that opinion's edit target where edits go;
  a field shows what the stage reads once its edit is made, not what was
  typed. **Add** in the Properties header adds an attribute (a name in
  namespaces; a value type from the runtime's registry, grouped by shape
  and role with the legacy types left out, scalar or array; varying or
  uniform), created with a starting value of its
  type so it is shown ready to edit, or a relationship and its targets;
  the dialog shows what will be authored, in USDA, and offers the names
  the prim's schemas define that no layer authors yet, with their type
  and fallback. A relationship's targets are edited in place: each one
  moved earlier or later or removed, and one added from the prim picker
  (a Find field over the stage's prims, narrowed to a schema where the
  caller wants one, and a chosen prim's properties), each change one
  `SetTargets` edit. Each operation is one undoable step where edits go,
  in Changes and History; one that would change nothing is refused, and
  says why.
- **Authoring composition.** The inspector's **Composition** section
  lists the arcs a prim composes from, strongest first, each arc's layer
  and spec live, and says where its edits go now (`/World/Robot → /Robot
  in assets/robot.usda` through the robot's reference). The stage does not
  say which layer authored an arc, so the editor reads it from the layers'
  text: an arc the layer edits go to authors can be removed there (its
  item leaves the list and is deleted from weaker layers', as
  `RemoveReference` does); one authored elsewhere names that layer, live,
  and offers to edit there. Any arc offers to edit through it
  (`CompositionArc.editTarget`), which the toolbar's target then names by
  its arc and layer. **Add** authors a reference or payload (the layer
  chosen from those the scene's source lists and the stage holds, offered
  as the asset path from the layer edits go to, or this layer for an
  internal arc, or a typed path; then its default prim, said when it has
  none, one of its root prims, read without composing the layer, or a
  typed path), an inherit or specialize (a prim of the stage, class prims
  first), prepended as OpenUSD does or appended, with a layer offset
  across a reference or payload; the dialog shows the USDA line and the
  spec it goes on. An arc to a layer the stage has not read runs the
  asset loop with the opening overlay, and **Cancel** takes it back; a
  missing asset is a notice and stays listed under the arcs, with the
  composition error, until it is removed. The **Variants** section edits
  inside any variant (`Prim.variantEditTarget`; it says when the variant
  is not selected, and goes back to the edit layer), adds a variant (then
  selected, with editing inside it a click away) or a variant set with its
  first variant, and removes a set from the layer edits go to when that
  layer holds it: its variants and what they author and the layer's
  selection go, and its name stays in `variantSetNames`, as OpenUSD keeps
  it, which the dialog says. After each, the layer's text is a click
  away, the lines it changed marked. Rename and delete, which edit the
  stage's own layer stack, say why they are not offered while edits go
  through an arc or inside a variant; a layer, wherever it is shown, can
  be referenced from the selected prim or added as its payload.
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
- **Curves and points.** A curves prim's view says what the draw list
  made of it, from its buffers' report: curves drawn, how they were
  evaluated (type, basis, wrap), segments and samples, what they were
  swept as, and each primvar's fate; curves swept as tubes are previewed
  as a mesh is, on the viewport's own geometry. A points prim's says what
  its points are drawn as, their widths' range, and the same of its
  primvars.
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
- **Lights, cameras, transforms, skeletons.** A light's view (anything
  with `LightAPI`) says what it emits, `intensity × 2^exposure` in its
  colour times its colour temperature's blackbody colour when enabled
  (`UsdLuxBlackbodyTemperatureAsRgb`), with those inputs' editors beside
  it. A camera's gives its projection, focal length and apertures in
  millimetres (tenths of a scene unit), the fields of view they make,
  clipping, focus and exposure, and looks through it. A transform's op
  order is listed as `GetOrderedXformOps` reads it. A skeleton's joints,
  and a skeleton animation's, are listed as their tree, with an
  animation's blend shapes.
- **Materials and shaders.** The inspector of a material or shader shows
  what it is (a shader's `info:id` and implementation; a material's
  outputs and what they read) and its inputs as their authors lay them
  out: grouped by `displayGroup`, labelled by `displayName`, ordered by
  `uiorder`; a number between `uimin` and `uimax` is a slider over its
  soft range (`uisoftmin`, `uisoftmax`) where one is given, a colour a
  swatch and picker. A shader of the `UsdPreviewSurface` family
  (`UsdPreviewSurface`, `UsdUVTexture`, the primvar readers,
  `UsdTransform2d`) also lists the inputs its definition has that the
  prim does not author, in italics with their fallback; the stage has no
  shader registry, so the editor keeps these definitions in one table.
  An edit authors through `setShaderInput`, which creates the input on
  its first edit. A connected input is a live link to its source, and
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
  until shown, all of them or a node's (its "+N"). A port's anchor says
  its type by its shape as well as its colour (a number a circle, a
  vector a diamond, a colour a square, a file a triangle), hollow for a
  port only the shader's definition has. Opening the tab grows a short
  bottom panel. Drag the background to pan, scroll to zoom; clicking a
  node selects its prim, a port explains its value.

  The network is edited in place, each edit one undoable action in the
  layer edits go to, which the line under the toolbar names with what was
  authored. Dragging from an output's row to an input's connects them
  (`ConnectToSource`, replacing what the input read); while dragging, the
  inputs that could read it are lit, amber where the types differ (USD
  connects any types; the renderer decides what it reads), and those
  that could not are dimmed, with why (a port reading itself, a
  connection already there, a cycle, named). Dropping on a node asks
  which of its inputs. A curve is selected by a click and disconnected
  with Delete (⌘⌫), or from its context menu or its input's
  ("Disconnect"). An unconnected input's row edits its value with the
  inspector's editors, creating the input on its first edit. "Add
  shader", in the toolbar or the background's context menu, adds a
  `Shader` with its `info:id` where it was asked for (with a
  `ui:nodegraph:node:pos` when the network places its nodes); a shader's
  menu deletes it, and the connections to it with it. Nodes stay where
  they were drawn across an edit. Connections that name nothing (a
  dashed stub and a cross) and cycles (a dashed red curve) are marked on
  the curve and the port, with the reason. The keyboard reaches all of
  it: the arrow keys move between nodes and their rows (left and right
  follow a row's connection), Enter selects or explains, Shift+F10 opens
  the focused row's, node's or curve's menu ("Connect to…" lists the
  ports that could connect), Delete deletes a shader or disconnects. A
  shading port's path, wherever it is shown, offers "Show in the shading
  graph" and "Disconnect what it reads".
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
presentations (`src/presentations/registry.ts`): every prim, value, path,
layer and point instancer instance is presented as an object of a
**presentation type**, and a
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
  `propertyPath`, then `path`; a layer is `layer`; an instance of a point
  instancer (its instancer, index, id and prototype) is `instance`.
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
  material, or binds one, duplicates or moves the prim where edits go; a
  property path explains (bringing the Explain tab forward), or
  clears, blocks, keys or removes it where edits go;
  a layer is shown in the Layers tab, shows its text, is exported, has
  its identifier copied, takes edits or is muted, or is referenced from
  the selected prim (or added as its payload); an
  asset is shown in the asset view; an instance (a row of its instancer's
  table, the chosen instance below it, which a viewport pick chooses) is
  selected and outlined, framed, hidden or deactivated (`invisibleIds`,
  `inactiveIds`), has its prototype selected or drawn alone, or its id,
  index or position copied. The prim tree's rows offer the same
  commands on their paths. Hovering a live object shows its `tooltip`
  presentation where its type has one: a prim path's says what is there
  (type, specifier, instance, children), a layer's whether it is muted,
  edited or read-only.
- A panel keys its reads by `usePrimRevision(store, path)`, which changes
  only when a recomposition reaches its prim, so an edit elsewhere reads
  nothing of it again. The inspector renders its panels again when the
  selection, the stage or the draws change, not as time moves: a view that
  shows a value at the time asks for it with `useTime(context.store)`
  (passing `false` when the value cannot vary), and `context.time` is the
  time whenever it is read. A panel that throws is replaced by a line
  saying so; the others still show.

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
  (HTTP, a picked directory, dropped files), `files.ts` (what a save writes,
  file names, what a package asks the host for), `layerText.ts` (a layer
  as text and the lines a change touched).
- `src/store.ts`: the editor's state outside React, as the panels read it
  and act through it (`EditorStore`). What it does is in `src/store/`, a
  concern a module: `opening.ts` (opening a scene, its phases, the payload
  budget, cancelling), `playback.ts` (the time, the range, playing),
  `selection.ts` (the selected prim, instance and property, and an
  instance's box), `view.ts` (the draw scope and budget, the camera, the
  lights and tone mapping, remembered for the viewer), `drawing.ts` (the
  draw list, its costs, and the viewport it is applied to), `notices.ts`
  (what actions reported, and the frame note), `files.ts` (the layer the
  Text tab shows, the texts read, where the next scene's edits go),
  `status.ts`, `actions.ts`
  (the queue the actions run in). The store holds the session and runs
  the actions on the stage, one at a time in the order asked: an action
  that fetches (an undo that brings back a reference, a variant, a
  payload) waits on the host, and one asked for meanwhile runs after it
  rather than into it. Each one runs,
  syncs the draw list, and bumps the signals of the concerns it changed
  (`src/store/signals.ts`: status, composition, selection, time, draws,
  view, history, target, notices, loop). A panel listens to the concerns
  it reads (`src/app/context.ts`: `useRevision`, `useStoreSelector`,
  `useTime`), so playing the timeline renders what shows the time and
  nothing else; `src/app/renders.ts` counts each panel's renders, for
  measuring that.
- `src/viewport/`: three.js, in its own chunk: one `WebGPURenderer`
  (`gpu.ts`) shared by the viewport and the material previews, each canvas
  a target of it; node materials and TSL where the editor shades its own
  way (light links, texture channels, eight-influence skinning, picking).
- `src/presentations/`: the presentation and command registries and the
  types they dispatch on, with no React or DOM (tested in Node), and the
  colour conversions swatches paint with.
- `src/app/`: the panels; `src/app/presentations/` the built-in
  presentations and commands, `src/app/views/` the views of prim types
  (mesh, point instancer, light, camera, skeleton). `src/components/ui/`:
  shadcn components on React Aria.
