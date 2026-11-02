// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Rect lights shade with one shared, untransformed pair of lookup tables,
// not two per-object texture transforms per light.
import assert from "node:assert/strict";
import { test } from "node:test";
import { RectAreaLight, RectAreaLightNode } from "three/webgpu";
import type { Node, NodeBuilder } from "three/webgpu";
import { vec2 } from "three/tsl";
import { useSharedRectAreaTables } from "../src/viewport/rectAreaLights.ts";

test("rect lights share one untransformed pair of lookup tables", () => {
  useSharedRectAreaTables();
  const nodeClass = (RectAreaLight.prototype as unknown as { _lightNode: typeof RectAreaLightNode })._lightNode;
  assert.notEqual(nodeClass, RectAreaLightNode);
  const builder = { isAvailable: () => true } as unknown as NodeBuilder;
  type Tables = { ltc_1: { updateMatrix: boolean; sample(uv: Node): { updateMatrix: boolean } }; ltc_2: unknown };
  const setup = (light: RectAreaLight) => (new nodeClass(light) as unknown as { setupDirectRectArea(builder: NodeBuilder): Tables }).setupDirectRectArea(builder);
  const first = setup(new RectAreaLight());
  const second = setup(new RectAreaLight());
  assert.equal(first.ltc_1, second.ltc_1);
  assert.equal(first.ltc_2, second.ltc_2);
  assert.notEqual(first.ltc_1, first.ltc_2);
  assert.equal(first.ltc_1.updateMatrix, false);
  assert.equal(first.ltc_1.sample(vec2(0.5, 0.5) as unknown as Node).updateMatrix, false, "a sample has no per-object texture transform");
});
