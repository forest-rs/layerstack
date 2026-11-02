// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The clustered draws (`clusters.ts`) write changes into their storage
// buffers in place and count on three.js's own code for the rest: a
// compute pass binding a storage buffer uploads it when its version
// changed, and then only the ranges marked changed, with no draw (bundled
// or not, `fixedNodes.ts`) needing to be refreshed. These check those
// premises against three.js itself, so an upgrade that changes them fails
// here rather than leaving bundled draws reading stale buffers.
import assert from "node:assert/strict";
import { test } from "node:test";
import { StorageBufferAttribute } from "three/webgpu";

// three.js's own internals, which have no type declarations.
type Internal = new (...args: never[]) => Record<string, (...args: never[]) => unknown>;
const internal = async (path: string): Promise<Internal> => ((await import(path)) as { default: Internal }).default;
const Attributes = await internal("three/src/renderers/common/Attributes.js");
const Bindings = await internal("three/src/renderers/common/Bindings.js");
const WebGPUAttributeUtils = await internal("three/src/renderers/webgpu/utils/WebGPUAttributeUtils.js");

/** A backend that records what three.js asks of it. */
function recordingBackend() {
  const data = new WeakMap<object, Record<string, unknown>>();
  const calls: string[] = [];
  const writes: { offset: number; bytes: number }[] = [];
  const backend = {
    calls,
    writes,
    get(object: object): Record<string, unknown> {
      let entry = data.get(object);
      if (entry === undefined) data.set(object, (entry = {}));
      return entry;
    },
    getBufferAttribute: (attribute: StorageBufferAttribute) => attribute,
    createStorageAttribute: () => calls.push("create"),
    createIndirectStorageAttribute: () => calls.push("create"),
    updateAttribute: () => calls.push("update"),
    device: { queue: { writeBuffer: (_buffer: unknown, offset: number, _array: unknown, _dataOffset: number, size: number) => writes.push({ offset, bytes: 4 * size }) } },
  };
  return backend;
}

const info = { createStorageAttribute() {}, createIndirectStorageAttribute() {} };

/** `AttributeType.STORAGE`. */
const STORAGE = 3;

test("a storage buffer is uploaded again only when its version moved", () => {
  const backend = recordingBackend();
  const attributes = new Attributes(backend as never, info as never);
  const attribute = new StorageBufferAttribute(new Uint32Array(64), 4);
  attributes.update!(attribute as never, STORAGE as never);
  assert.deepEqual(backend.calls, ["create"]);
  attributes.update!(attribute as never, STORAGE as never);
  assert.deepEqual(backend.calls, ["create"], "unchanged: not uploaded");
  attribute.addUpdateRange(8, 4);
  attribute.needsUpdate = true;
  attributes.update!(attribute as never, STORAGE as never);
  assert.deepEqual(backend.calls, ["create", "update"]);
});

test("an upload writes the ranges marked and no more, then forgets them", () => {
  const backend = recordingBackend();
  const utils = new WebGPUAttributeUtils(backend as never);
  const attribute = new StorageBufferAttribute(new Uint32Array(1024), 4);
  backend.get(attribute).buffer = {};
  attribute.addUpdateRange(16, 8);
  attribute.addUpdateRange(400, 4);
  utils.updateAttribute!(attribute as never);
  assert.deepEqual(backend.writes, [
    { offset: 64, bytes: 32 },
    { offset: 1600, bytes: 16 },
  ]);
  assert.equal(attribute.updateRanges.length, 0);
});

test("a compute pass's bindings upload the storage buffers it binds", () => {
  const updated: unknown[] = [];
  const attribute = new StorageBufferAttribute(new Uint32Array(4), 4);
  const backend = { get: () => ({}), updateBindings: () => undefined };
  const nodes = { updateGroup: () => true };
  const attributes = { update: (a: unknown) => updated.push(a), get: () => ({ generation: 0 }) };
  const bindings = new Bindings(backend as never, nodes as never, {} as never, attributes as never, {} as never, {} as never);
  const binding = { isStorageBuffer: true, attribute };
  bindings._update!({ bindings: [binding] } as never, [] as never);
  assert.deepEqual(updated, [attribute]);
});
