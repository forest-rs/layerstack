// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The one camera the viewport draws through follows whichever camera is
// active, and the cluster cut reads the same view from it: switching
// between the free camera and a stage camera leaves nothing of the other
// behind. And the three.js behavior that makes one camera necessary.
import assert from "node:assert/strict";
import { test } from "node:test";
import { BoxGeometry, BundleGroup, Camera, EventDispatcher, Matrix4, Mesh, MeshBasicNodeMaterial, PerspectiveCamera, Scene, Vector3, WebGPUCoordinateSystem } from "three/webgpu";
import RenderBundles from "three/src/renderers/common/RenderBundles.js";
import RenderObjects from "three/src/renderers/common/RenderObjects.js";
import { cutViewOf, followCamera, followMatrices, makeCutView, makeViewCamera } from "../src/viewport/viewCamera.ts";
import type { CutView } from "../src/viewport/viewCamera.ts";

/** A free camera at `position` looking at `target`, its matrices current. */
function freeCamera(position: [number, number, number], target: [number, number, number], fov = 45, aspect = 1.5): PerspectiveCamera {
  const camera = new PerspectiveCamera(fov, aspect, 0.5, 400);
  camera.coordinateSystem = WebGPUCoordinateSystem;
  camera.position.set(...position);
  camera.lookAt(...target);
  camera.updateProjectionMatrix();
  camera.updateMatrixWorld();
  return camera;
}

/** A stage camera's matrices: world to eye, and a projection in WebGPU's clip space, as the viewport derives them. */
function stageMatrices(): { worldToEye: number[]; projection: Matrix4; near: number; far: number } {
  const stage = freeCamera([30, 5, -12], [0, 0, 0], 70, 1.5);
  stage.near = 2;
  stage.far = 90;
  stage.updateProjectionMatrix();
  return { worldToEye: stage.matrixWorldInverse.toArray(), projection: stage.projectionMatrix.clone(), near: 2, far: 90 };
}

function assertMatrix(actual: Matrix4, expected: Matrix4, message: string): void {
  actual.elements.forEach((value, i) => assert.ok(Math.abs(value - expected.elements[i]!) < 1e-9, `${message}: element ${i} is ${value}, not ${expected.elements[i]}`));
}

function assertSameCut(actual: CutView, expected: CutView): void {
  for (let i = 0; i < 6; i++) {
    for (const axis of ["x", "y", "z", "w"] as const) {
      assert.ok(Math.abs(actual.planes[i]![axis] - expected.planes[i]![axis]) < 1e-9, `plane ${i}.${axis}`);
    }
  }
  assert.ok(actual.position.distanceTo(expected.position) < 1e-9, "position");
  assert.equal(actual.projectionY, expected.projectionY);
  assert.equal(actual.zNear, expected.zNear);
  assert.equal(actual.projectionScale, expected.projectionScale);
  assertMatrix(actual.projectionView, expected.projectionView, "projection times view");
}

/** Whether `point` is inside every plane of `view`. */
function inside(view: CutView, point: Vector3): boolean {
  return view.planes.every((plane) => plane.x * point.x + plane.y * point.y + plane.z * point.z + plane.w >= 0);
}

test("three.js records a bundle per camera but shares render objects between cameras", () => {
  // `RenderObjects.get` keys a render object without the camera and points
  // it at the camera it is fetched for; `RenderBundles.get` keys a bundle by
  // camera. A bundle still current replays its render objects as last
  // fetched, for whichever camera that was: the viewport draws through one
  // camera so they never point anywhere else. If three.js keys render
  // objects by camera, this fails and the view camera is no longer needed.
  const renderer = { _currentSourceMaterial: null, contextNode: { id: 0 } };
  const nodes = { getCacheKey: () => "", getCustomProgramCacheKey: () => "" };
  const objects = new RenderObjects(renderer, nodes, {}, {}, {}, {});
  const mesh = new Mesh(new BoxGeometry(), new MeshBasicNodeMaterial());
  const [free, stage] = [new PerspectiveCamera(), new Camera()];
  const [scene, context, lights] = [new Scene(), {}, new EventDispatcher()];
  const forFree = objects.get(mesh, mesh.material, scene, free, lights, context, null);
  const forStage = objects.get(mesh, mesh.material, scene, stage, lights, context, null);
  assert.equal(forStage, forFree, "one render object for both cameras");
  assert.equal(forFree.camera, stage, "pointed at the camera it was last fetched for");
  const bundles = new RenderBundles();
  const group = new BundleGroup();
  assert.notEqual(bundles.get(group, free, context), bundles.get(group, stage, context), "a bundle per camera");
});

test("the view camera follows the free camera, and the cut reads the same view from either", () => {
  const free = freeCamera([10, 4, 20], [0, 1, 0]);
  const view = makeViewCamera();
  followCamera(view, free);
  assertMatrix(view.matrixWorld, free.matrixWorld, "world");
  assertMatrix(view.matrixWorldInverse, free.matrixWorldInverse, "world to eye");
  assertMatrix(view.projectionMatrix, free.projectionMatrix, "projection");
  assertMatrix(view.projectionMatrixInverse, free.projectionMatrixInverse, "inverse projection");
  assert.equal(view.near, 0.5);
  assert.equal(view.far, 400);
  assert.ok(view.position.distanceTo(free.position) < 1e-9, "its position, from its world matrix");
  assert.equal(view.coordinateSystem, WebGPUCoordinateSystem);
  assertSameCut(cutViewOf(view, 700, makeCutView()), cutViewOf(free, 700, makeCutView()));
  // Drawing does not recompute what was set.
  assert.equal(view.matrixAutoUpdate, false);
  assert.equal(view.matrixWorldAutoUpdate, false);
});

test("the view camera follows a stage camera's matrices as given", () => {
  const { worldToEye, projection, near, far } = stageMatrices();
  const view = makeViewCamera();
  followMatrices(view, worldToEye, projection, near, far);
  assertMatrix(view.matrixWorldInverse, new Matrix4().fromArray(worldToEye), "world to eye");
  assertMatrix(view.projectionMatrix, projection, "projection");
  assert.ok(view.position.distanceTo(new Vector3(30, 5, -12)) < 1e-9, "the eye");
  assert.equal(view.near, 2);
  assert.equal(view.far, 90);
  const cut = cutViewOf(view, 700, makeCutView());
  assert.ok(inside(cut, new Vector3(0, 0, 0)), "what it looks at is in view");
  assert.ok(!inside(cut, new Vector3(60, 10, -24)), "what is behind it is not");
  assert.ok(!inside(cut, new Vector3(-80, -10, 32)), "nor what is past its far plane");
});

test("switching cameras leaves nothing of the last one behind", () => {
  // Free, then the stage camera, then the free camera moved: the view and
  // its cut are a fresh camera's at the free camera's pose, every time.
  const free = freeCamera([10, 4, 20], [0, 1, 0]);
  const view = makeViewCamera();
  const cut = makeCutView();
  followCamera(view, free);
  cutViewOf(view, 700, cut);
  const { worldToEye, projection, near, far } = stageMatrices();
  followMatrices(view, worldToEye, projection, near, far);
  cutViewOf(view, 700, cut);
  free.position.set(-15, 8, 5);
  free.lookAt(0, 1, 0);
  free.fov = 60;
  free.near = 0.1;
  free.far = 900;
  free.updateProjectionMatrix();
  free.updateMatrixWorld();
  followCamera(view, free);
  const fresh = freeCamera([-15, 8, 5], [0, 1, 0], 60);
  fresh.near = 0.1;
  fresh.far = 900;
  fresh.updateProjectionMatrix();
  assertSameCut(cutViewOf(view, 700, cut), cutViewOf(fresh, 700, makeCutView()));
  assertMatrix(view.projectionMatrix, fresh.projectionMatrix, "projection");
  // And back to the stage camera.
  followMatrices(view, worldToEye, projection, near, far);
  const stage = makeViewCamera();
  followMatrices(stage, worldToEye, projection, near, far);
  assertSameCut(cutViewOf(view, 700, cut), cutViewOf(stage, 700, makeCutView()));
});

test("the cut's view: its error scale, position and near plane", () => {
  const free = freeCamera([0, 0, 10], [0, 0, 0], 90);
  const cut = cutViewOf(free, 600, makeCutView());
  // `proj[1][1]` of a 90 degree field of view is 1: an error of 1 at distance 1 is half the height.
  assert.ok(Math.abs(cut.projectionY - 1) < 1e-12);
  assert.ok(Math.abs(cut.projectionScale - 300) < 1e-9);
  assert.ok(cut.position.distanceTo(new Vector3(0, 0, 10)) < 1e-12);
  assert.equal(cut.zNear, 0.5);
  assert.ok(inside(cut, new Vector3(0, 0, 0)));
  assert.ok(!inside(cut, new Vector3(0, 0, 9.9)), "nearer than the near plane");
  // A bare camera has no near plane: the cut takes a hundredth of a unit.
  const bare = new Camera();
  bare.projectionMatrix.copy(free.projectionMatrix);
  bare.matrixWorld.copy(free.matrixWorld);
  bare.matrixWorldInverse.copy(free.matrixWorldInverse);
  assert.equal(cutViewOf(bare, 600, makeCutView()).zNear, 0.01);
});
