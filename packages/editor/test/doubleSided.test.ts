// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which surfaces the viewport draws from both sides: one rule for every
// material of an item, bound or not.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { DrawItem } from "@forest-rs/layerstack_stagehand";
import { drawsBothSides } from "../src/model/draws.ts";

test("an authored doubleSided gprim, or any mesh or plane, draws both sides", () => {
  const view = (kind: string, doubleSided: boolean) => ({ kind, item: { doubleSided } as DrawItem });
  assert.equal(drawsBothSides(view("Sphere", true)), true, "an authored doubleSided sphere, bound or not");
  assert.equal(drawsBothSides(view("Sphere", false)), false, "a closed shape authored without it");
  assert.equal(drawsBothSides(view("mesh", false)), true, "meshes, often open, as usdview draws them");
  assert.equal(drawsBothSides(view("Plane", false)), true);
});
