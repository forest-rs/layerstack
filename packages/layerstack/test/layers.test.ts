// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import { bytes, mockSource, scene } from "./fixtures.ts";

const layerstack = await loadLayerstack();

function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, String(error));
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("layers by id", () => {
  it("gives the same layer for an id, one reached through an arc included", async () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
    await stage.resolve(mockSource(scene));
    for (const layer of stage.layers()) assert.equal(stage.layer(layer.id), layer);
    const explained = stage.attribute("/Ball.radius")?.explain();
    assert.ok(explained);
    assert.deepEqual(
      explained.opinions.map((opinion) => stage.layer(opinion.layer.id).identifier),
      ["parts/ball.usda"],
      "the referenced layer's opinion",
    );
  });

  it("refuses an id the stage holds no layer for", () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
    const unknown = failure(() => stage.layer(2n ** 64n - 1n));
    assert.equal(unknown.code, "layer.unknown");
    assert.equal(unknown.operation, "layer");
    assert.equal(failure(() => stage.layer(1 as unknown as bigint)).code, "argument.invalid");
    stage.free();
    assert.equal(failure(() => stage.layer(0n)).code, "stage.disposed");
  });
});
