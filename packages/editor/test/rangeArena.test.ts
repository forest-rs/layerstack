// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The clustered draws' range allocator: first fit, merged as ranges come
// back, the high-water mark lowered by a range freed at the end, and a
// request that fits nowhere refused rather than grown.
import assert from "node:assert/strict";
import { test } from "node:test";
import { RangeArena } from "../src/viewport/rangeArena.ts";

test("ranges are handed out in order, and a freed one is reused first fit", () => {
  const arena = new RangeArena(100);
  assert.equal(arena.allocate(10), 0);
  assert.equal(arena.allocate(20), 10);
  assert.equal(arena.allocate(5), 30);
  arena.free(10, 20);
  assert.deepEqual(arena.use, { capacity: 100, used: 15, end: 35, holes: 20 });
  // Fits the hole: taken from its start.
  assert.equal(arena.allocate(8), 10);
  assert.equal(arena.allocate(12), 18);
  assert.deepEqual(arena.use, { capacity: 100, used: 35, end: 35, holes: 0 });
});

test("neighbouring free ranges merge, and one reaching the end lowers it", () => {
  const arena = new RangeArena(100);
  const [a, b, c, d] = [10, 10, 10, 10].map((length) => arena.allocate(length));
  arena.free(a!, 10);
  arena.free(c!, 10);
  // b between two holes: one hole of thirty.
  arena.free(b!, 10);
  assert.equal(arena.allocate(30), 0);
  arena.free(0, 30);
  // d is at the end: freeing it frees everything.
  arena.free(d!, 10);
  assert.deepEqual(arena.use, { capacity: 100, used: 0, end: 0, holes: 0 });
});

test("a request nothing fits is refused, though compacting would make room", () => {
  const arena = new RangeArena(30);
  arena.allocate(10);
  arena.allocate(10);
  arena.allocate(10);
  arena.free(0, 10);
  assert.equal(arena.allocate(15), -1);
  assert.equal(arena.fitsCompacted(10), true);
  assert.equal(arena.fitsCompacted(15), false);
  assert.equal(arena.allocate(10), 0);
});

test("a range of nothing takes nothing; ranges not handed out are refused", () => {
  const arena = new RangeArena(10);
  assert.equal(arena.allocate(0), 0);
  arena.free(0, 0);
  assert.equal(arena.used, 0);
  arena.allocate(4);
  assert.throws(() => arena.free(2, 4), RangeError);
  arena.allocate(4);
  arena.free(0, 4);
  assert.throws(() => arena.free(1, 2), RangeError, "free already");
});
