// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Node properties three.js need not watch on every frame.
//
// three.js decides per object and per frame how much of an object's state
// to refresh (`NodeMaterialObserver.needsRefresh`). A material with any
// node-valued property it can enumerate (`containsNode`, a `for...in` over
// the material) is refreshed in full for every object on every frame,
// bundled or not, since such a node could hide values that change without
// the material changing: each object's update nodes run, and each of its
// bind groups (its uniforms, its textures and samplers, its storage
// buffers) is checked again. A material without one is refreshed once per
// shader and frame (its shared uniforms), and a bundled object's other
// state only when its bundle is recorded again.
//
// The viewport gives its materials such nodes whose values never change
// behind three.js's back: a draw's light set (`lightsNode`, a `LightsNode`
// of fixed lights whose uniforms are the render's, shared), a clustered
// draw's context (`contextNode`, reads of storage buffers the GPU writes),
// and a card stand-in's color and roughness (`colorNode`, `roughnessNode`,
// shared by every card material and reading its own textures by
// reference, `standins.ts`).
// Each is replaced, never changed, and replacing one is a material change
// (`needsUpdate`) that builds the material's shader again. So they are set
// as own properties that are not enumerable: three.js still builds them
// into the shader and its cache key (`NodeMaterial.customProgramCacheKey`
// reads every own property), and its observer no longer sees them; the
// tests check both against three.js's own code, so an upgrade that changes
// either fails there. Without this, a lit production element of 2,302
// draws, each linked to its lights, refreshed every draw in full on every
// frame: about 6 ms a frame, against 0.6 ms with it (its clustered draws'
// contexts alone, unlit, cost 3.7 ms).

import type { Material, Node } from "three/webgpu";

/** The node properties of a material the viewport sets as fixed. */
export type FixedNodeProperty = "lightsNode" | "contextNode" | "colorNode" | "roughnessNode";

/**
 * Sets `material`'s node property `property` to `node` (or `null`), as a
 * fixed node three.js builds into the shader but does not refresh each
 * object for on each frame. A copy of the material (`clone`) has the
 * property as three.js sets it: {@link fixNodes} makes it fixed again.
 */
export function setFixedNode(material: Material, property: FixedNodeProperty, node: Node | null): void {
  Object.defineProperty(material, property, { value: node, writable: true, enumerable: false, configurable: true });
}

/** Makes `material`'s light set and context, as they are, fixed ({@link setFixedNode}): for a copy of a material that had them. */
export function fixNodes(material: Material): void {
  for (const property of ["lightsNode", "contextNode"] as const) {
    if (property in material) setFixedNode(material, property, (material as unknown as Record<FixedNodeProperty, Node | null>)[property]);
  }
}
