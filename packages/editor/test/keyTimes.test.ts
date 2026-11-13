// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The timeline's key arithmetic: which key a jump reaches, which ticks are
// drawn for a width, and what the bracketing samples say.
import assert from "node:assert/strict";
import { test } from "node:test";
import { bracketNote, heldBetweenSamples, mergeTimes, nextKey, thinTicks } from "../src/model/keyTimes.ts";

test("nextKey finds the key past the time: on a key, between keys, before the first, after the last, with none", () => {
  const keys = [0, 12, 30, 48];
  assert.equal(nextKey(keys, 12, 1), 30, "on a key, the next one past it");
  assert.equal(nextKey(keys, 12, -1), 0);
  assert.equal(nextKey(keys, 20, 1), 30, "between keys");
  assert.equal(nextKey(keys, 20, -1), 12);
  assert.equal(nextKey(keys, -5, 1), 0, "before the first");
  assert.equal(nextKey(keys, -5, -1), null);
  assert.equal(nextKey(keys, 60, -1), 48, "after the last");
  assert.equal(nextKey(keys, 60, 1), null);
  assert.equal(nextKey(keys, 48, 1), null);
  assert.equal(nextKey(keys, 0, -1), null);
  assert.equal(nextKey([], 5, 1), null, "no keys");
  assert.equal(nextKey([], 5, -1), null);
  assert.equal(nextKey([0.5, 1.5], 0.5 + 1e-9, 1), 1.5, "a time a rounding error off a key is on it");
});

test("mergeTimes keeps both lists ascending and each time once", () => {
  assert.deepEqual(mergeTimes([0, 4, 8], [4, 6, 9]), [0, 4, 6, 8, 9]);
  assert.deepEqual(mergeTimes([], [1, 2]), [1, 2]);
  assert.deepEqual(mergeTimes([], []), []);
});

test("thinTicks draws every key that has a pixel to itself", () => {
  const ticks = thinTicks([0, 25, 50, 100], 0, 100, 101);
  assert.deepEqual(ticks, [
    { time: 0, x: 0 },
    { time: 25, x: 25 },
    { time: 50, x: 50 },
    { time: 100, x: 100 },
  ]);
});

test("thinTicks draws at most one tick per pixel and keeps the first and the last key", () => {
  const times = Array.from({ length: 10001 }, (_, index) => index / 100);
  for (const width of [1, 2, 7, 100, 333]) {
    const ticks = thinTicks(times, 0, 100, width);
    assert.ok(ticks.length <= width, `${ticks.length} ticks in ${width} pixels`);
    assert.equal(new Set(ticks.map((tick) => tick.x)).size, ticks.length, "a pixel once");
    assert.equal(ticks[0]!.time, 0, "the first key");
    if (width > 1) assert.equal(ticks.at(-1)!.time, 100, "the last key");
    const xs = ticks.map((tick) => tick.x);
    assert.deepEqual(xs, [...xs].sort((a, b) => a - b), "left to right");
    for (const x of xs) assert.ok(x >= 0 && x <= width - 1);
  }
});

test("thinTicks lets the last key take the pixel it shares, and never takes the first key's", () => {
  assert.deepEqual(thinTicks([0, 99.99, 100], 0, 100, 101).map((tick) => tick.time), [0, 100]);
  assert.deepEqual(thinTicks([0, 0.1], 0, 100, 101).map((tick) => tick.time), [0], "the first is never replaced");
});

test("thinTicks leaves out keys outside the range, and draws nothing for nothing", () => {
  assert.deepEqual(thinTicks([-5, 10, 200], 0, 100, 100).map((tick) => tick.time), [10]);
  assert.deepEqual(thinTicks([], 0, 100, 100), []);
  assert.deepEqual(thinTicks([5], 0, 100, 0), []);
});

test("bracketNote says where a time falls among the samples", () => {
  assert.deepEqual(bracketNote({ kind: "samples", lower: 12, upper: 12 }, 12, "double"), { kind: "on", time: 12 });
  assert.deepEqual(bracketNote({ kind: "samples", lower: 0, upper: 0 }, -3, "double"), { kind: "before", time: 0 });
  assert.deepEqual(bracketNote({ kind: "samples", lower: 48, upper: 48 }, 60, "double"), { kind: "after", time: 48 });
  assert.deepEqual(bracketNote({ kind: "samples", lower: 0, upper: 48 }, 10, "double"), { kind: "between", lower: 0, upper: 48, held: false });
  assert.deepEqual(bracketNote({ kind: "samples", lower: 0, upper: 48 }, 10, "token"), { kind: "between", lower: 0, upper: 48, held: true });
  assert.deepEqual(bracketNote({ kind: "noSamples" }, 10, "double"), { kind: "none" });
  assert.deepEqual(bracketNote({ kind: "noValue" }, 10, "double"), { kind: "none" });
});

test("only the types USD never interpolates are held", () => {
  for (const held of ["bool", "int", "int[]", "uint64", "token", "token[]", "string", "asset"]) assert.ok(heldBetweenSamples(held), held);
  for (const moved of ["float", "double", "double3", "float3[]", "matrix4d", "quatf", "color3f"]) assert.ok(!heldBetweenSamples(moved), moved);
});
