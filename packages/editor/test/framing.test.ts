// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Framing: where the free camera stands to fit a box, and the clipping
// range it keeps. A box that is not one frames nothing, a box of no size
// is framed in its surroundings, framing the same box again lands in the
// same place, and nothing it returns is NaN.
import assert from "node:assert/strict";
import { test } from "node:test";
import { clipRange, frameBox, isBox, localView, MAX_DEPTH_RATIO, orbitFrom, reachFrom, shouldFrame, upVector, verticalFov, viewDirection } from "../src/viewport/framing.ts";

const options = { fov: 45, aspect: 16 / 9, direction: viewDirection("Y") } as const;
const finite = (values: readonly number[]) => values.every(Number.isFinite);

test("an empty, inverted, NaN or infinite box frames nothing", () => {
  // `GfRange3d`'s empty range, as the stage's bounds give it.
  const empty = [Number.MAX_VALUE, Number.MAX_VALUE, Number.MAX_VALUE, -Number.MAX_VALUE, -Number.MAX_VALUE, -Number.MAX_VALUE];
  for (const box of [empty, [1, 0, 0, 0, 1, 1], [Number.NaN, 0, 0, 1, 1, 1], [-Infinity, 0, 0, 1, 1, 1], [0, 0, 0, 1, 1], []]) {
    assert.equal(isBox(box), false, String(box));
    assert.equal(frameBox(box, options), null, String(box));
  }
  assert.equal(frameBox(null, options), null);
});

test("a box frames at its center, its sphere inside the narrower angle", () => {
  const framing = frameBox([-1, -1, -1, 1, 1, 1], options)!;
  assert.deepEqual(framing.center, [0, 0, 0]);
  assert.equal(framing.radius, Math.sqrt(3));
  const half = (45 * Math.PI) / 360;
  assert.ok(Math.abs(framing.distance - (1.1 * Math.sqrt(3)) / Math.tan(half)) < 1e-12);
  const [x, y, z] = framing.position;
  assert.ok(Math.abs(Math.hypot(x, y, z) - framing.distance) < 1e-9);
  // Taller than wide, the width decides.
  const narrow = frameBox([-1, -1, -1, 1, 1, 1], { ...options, aspect: 0.5 })!;
  assert.ok(narrow.distance > framing.distance);
  assert.ok(framing.near > 0 && framing.near < framing.distance - framing.radius);
  assert.ok(framing.far > framing.distance + framing.radius);
});

test("framing the same box again lands in the same place", () => {
  const box = [2, 0, -3, 4, 1, -1];
  assert.deepEqual(frameBox(box, options), frameBox(box, options));
});

test("a box of no size is framed at the fallback radius, not magnified to nothing", () => {
  const point = frameBox([1, 2, 3, 1, 2, 3], { ...options, fallbackRadius: 0.5 })!;
  assert.deepEqual(point.center, [1, 2, 3]);
  assert.equal(point.radius, 0.5);
  assert.ok(finite([...point.position, point.near, point.far, point.distance]));
  // Without one, a unit radius.
  assert.equal(frameBox([5, 5, 5, 5, 5, 5], options)!.radius, 1);
  // A bad fallback is ignored.
  assert.equal(frameBox([5, 5, 5, 5, 5, 5], { ...options, fallbackRadius: Number.NaN })!.radius, 1);
});

test("the far plane reaches the whole scene around something small", () => {
  const scene = [-100, 0, -100, 100, 20, 100];
  const framing = frameBox([0, 0, 0, 0.1, 0.1, 0.1], { ...options, scene })!;
  assert.ok(framing.far >= reachFrom(framing.position, scene));
  assert.ok(framing.far / framing.near <= MAX_DEPTH_RATIO * (1 + 1e-12));
});

test("the clipping range follows the distance and keeps its ratio", () => {
  assert.deepEqual(clipRange(10), { near: 0.1, far: 100 });
  assert.deepEqual(clipRange(10, 1000), { near: 0.1, far: 1050 });
  // Zoomed far in on a huge scene, far is brought in rather than near pushed past the target.
  const close = clipRange(0.001, 1e9)!;
  assert.equal(close.near, 0.00001);
  assert.equal(close.far, close.near * MAX_DEPTH_RATIO);
  for (const distance of [0, -1, Number.NaN, Infinity]) assert.equal(clipRange(distance), null);
  assert.deepEqual(clipRange(10, Number.NaN), { near: 0.1, far: 100 });
});

test("the view direction is the same view about either up axis", () => {
  const [x, y, z] = viewDirection("Y");
  const [zx, zy, zz] = viewDirection("Z");
  // Z-up is Y-up rotated a quarter turn about x: (x, y, z) to (x, -z, y).
  assert.ok(Math.abs(zx - x) < 1e-12 && Math.abs(zy + z) < 1e-12 && Math.abs(zz - y) < 1e-12);
  assert.ok(Math.abs(Math.hypot(x, y, z) - 1) < 1e-12);
});

/** Whether two directions agree to within rounding. */
const same = (a: readonly number[], b: readonly number[]) => a.length === b.length && a.every((value, at) => Math.abs(value - b[at]!) < 1e-9);

test("a prim's own space sees the stage's up as its transform turns it", () => {
  const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  for (const up of ["Y", "Z"] as const) {
    const view = localView(up, identity);
    assert.ok(same(view.up, upVector(up)) && same(view.direction, viewDirection(up)), up);
  }
  assert.deepEqual(localView("Z", null), { up: upVector("Z"), direction: viewDirection("Z") });
  // A Z-up asset turned to stand in a Y-up stage (rows: x to x, y to -z, z to y): its up is its own +z,
  // and it is seen as an asset authored Z-up is in a Z-up stage.
  const zUpInYUp = [1, 0, 0, 0, 0, 0, -1, 0, 0, 1, 0, 0, 5, 6, 7, 1];
  const view = localView("Y", zUpInYUp);
  assert.ok(same(view.up, [0, 0, 1]), String(view.up));
  assert.ok(same(view.direction, viewDirection("Z")), String(view.direction));
  // Scale does not matter; a singular matrix leaves the world's view.
  assert.ok(same(localView("Y", [2, 0, 0, 0, 0, 0, -2, 0, 0, 2, 0, 0, 0, 0, 0, 1]).up, [0, 0, 1]));
  assert.deepEqual(localView("Y", new Array(16).fill(0)), { up: upVector("Y"), direction: viewDirection("Y") });
});

test("taking over from a stage camera orbits a point on its view axis", () => {
  const scene = [-10, -10, -10, 10, 10, 10];
  // Looking down -z from z = 30 at the scene: its center, 30 ahead.
  const ahead = orbitFrom([0, 0, 30], [0, 0, -1], 0, scene)!;
  assert.ok(same(ahead.target, [0, 0, 0]));
  assert.equal(ahead.distance, 30);
  assert.ok(ahead.far >= reachFrom([0, 0, 30], scene));
  // Off to the side, the center's projection on the axis.
  assert.ok(same(orbitFrom([5, 0, 30], [0, 0, -2], 0, scene)!.target, [5, 0, 0]));
  // An authored focus distance wins.
  assert.equal(orbitFrom([0, 0, 30], [0, 0, -1], 12, scene)!.distance, 12);
  // Facing away from the scene: half its radius ahead; with no scene, one unit.
  assert.ok(Math.abs(orbitFrom([0, 0, 30], [0, 0, 1], 0, scene)!.distance - Math.sqrt(300) / 2) < 1e-12);
  assert.equal(orbitFrom([0, 0, 30], [0, 0, 1], 0, null)!.distance, 1);
  assert.equal(orbitFrom([Number.NaN, 0, 0], [0, 0, 1], 0, scene), null);
  assert.equal(orbitFrom([0, 0, 0], [0, 0, 0], 0, scene), null);
});

test("a stage camera's window gives its vertical field of view", () => {
  // 24 mm focal length, 20.25 mm vertical aperture: a half height of 10.125 / 24 at depth 1.
  const half = 10.125 / 24;
  assert.ok(Math.abs(verticalFov([-0.75, -half, 0.75, half])! - (2 * Math.atan(half) * 180) / Math.PI) < 1e-9);
  // An offset window spans the same angle between its edges.
  assert.ok(Math.abs(verticalFov([-1, 0, 1, 1])! - 45) < 1e-9);
  assert.equal(verticalFov([-1, 0, 1, 0]), null);
});

test("a scene is framed when it opens, and an edit keeps the view the person made", () => {
  const open = { framed: "", moved: false, timeChanged: false };
  assert.equal(shouldFrame(open, null), false, "nothing to frame yet");
  assert.equal(shouldFrame(open, "[0,0,0,1,1,1]"), true, "framed on open");
  // Bounds that grow as the scene loads are followed until the person takes the camera.
  const loading = { framed: "[0,0,0,1,1,1]", moved: false, timeChanged: false };
  assert.equal(shouldFrame(loading, "[0,0,0,2,2,2]"), true);
  assert.equal(shouldFrame(loading, "[0,0,0,1,1,1]"), false, "the same bounds again");
  // An edit changes the bounds; a view the person orbited stays.
  assert.equal(shouldFrame({ ...loading, moved: true }, "[0,0,0,2,2,2]"), false);
  // But a scene that was never framed is framed, moved or not.
  assert.equal(shouldFrame({ framed: "", moved: true, timeChanged: false }, "[0,0,0,1,1,1]"), true);
  // Time moving an animated extent never moves the camera, once framed.
  assert.equal(shouldFrame({ ...loading, timeChanged: true }, "[0,0,0,2,2,2]"), false);
  assert.equal(shouldFrame({ framed: "", moved: false, timeChanged: true }, "[0,0,0,1,1,1]"), true);
});
