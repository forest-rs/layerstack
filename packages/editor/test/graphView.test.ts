// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where the shading graph's drawing sits in its frame: fitted whole or
// legible around a node, and fitted again when the frame changes size
// while the view is still the editor's, not once the person moved it.
// The sizes are the sample's checker network in the browser: the frame is
// 66 px tall when the tab first draws, and 348 px once the bottom panel
// has grown.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { type Box, fitView, PADDING, READABLE, resizedView } from "../src/model/graphView.ts";

const CHECKER = { width: 1481, height: 150 };
const OUTPUTS: Box = { x: 1406, y: 88, width: 150, height: 62 };
const boxOf = (key: string) => (key === "outputs" ? OUTPUTS : null);

describe("the shading graph's view", () => {
  it("is legible around the outputs while the frame is too short for the whole network", () => {
    const first = fitView({ width: 1600, height: 66 }, CHECKER, true, OUTPUTS)!;
    assert.equal(first.scale, READABLE);
    // The network's left nodes and its top are outside the frame: what the tab showed when it opened.
    assert.ok(first.x < 0 && first.y < 0);
  });

  it("is fitted again, whole, when the frame grows while the view is still the editor's", () => {
    const origin = { readable: true, focus: "outputs" };
    const grown = resizedView(origin, { width: 1600, height: 348 }, CHECKER, boxOf)!;
    assert.deepEqual(grown, { scale: 1, x: (1600 - 1481) / 2, y: (348 - 150) / 2 });
    assert.ok(grown.x >= 0 && grown.x + CHECKER.width * grown.scale <= 1600, "every node inside, left to right");
    assert.ok(grown.y >= PADDING && grown.y + CHECKER.height * grown.scale <= 348, "titles not clipped at the top");
  });

  it("stays where the person put it when the frame changes size after a pan or zoom", () => {
    assert.equal(resizedView(null, { width: 1600, height: 348 }, CHECKER, boxOf), null);
  });

  it("is not made for a frame with no width yet", () => {
    assert.equal(fitView({ width: 0, height: 0 }, CHECKER, true, null), null);
  });
});
