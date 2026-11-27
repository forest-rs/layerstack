// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Stagehand's notes on a UsdPreviewSurface material, as the material
// preview lists them.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { readMaterial } from "@forest-rs/layerstack_stagehand";
import { describeMaterialNote } from "../src/model/materialNotes.ts";

const LAYER = `#usda 1.0
def Material "Stone"
{
    token outputs:surface.connect = </Stone/surface.outputs:surface>

    def Shader "surface"
    {
        uniform token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor.connect = </Stone/noise.outputs:out>
        token outputs:surface
    }

    def Shader "noise"
    {
        uniform token info:id = "ND_noise2d_color3"
        color3f outputs:out
    }
}
`;

test("a preview surface's notes say which input the viewport could not draw", async () => {
  const layerstack = await loadLayerstack();
  const stage = layerstack.open({ identifier: "looks/stone.usda", bytes: new TextEncoder().encode(LAYER) });
  const record = readMaterial(stage, "/Stone", undefined, []);
  assert.ok(record !== null);
  assert.deepEqual(record.item.notes.map(describeMaterialNote), [
    "diffuseColor reads ND_noise2d_color3, which is not evaluated; its fallback is drawn",
  ]);
});
