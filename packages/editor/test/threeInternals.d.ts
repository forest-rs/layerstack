// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Types for the three.js internals the tests check, which `@types/three`
// does not declare: only what the tests call.

declare module "three/src/renderers/common/RenderObjects.js" {
  import type { Camera, Material, Object3D, Scene } from "three/webgpu";
  /** A render object: what three.js draws an object's material with, and the camera it last drew it for. */
  export interface RenderObject {
    camera: Camera;
  }
  /** three.js's render objects, by object, material, render context and light set. */
  export default class RenderObjects {
    constructor(renderer: unknown, nodes: unknown, geometries: unknown, pipelines: unknown, bindings: unknown, info: unknown);
    get(object: Object3D, material: Material, scene: Scene, camera: Camera, lightsNode: unknown, renderContext: unknown, clippingContext: unknown, passId?: string): RenderObject;
  }
}

declare module "three/src/renderers/common/RenderBundles.js" {
  import type { BundleGroup, Camera } from "three/webgpu";
  /** three.js's render bundles, by bundle group, camera and render context. */
  export default class RenderBundles {
    get(bundleGroup: BundleGroup, camera: Camera, renderContext: unknown): object;
  }
}
