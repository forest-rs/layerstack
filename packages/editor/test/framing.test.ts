// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Framing: where the free camera stands to fit a box, and the clipping
// range it keeps. A box that is not one frames nothing, a box of no size
// is framed in its surroundings, framing the same box again lands in the
// same place, and nothing it returns is NaN.
import assert from "node:assert/strict";
import { test } from "node:test";
import { clipRange, frameBox, isBox, MAX_DEPTH_RATIO, reachFrom, viewDirection } from "../src/viewport/framing.ts";

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
