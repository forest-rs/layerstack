# @forest-rs/layerstack

Open, resolve, inspect and explain USD stages in the browser or Node, over
[Layerstack](https://github.com/forest-rs/layerstack) compiled to WebAssembly.

```ts
import { loadLayerstack, LayerstackError } from "@forest-rs/layerstack";

const layerstack = await loadLayerstack();
using stage = layerstack.open({ identifier: "scene.usda", bytes: rootBytes });

// The stage never fetches: it lists what it wants, you fetch it.
await stage.resolve({
  async fetch(identifier) {
    const response = await fetch(new URL(identifier, baseUrl));
    return response.ok ? new Uint8Array(await response.arrayBuffer()) : null; // null: confirmed absent
  },
});

const size = stage.attribute("/World/Rock.size");
size?.get();     // { kind: "scalar", type: "double", value: 2 }
size?.explain(); // { value, source: "default", opinions: [{ layer, specPath: "/Base.size", arc: "inherits", role: "contributed", ... }] }
stage.status();  // { prims, compositionErrors, unavailable, unresolved, complete }

// Edits are plain data, applied atomically to one layer.
const stop = stage.onChange((changes) => console.log(changes.changedInfoOnly));
const receipt = stage.apply([
  { type: "definePrim", path: "/World/Moss", typeName: "Cube" },
  { type: "setValue", path: "/World/Rock.size", value: { type: "double", value: 3 } },
]);
const redo = receipt.undo(); // the stage is as it was; redo.undo() redoes

// A rename moves every layer's opinions and repairs the paths naming the prim.
stage.move("/World/Rock", "/World/Boulder"); // a receipt, like apply's

// Switching a variant may need assets the host has not supplied: resolve fetches them.
stage.setVariantSelection("/World", "shade", "dark");
await stage.resolve(source);
```

## The model

- **One instance.** `loadLayerstack()` instantiates the module once per
  JavaScript realm and returns its `Layerstack`. Pass a URL, bytes or a
  `WebAssembly.Module` to load this package's module from elsewhere, or a
  `NativeModule` (the generated glue of a module that links
  `layerstack_wasm`; its type is in `@forest-rs/layerstack/native`) to use
  that module.
- **Native extensions share the stage.** A module linking `layerstack_wasm`
  can export functions taking `&layerstack_wasm::Stage`, which reach the
  document through `Stage::document` and `Stage::document_mut`. Call one with
  `stage.withNative("domain.inspect", native => module.inspect(native))`.
  The callback borrows the stage synchronously; it must not retain, free or
  consume it, and must not return a promise. A native error it throws is a
  `LayerstackError` named for the given operation, with the code the
  extension's Rust gave it (typed `HostErrorCode`, any string: give your
  codes a family of your own, such as `domain.*`). A `LayerId` it returns
  is `stage.layer(id)` in TypeScript.
  Native changes reach `onChange` listeners before `withNative` returns,
  including when the callback throws after committing an edit; a listener's
  edits are dispatched after the current change batch. A transaction is heard
  at once; a queued control such as layer muting, or a direct store edit, once
  the extension calls `StageDocument::synchronize`. Dispatch can call the
  module again, so return owned data or a handle rather than a borrowed view
  into Wasm memory.
  It is not a transaction: committed edits remain. Native edits make no
  receipt, and a receipt whose inverse covers a spec they changed since fails
  to undo with `edit.stale`, changing nothing, so keep natively derived
  content in a layer the user's edits do not target (the session layer while
  they target the root).
  Open with `{ provenance: true }` (read back as `stage.provenance`) when an
  extension needs the winning source layer, spec path and field in native
  resolved values. It is off by default, costs no measurable time or memory,
  and changes none of this package's reads; `attribute.explain()` works
  either way.
  Open with `{ schemas: [{ name: "RockMesh", base: "Mesh" }] }` to register
  typed schemas derived from registered ones, as codeless schemas declare
  them: a `RockMesh` prim is a `Mesh` to every `IsA` read, with `Mesh`'s
  properties. `prim.isA("Mesh")` (`UsdPrim::IsA`) is how a reader asks
  what a prim is, rather than comparing `prim.typeName`;
  `prim.hasAPI("ShapingAPI")` (`UsdPrim::HasAPI`) which API schemas it has.
- **Opening is synchronous and complete.** `open` composes the root layer
  alone. Every dependency it cannot read yet is *unavailable*: it may still
  arrive. One confirmed absent, or whose read failed, is *unresolved*: its
  arc contributes nothing (`status().unresolved`, with the failure's code).
- **Loading is a loop you can see.** `unavailable()` lists
  `UnavailableAsset`s (`identifier`, the `assetPath` as authored, the
  `anchor` layer, what it is `usedBy`: a sublayer, reference or payload,
  and the `prims` composed without it meanwhile, so you can tell which
  prims are incomplete).
  `supply(identifier, bytes | null)` answers one; `loadSupplied()` reads the
  answers, composes, and may make more unavailable. It reports where its
  work went: each layer read (`reads`: bytes, prims, and its time on
  `performance.now()`'s timeline, its own apart from the reads nested in
  it) and each composition after them (`compositions`). `readSupplied()`
  reads them without composing (its result has `reads` and no
  `compositions`): the layers read list what they need themselves, so a
  host fetches round after round and calls `loadSupplied()` once at the
  end, rather than recomposing every prim each round reaches further into.
  `resolve(source)` runs that loop for you, fetching each round's
  identifiers at once. A USDZ is one buffer: its
  members resolve inside it, and what it references outside is unavailable
  like anything else. Identifiers are the authored path anchored to the anchor layer's
  directory (`./a.usda` from `set/scene.usda` is `set/a.usda`); map them to
  URLs however you like. The module keeps supplied bytes only until the
  stage has read a layer from them, unless the layer keeps views of them
  (a USDZ does), so a text layer's source is freed once parsed; an asset
  wanted again after that (another layer of the same file, under other
  format arguments) is unavailable again, and `resolve` fetches it again.
- **Value clips load per query.** Clips are not arcs: `unavailable()` never
  lists them, and a stage reads only the clips a query needs.
  `prepareClipQuery(path, { time? })` plans what the attribute needs at
  `time` (without one, what settles whether it might vary with time), reads
  what was supplied, and returns a `ClipPreparation`: its `demands` (each
  with the `reason` the query needs it and its `status`), the `unavailable`
  clips to `supply`, `failures`, the clip sets' `issues`, and whether it is
  `complete`. `resolveClipQuery(path, source, { time? })` runs that loop as
  `resolve` does. Until a clip set's metadata is loaded, whether the
  attribute might vary is unknown and answered `true`.
- **Reads are snapshots, queries are live.** `stage.prim(path)`,
  `stage.attribute(path)` and `stage.relationship(path)` return a `Prim`,
  `Attribute` or `Relationship` whose fields are read once; their methods
  (`get`, `explain`, `children`, `attributeNames`, `relationshipNames`,
  `variantSets` with each set's variant names, `targets`, `metadata`)
  query the stage now.
- **The stage describes itself.** `stage.metadata(key)` reads its
  metadata as `UsdStage::GetMetadata` does, the session layer over the
  root, else the field's fallback: `upAxis` (`Y`), `metersPerUnit`
  (`0.01`), `startTimeCode`, `endTimeCode`, `timeCodesPerSecond`.
  `stage.schemas()` lists the schemas it was composed with, sorted by name:
  each one's `kind` (`concreteTyped`, the types a `definePrim` may name;
  `abstractTyped`; `singleApplyApi` or `multipleApplyApi`, what
  `apiSchemas` may list), a typed schema's `bases` nearest first, and an
  applied schema's `canOnlyApplyTo` and `allowedInstanceNames`.
- **Paths are text**: `/World/Rock` for prims, `/World/Rock.size` for
  properties.
- **Values say their shape.** `Value` is a union on `kind`: `scalar`
  (numbers, `bigint` for `int64`/`uint64`, strings for text types), `tuple`
  (vectors, row-major matrices, quaternions as `[i, j, k, r]`), `array`,
  `dictionary`, `list`, or `opaque`. `type` is the value type name, roles
  included (`point3f[]`).
- **Arrays stay in the module.** A numeric array value's `array` is an
  `ArrayHandle`: the stage's own buffer, pinned, with its layout (`scalar`,
  `components`, `length`, `byteLength`). `view()` is a typed array over it
  with no copy (`Float32Array` for `point3f[]`, half floats as raw bits in a
  `Uint16Array`); **a view is valid until the next call into the module**,
  which may grow its memory and detach the view (its `byteLength` becomes
  0), so call `view()` again rather than keeping one, and never write to it.
  `copy()` is one `slice` of the view, yours to keep. A handle is a
  snapshot that later edits do not change, and it outlives its stage;
  `release()` it (or `using`) when done. Sent back in an edit, a handle is
  shared, not copied. Text arrays (`token[]`, `string[]`) are copied out
  once as strings. Rust never builds arrays element by element.
- **Layer ids are `bigint`** and may exceed 2^53; you rarely need them.
  `stage.layer(id)` is the `Layer` of an id native code returns (the same
  object every time, for any layer the stage holds; `layer.unknown`
  otherwise, as for a layer muted before the stage read it, which OpenUSD
  never opens either: `mutedIdentifiers()` lists it, and once unmuted and
  read its id names the layer). A `Layer` belongs to its stage: passing it to another stage
  fails with `layer.affinity`.
- **Muting is stage state.** `stage.muteLayer(layer)` recomposes the stage
  without a layer's opinions and sublayers wherever it reaches it (a
  sublayer, the session layer, a referenced layer), as OpenUSD's
  `UsdStage::MuteLayer` does, and `unmuteLayer(layer)` brings them back;
  each returns whether the muting changed. A layer is a `Layer` or an
  identifier, anchored to the root layer as OpenUSD anchors one
  (`muteLayer("./lamp.usda")`), which may name a layer the stage has not
  read: it is then not read, and an arc to it, now or added later,
  composes muted (`composition.muted_asset`) instead of being asked for.
  `muteAndUnmuteLayers(mute, unmute)` does both as one request, heard as
  one change (`UsdStage::MuteAndUnmuteLayers`); an unmute spelled as a
  mute is dropped, as in OpenUSD. `mutedLayers()` lists the muted layers
  the stage holds, `mutedIdentifiers()` every muted identifier, read or
  not (`UsdStage::GetMutedLayers`), and `isMuted(layer)` asks about one.
  The root cannot be muted (`mute.root_layer`, refusing the whole
  request). Muting is not an edit: it makes no receipt and undo neither
  reverts it nor is blocked by it. A muted layer keeps its content and
  unsaved edits; edits targeting it fail with `layer.muted`, `save()`
  skips it until it is unmuted (`save(layer)` writes it), and `move` and
  `delete` neither read nor edit it, as OpenUSD's `UsdNamespaceEditor`
  does: its specs stay at the old path.
- **Payloads load by rule.** `open(root, { load: "none" })` composes a
  scene without its payloads, as OpenUSD's `UsdStage::Open(...,
  UsdStage::LoadNone)` does, and asks for none of their assets: a large
  scene opens with only what it shows by default. `findLoadable(root)`
  lists the prims with payloads (`UsdStage::FindLoadable`), and each
  `Prim` says whether it is `loaded` and `hasAuthoredPayloads`.
  `stage.load(path)` loads a prim's payloads and those beneath it
  (`"withoutDescendants"` for its own only), `unload(path)` unloads them,
  and `loadAndUnload(load, unload)` does both as one request
  (`UsdStage::Load`, `Unload`, `LoadAndUnload`); each recomposes at once,
  heard as one change, and returns `{ changed, refused }`: whether the
  rules changed, which, as in OpenUSD, a request already holding does
  not, and the paths it skipped, each with why (`inactive`,
  `not_on_stage`, `prototype`), applying the rest as OpenUSD does. A load
  wants the assets its payloads name: `await stage.resolve(source)`
  fetches them.
  `loadRules()` and `setLoadRules(rules)` read and replace the literal
  rules (`UsdStageLoadRules`), and `loadSet()` lists the prims whose
  payloads are included (`UsdStage::GetLoadSet`). Load rules
  are not edits: they make no receipt, and undo neither reverts them nor
  is blocked by them.
- **Everything native is disposable.** A `Stage` owns native memory: call
  `free()` or declare it with `using`. Afterwards every call fails with
  `stage.disposed`.
- **Errors carry a stable dotted `code`.** Every failure is a
  `LayerstackError` with `code`, `operation` (the method), and `path` or
  `identifier` when it is about one. Composition errors are not thrown;
  `status().compositionErrors` lists them with `composition.*` codes.
- **Edits are plain data.** `stage.apply(edits, target?)` applies an `Edit[]`
  (`definePrim`, `removePrim`, `removeProperty`, `createAttribute`,
  `setValue`, `clearValue`, `blockValue`, `setTargets`, `addReference`,
  `addPayload`, `removeReference`, `removePayload`, `setVariantSelection`,
  `setActive`, `setInstanceable`, `setKind`, `setMetadata`, `clearMetadata`,
  the target layer's `setLayerMetadata` and `clearLayerMetadata`
  (`documentation`, `metersPerUnit`, `startTimeCode`, ...), and the
  schema-shaped `bindMaterial`, `unbindMaterial`, `setShaderInput`,
  `connectToSource`, `disconnectSource`, `setBaseMaterial`,
  `createGeomSubset`, `createMaterialSubset`, `setSubsetFamilyType`,
  `bindCoordSys`, `blockCoordSys`, `clearCoordSys`, `setSdrMetadata`)
  through one edit target atomically: all of them or none. Each is named after the
  OpenUSD call whose semantics it has (`UsdStage::DefinePrim`,
  `UsdAttribute::Set`, `UsdShadeMaterialBindingAPI::Bind`, ...). A failure
  names the edit (`error.edit`, its index) and changes nothing. A schema-shaped
  edit sees the prims, types and API schemas earlier edits of the same
  `apply` define and apply, so a material, its shaders, their connections
  and its bindings can be one undoable step.
- **Values to author say their type.** A `ValueInput` is `{ type, value }`
  in the layout values are read in (`{ type: "color3f", value: [1, 0, 0] }`,
  `{ type: "point3f[]", value: new Float32Array(...) }`, a `bigint` for an
  exact `int64`), or a `Value` read from a stage, sent back as it is (an
  `ArrayHandle` without a copy). A dictionary (`customData`, `assetInfo`)
  is `{ type: "dictionary", value: { key: ValueInput, ... } }` (or a
  `Map`), nested dictionaries included; it is authored whole and ordered by
  key.
- **Namespace edits move what names the object.** `stage.move(path, to,
  target?)` renames or reparents a prim or property and `stage.delete(path,
  target?)` deletes one, as OpenUSD's `UsdNamespaceEditor` does: every
  opinion of the target's layer stack moves (or goes) together, a prim's
  variant sets and the specs inside its variants with it, and the
  relationship targets, connections, internal arcs and path expressions
  that name it are repaired, so one call may edit several layers. As
  OpenUSD, only the specs a composed prim uses are repaired: the paths in
  a variant no prim selects keep naming the old location, and
  `receipt.unrepaired` lists each (`layer`, `spec`, `field`, `path`), so
  a later variant switch that would compose a dangling target or arc is
  caught at the edit. Each is atomic and returns a receipt; one that
  cannot be made throws a `namespace.*` code and changes nothing. A
  `removePrim` edit, by contrast, removes one spec from one layer.
- **Undo is a receipt.** `apply` (and `move`, `delete`) returns an `EditReceipt` holding the
  native inverse: `receipt.undo()` restores exactly what the edits changed
  and returns the receipt that redoes them. An undo whose slots another
  edit changed since fails with `edit.stale` and changes nothing. The app
  keeps the receipts it wants; `release()` (or `using`) the rest.
  `stage.undo([a, b, c])` undoes several receipts as one atomic
  transaction, and `UndoHistory` keeps them as undo and redo steps
  ([Undo history](#undo-history)).
- **Edits go to an explicit layer.** `stage.editLayer` is the default
  target: the root, or for a stage opened with `{ editLayer: "session" }`
  an anonymous session layer in front of it, or with `{ sessionLayer:
  { identifier, bytes } }` that session layer, read as OpenUSD's
  `UsdStage::Open(rootLayer, sessionLayer)` reads one: its sublayers are
  wanted like any dependency and `save(layer)` writes it back. A USDZ package is read-only:
  open it with `editLayer: "session"` to edit it, and the package stays as
  read. Edits to a read-only layer fail with `layer.read_only`. A layer of
  the stage's own stack is edited at the offset the stack reads it at
  (`stage.editTargetForLayer(layer)`, OpenUSD's
  `GetEditTargetForLocalLayer`): a sample set at stage time 15 in a
  sublayer authored with `(offset = 10)` lands at its time 5.
- **Edits can go through composition arcs.** An `EditTarget` (OpenUSD's
  `UsdEditTarget`) says where edits are written and how stage paths and
  times map there. Take one from where an opinion lives
  (`attribute.explain().opinions[i].editTarget`), from any arc of a prim
  (`prim.compositionArcs()[i].editTarget(layer?)`), or inside a variant
  (`prim.variantEditTarget(set, variant)`), and pass it as `apply`'s
  target. Edits still name stage paths and stage times (a variant target
  is the exception for times, below):

  ```ts
  const height = stage.attribute("/World/Robot.height")!;
  const where = height.explain()!.opinions[0]!.editTarget!; // robot.usda, through the reference
  stage.apply([{ type: "setValue", path: height.path, value: { type: "double", value: 1.5 } }], where);
  // robot.usda now authors /Robot.height = 1.5, for every prim that references it
  stage.isDirty(where.layer); // true; stage.save() writes it
  ```

  `target.mapPath(path)` and `target.mapTime(time)` say where an edit
  lands, and `target.composed()` whether the stage reads it now (false for
  an unselected variant). A path the target does not map is rejected
  (`edit.rejected.unmappable`), as is a relationship target outside it
  (`edit.rejected.unmappable_target`); an internal arc's root prim path is
  authored as given, as OpenUSD does. Targets are values, equal ones the
  same object; one survives edits and recomposition, and fails with
  `target.stale` once no arc maps it (the reference was removed, the prim
  renamed). A `variantEditTarget` is OpenUSD's `ForLocalDirectVariant`,
  with no layer offset: in a sublayer authored with `(offset = 10)` it
  writes a sample set at stage time 15 at the sublayer's time 15, read at
  25, as OpenUSD does (in the root layer this changes nothing). To edit a
  selected variant in stage time there, use its arc from
  `prim.compositionArcs()` or an opinion's `editTarget`, which map times
  through the offset. Editing an opinion that a stronger one shadows changes nothing
  shown; check `opinion.role` first. A package member is read-only
  (`target.layer.readOnly`): author an override through
  `stage.editTargetForLayer(stage.editLayer)` instead. `move` and `delete`
  edit the stage's own layer stack, so a target through an arc or a variant
  fails with `namespace.unmappable`. A receipt names its `target` and the
  `layers` it wrote.
- **Changes are heard synchronously.** `stage.onChange(listener)` hears
  `{ created, removed, resynced, changedInfoOnly }` (prim paths) and
  `resyncedProperties` (property paths a spec was added to or removed from,
  as OpenUSD resyncs them) for every
  recomposition, in order, before the call that caused it (`apply`, `undo`,
  `muteLayer`, `loadSupplied`, `resolve` once its rounds are read) returns, with the
  `mutedLayers` and `unmutedLayers` whose muting changed, and their
  `mutedIdentifiers` and `unmutedIdentifiers` (as OpenUSD's
  `UsdNotice::LayerMutingChanged`), natively muted ones included. A prim
  created or removed is resynced alone: its parent is not reported, as
  OpenUSD does not report it. A listener that throws
  is reported as `listener.failed` and the others still run.
- **Saving hands you the bytes.** The module writes nothing anywhere.
  `stage.save()` (or `save(layer)`) writes each dirty layer the stage uses
  in its own format and returns `written: { layer, identifier, bytes }[]`
  for you to persist; the layers are clean afterwards, and the saved bytes
  are what the stage reads those identifiers from from then on. The session
  layer is anonymous, so it is never saved (`anonymous` lists it); a package
  member fails with `io.read_only`. A dirty layer the stage mutes or no
  longer composes (an asset edited through a reference since removed) is
  left dirty and listed in `skipped`; `save(layer)` writes it. Exports copy and change nothing:
  `exportLayer(layer, "usda" | "usdc")` returns one layer's bytes (the
  session layer too), `exportFlattened(format)` the composed stage as one
  layer with no arcs and its asset paths anchored to the layers authoring
  them, as OpenUSD flattens (`lossless`, and `lost` for anything a layer
  cannot hold; `./x.png` in `set/a.usda` is written `./set/x.png`,
  relative to the host's root, where the flattened layer belongs), and
  `exportPackage()` a USDZ of the root and everything it
  depends on, unsaved edits included (`bytes` is `null`, with `unresolved`
  saying why, while a dependency is not supplied; the session layer is
  `omitted`).
- **Transfer is measured.** `layerstack.transfer()` reports the bytes copied
  in (sources) and out (array copies, saved and exported bytes), the views
  built and the bytes they exposed without a copy, and the module's memory.

## Undo history

`UndoHistory` keeps a stage's receipts as an undo stack and a redo
stack of labelled steps, for an editor's undo menu. It applies nothing
itself: apply as usual and `record` the receipt, which the history takes
(the receipt reads as released after).

```ts
import { UndoHistory } from "@forest-rs/layerstack";

using history = new UndoHistory(stage, { limit: 100 });
history.record("set size", stage.apply([setSize]));
history.group("define and set radius", () => {
  history.record("define", stage.apply([{ type: "definePrim", path: "/World/Ball", typeName: "Sphere" }]));
  history.record("radius", stage.apply([{ type: "setValue", path: "/World/Ball.radius", value: { type: "double", value: 2 } }]));
});
history.undoSteps.map((step) => step.label); // ["set size", "define and set radius"]
history.undo(); // the ball and its radius, as one transaction
history.redo();
```

- **Groups.** What is recorded inside `group(label, fn)`, or between
  `beginGroup(label)` and `endGroup()` (a drag that spans events), is one
  step that undoes as one atomic transaction. Groups nest and flatten
  into the outermost, whose label names the step. If `fn` throws, what it
  recorded is undone and its error is thrown; `abandonGroup()` does the
  same for an open group. A group that recorded nothing, or a receipt that
  wrote no layer, adds no step.
- **Stale steps.** An edit the history did not record that changed a slot
  a step restores makes the step stale: `undo()` throws `history.stale`
  (its `cause` is the `edit.stale` error), changes nothing and keeps the
  step. The history never skips a step; `reset()` forgets them all without
  undoing anything.
- **The bound.** `limit` keeps at most that many steps, undo and redo
  together; recording past it drops the oldest undo step, which
  `record(...).dropped` returns and `history.dropped` counts.
- **Unwinding.** `mark()` notes where the history stands;
  `unwindTo(mark)` undoes every step since as one transaction and forgets
  them, for a cancelled tool. `history.mark_gone` says the mark's step was
  undone, dropped or reset.
- **For a UI.** `canUndo` and `canRedo` (false while a group is open),
  `undoSteps` and `redoSteps` (each `{ label, layers, receipts }`, the next
  step last), `nextUndo`, `nextRedo`, `openGroup` and `groupDepth`.

The history rests on the receipts' own guarantees: an inverse restores
exactly the slots its edits wrote, all or nothing, and the inverse of an
undo redoes. OpenUSD has no counterpart (`usd-core` records no inverses),
so the tests check those invariants: random edits recorded alone and in
groups undo and redo to byte-identical layers.

## Transforms, visibility, primvars, bounds and instances

What a renderer reads to place, show and frame geometry, as OpenUSD's
`UsdGeomXformCache`, `UsdGeomImageable`, `UsdGeomPrimvarsAPI`,
`UsdGeomBBoxCache` and `UsdGeomPointInstancer` compute it (through
`layerstack_schemas`). Batched reads take many paths and return one
JS-owned typed array.

| Read | Returns |
|---|---|
| `stage.worldTransforms(paths, { time? })`, `parentToWorldTransforms(...)`, `prim.worldTransform({ time? })` | One `Float64Array`, 16 numbers per prim, row by row (row-vector `GfMatrix4d`: translation in the last row; read as column-major, the matrix WebGL takes) |
| `stage.localTransform(path, { time? })`, `prim.localTransform(...)` | `LocalTransform`: `matrix`, `resetsXformStack`, `problems` |
| `stage.worldTransformsMightVary(paths, { time? })` | `boolean[]`: whether each world transform might differ at another time (an op of its chain, up to a reset, has samples, a spline or clips): what a renderer skips when the time changes |
| `stage.valuesMightBeTimeVarying(paths)`, `attribute.mightBeTimeVarying()` | `boolean[]` (one `boolean`): whether each attribute's value might vary with time: several samples, a spline, or clips that may supply several (`UsdAttribute::ValueMightBeTimeVarying`); `true` until `resolveClipQuery(path, source)` loads the clips that settle it |
| `stage.valueSourcesMightBeTimeVarying(paths)`, `attribute.valueSourceMightBeTimeVarying()` | `boolean[]` (one `boolean`): whether each attribute's value source might vary with time, the default time included (`UsdAttributeQuery::ValueSourceMightBeTimeVarying`) |
| `stage.xformable(path)`, `prim.xformable()` | `Xformable \| null`: `resetsXformStack`, `ops`, `transformMightBeTimeVarying`, `transformTimeSamples` |
| `xformable.commonTransform(time?)` | `CommonTransform`: `translation`, `rotation` (degrees, in `rotationOrder`), `scale`, `pivot`, `source` (`ops`, `accumulated`, `decomposed`) and `compatible`, as `UsdGeomXformCommonAPI::GetXformVectorsByAccumulation` reads them |
| `stage.camera(path, { time?, conform? })` | `Camera \| null`: a `UsdGeomCamera` as a renderer reads it: `view` and `projectionMatrix` as `GfFrustum` computes them, `cameraToWorld`, `window`, `clippingRange`, lens (`aperture`, `apertureOffset`, `focalLength`, `fStop`, `focusDistance`), `clippingPlanes`, `shutter`, `exposure` and `linearExposureScale`; with `conform: { policy, aspect }` fitted to a viewport as `CameraUtilConformWindow` fits it |
| `stage.visibilities(paths, { time?, purpose? })`, `prim.visibility(...)` | `inherited` or `invisible` (`ComputeVisibility`), or with a purpose `visible` among them (`ComputeEffectiveVisibility`); `null` for a prim that is not `Imageable` |
| `stage.purposeInfos(paths)`, `prim.purposeInfo()` | `PurposeInfo \| null`: `purpose`, `inheritable`, `authoredOn` |
| `stage.primvars(path, { inherited? })`, `stage.primvar(path, name, { inherited? })`, `prim.primvars(...)`, `prim.primvar(...)` | `Primvar`: `prim`, `name`, `typeName`, `interpolation`, `elementSize`, `indexed`, `idTarget`, `mightBeTimeVarying`; `get(time?)`, `flattened(time?)` (through its indices), `indices(time?)` |
| `stage.worldBounds(paths, options)`, `localBounds`, `untransformedBounds` | `BoundingBoxes`: `ranges` (6 per prim), `matrices` (16), `alignedRanges` (6), with `options` `{ time?, purposes?, useExtentsHint?, ignoreVisibility? }` |
| `stage.extentsHint(path, options)` | `Float64Array`: the `extentsHint` OpenUSD computes, 6 numbers per purpose |
| `stage.pointInstances(path, { time?, baseTime?, includePrototypeTransform?, applyMask? })` | `PointInstances`: `prototypes`, `matrices` (16 per instance), `prototypeIndices`, `ids`, `indices` |
| `stage.instanceMask(path, { time? })` | `Uint8Array`: 1 per drawn instance, 0 per inactive or invisible one; empty when all are drawn |
| `stage.transformCache()`, `stage.boundsCache()` | `TransformCacheStats`, `BoundsCacheStats`: what the retained caches computed, reused, invalidated and hold |

**A renderer drawing a frame.** At load it lists the prims to draw, and
for each its `primvars({ inherited: true })` (a `displayColor` from an
ancestor included) and bound material. Each frame it makes one
`stage.worldTransforms(paths, { time })` call and uploads the
`Float64Array` slices as model matrices, one `stage.visibilities(paths,
{ time })` and `purposeInfos(paths)` to skip what is hidden or of another
purpose, and frames the camera on `stage.worldBounds([root]).alignedRanges`.
A point instancer's instances come from `stage.pointInstances(path, { time })`,
in the instancer's space: each matrix times the instancer's world
transform. The stage keeps one native transform cache and a bounds cache
per set of bounds options (at most four), which hear the stage's edits
before each read and invalidate only what they touch: a frame after an
edit recomputes the edited prim's transform and its descendants' world
transforms, and nothing else. `stage.transformCache()` and
`stage.boundsCache()` report what each frame computed and reused.

Transforms are authored as `UsdGeomXformCommonAPI` authors them:
`setTranslate`, `setRotate` (with a `rotationOrder`), `setScale` and
`setPivot` each author one component of the common stack (translation,
pivot, rotation, scale, inverse pivot), adding only its op;
`setCommonTransform` authors all four (`SetXformVectors`). Each takes a
`time`, or authors the default. A prim whose ops are not a common stack,
or whose rotation op has another order, is refused with nothing authored
(`xform.*`), where OpenUSD returns `false`.

## Materials, subsets, coordinate systems and skinning

The shading, subset, coordinate-system and skinning schemas are read on the
stage objects and authored as `Edit`s, as OpenUSD's `UsdShade`,
`UsdGeomSubset` and `UsdSkel` define them (through `layerstack_schemas`).

| Read | Returns |
|---|---|
| `prim.boundMaterial({ purpose? })`, `stage.boundMaterials(paths, { purpose? })` | `MaterialBinding \| null`: `material` (`null` when the target is no material), `target`, `relationship`, `kind` (`direct`, `collection`), `collection`, `strength`, `purpose`, `withoutBindingApi` |
| `stage.collectionMembership(path, name, paths)` | `CollectionMembership`: `key` (equal for equal memberships), `everything` (the rule map is `/` with `expandPrims` alone, as the `UsdLuxLightAPI` link fallback), `included` (per path, `IsPathIncluded`), `reads` (the collections it includes or references) and `readsAny` |
| `prim.materialSubsets({ purpose?, time? })` | `MaterialSubsets`: `familyType`, `fallback` binding, `subsets` (`path`, `indices` as an `int[]` array, `binding`), `problems` |
| `stage.material(path)` | `Material \| null`: `baseMaterial`, `surface(contexts?)` / `displacement` / `volume` (`TerminalSource`: `shader`, `context`, `sources`), `inputs()`, `outputs()`, `input(name)`, `output(name)` |
| `stage.shader(path)` | `Shader \| null`: `id`, `implementationSource`, `sdrMetadata`, `inputs()`, `outputs()`, `input(name)`, `output(name)` |
| `prim.inputs()`, `prim.outputs()`, `stage.port(path)` | `ShaderPort`: `name`, `kind`, `typeName`, `sdrMetadata`, `connections`, `invalidConnections`, `get(time?)`, `valueSources()` |
| `stage.connectionSourceInfo(path)` | `ConnectionSourceInfo`: `prim`, `name`, `kind`, `typeName`, `source`, `valid`: the source a path names, checked against the stage |
| `prim.geomSubsets({ elementType?, familyName?, time? })`, `subsetFamilyType(family)`, `validateSubsetFamily(family, elementType, { time? })` | `GeomSubset[]`, the family type, `SubsetProblem[]` |
| `prim.coordSysBindings({ inherited? })` | `CoordSysBinding[]`: `name`, `relationship`, `target` |
| `stage.skinning(path)`, `prim.skinning()` | `SkinningQuery \| null`: `skeleton`, `joints`, `jointMapping`, `elementSize`, `interpolation`, `rigid`, `method`, `animation`, `blendShapes`, `blendShapeWeightsMightBeTimeVarying`; `influences(time?)`, `interleavedInfluences(time?)`, `skinningTransforms(time?)`, `blendShapedPoints(time?)`, `blendShapedNormals(time?)`, `deformedPoints(time?)`, `deformedNormals(time?)` |
| `layerstack.blackbodyTemperatureRgb(kelvin)` | `Float32Array`: the color a light's `colorTemperature` multiplies its color by (`UsdLuxBlackbodyTemperatureAsRgb`) |
| `layerstack.skinPoints`, `skinNormals`, `skinTransform` | The skinning functions over your own typed arrays, with separate or interleaved influences |
| `stage.skinningCache()` | `SkinningCacheStats`: what the stage's skinning cache reused, recomputed, invalidated and holds |

Edits: `bindMaterial`, `unbindMaterial`, `setShaderInput`,
`connectToSource`, `disconnectSource`, `setBaseMaterial`,
`createGeomSubset`, `createMaterialSubset`, `setSubsetFamilyType`,
`bindCoordSys`, `blockCoordSys`, `clearCoordSys`, `setSdrMetadata`. They
are applied with the others through `stage.apply(edits, target?)`: one edit
target, one atomic transaction, one receipt that undoes.

**A renderer resolving materials.** For every mesh it draws, a renderer
calls `stage.boundMaterials(meshPaths)`, which resolves the binding that
decides each prim's material as `ComputeBoundMaterial` does (the prim's own
or an ancestor's, direct or through a collection, strength and purpose),
sharing the ancestors' bindings and the collections' membership across the
call. For a mesh with face subsets it calls `prim.materialSubsets()`: each
`materialBind` subset's face indices as an `Int32Array` view and its
binding, the binding of the faces in no subset, and the family's validation
problems, which never hide a subset. A binding to a path where no material
is keeps its `target` with a `null` `material`. It then walks the network:
`stage.material(path).surface(["mtlx"])` names the surface shader through
any node graphs; `stage.shader(path)` gives its `id` and `sdrMetadata`;
`shader.inputs()` gives each input's type, `sdrMetadata` and direct
connections, `input.get(time)` its own value, and `input.valueSources()`
where its value comes from: an upstream shader output to evaluate, or an
attribute whose authored value to read. An unconnected input is its own
`authoredValue` source.

**An engine skinning a mesh each frame.** `stage.skinning(meshPath)`
returns the binding once: skeleton, joints, joint mapping, element size,
interpolation and method. To skin on the GPU, the engine uploads
`interleavedInfluences()` (one `float2[]` of `[index, weight]` pairs, or
`influences()` as separate `int[]` and `float[]`) once, and each frame
`skinningTransforms(time)`, the binding-order joint palette as a
`Float64Array` view with the geometry bind transform. A mesh with blend
shapes uploads `blendShapedPoints(time)` (and `blendShapedNormals(time)`)
as its rest points: its points with the shapes applied, not skinned, which
the palette skins to exactly what the CPU deforms; read them once, and
again each frame only when `blendShapeWeightsMightBeTimeVarying`. To skin on the CPU,
`deformedPoints(time)` and `deformedNormals(time)` return the blend-shaped,
skinned arrays as `Float32Array` views, or `layerstack.skinPoints({...})`
skins the engine's own arrays with separate or interleaved influences.
Nothing is a per-element JavaScript object. The stage keeps one native
skinning cache, which hears the stage's edits before each read and
invalidates only what they touch, so a frame recomputes only what changes
with time, and an edit of the animation (or its undo) shows in the next
frame; `stage.skinningCache()` reports what each frame reused and
recomputed. Joint indices without as many weights fail with `skel.influences`;
influences that do not fit the points with `skel.deformation`.

**An editor authoring materials.** A new material is one `apply`:
`definePrim` the `Material` and its `Shader`, `setShaderInput` (the input is
created with the value's type), `connectToSource` the material's
`outputs:surface` to the shader's (a port that does not exist is created
with `typeName`; an editor taking a source path from its user first asks
`stage.connectionSourceInfo(path)`, as OpenUSD's
`UsdShadeConnectionSourceInfo(stage, sourcePath)` does, whether it names a
prim on the stage, an input or an output and an existing attribute's
type), `setSdrMetadata`, `setBaseMaterial`, then `bindMaterial`
(which applies `MaterialBindingAPI`, as `Apply(prim).Bind(material)`),
`createMaterialSubset` and a `bindMaterial` of the new subset, and
`bindCoordSys`. Schema-shaped edits see the prims, types and API schemas
earlier edits of the same `apply` define, so the material, its shader,
their connection and its bindings are one undoable step, through the
edit layer or any edit target (a reference's, to author in the asset), and
the receipt undoes all of it. A refused edit names its index
and changes nothing: `material.not_found` for a binding to a path where no
prim is (as OpenUSD, any existing prim binds, resolving no material when it
is not a `Material`), `shading.no_port` for connecting an input that does not
exist, `subset.indices` for a face past the mesh, `coord_sys.no_binding`,
`schema.cannot_edit` beneath an instance.

## Error codes

| Code | When |
|---|---|
| `module.load` | The module could not be fetched or instantiated. |
| `module.abi` | The module speaks another ABI than this package. |
| `module.trapped` | The module trapped; this instance is dead. Load a new one. |
| `stage.disposed` | The stage was disposed. |
| `layer.affinity` | A layer of another stage was passed in. |
| `argument.invalid` | An argument has the wrong type. |
| `resolve.fetch` | The asset source's `fetch` threw; `identifier` names it, `cause` is the error. |
| `resolve.aborted` | The `signal` aborted. |
| `resolve.stuck` | An asset was supplied but is still unavailable. |
| `resolve.limit` | `maxRounds` (100) rounds did not finish. |
| `target.affinity` | An edit target of another stage was passed in. |
| `target.stale` | No arc maps the edit target any more (its arc was removed, its prim renamed); `path` is the prim. |
| `target.not_in_arc` | `arc.editTarget(layer)` was given a layer outside the arc's layer stack. |
| `target.relocate_source` | `arc.editTarget()` of a `relocates` arc, whose specs never contribute. |
| `receipt.affinity` | A receipt of another stage was passed in. |
| `receipt.released` | The receipt was undone or released already. |
| `array.released` | The array handle was released. |
| `flatten.failed` | No layer id is free for the flattened layer. |
| `package.failed` | The stage has no root layer to package. |
| `array.no_memory` | The module does not expose its memory, so a handle cannot be read. |
| `listener.failed` | Reported, not thrown: a change listener threw; `cause` is its error. |
| `path.invalid` | A path is not an absolute prim path, or not a property path where one is asked for. |
| `prim.not_found` | No composed prim is at the path. |
| `layer.unknown` | The stage holds no layer with the id: it has not read that asset yet, or it mutes it and never read it. |
| `value.decode` | A retained array failed to decode when read. |
| `value.unknown_type` | A value's `type` is not a registered value type name. |
| `value.kind` | A value's scalars are of the wrong kind (strings for a `float`). |
| `value.shape` | A scalar or tuple with the wrong number of components, or an array length that is not a multiple of them. |
| `value.range` | A number that is not an exact integer of the type (past 2^53: send a `bigint`), a `bool` not 0 or 1, a `float` overflow. |
| `value.unsupported` | A value that cannot be authored yet (a `list` or `opaque` value read from a stage). |
| `layer.read_only` | The edits target a layer that cannot be saved; open with `editLayer: "session"`. |
| `layer.outside_stack` | The edits target a layer the stage reaches only through an arc: pass an opinion's `editTarget` or an arc's instead. |
| `layer.muted` | The edits target a muted layer; unmute it to edit it. |
| `mute.root_layer` | `muteLayer` or `muteAndUnmuteLayers` was given the root layer, by any spelling, which cannot be muted; nothing changed. |
| `mute.refused` | The module refused a mute request for a reason this package does not know yet. |
| `edit.stale` | An undo whose slots changed since; nothing changed and the receipt is still undoable. |
| `history.empty` | `UndoHistory.undo()` or `redo()` with no step. |
| `history.group_open` | `undo`, `redo` or `unwindTo` while a group is open; end or abandon it first. |
| `history.no_group` | `endGroup()` or `abandonGroup()` with no group open. |
| `history.mark_gone` | `unwindTo(mark)` whose step was undone, dropped by the bound or reset. |
| `history.stale` | A step whose slots an edit the history did not record changed since; nothing changed and the step stays. `cause` is the `edit.stale` error. |
| `history.rejected` | A step's undo was refused otherwise; `cause` is the native error. |
| `edit.rejected.*` | The edits were rejected and nothing changed: `edit.rejected.type_mismatch`, `edit.rejected.reserved_field`, ... |
| `namespace.*` | A `move` or `delete` cannot be made and nothing changed: `namespace.collision` (the destination is occupied), `namespace.missing_parent`, `namespace.not_found`, `namespace.cycle`, `namespace.instance_proxy`, `namespace.invalid_paths`, `namespace.unmappable`, `namespace.split_opinions`, `namespace.unsupported`. |
| `subset.*` | `subset.geometry`: a subset read or created on a prim that is not geometry of the subsets' element type (`materialSubsets()` of a prim that is not a mesh); `subset.indices` (an index outside the geometry), `subset.invalid_name`, `subset.name_taken`, `subset.family_declaration`, `subset.unrestricted_material_bind`. |
| `material.not_found` | `bindMaterial` names a material where no prim is, on the stage or defined earlier in the same edits, as OpenUSD's `Bind` refuses it; any existing prim binds. |
| `material.not_material` | `setBaseMaterial` of a prim that is not a `Material`. |
| `material.binding_name` | A collection binding name with a namespace, or a purpose that is not an identifier. |
| `shading.*` | A refused shading edit: `shading.no_port` (no input or output at the path), `shading.not_shader`, `shading.type_mismatch`, `shading.invalid_name`, `shading.invalid_attribute`. |
| `schema.cannot_edit` | A schema edit (or `definePrim`) of a prim beneath an instance or in a prototype, which OpenUSD refuses. `schema.cannot_apply`: an API schema the prim may not have. |
| `prim.not_class` | `definePrim` with specifier `class` of a defined prim that is not a class, which OpenUSD's `CreateClassPrim` refuses. |
| `coord_sys.no_binding` | `clearCoordSys` of a binding that does not exist. |
| `xform.*` | A refused common transform edit, as `UsdGeomXformCommonAPI` refuses it: `xform.incompatible_ops` (the ops are not a common stack), `xform.rotation_order` (an existing rotation op has another order), `xform.op_listed`, `xform.attribute_type`, `xform.not_xformable`. |
| `metadata.*` | `metadata.key_path` (an `sdrMetadata` key with an empty part), `metadata.field`. |
| `skel.*` | A skinning read or computation that cannot be made: `skel.unbound` (nothing skins the prim), `skel.influences` (not as many weights as joint indices, or not whole blocks), `skel.deformation` (influences that do not fit the points or name a joint past the palette), `skel.input`, `skel.array_length`, `skel.target`, `skel.topology`, `skel.primvar`, `skel.method`, `skel.singular`. |
| `primvar.*` | `primvar.not_found` (the prim declares no such primvar), `primvar.indices` (an indexed primvar without readable indices), `primvar.index` (an index past the values). |
| `bounds.*` | A bound that cannot be computed: `bounds.extent` (a boundable without a valid extent nor geometry to compute one from), `bounds.extent_provider`, `bounds.instancer`, `bounds.instancer_cycle`, `bounds.singular`. |
| `instancer.*` | `instancer.not_instancer` (the prim is not a `PointInstancer`), `instancer.length` (an array of another length than the instances), `instancer.prototype_index`, `instancer.prototypes`, `instancer.missing_attribute`, `instancer.time`, `instancer.motion`. |
| `io.*` | A read or import failed: `io.rejected` (does not parse), `io.not_found`, `io.unavailable`, ... |
| `composition.*` | In `status().compositionErrors`: `composition.unresolved_asset`, `composition.arc_cycle`, ... |

A released code keeps its spelling and meaning. A native extension's own
errors, thrown through `withNative`, carry its codes (`HostErrorCode`).

## Versions

The package and the `layerstack_wasm` crate are released together, at the
same version. They meet at an ABI number, the layout of the records the
module exchanges with this package: `layerstack.version` is `{ package,
crate, revision, abi }`, and loading a module of another ABI fails with
`module.abi`. This package speaks ABI 8; changing an export's arguments or
record layout brings a new ABI.

## Building and testing

```sh
npm install
npm run build:wasm   # cargo, wasm-bindgen (the Cargo.lock version), wasm-opt into wasm/
npm run size         # sizes, and the budget in size-budget.json
npm run build        # tsc into dist/
npm run check        # type-check the tests
npm test             # node --test over the built module
```
