# Layerstack TypeScript client

This private package connects to a host-owned native Stage through an injected
transport. It contains no USD composition, scene tree or Wasm runtime. A host can
provide a standalone Layerstack module, a combined application module or a
service. One combined module can attach geometry providers to the same Stage.

```ts
import { Stage } from "@layerstack/client";

const stage = await Stage.connect({ request: json => nativeStage.request(json) });
const info = await stage.inspect();
const edit = await stage.transaction(info.layers[0], [
  { type: "setAttribute", path: "/World/Part", name: "size",
    value: { type: "double3", value: [120, 80, 12] } },
]);
const part = await stage.query("/World/Part", ["size"]);
await stage.undo(edit.receipt);
```

The version-one protocol carries explicit stage/store affinity and an explicit
edit layer. Paths are owned spellings; IDs and 64-bit integers use decimal
strings. Layer handles and inverse receipts are owned by their connected client;
foreign handles are refused before transport dispatch. Endpoints must assign a
unique backend realm, even across separate Wasm module instances. The endpoint
lowers typed operations into native atomic transactions,
retains inverse receipts, resolves composed queries and owns mute controls.
Await operations to preserve edit order. Failed requests carry a machine-readable
code and context. Native endpoints may declare a bounded value vocabulary and
refuse unsupported encodings explicitly.

Source edits and procedural synchronization remain separate. Exedra extensions
use their own protocol namespace on the same transport; generic Stage clients do
not evaluate or mesh geometry. `dispose()` releases the application stage. It is
idempotent, and subsequent calls fail without reaching the transport. Hosts own
transport/runtime disposal after all attached providers and views are released.

This unreleased package exports TypeScript source for modern bundlers and Node
with TypeScript stripping. It has no runtime dependencies.
