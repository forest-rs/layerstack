// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the asset view makes of a selection, the draw scope it asks the
// draw list for, the draw budget, and what an image's header says.
import assert from "node:assert/strict";
import { test } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import type { BudgetGrant, BudgetReport } from "@forest-rs/layerstack_stagehand";
import { assetFocus, budgetFor, budgetNote, DRAW_BUDGET, rememberBudget, rememberedBudget, scopeFor } from "../src/model/assetView.ts";
import { imageInfo } from "../src/model/images.ts";

const LAYER = `#usda 1.0
def Xform "World" {
    def Mesh "Tile" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (0, 1, 0), (1, 1, 0), (2, 1, 0)]
        asset info:note = @notes/tile.png@
        def GeomSubset "Left" {
            uniform token elementType = "face"
            int[] indices = [0]
        }
    }
    def Material "Look" {
        def Shader "Image" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @textures/look.png@
        }
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
        }
    }
}
`;

const layerstack = await loadLayerstack();

test("the asset view draws geometry alone, and shows textures and materials apart", () => {
  const stage = layerstack.open({ identifier: "asset.usda", bytes: new TextEncoder().encode(LAYER) });
  try {
    assert.deepEqual(assetFocus(stage, null, null, undefined), { kind: "none" });
    assert.deepEqual(assetFocus(stage, "/World", null, undefined), { kind: "geometry", root: "/World", subset: null });
    assert.deepEqual(assetFocus(stage, "/World/Tile/Left", null, undefined), { kind: "geometry", root: "/World/Tile", subset: "/World/Tile/Left" });
    assert.deepEqual(assetFocus(stage, "/World/Look/Image", null, undefined), { kind: "texture", attribute: "/World/Look/Image.inputs:file" });
    assert.deepEqual(assetFocus(stage, "/World/Look/Surface", null, undefined), { kind: "material", material: "/World/Look" });
    // An asset-valued property naming an image, chosen in the inspector.
    assert.deepEqual(assetFocus(stage, "/World/Tile", "/World/Tile.info:note", undefined), { kind: "texture", attribute: "/World/Tile.info:note" });
    assert.deepEqual(assetFocus(stage, "/World/Tile", "/World/Tile.points", undefined), { kind: "geometry", root: "/World/Tile", subset: null });
  } finally {
    stage.free();
  }
});

test("each draw scope choice asks the draw list for its scope", () => {
  const geometry = { kind: "geometry", root: "/World/Tile", subset: "/World/Tile/Left" } as const;
  assert.deepEqual(scopeFor("scene", geometry), { roots: ["/"] });
  assert.deepEqual(scopeFor("nothing", geometry), { roots: [] });
  assert.deepEqual(scopeFor("selection", geometry), { roots: ["/World/Tile"], space: "/World/Tile" });
  assert.deepEqual(scopeFor("selection", { kind: "material", material: "/World/Look" }), { roots: [] });
});

test("the draw budget is one constant, raised by a choice, and says what it simplified", () => {
  assert.deepEqual(budgetFor("default"), DRAW_BUDGET);
  assert.deepEqual(budgetFor("double"), { gpuBytes: 2 * 1024 ** 3, triangles: 100_000_000 });
  assert.deepEqual(budgetFor("quadruple"), { gpuBytes: 4 * 1024 ** 3, triangles: 200_000_000 });
  assert.equal(budgetFor("unlimited"), null);
  // No storage here: the default.
  assert.equal(rememberedBudget(), "default");
  rememberBudget("double");
  const grant = (rung: "standin" | "skipped"): BudgetGrant => ({
    item: null,
    source: { kind: "gprim", path: "/World/Tile" },
    prim: "/World/Tile",
    rung,
    requested: { gpuBytes: 1, triangles: 1, instances: 1 },
    granted: { gpuBytes: 0, triangles: 0, instances: 0 },
    over: ["triangles"],
    promoted: false,
    order: 0,
  });
  const report = (degraded: BudgetGrant[]): BudgetReport => ({
    budget: DRAW_BUDGET,
    requested: { gpuBytes: 1, triangles: 1, instances: 1 },
    granted: { gpuBytes: 0, triangles: 0, instances: 0 },
    items: 3,
    degraded,
  });
  assert.equal(budgetNote(null), null);
  assert.equal(budgetNote(report([])), null);
  assert.equal(budgetNote(report([grant("standin"), grant("standin"), grant("skipped")])), "3 items simplified to fit the budget (2 as boxes, 1 skipped).");
  assert.equal(budgetNote(report([grant("skipped")])), "1 item simplified to fit the budget (1 skipped).");
});

test("an image's header gives its size and channels", () => {
  // A PNG signature and IHDR: 640 × 480, 8 bits, RGBA (color type 6).
  const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 2, 128, 0, 0, 1, 224, 8, 6]);
  assert.deepEqual(imageInfo(png), { format: "png", width: 640, height: 480, bits: 8, channels: "RGBA" });
  assert.equal(imageInfo(new Uint8Array([0x76, 0x2f, 0x31, 0x01])).format, "exr");
  assert.equal(imageInfo(new Uint8Array([1, 2, 3])).format, "unknown");
});
