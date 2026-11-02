// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which lights light a linked draw: every light but those its links leave
// it out of, the viewport's own lights always; equal sets keyed alike.
import assert from "node:assert/strict";
import { test } from "node:test";
import { lightSetKey, linkedLights } from "../src/viewport/lightLinks.ts";

interface Light {
  readonly id: number;
  readonly key?: string;
}

const camera: Light = { id: 1 };
const ambient: Light = { id: 2 };
const sun: Light = { id: 7, key: "/World/Sun" };
const spot: Light = { id: 9, key: "/World/Spot" };
const all = [camera, ambient, sun, spot];
const keyOf = (light: Light) => light.key;

test("a draw left out of no light is lit by all of them, the same array", () => {
  assert.equal(linkedLights(all, keyOf, new Set()), all);
  assert.equal(linkedLights(all, keyOf, new Set(["/World/Elsewhere"])), all, "a light the view does not draw changes nothing");
});

test("a draw left out of a light is lit by the rest, the default lights always", () => {
  assert.deepEqual(linkedLights(all, keyOf, new Set(["/World/Spot"])), [camera, ambient, sun]);
  assert.deepEqual(linkedLights(all, keyOf, new Set(["/World/Spot", "/World/Sun"])), [camera, ambient]);
});

test("a light drawn as several three.js lights is left out whole", () => {
  const target: Light = { id: 10, key: "/World/Spot" };
  assert.deepEqual(linkedLights([...all, target], keyOf, new Set(["/World/Spot"])), [camera, ambient, sun]);
});

test("sets of the same lights in any order share a key", () => {
  assert.equal(lightSetKey([9, 1, 2]), lightSetKey([1, 2, 9]));
  assert.notEqual(lightSetKey([1, 2]), lightSetKey([1, 2, 9]));
  assert.equal(lightSetKey([10, 9]), "9 10", "ids sort as numbers");
});
