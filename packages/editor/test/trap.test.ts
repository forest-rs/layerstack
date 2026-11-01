// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which errors mean a Wasm module trapped, so the editor stops calling it.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError } from "@forest-rs/layerstack";
import { moduleTrap } from "../src/store.ts";

describe("moduleTrap", () => {
  it("knows the package's trap codes, and which one is out of memory", () => {
    const oom = new LayerstackError("module.out_of_memory", "readSupplied", "the module ran out of memory with 4089 MiB of memory");
    assert.deepEqual(moduleTrap(oom), { outOfMemory: true, message: oom.message, raw: false });
    const panic = new LayerstackError("module.trapped", "apply", "the module trapped: unreachable");
    assert.deepEqual(moduleTrap(panic), { outOfMemory: false, message: panic.message, raw: false });
  });

  it("finds a trap among an error's causes, and a raw trap from another module", () => {
    const trap = new WebAssembly.RuntimeError("unreachable");
    assert.deepEqual(moduleTrap(new Error("drawing failed", { cause: trap })), { outOfMemory: false, message: "unreachable", raw: true });
  });

  it("leaves other errors to the caller", () => {
    assert.equal(moduleTrap(new LayerstackError("io.not_found", "open", "no such layer")), null);
    assert.equal(moduleTrap(new Error("fetch failed")), null);
    assert.equal(moduleTrap("not an error"), null);
  });
});
