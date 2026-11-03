// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's lights as Storm draws them (`HdStLight::_ApproximateAreaLight`).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { LightEmission, LightGeometry, LightItem, LightShaping } from "@forest-rs/layerstack_stagehand";
import { stormArea, stormLight } from "../src/viewport/sceneLights.ts";

/** A light of `geometry`, emission as given over the `UsdLux` fallbacks. */
function light(geometry: LightGeometry, emission: Partial<LightEmission> = {}, shaping: Partial<LightShaping> | null = null, hidden = false): LightItem {
  return {
    geometry,
    hidden,
    emission: { color: [1, 1, 1], intensity: 1, exposure: 0, normalize: false, colorTemperature: null, temperatureColor: [1, 1, 1], diffuse: 1, specular: 1, sizeFactor: 1, radiance: [1, 1, 1], ...emission },
    shaping: shaping === null ? null : { coneAngle: 90, coneSoftness: 0, focus: 0, focusTint: [0, 0, 0], iesFile: null, iesAngleScale: 0, iesNormalize: false, ...shaping },
  } as unknown as LightItem;
}

const close = (actual: number, expected: number, what = "") => assert.ok(Math.abs(actual - expected) <= 1e-9 * Math.max(1, Math.abs(expected)), `${what}: ${actual} vs ${expected}`);

describe("stormLight", () => {
  it("gives a distant light the solid angle of its cone, as Storm does", () => {
    // light.cpp:198-206: 2π (1 − cos(angle / 2)), the angle in degrees.
    const sun = stormLight(light({ kind: "distant", angle: 1 }, { intensity: 1000 }))!;
    assert.equal(sun.kind, "directional");
    close(sun.intensity, 1000 * 2 * Math.PI * (1 - Math.cos(Math.PI / 360)), "angle 1°");
    // Not the first-order π sin²(angle / 2): they part at large angles.
    close(stormArea({ kind: "distant", angle: 180 }), 2 * Math.PI, "a hemisphere");
    assert.equal(stormLight(light({ kind: "distant", angle: 0 })), null, "angle 0 unnormalized: Storm's area is 0");
  });

  it("leaves the area out of a normalized light", () => {
    close(stormLight(light({ kind: "distant", angle: 0 }, { intensity: 3, normalize: true }))!.intensity, 3, "distant");
    close(stormLight(light({ kind: "sphere", radius: 2, treatAsPoint: false }, { intensity: 3, normalize: true }))!.intensity, 3, "sphere");
  });

  it("scales a point light by its authored area", () => {
    close(stormLight(light({ kind: "sphere", radius: 0.5, treatAsPoint: false }, { intensity: 8 }))!.intensity, 8 * Math.PI * 0.25, "sphere: π r²");
    close(stormLight(light({ kind: "cylinder", radius: 0.5, length: 4, treatAsLine: false }, { intensity: 8 }))!.intensity, 8 * 4 * 0.5, "cylinder: length × radius");
    const rect = stormLight(light({ kind: "rect", width: 2, height: 3, texture: null, file: null }, { intensity: 5 }))!;
    assert.equal(rect.kind, "spot");
    close(rect.intensity, 30, "rect: width × height");
    close(stormLight(light({ kind: "disk", radius: 1 }, { intensity: 2 }))!.intensity, 2 * Math.PI, "disk: π r²");
  });

  it("makes a rect or a disk a spot of its cone angle, 90° without shaping", () => {
    const plain = stormLight(light({ kind: "disk", radius: 1 }))!;
    assert.ok(plain.kind === "spot");
    close(plain.angle, Math.PI / 2, "default cone");
    const shaped = stormLight(light({ kind: "rect", width: 1, height: 1, texture: null, file: null }, {}, { coneAngle: 30 }))!;
    assert.ok(shaped.kind === "spot");
    close(shaped.angle, Math.PI / 6, "shaped cone");
  });

  it("applies exposure and colors, keeping the largest channel as the intensity", () => {
    const tinted = stormLight(light({ kind: "sphere", radius: 1, treatAsPoint: false }, { intensity: 1, exposure: 2, color: [0.5, 1, 0.25], temperatureColor: [1, 0.5, 1], normalize: true }))!;
    // color × blackbody: (0.5, 0.5, 0.25), × 2² = (2, 2, 1).
    close(tinted.intensity, 2, "peak");
    assert.deepEqual(tinted.color, [1, 1, 0.5]);
  });

  it("lights a dome with its color, no area", () => {
    const dome = stormLight(light({ kind: "dome", texture: null, file: null, format: "automatic", poleAxis: null }, { intensity: 0.5 }))!;
    assert.equal(dome.kind, "dome");
    close(dome.intensity, 0.5);
  });

  it("draws nothing for a hidden, black or shapeless light", () => {
    assert.equal(stormLight(light({ kind: "disk", radius: 1 }, {}, null, true)), null);
    assert.equal(stormLight(light({ kind: "disk", radius: 1 }, { intensity: 0 })), null);
    assert.equal(stormLight(light({ kind: "portal", width: 1, height: 1 })), null);
    assert.equal(stormLight(light({ kind: "mesh", area: 2 })), null);
  });
});
