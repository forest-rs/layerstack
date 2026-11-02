// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport's default lights: usdview's camera light and scene ambient,
// in three.js intensities that shade as Storm does.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  AMBIENT_INTENSITY,
  CAMERA_LIGHT_INTENSITY,
  DEFAULT_LIGHT_SETTINGS,
  MATERIAL_AMBIENT,
  SCENE_AMBIENT,
  defaultLights,
} from "../src/viewport/defaultLights.ts";

test("the camera light is on by default, as usdview's is", () => {
  assert.equal(DEFAULT_LIGHT_SETTINGS.cameraLight, true);
  assert.equal(defaultLights(DEFAULT_LIGHT_SETTINGS).cameraLight, CAMERA_LIGHT_INTENSITY);
});

test("switching the camera light off zeroes it and keeps the ambient", () => {
  const lights = defaultLights({ cameraLight: false });
  assert.equal(lights.cameraLight, 0);
  assert.equal(lights.ambient, AMBIENT_INTENSITY);
});

test("the ambient adds Storm's scene ambient term to a white surface", () => {
  // Storm: scene ambient × material ambient. three.js: intensity × albedo / π.
  const white = 1;
  assert.ok(Math.abs((AMBIENT_INTENSITY * white) / Math.PI - SCENE_AMBIENT * MATERIAL_AMBIENT) < 1e-12);
});
