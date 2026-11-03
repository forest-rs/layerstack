// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Framing something with no bounds leaves the view as it was and says so,
// for a few seconds; framing something with bounds clears the note.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { loadStagehand } from "@forest-rs/layerstack_stagehand";
import { EditorStore, FRAME_NOTE_MS } from "../src/store.ts";
import type { SceneSource } from "../src/model/sources.ts";
import type { Viewport } from "../src/viewport/viewport3d.ts";

globalThis.requestAnimationFrame ??= ((callback: FrameRequestCallback) => setTimeout(() => callback(performance.now()), 0) as unknown as number) as typeof requestAnimationFrame;
globalThis.cancelAnimationFrame ??= ((id: number) => clearTimeout(id)) as typeof cancelAnimationFrame;

const SCENE = `#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Mesh "Panel" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        rel material:binding = </World/Looks/Checker>
    }
    def Scope "Looks"
    {
        def Material "Checker"
        {
        }
    }
    def Camera "Cam"
    {
    }
}
`;

const SOURCE: SceneSource = {
  label: "scene.usda",
  root: "scene.usda",
  fetch: async (identifier: string) => (identifier === "scene.usda" ? new TextEncoder().encode(SCENE) : null),
  exists: async (identifier: string) => identifier === "scene.usda",
  size: async () => null,
};

/** A view that records what it framed. */
function view(framed: (readonly number[])[]): Viewport {
  const stub = {
    lookThrough() {},
    setLightSettings() {},
    setUpAxis() {},
    setMarker() {},
    update() {},
    frame(box: readonly number[]) {
      framed.push(box);
      return true;
    },
  };
  return stub as unknown as Viewport;
}

test("framing something with no bounds says so and leaves the view, for a while", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const [layerstack, stagehand] = await Promise.all([loadLayerstack(), loadStagehand()]);
  const store = new EditorStore(layerstack, stagehand);
  const opening = store.open(SOURCE);
  // The open waits on frames between its phases.
  for (let i = 0; i < 50 && store.status.kind !== "ready"; i++) {
    t.mock.timers.tick(1);
    await new Promise((resolve) => setImmediate(resolve));
  }
  await opening;
  assert.equal(store.status.kind, "ready");
  const framed: (readonly number[])[] = [];
  store.attachViewport(view(framed));

  store.select("/World/Looks/Checker");
  assert.equal(store.frameSelection(), false);
  assert.equal(framed.length, 0, "the view does not move");
  assert.equal(store.frameNote?.message, "Nothing to frame: /World/Looks/Checker has no bounds");
  const first = store.frameNote!.id;

  store.select("/World/Cam");
  assert.equal(store.frameSelection(), false);
  assert.match(store.frameNote!.message, /^Nothing to frame: \/World\/Cam has no bounds; look through it instead$/);
  assert.notEqual(store.frameNote!.id, first, "asking again shows the note again");

  t.mock.timers.tick(FRAME_NOTE_MS);
  assert.equal(store.frameNote, null, "it fades");

  store.select("/World/Looks");
  store.frameSelection();
  assert.notEqual(store.frameNote, null);
  store.select("/World/Panel");
  assert.equal(store.frameSelection(), true);
  assert.equal(framed.length, 1);
  assert.equal(store.frameNote, null, "framing clears it");
});
