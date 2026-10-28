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
stage.status();  // { prims, compositionErrors, wanted, missing, complete }

// Edits are plain data, applied atomically to one layer.
const stop = stage.onChange((changes) => console.log(changes.changedInfoOnly));
const receipt = stage.apply(stage.root, [
  { type: "definePrim", path: "/World/Moss", typeName: "Cube" },
  { type: "setValue", path: "/World/Rock.size", value: { type: "double", value: 3 } },
]);
const redo = receipt.undo(); // the stage is as it was; redo.undo() redoes

// Switching a variant may want assets: resolve fetches them.
stage.setVariantSelection("/World", "shade", "dark");
await stage.resolve(source);
```

## The model

- **One instance.** `loadLayerstack()` instantiates the module once per
  JavaScript realm and returns its `Layerstack`. Pass a URL, bytes or a
  `WebAssembly.Module` to load this package's module from elsewhere, or a
  `NativeModule` (the generated glue of a module that links
  `layerstack_wasm`) to use that module.
- **Opening is synchronous and complete.** `open` composes the root layer
  alone. Every dependency it cannot read yet is *wanted*, not missing.
- **Loading is a loop you can see.** `wanted()` lists `AssetNeed`s
  (`identifier`, the asset as authored, the `anchor` layer and the arc).
  `supply(identifier, bytes | null)` answers one; `loadSupplied()` reads the
  answers and may make more wanted. `resolve(source)` runs the loop for you,
  fetching each round's identifiers at once. A USDZ is one buffer: its members
  resolve inside it, and what it references outside is wanted like anything
  else. Identifiers are the authored path anchored to the anchor layer's
  directory (`./a.usda` from `set/scene.usda` is `set/a.usda`); map them to
  URLs however you like.
- **Reads are snapshots, queries are live.** `stage.prim(path)`,
  `stage.attribute(path)` and `stage.relationship(path)` return a `Prim`,
  `Attribute` or `Relationship` whose fields are read once; their methods
  (`get`, `explain`, `children`, `attributeNames`, `variantSets` with each
  set's variant names, `targets`, `metadata`) query the stage now.
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
- **Layer ids are `bigint`** and may exceed 2^53; you rarely need them. A
  `Layer` belongs to its stage: passing it to another stage fails with
  `layer.affinity`.
- **Everything native is disposable.** A `Stage` owns native memory: call
  `free()` or declare it with `using`. Afterwards every call fails with
  `stage.disposed`.
- **Errors carry a stable dotted `code`.** Every failure is a
  `LayerstackError` with `code`, `operation` (the method), and `path` or
  `identifier` when it is about one. Composition errors are not thrown;
  `status().compositionErrors` lists them with `composition.*` codes.
- **Edits are plain data.** `stage.apply(layer, edits)` applies an `Edit[]`
  (`definePrim`, `removePrim`, `removeProperty`, `createAttribute`,
  `setValue`, `clearValue`, `blockValue`, `setTargets`, `addReference`,
  `addPayload`, `removeReference`, `removePayload`, `setVariantSelection`,
  `setActive`, `setInstanceable`, `setKind`, `setMetadata`, `clearMetadata`)
  to one layer atomically: all of them or none. Each is named after the
  OpenUSD call whose semantics it has (`UsdStage::DefinePrim`,
  `UsdAttribute::Set`, ...). A failure names the edit (`error.edit`, its
  index) and changes nothing.
- **Values to author say their type.** A `ValueInput` is `{ type, value }`
  in the layout values are read in (`{ type: "color3f", value: [1, 0, 0] }`,
  `{ type: "point3f[]", value: new Float32Array(...) }`, a `bigint` for an
  exact `int64`), or a `Value` read from a stage, sent back as it is (an
  `ArrayHandle` without a copy). A dictionary (`customData`, `assetInfo`)
  is `{ type: "dictionary", value: { key: ValueInput, ... } }` (or a
  `Map`), nested dictionaries included; it is authored whole and ordered by
  key.
- **Undo is a receipt.** `apply` returns an `EditReceipt` holding the
  native inverse: `receipt.undo()` restores exactly what the edits changed
  and returns the receipt that redoes them. An undo whose slots another
  edit changed since fails with `edit.stale` and changes nothing. The app
  keeps the receipts it wants; `release()` (or `using`) the rest.
- **Edits go to an explicit layer.** `stage.editLayer` is the default
  target: the root, or for a stage opened with `{ editLayer: "session" }`
  an anonymous session layer in front of it. A USDZ package is read-only:
  open it with `editLayer: "session"` to edit it, and the package stays as
  read. Edits to a read-only layer fail with `layer.read_only`.
- **Changes are heard synchronously.** `stage.onChange(listener)` hears
  `{ created, removed, resynced, changedInfoOnly }` (prim paths) for every
  recomposition, in order, before the call that caused it (`apply`, `undo`,
  `loadSupplied`, each round of `resolve`) returns. A listener that throws
  is reported as `listener.failed` and the others still run.
- **Saving hands you the bytes.** The module writes nothing anywhere.
  `stage.save()` (or `save(layer)`) writes each dirty layer the stage uses
  in its own format and returns `written: { layer, identifier, bytes }[]`
  for you to persist; the layers are clean afterwards, and the saved bytes
  are what the stage reads those identifiers from from then on. The session
  layer is anonymous, so it is never saved (`anonymous` lists it); a package
  member fails with `io.read_only`. Exports copy and change nothing:
  `exportLayer(layer, "usda" | "usdc")` returns one layer's bytes (the
  session layer too), `exportFlattened(format)` the composed stage as one
  layer with no arcs (`lossless`, and `lost` for anything a layer cannot
  hold), and `exportPackage()` a USDZ of the root and everything it
  depends on, unsaved edits included (`bytes` is `null`, with `unresolved`
  saying why, while a dependency is not supplied; the session layer is
  `omitted`).
- **Transfer is measured.** `layerstack.transfer()` reports the bytes copied
  in (sources) and out (array copies, saved and exported bytes), the views
  built and the bytes they exposed without a copy, and the module's memory.

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
| `resolve.stuck` | An asset was supplied but is still wanted. |
| `resolve.limit` | `maxRounds` (100) rounds did not finish. |
| `receipt.affinity` | A receipt of another stage was passed in. |
| `receipt.released` | The receipt was undone or released already. |
| `array.released` | The array handle was released. |
| `flatten.failed`, `package.failed` | The stage has no root layer to flatten or package. |
| `array.no_memory` | The module does not expose its memory, so a handle cannot be read. |
| `listener.failed` | Reported, not thrown: a change listener threw; `cause` is its error. |
| `path.invalid` | A path is not an absolute prim path, or not a property path where one is asked for. |
| `prim.not_found` | No composed prim is at the path. |
| `layer.unknown` | The stage has no layer with the id. |
| `value.decode` | A retained array failed to decode when read. |
| `value.unknown_type` | A value's `type` is not a registered value type name. |
| `value.kind` | A value's scalars are of the wrong kind (strings for a `float`). |
| `value.shape` | A scalar or tuple with the wrong number of components, or an array length that is not a multiple of them. |
| `value.range` | A number that is not an exact integer of the type (past 2^53: send a `bigint`), a `bool` not 0 or 1, a `float` overflow. |
| `value.unsupported` | A value that cannot be authored yet (a `list` or `opaque` value read from a stage). |
| `layer.read_only` | The edits target a layer that cannot be saved; open with `editLayer: "session"`. |
| `layer.outside_stack` | The edits target a layer the stage reaches only through an arc. |
| `edit.stale` | An undo whose slots changed since; nothing changed and the receipt is still undoable. |
| `edit.rejected.*` | The edits were rejected and nothing changed: `edit.rejected.type_mismatch`, `edit.rejected.reserved_field`, ... |
| `io.*` | A read or import failed: `io.rejected` (does not parse), `io.not_found`, `io.unavailable`, ... |
| `composition.*` | In `status().compositionErrors`: `composition.unresolved_asset`, `composition.arc_cycle`, ... |

A released code keeps its spelling and meaning.

## Versions

The package and the `layerstack_wasm` crate are versioned independently. They
meet at an ABI number: `layerstack.version` is `{ package, crate, revision,
abi }`, and loading a module of another ABI fails with `module.abi`.

## Building and testing

```sh
npm install
npm run build:wasm   # cargo, wasm-bindgen (the Cargo.lock version), wasm-opt into wasm/
npm run size         # sizes, and the budget in size-budget.json
npm run build        # tsc into dist/
npm run check        # type-check the tests
npm test             # node --test over the built module
```
