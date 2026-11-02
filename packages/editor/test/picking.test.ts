// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// GPU picking's ids: a run per object, one id per instance, packed into
// four bytes and found again; the pixels read nearest the click first;
// the projection narrowed to the pick window.
import assert from "node:assert/strict";
import { test } from "node:test";
import { MAX_PICK_ID, PickIds, packPickId, pickOrder, pickWindow, unpackPickId } from "../src/viewport/picking.ts";

test("pack and unpack every byte of an id, low byte first", () => {
  const pixels = new Uint8Array(8);
  for (const id of [0, 1, 255, 256, 65535, 0x01020304, 0x80000000, MAX_PICK_ID]) {
    packPickId(id, pixels, 4);
    assert.equal(unpackPickId(pixels, 4), id);
  }
  packPickId(0x01020304, pixels, 0);
  assert.deepEqual([...pixels.subarray(0, 4)], [4, 3, 2, 1], "red holds the low byte");
});

test("give each object a run of ids, one per instance, and find the instance again", () => {
  const ids = new PickIds();
  assert.equal(ids.add(7, 1), 1, "0 is the background");
  assert.equal(ids.add(9, 1000), 2);
  assert.equal(ids.add(4, 0), 1002, "an object drawing no instance still takes one id");
  assert.equal(ids.add(5, 3), 1003);
  assert.deepEqual(ids.find(1), { owner: 7, instance: 0 });
  assert.deepEqual(ids.find(2), { owner: 9, instance: 0 });
  assert.deepEqual(ids.find(1001), { owner: 9, instance: 999 });
  assert.deepEqual(ids.find(1002), { owner: 4, instance: 0 });
  assert.deepEqual(ids.find(1005), { owner: 5, instance: 2 });
  assert.equal(ids.find(0), null);
  assert.equal(ids.find(1006), null, "past the last run");
  ids.clear();
  assert.equal(ids.find(1), null);
  assert.equal(ids.add(3, 2), 1, "numbered from 1 again");
});

test("refuse a run past four bytes", () => {
  const ids = new PickIds();
  assert.equal(ids.add(1, MAX_PICK_ID - 1), 1);
  assert.equal(ids.add(2, 2), null);
  assert.equal(ids.add(3, 1), MAX_PICK_ID);
  assert.deepEqual(ids.find(MAX_PICK_ID), { owner: 3, instance: 0 });
});

test("read the center pixel first, then outward", () => {
  const order = pickOrder(7);
  assert.equal(order.length, 49);
  assert.equal(order[0], 24, "the center of a 7 × 7 target");
  assert.deepEqual([...order.subarray(1, 5)].sort((a, b) => a - b), [17, 23, 25, 31], "then its four neighbors");
  assert.equal(new Set(order).size, 49);
});

test("narrow the projection to the pixels about the point", () => {
  // The center of a 100 × 50 view, 10 pixels wide: clip x is scaled by 10.
  const m = pickWindow(50, 25, 10, 10, 100, 50);
  assert.deepEqual(m, [10, 0, 0, -0, 0, 5, 0, -0, 0, 0, 1, 0, 0, 0, 0, 1]);
  // The top left pixel's window: its center maps to clip (0, 0).
  const corner = pickWindow(0.5, 0.5, 1, 1, 100, 50);
  const ndcX = (0.5 / 100) * 2 - 1;
  const ndcY = 1 - (0.5 / 50) * 2;
  assert.ok(Math.abs(corner[0]! * ndcX + corner[3]!) < 1e-9);
  assert.ok(Math.abs(corner[5]! * ndcY + corner[7]!) < 1e-9);
});
