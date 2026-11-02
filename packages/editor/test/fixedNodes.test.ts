// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A material's light set and a clustered draw's context are built into its
// shader and keyed apart, but not seen by three.js's per-frame observer as
// reasons to refresh each object drawn with it in full.
import assert from "node:assert/strict";
import { test } from "node:test";
import { MeshStandardNodeMaterial, PointLight } from "three/webgpu";
import type { Material, Node } from "three/webgpu";
import { lights, vec2 } from "three/tsl";
import NodeMaterialObserver from "three/src/materials/nodes/manager/NodeMaterialObserver.js";
import { fixNodes, setFixedNode } from "../src/viewport/fixedNodes.ts";

/** Whether three.js's observer would refresh every object drawn with `material` in full on every frame. */
function refreshedInFull(material: Material): boolean {
  const containsNode = (NodeMaterialObserver.prototype as unknown as { containsNode(builder: unknown): boolean }).containsNode;
  return containsNode.call(null, { material, context: {} });
}

test("a fixed light set is built into the shader but not watched", () => {
  const sun = lights([new PointLight()]);
  const spot = lights([new PointLight(), new PointLight()]);
  const a = new MeshStandardNodeMaterial();
  const b = new MeshStandardNodeMaterial();
  assert.equal(refreshedInFull(a), false);
  a.lightsNode = sun;
  assert.equal(refreshedInFull(a), true, "three.js watches a light set assigned as it is");
  setFixedNode(a, "lightsNode", sun);
  setFixedNode(b, "lightsNode", spot);
  assert.equal(a.lightsNode, sun);
  assert.equal(refreshedInFull(a), false);
  assert.notEqual(a.customProgramCacheKey(), b.customProgramCacheKey(), "materials of different light sets build different shaders");
  setFixedNode(b, "lightsNode", sun);
  assert.equal(a.customProgramCacheKey(), b.customProgramCacheKey(), "and of the same set, the same shader");
  setFixedNode(b, "lightsNode", null);
  assert.equal(b.lightsNode, null);
});

test("a copy of a material is fixed again", () => {
  const material = new MeshStandardNodeMaterial();
  const context = vec2(0, 0) as unknown as Node;
  setFixedNode(material, "lightsNode", lights([new PointLight()]));
  setFixedNode(material, "contextNode", context);
  const copy = material.clone();
  assert.equal(refreshedInFull(copy), true, "three.js copies them as plain properties");
  fixNodes(copy);
  assert.equal(refreshedInFull(copy), false);
  assert.equal(copy.lightsNode, material.lightsNode);
  assert.equal((copy as unknown as { contextNode: Node }).contextNode, context);
});
