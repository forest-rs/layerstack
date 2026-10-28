// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { ArrayHandle, LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/** A stage whose root authors `/Mesh.points` from `points` (a copy into the module). */
function meshStage(points: Float32Array): Stage {
  const stage = layerstack.open({ identifier: "mesh.usda", bytes: text.encode('#usda 1.0\ndef Mesh "Mesh" {\n}\n') });
  stage
    .apply([
      { type: "createAttribute", path: "/Mesh.points", typeName: "point3f[]", custom: false, value: { type: "point3f[]", value: points } },
    ], stage.root)
    .release();
  return stage;
}

function handle(stage: Stage): ArrayHandle {
  const array = stage.attribute("/Mesh.points")?.array();
  assert.ok(array instanceof ArrayHandle, "a numeric array is a handle");
  return array;
}

async function failure(f: () => unknown): Promise<LayerstackError> {
  try {
    await f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("array handles", () => {
  it("view the native buffer, and the view equals a copy", () => {
    using stage = meshStage(new Float32Array([0, 1, 2, 3, 4, 5]));
    using points = handle(stage);
    const before = layerstack.transfer();
    const view = points.view();
    assert.ok(view instanceof Float32Array);
    assert.equal(view.length, 6);
    assert.equal(view.byteLength, points.byteLength);
    assert.equal(points.view(), view, "the same view until memory grows");
    const after = layerstack.transfer();
    assert.equal(after.views - before.views, 1);
    assert.equal(after.bytesViewed - before.bytesViewed, 24);
    assert.equal(after.bytesOut, before.bytesOut, "a view copies nothing");
    assert.deepEqual(Array.from(view as Float32Array), Array.from(points.copy() as Float32Array));
  });

  it("are snapshots: an edit after reading does not change them", () => {
    using stage = meshStage(new Float32Array([0, 1, 2]));
    using points = handle(stage);
    stage.apply([{ type: "setValue", path: "/Mesh.points", value: { type: "point3f[]", value: [7, 8, 9] } }], stage.root).release();
    assert.deepEqual(Array.from(points.copy() as Float32Array), [0, 1, 2]);
    using now = handle(stage);
    assert.deepEqual(Array.from(now.view() as Float32Array), [7, 8, 9]);
  });

  it("detach their views when a later call grows the module's memory", () => {
    using stage = meshStage(new Float32Array([0, 1, 2, 3, 4, 5]));
    using points = handle(stage);
    const view = points.view();
    const memory = layerstack.transfer().memoryBytes;
    // Supplying a large source grows the module's memory: views made
    // before the call are detached, as the documentation says.
    stage.supply("large.bin", new Uint8Array(memory + (64 << 20)));
    assert.ok(layerstack.transfer().memoryBytes > memory, "the memory grew");
    assert.equal(view.byteLength, 0, "the old view is detached");
    const fresh = points.view();
    assert.notEqual(fresh, view);
    assert.deepEqual(Array.from(fresh as Float32Array), [0, 1, 2, 3, 4, 5]);
  });

  it("release their pin, after which they refuse to read", async () => {
    using stage = meshStage(new Float32Array([0, 1, 2]));
    const points = handle(stage);
    assert.equal(points.released, false);
    points.release();
    points.release();
    assert.equal(points.released, true);
    assert.equal((await failure(() => points.view())).code, "array.released");
    assert.equal((await failure(() => points.copy())).code, "array.released");
    {
      using scoped = handle(stage);
      assert.equal(scoped.length, 1);
    }
  });

  it("outlive their stage", () => {
    const stage = meshStage(new Float32Array([4, 5, 6]));
    using points = handle(stage);
    stage.free();
    assert.deepEqual(Array.from(points.view() as Float32Array), [4, 5, 6]);
  });

  it("are authored back without a copy, and converted for another storage type", () => {
    using stage = meshStage(new Float32Array([0, 1, 2, 3, 4, 5]));
    using points = handle(stage);
    const value = stage.attribute("/Mesh.points")?.get();
    assert.equal(value?.kind, "array");
    const before = layerstack.transfer().bytesOut;
    stage
      .apply([
        { type: "createAttribute", path: "/Mesh.rest", typeName: "point3f[]", value: { kind: "array", type: "point3f[]", array: points } },
        { type: "createAttribute", path: "/Mesh.wide", typeName: "double3[]", value: { kind: "array", type: "double3[]", array: points } },
      ], stage.root)
      .release();
    assert.equal(layerstack.transfer().bytesOut, before, "nothing was copied out to send them");
    using rest = stage.attribute("/Mesh.rest")?.array();
    assert.deepEqual(Array.from(rest?.view() as Float32Array), [0, 1, 2, 3, 4, 5]);
    using wide = stage.attribute("/Mesh.wide")?.array();
    assert.ok(wide?.view() instanceof Float64Array);
    assert.deepEqual(Array.from(wide.view() as Float64Array), [0, 1, 2, 3, 4, 5]);
    if (value?.kind === "array") value.array.release();
  });

  it("text arrays are copied once and view their frozen strings", () => {
    using stage = layerstack.open({
      identifier: "tags.usda",
      bytes: text.encode('#usda 1.0\ndef "A" {\n    token[] tags = ["a", "b"]\n}\n'),
    });
    using tags = stage.attribute("/A.tags")?.array();
    assert.ok(tags !== null && tags !== undefined && !(tags instanceof ArrayHandle));
    assert.deepEqual(tags.view(), ["a", "b"]);
    assert.ok(Object.isFrozen(tags.view()));
    assert.deepEqual(tags.copy(), ["a", "b"]);
  });

  it("view 1M point3f without a copy, far faster than copying", (t) => {
    const count = 1_000_000;
    const source = new Float32Array(count * 3);
    for (let i = 0; i < source.length; i += 1) source[i] = i;
    using stage = meshStage(source);
    using points = handle(stage);
    assert.equal(points.length, count);
    assert.equal(points.byteLength, 12_000_000);
    const rounds = 20;
    const time = (f: () => unknown): number => {
      let best = Infinity;
      for (let i = 0; i < rounds; i += 1) {
        const start = performance.now();
        f();
        best = Math.min(best, performance.now() - start);
      }
      return best;
    };
    // Each view after the first is the cached one; time a fresh view by
    // reading a fresh handle, which builds its own.
    const read = time(() => handle(stage).release());
    const view = time(() => {
      using fresh = handle(stage);
      return fresh.view()[0];
    });
    const copy = time(() => points.copy());
    const viewed = points.view();
    assert.equal(viewed[3 * count - 1], 3 * count - 1);
    assert.deepEqual(viewed.subarray(0, 6), points.copy().subarray(0, 6));
    t.diagnostic(
      `1M point3f (12 MB), best of ${rounds}: read a handle ${read.toFixed(3)} ms, read and view ${view.toFixed(3)} ms, copy ${copy.toFixed(3)} ms`,
    );
    assert.ok(view < copy, `a view (${view} ms) is cheaper than a copy (${copy} ms)`);
  });
});
