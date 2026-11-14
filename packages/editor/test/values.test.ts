// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Values as the editor shows them. The bindings hand out half-float arrays
// as their raw bits (a typed view over the module's memory), so the text
// of `half[]`, `half3[]`, `quath[]` and `color3h[]` values decodes them;
// scalars and tuples of halves arrive as numbers already.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { formatValue, leadingNumbers, releaseValue } from "../src/model/values.ts";

const LAYER = `#usda 1.0
def "Halves"
{
    half one = 0.5
    half3 three = (1, 2, 0.5)
    quath turn = (1, 0, 0, 0)
    half[] ones = [0.5, 1, -2]
    half3[] threes = [(1, 2, 0.5)]
    quath[] turns = [(1, 0, 0, 0), (0.70703125, 0, 0.70703125, 0)]
    color3h[] tints = [(0.25, 0.5, 1)]
    float[] floats = [0.5, 1]
}
`;

const layerstack = await loadLayerstack();
const stage = layerstack.open({ identifier: "halves.usda", bytes: new TextEncoder().encode(LAYER) });
const text = (name: string) => {
  const value = stage.attribute(`/Halves.${name}`)!.get();
  try {
    return formatValue(value);
  } finally {
    releaseValue(value);
  }
};

describe("half-float values", () => {
  it("read as their values, not their bits", () => {
    assert.equal(text("one"), "0.5");
    assert.equal(text("three"), "(1, 2, 0.5)");
    assert.equal(text("turn"), "(1, 0, 0, 0)", "a quaternion is shown as USDA writes it, real part first, though it is read [i, j, k, r]");
    assert.equal(text("ones"), "[0.5, 1, -2] (3 elements)");
    assert.equal(text("threes"), "[(1, 2, 0.5)] (1 element)");
    assert.equal(text("turns"), "[(1, 0, 0, 0), (0.707031, 0, 0.707031, 0)] (2 elements)");
    assert.equal(text("tints"), "[(0.25, 0.5, 1)] (1 element)");
    assert.equal(text("floats"), "[0.5, 1] (2 elements)");
  });

  it("give a colour strip their numbers", () => {
    const array = stage.attribute("/Halves.tints")!.array()!;
    try {
      assert.equal(array.scalar, "f16", "the bindings hand out bits");
      assert.deepEqual([...leadingNumbers(array, 8)], [0.25, 0.5, 1]);
    } finally {
      array.release();
    }
  });
});
