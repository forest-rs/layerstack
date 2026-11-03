// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A dome's texture as the sky and ground of a hemisphere light.

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { domeImage, latlongHemisphere } from "../src/viewport/domeLights.ts";

/** A `width` × `height` RGBA image: `top` over the upper half, `bottom` over the lower. */
function twoTone(width: number, height: number, top: readonly number[], bottom: readonly number[]): Uint8Array {
  const texels = new Uint8Array(4 * width * height);
  for (let row = 0; row < height; row++) {
    const color = row < height / 2 ? top : bottom;
    for (let column = 0; column < width; column++) texels.set([...color, 255], 4 * (row * width + column));
  }
  return texels;
}

const close = (actual: readonly number[], expected: readonly number[]) =>
  actual.forEach((value, i) => assert.ok(Math.abs(value - expected[i]!) < 1e-4, `${actual} vs ${expected}`));

describe("latlongHemisphere", () => {
  it("colors the sky with the upper half and the ground with the lower", () => {
    const texels = twoTone(8, 4, [255, 0, 51], [0, 255, 0]);
    const { sky, ground } = latlongHemisphere(texels, 8, 4, false, true);
    close(sky, [1, 0, 0.2]);
    close(ground, [0, 1, 0]);
  });

  it("decodes sRGB texels before averaging", () => {
    const { sky } = latlongHemisphere(twoTone(4, 4, [136, 172, 255], [0, 0, 0]), 4, 4, true, true);
    close(sky, [0.2462, 0.4125, 1]);
  });

  it("reads a turned image from its last row up", () => {
    const { sky, ground } = latlongHemisphere(twoTone(4, 4, [255, 255, 255], [0, 0, 0]), 4, 4, false, false);
    close(sky, [0, 0, 0]);
    close(ground, [1, 1, 1]);
  });

  it("weights rows by their solid angle and cosine to the pole", () => {
    // A sky bright only in its top quarter: rows near the pole weigh
    // sin φ cos φ, so it is the integral of that over [π/4, π/2] over the
    // half's, 1/2 (exact at many rows).
    const height = 2000;
    const texels = new Uint8Array(4 * height);
    for (let row = 0; row < height / 4; row++) texels.set([255, 255, 255, 255], 4 * row);
    const { sky } = latlongHemisphere(texels, 1, height, false, true);
    close([sky[0]], [0.5]);
  });
});

describe("domeImage", () => {
  const dome = (assetPath: string, identifier: string | null, format = "latlong") =>
    ({ kind: "dome", texture: assetPath, file: { assetPath, layer: "scene.usdz", identifier, udim: false }, format, poleAxis: null }) as const;

  it("reads a browser image, packaged or not", () => {
    assert.equal(domeImage(dome("./sky.png", "scene.usdz[sky.png]"))?.identifier, "scene.usdz[sky.png]");
    assert.equal(domeImage(dome("sky.JPG", "sky.JPG", "automatic"))?.identifier, "sky.JPG");
  });

  it("leaves out what a browser does not decode, and other layouts", () => {
    assert.equal(domeImage(dome("./sky.exr", "./sky.exr")), null);
    assert.equal(domeImage(dome("./sky.hdr", "scene.usdz[sky.hdr]")), null);
    assert.equal(domeImage(dome("./sky.png", "./sky.png", "angular")), null);
    assert.equal(domeImage(dome("./sky.png", null)), null);
  });
});
