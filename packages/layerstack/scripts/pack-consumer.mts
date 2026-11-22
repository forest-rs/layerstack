// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { NativeModule } from "@forest-rs/layerstack/native";

await import("@forest-rs/layerstack/native");
const load: (native?: NativeModule) => ReturnType<typeof loadLayerstack> = loadLayerstack;
const layerstack = await load();
const bytes = new TextEncoder().encode('#usda 1.0\ndef Xform "World" {}\n');
using stage = layerstack.open({ identifier: "smoke.usda", bytes });
assert.equal(stage.prim("/World")?.typeName, "Xform");

const main = import.meta.resolve("@forest-rs/layerstack");
for (const file of [
  "LICENSE-APACHE",
  "LICENSE-MIT",
  "LICENSE-TOST-1.0",
  "LICENSE-UNICODE",
  "LICENSE-ZLIB",
  "LICENSE-compiler-builtins",
  "LICENSE-libm",
  "NOTICE",
  "THIRD-PARTY-NOTICES",
]) {
  assert.ok(readFileSync(new URL(`../wasm/licenses/${file}`, main), "utf8").trim(), file);
}
