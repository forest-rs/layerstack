// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The one camera the viewport draws through, and the view the cluster cut
// reads from it, apart from the viewport so they can be tested in Node.
//
// The viewport has two cameras: the free one the orbit controls move, and
// the stage camera it looks through, whose matrices layerstack computes.
// Every pass draws through neither of them but through the view camera
// here, which takes the active one's matrices before each pass ({@link
// followCamera}, {@link followMatrices}). Drawing through both directly
// left the bundled draws looking through the stage camera after leaving
// it: three.js records a render bundle per camera (`RenderBundles.get`,
// keyed by bundle group, camera and render context) but keeps one render
// object per object, material, render context and light set, whatever the
// camera (`RenderObjects.get`), and points that render object at the
// camera it is fetched for. A direct draw is fetched every frame; a
// bundled one only when its bundle is recorded. So once the stage
// camera's bundle was recorded, the free camera's bundle, still current,
// replayed render objects pointing at the stage camera, and refreshing
// them wrote the stage camera's view and projection into the shared
// camera uniforms (`cameraViewMatrix`, `cameraProjectionMatrix`, updated
// from the render object's camera): the bundled draws stayed where the
// stage camera saw them while the direct ones (the translucent draws)
// followed the free camera, and the cluster cut, chosen for the free
// camera, culled what the bundled draws showed. With one camera object,
// a render object's camera never changes, a bundle's recording stays
// valid however the view moves, and every pass (bundled, direct,
// clustered, translucent, picking, the outline, the cut) reads the same
// matrices.
//
// The view camera is a `PerspectiveCamera` whose matrices are set, never
// computed (`matrixAutoUpdate`, `matrixWorldAutoUpdate` off; its
// `updateProjectionMatrix` is never called): three.js tells cameras apart
// only by `isOrthographicCamera`, which neither the free camera nor a stage
// camera's is, so drawing through it builds the shaders drawing through
// either did.

import { Frustum, Matrix4, PerspectiveCamera, Vector3, Vector4, WebGPUCoordinateSystem } from "three/webgpu";
import type { Camera } from "three/webgpu";

/** Makes a view camera: matrices set by {@link followCamera} or {@link followMatrices}, in WebGPU's clip space. */
export function makeViewCamera(): PerspectiveCamera {
  const view = new PerspectiveCamera();
  view.matrixAutoUpdate = false;
  view.matrixWorldAutoUpdate = false;
  view.coordinateSystem = WebGPUCoordinateSystem;
  return view;
}

/**
 * Points `view` where `camera` is, as `camera` is now: its world matrix
 * (and the position, rotation and scale it decomposes into), its view and
 * projection matrices and their inverses, and its clipping planes. The
 * caller brings `camera`'s matrices up to date first.
 */
export function followCamera(view: PerspectiveCamera, camera: PerspectiveCamera): void {
  view.matrixWorld.copy(camera.matrixWorld);
  view.matrixWorldInverse.copy(camera.matrixWorldInverse);
  view.projectionMatrix.copy(camera.projectionMatrix);
  view.projectionMatrixInverse.copy(camera.projectionMatrixInverse);
  view.fov = camera.fov;
  view.aspect = camera.aspect;
  view.zoom = camera.zoom;
  setClipping(view, camera.near, camera.far);
}

/**
 * Points `view` along the world-to-eye matrix `worldToEye` with the
 * projection `projection` (already in WebGPU's clip space), as a stage
 * camera's are given: both 16 numbers three.js reads as they are
 * (`fromArray`). `near` and `far` are its clipping range.
 */
export function followMatrices(view: PerspectiveCamera, worldToEye: ArrayLike<number>, projection: Matrix4, near: number, far: number): void {
  view.matrixWorldInverse.fromArray(worldToEye);
  view.matrixWorld.copy(view.matrixWorldInverse).invert();
  view.projectionMatrix.copy(projection);
  view.projectionMatrixInverse.copy(projection).invert();
  setClipping(view, near, far);
}

/** Sets `view`'s clipping planes and the transform its world matrix decomposes into. */
function setClipping(view: PerspectiveCamera, near: number, far: number): void {
  view.near = near;
  view.far = far;
  view.matrixWorld.decompose(view.position, view.quaternion, view.scale);
}

/**
 * What the cluster cut reads from a view (`clusters.ts`): its frustum's
 * planes (normal and constant, inside where positive), its position in
 * world space, `proj[1][1]` and the near plane (`layerstack_clusters`'s
 * `CutCamera`), and the scale taking a world-space error at distance 1 to
 * pixels (`proj[1][1] / 2 * height`, `CutCamera::projected_error`).
 */
export interface CutView {
  readonly planes: readonly Vector4[];
  readonly position: Vector3;
  projectionY: number;
  zNear: number;
  projectionScale: number;
  /** The view's projection times its world-to-eye matrix. */
  readonly projectionView: Matrix4;
}

/** A {@link CutView} to fill with {@link cutViewOf}, made once and reused. */
export function makeCutView(): CutView {
  return {
    planes: [0, 1, 2, 3, 4, 5].map(() => new Vector4()),
    position: new Vector3(),
    projectionY: 1,
    zNear: 0.01,
    projectionScale: 1,
    projectionView: new Matrix4(),
  };
}

const FRUSTUM = new Frustum();

/**
 * Fills `out` with what the cut reads from `camera` (its matrices current)
 * at a viewport `height` pixels tall. A camera without a near plane (a
 * bare `Camera`) is cut as if it were at a hundredth of a unit. Allocates
 * nothing.
 */
export function cutViewOf(camera: Camera, height: number, out: CutView): CutView {
  out.projectionView.multiplyMatrices(camera.projectionMatrix, camera.matrixWorldInverse);
  FRUSTUM.setFromProjectionMatrix(out.projectionView, camera.coordinateSystem);
  for (let i = 0; i < 6; i++) {
    const plane = FRUSTUM.planes[i]!;
    out.planes[i]!.set(plane.normal.x, plane.normal.y, plane.normal.z, plane.constant);
  }
  out.position.setFromMatrixPosition(camera.matrixWorld);
  out.projectionY = camera.projectionMatrix.elements[5]!;
  out.zNear = (camera as { near?: number }).near ?? 0.01;
  out.projectionScale = out.projectionY * 0.5 * height;
  return out;
}
