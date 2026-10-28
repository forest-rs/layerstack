# @layerstack/stage

Open, resolve, inspect and explain USD stages in the browser or Node, over
[Layerstack](https://github.com/forest-rs/layerstack) compiled to WebAssembly.

```ts
import { loadLayerstack, LayerstackError } from "@layerstack/stage";

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
- **Reads are snapshots, queries are live.** `stage.prim(path)` and
  `stage.attribute(path)` return a `Prim` or `Attribute` whose fields are read
  once; their methods (`get`, `explain`, `children`, `attributeNames`,
  `variantSets`) query the stage now.
- **Paths are text**: `/World/Rock` for prims, `/World/Rock.size` for
  properties.
- **Values say their shape.** `Value` is a union on `kind`: `scalar`
  (numbers, `bigint` for `int64`/`uint64`, strings for text types), `tuple`
  (vectors, row-major matrices, quaternions as `[i, j, k, r]`), `array`,
  `dictionary`, `list`, or `opaque`. `type` is the value type name, roles
  included (`point3f[]`).
- **Arrays are one buffer.** An array value's `array` has its layout
  (`scalar`, `components`, `length`, `byteLength`) and `copy()`, which returns
  a new typed array (`Float32Array` for `point3f[]`, half floats as raw bits
  in a `Uint16Array`). Rust never builds arrays element by element.
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
- **Transfer is measured.** `layerstack.transfer()` reports the bytes copied
  in (sources) and out (arrays), and the module's memory.

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
| `path.invalid` | A path is not an absolute prim path, or not a property path where one is asked for. |
| `prim.not_found` | No composed prim is at the path. |
| `layer.unknown` | The stage has no layer with the id. |
| `value.decode` | A retained array failed to decode when read. |
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
