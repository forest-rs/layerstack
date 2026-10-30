// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Changes, Layer, Stage } from "../dist/index.js";
import { bytes, mockSource, scene } from "./fixtures.ts";

const layerstack = await loadLayerstack();

async function resolved(options: { editLayer?: "session" } = {}): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") }, options);
  await stage.resolve(mockSource(scene));
  return stage;
}

function layerNamed(stage: Stage, identifier: string): Layer {
  const layer = stage.layers().find((candidate) => candidate.identifier === identifier);
  assert.ok(layer, identifier);
  return layer;
}

function size(stage: Stage): unknown {
  const value = stage.attribute("/Ball.size")?.get();
  return value?.kind === "scalar" ? value.value : value;
}

function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, String(error));
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("layer muting", () => {
  it("hides a sublayer until it is unmuted, and listeners hear both", async () => {
    using stage = await resolved();
    const sub = layerNamed(stage, "sub.usda");
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    assert.equal(size(stage), 5);

    assert.equal(stage.muteLayer(sub), true);
    assert.equal(size(stage), 2, "the root's class gives the size again");
    assert.equal(stage.attribute("/Ball.serial"), null);
    assert.deepEqual(stage.mutedLayers(), [sub]);
    assert.equal(stage.isMuted(sub), true);
    assert.equal(stage.isMuted(stage.root), false);
    assert.equal(heard.length, 1, "one recomposition, heard before muteLayer returned");
    const [muted] = heard as [Changes];
    assert.deepEqual(muted.mutedLayers, [sub]);
    assert.deepEqual(muted.unmutedLayers, []);
    assert.ok(muted.resynced.length + muted.changedInfoOnly.length > 0, "the prims it gave opinions to changed");
    assert.deepEqual(stage.unavailable(), [], "muting asks for nothing");

    assert.equal(stage.muteLayer(sub), false, "muted already");
    assert.equal(heard.length, 1, "nothing to hear");

    assert.equal(stage.unmuteLayer(sub), true);
    assert.equal(size(stage), 5);
    assert.deepEqual(stage.mutedLayers(), []);
    assert.equal(heard.length, 2);
    assert.deepEqual(heard[1]?.mutedLayers, []);
    assert.deepEqual(heard[1]?.unmutedLayers, [sub]);
    assert.equal(stage.unmuteLayer(sub), false, "not muted");
    assert.equal(heard.length, 2);
  });

  it("is stage state, not an edit: no receipt, and undo is not blocked by it", async () => {
    using stage = await resolved();
    const sub = layerNamed(stage, "sub.usda");
    const receipt = stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 7 } }]);
    assert.deepEqual(receipt.changes.mutedLayers, []);
    stage.muteLayer(sub);
    assert.equal(size(stage), 7);
    const redo = receipt.undo();
    assert.equal(size(stage), 2, "the root's edit is undone; the sublayer stays muted");
    assert.deepEqual(stage.mutedLayers(), [sub]);
    redo.undo();
    assert.equal(size(stage), 7);
  });

  it("keeps a muted layer's content and edits, which save skips and edits refuse", async () => {
    using stage = await resolved();
    const sub = layerNamed(stage, "sub.usda");
    stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 8 } }], sub);
    stage.muteLayer(sub);
    assert.equal(stage.isDirty(sub), true, "the unsaved edit stays");
    const error = failure(() =>
      stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 9 } }], sub),
    );
    assert.equal(error.code, "layer.muted");
    assert.equal(error.identifier, "sub.usda");
    assert.equal(stage.save().written.length, 0, "the stage does not use the muted layer");
    assert.equal(stage.isDirty(sub), true);
    stage.unmuteLayer(sub);
    assert.equal(size(stage), 8, "unmuted, its edit composes again");
    assert.deepEqual(
      stage.save().written.map((written) => written.identifier),
      ["sub.usda"],
    );
  });

  it("mutes a referenced layer and the session layer, but never the root", async () => {
    using stage = await resolved({ editLayer: "session" });
    const ball = layerNamed(stage, "parts/ball.usda");
    assert.ok(stage.attribute("/Ball.points"));
    stage.muteLayer(ball);
    assert.equal(stage.attribute("/Ball.points"), null, "the reference contributes nothing");
    assert.deepEqual(stage.unavailable(), []);
    assert.equal(stage.layer(ball.id), ball, "a muted layer's id still names it");

    const session = stage.editLayer as Layer;
    stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 3 } }]);
    assert.equal(size(stage), 3);
    stage.muteLayer(session);
    assert.equal(size(stage), 5, "the session layer's opinion is gone");
    assert.equal(
      failure(() => stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 4 } }])).code,
      "layer.muted",
    );
    assert.deepEqual(new Set(stage.mutedLayers()), new Set([ball, session]));

    const root = failure(() => stage.muteLayer(stage.root));
    assert.equal(root.code, "mute.root_layer");
    assert.equal(root.operation, "muteLayer");
    assert.equal(stage.unmuteLayer(stage.root), false, "the root is never muted");
  });

  it("refuses another stage's layer and a disposed stage", async () => {
    using stage = await resolved();
    using other = await resolved();
    const sub = layerNamed(other, "sub.usda");
    assert.equal(failure(() => stage.muteLayer(sub)).code, "layer.affinity");
    assert.equal(failure(() => stage.unmuteLayer(sub)).code, "layer.affinity");
    assert.equal(failure(() => stage.isMuted(sub)).code, "layer.affinity");
    other.free();
    assert.equal(failure(() => other.mutedLayers()).code, "stage.disposed");
  });

  it("hears muting a native extension composes", async () => {
    using stage = await resolved();
    const sub = layerNamed(stage, "sub.usda");
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    stage.withNative("domain.mute", (native) => native.setLayerMuted(sub.id, true));
    assert.equal(heard.length, 1);
    assert.deepEqual(heard[0]?.mutedLayers, [sub]);
    assert.deepEqual(heard[0]?.mutedIdentifiers, ["sub.usda"]);
    assert.deepEqual(stage.mutedLayers(), [sub]);
  });

  it("mutes a layer by identifier before the stage reads it, as OpenUSD does", async () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    assert.deepEqual(
      stage.unavailable().map((asset) => asset.identifier),
      ["parts/ball.usda", "sub.usda"],
    );

    assert.equal(stage.muteLayer("./parts/../parts/ball.usda"), true, "anchored to the root layer");
    assert.deepEqual(stage.unavailable().map((asset) => asset.identifier), ["sub.usda"], "no longer asked for");
    assert.deepEqual(stage.mutedLayers(), [], "the stage holds no layer for it");
    assert.deepEqual(stage.mutedIdentifiers(), ["parts/ball.usda"]);
    assert.equal(stage.isMuted("parts/ball.usda"), true);
    assert.equal(stage.isMuted("./parts/ball.usda"), true);
    assert.equal(stage.isMuted("never.usda"), false);
    assert.deepEqual(
      stage.status().compositionErrors.map((error) => [error.code, error.prim]),
      [["composition.muted_asset", "/Ball"]],
    );
    assert.equal(heard.length, 1);
    assert.deepEqual(heard[0]?.mutedLayers, []);
    assert.deepEqual(heard[0]?.mutedIdentifiers, ["parts/ball.usda"]);

    const source = mockSource({ ...scene, "lamp.usda": '#usda 1.0\ndef "Lamp" {\n    double glow = 1\n}\n' });
    await stage.resolve(source);
    assert.deepEqual(source.asked, ["sub.usda"], "a muted layer is not read");
    assert.equal(stage.attribute("/Ball.points"), null);

    // A later arc to a layer muted before anything named it composes muted.
    assert.equal(stage.muteLayer("lamp.usda"), true);
    const receipt = stage.apply([
      { type: "definePrim", path: "/Lamp" },
      { type: "addReference", path: "/Lamp", asset: "./lamp.usda", primPath: "/Lamp" },
    ]);
    assert.deepEqual(receipt.unavailable, [], "nothing to fetch");
    assert.equal(stage.attribute("/Lamp.glow"), null);
    assert.deepEqual(stage.mutedIdentifiers(), ["lamp.usda", "parts/ball.usda"]);

    heard.length = 0;
    assert.equal(stage.muteAndUnmuteLayers([], ["parts/ball.usda", "./lamp.usda"]), true);
    assert.equal(heard.length, 1, "one request, one change");
    assert.deepEqual(heard[0]?.unmutedIdentifiers, ["lamp.usda", "parts/ball.usda"]);
    assert.deepEqual(heard[0]?.unmutedLayers, [], "neither was read");
    assert.deepEqual(
      stage.unavailable().map((asset) => asset.identifier),
      ["lamp.usda", "parts/ball.usda"],
      "unmuted, they are asked for",
    );
    source.asked.length = 0;
    await stage.resolve(source);
    assert.deepEqual(source.asked.sort(), ["lamp.usda", "parts/ball.usda", "parts/trim.usda"]);
    assert.ok(stage.attribute("/Ball.points"));
    assert.ok(stage.attribute("/Lamp.glow"));
    assert.deepEqual(stage.status().compositionErrors, []);
  });

  it("mutes and unmutes in one request, dropping an unmute spelled as a mute", async () => {
    using stage = await resolved();
    const sub = layerNamed(stage, "sub.usda");
    const ball = layerNamed(stage, "parts/ball.usda");
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));

    assert.equal(stage.muteAndUnmuteLayers([sub, "parts/ball.usda"], []), true);
    assert.equal(heard.length, 1, "one recomposition, one change");
    assert.deepEqual(new Set(heard[0]?.mutedLayers), new Set([sub, ball]));
    assert.deepEqual(heard[0]?.mutedIdentifiers, ["parts/ball.usda", "sub.usda"]);
    assert.deepEqual(stage.mutedIdentifiers(), ["parts/ball.usda", "sub.usda"]);

    assert.equal(stage.muteAndUnmuteLayers(["sub.usda"], [sub]), false, "a layer is spelled by its identifier");
    assert.equal(stage.isMuted(sub), true);
    assert.equal(stage.muteAndUnmuteLayers(["./sub.usda"], [sub]), true, "two spellings: muted, then unmuted");
    assert.equal(stage.isMuted(sub), false);
    assert.equal(heard.length, 2);
    assert.deepEqual(heard[1]?.unmutedLayers, [sub]);

    const root = failure(() => stage.muteAndUnmuteLayers(["./scene.usda", sub], [ball]));
    assert.equal(root.code, "mute.root_layer");
    assert.equal(root.operation, "muteAndUnmuteLayers");
    assert.deepEqual(stage.mutedLayers(), [ball], "refused whole");
    assert.equal(failure(() => stage.muteLayer("scene.usda")).code, "mute.root_layer");
    assert.equal(stage.muteLayer(""), false, "the empty identifier names nothing");
    assert.equal(failure(() => stage.muteLayer(7 as unknown as string)).code, "argument.invalid");
  });
});
