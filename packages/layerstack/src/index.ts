// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/**
 * `@forest-rs/layerstack`: open, resolve, inspect and explain USD stages in
 * the browser or Node, over Layerstack compiled to WebAssembly.
 *
 * ```ts
 * const layerstack = await loadLayerstack();
 * using stage = layerstack.open({ identifier: "scene.usda", bytes });
 * await stage.resolve({ fetch: async (id) => new Uint8Array(await (await fetch(id)).arrayBuffer()) });
 * const size = stage.attribute("/World/Rock.size");
 * size?.get();     // { kind: "scalar", type: "double", value: 2 }
 * size?.explain(); // the opinions, strongest first, with layer, arc and role
 *
 * const receipt = stage.apply([
 *   { type: "setValue", path: "/World/Rock.size", value: { type: "double", value: 3 } },
 * ]);
 * receipt.undo(); // returns the receipt that redoes it
 *
 * const { written } = stage.save(); // [{ layer, identifier, bytes }]: persist them
 * const usdz = stage.exportPackage().bytes;
 * ```
 *
 * @module
 */

import init, { EditBatch as GlueEditBatch, Stage as GlueStage, nativeVersion } from "../wasm/layerstack.js";
import { LayerstackError } from "./errors.js";
import { Layerstack } from "./stage.js";
import type { NativeModule } from "./stage.js";

export { ArrayCopy, ArrayHandle } from "./arrays.js";
export { LayerstackError } from "./errors.js";
export type { ErrorCode, ErrorDetails, HostErrorCode } from "./errors.js";
export {
  ABI,
  Attribute,
  CompositionArc,
  EditReceipt,
  EditTarget,
  Layer,
  Layerstack,
  PACKAGE_VERSION,
  Prim,
  Relationship,
  Stage,
} from "./stage.js";
export type * from "./types.js";

let own: Promise<Layerstack> | undefined;

/**
 * Loads the native module and returns its {@link Layerstack}.
 *
 * - No argument: this package's module (`wasm/layerstack_bg.wasm` beside
 *   it), fetched in a browser and read from disk in Node.
 * - A `URL`, string, `BufferSource` or `WebAssembly.Module`: this package's
 *   glue over that module.
 * - A `NativeModule` (`@forest-rs/layerstack/native`): an initialized
 *   module built by another crate that links `layerstack_wasm`.
 *
 * This package's module is instantiated once per JavaScript realm: every
 * call without a {@link NativeModule} returns the same instance.
 *
 * Throws `module.load` when the module cannot be read or instantiated,
 * `module.abi` when it speaks another ABI.
 */
export function loadLayerstack(
  source?: URL | string | BufferSource | WebAssembly.Module | NativeModule,
): Promise<Layerstack> {
  if (isNativeModule(source)) {
    return Promise.resolve(new Layerstack(source));
  }
  own ??= instantiate(source);
  return own;
}

async function instantiate(source: URL | string | BufferSource | WebAssembly.Module | undefined): Promise<Layerstack> {
  let input: URL | string | BufferSource | WebAssembly.Module = source ?? new URL("../wasm/layerstack_bg.wasm", import.meta.url);
  let memory: WebAssembly.Memory;
  try {
    if (input instanceof URL && input.protocol === "file:") {
      input = await readModuleFile(input);
    }
    ({ memory } = await init({ module_or_path: input }));
  } catch (error) {
    own = undefined;
    throw new LayerstackError("module.load", "loadLayerstack", `could not instantiate the module: ${String(error)}`, {
      cause: error,
    });
  }
  return new Layerstack({ Stage: GlueStage, EditBatch: GlueEditBatch, nativeVersion, memory });
}

/**
 * Reads the module from disk, in Node, where `fetch` does not take `file:`
 * URLs. The built-in is reached through `process.getBuiltinModule`, not an
 * `import`, so a bundler building for the browser has nothing to resolve.
 */
async function readModuleFile(url: URL): Promise<Uint8Array> {
  const fs = globalThis.process?.getBuiltinModule?.("fs/promises");
  if (fs === undefined) throw new Error(`cannot read ${url.href}: no filesystem in this runtime`);
  return fs.readFile(url);
}

function isNativeModule(source: unknown): source is NativeModule {
  return (
    typeof source === "object" &&
    source !== null &&
    typeof (source as NativeModule).nativeVersion === "function" &&
    typeof (source as NativeModule).Stage?.open === "function"
  );
}
