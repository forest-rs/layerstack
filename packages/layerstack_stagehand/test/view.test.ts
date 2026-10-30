// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The draw list's cameras and lights: their values against the OpenUSD
// oracles of `layerstack_conformance`, and the same diffs and work counts
// as `layerstack_stagehand`'s light tests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit, Stage } from "@forest-rs/layerstack";
import { conformCamera, loadStagehand } from "../dist/index.js";
import type { DrawDiff, DrawList, LightGeometry } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();
const fixtures = new URL("../../../layerstack_conformance/fixtures/", import.meta.url);

/** The scene of `layerstack_stagehand`'s light tests. */
const SCENE = `#usda 1.0
def Xform "World" {
    def Camera "Eye" {
        float focalLength = 35
        float2 clippingRange = (0.1, 100)
        double3 xformOp:translate.timeSamples = {0: (0, 1, 8), 10: (0, 1, 4)}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "Rig" {
        double3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:scale"]
        def SphereLight "Bulb" {
            float inputs:radius = 0.5
            float inputs:intensity = 8
            float inputs:exposure = 1
            bool inputs:normalize = true
            double3 xformOp:translate = (0, 1, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def RectLight "Panel" {
        float inputs:width = 2
        float inputs:height = 0.5
        bool inputs:normalize = true
        asset inputs:texture:file = @panel.png@
        double3 xformOp:scale = (3, 2, 1)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }
    def DistantLight "Sun" (
        prepend apiSchemas = ["ShadowAPI"]
    ) {
        bool inputs:enableColorTemperature = true
        float inputs:colorTemperature = 4000
        float inputs:intensity.timeSamples = {0: 1000, 10: 3000}
        float3 xformOp:rotateXYZ = (-90, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
    }
    def DiskLight "Spot" (
        prepend apiSchemas = ["ShapingAPI"]
    ) {
        float inputs:shaping:cone:angle = 30
        float inputs:shaping:cone:softness = 0.25
        rel collection:lightLink:includes = </World/Rig>
        uniform bool collection:lightLink:includeRoot = false
    }
    def DomeLight_1 "Sky" {
        asset inputs:texture:file = @sky.exr@
        token inputs:texture:format = "latlong"
    }
    def CylinderLight "Tube" {
        float inputs:length = 2
        float inputs:radius = 0.25
        bool inputs:normalize = true
        bool treatAsLine = true
        double3 xformOp:scale = (2, 3, 3)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }
}
`;

function fixture(): { stage: Stage; list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { time: 0 });
  const first = list.refresh();
  return {
    stage,
    list,
    first,
    edit(edits) {
      stage.apply(edits, stage.root);
      const diffs = heard.splice(0).map((changes) => list.applyChanges(changes));
      assert.equal(diffs.length, 1);
      return diffs[0]!;
    },
  };
}

function close(actual: number, expected: number, tolerance = 1e-12): void {
  assert.ok(Math.abs(actual - expected) <= tolerance * Math.max(1, Math.abs(expected)), `${actual} != ${expected}`);
}

describe("a draw list's cameras and lights", () => {
  it("are listed with renderer-ready values", () => {
    const { stage, list, first } = fixture();
    try {
      assert.equal(first.lightsAdded.length, 6);
      assert.deepEqual(first.camerasAdded, ["/World/Eye"]);
      assert.equal(first.work.lightsRead, 6);
      assert.equal(first.work.camerasRead, 1);

      const bulb = list.light("/World/Rig/Bulb")!;
      assert.equal(bulb.kind, "sphere");
      assert.deepEqual(bulb.position, [0, 2, 0]);
      close(bulb.emission.sizeFactor, 4 * Math.PI);
      close(bulb.emission.radiance[0], (8 * 2) / (4 * Math.PI));

      const panel = list.light("/World/Panel")!;
      close(panel.emission.sizeFactor, 6);
      assert.deepEqual(panel.direction, [-0, -0, -1]);
      assert.equal((panel.geometry as Extract<LightGeometry, { kind: "rect" }>).texture, "panel.png");

      const sun = list.light("/World/Sun")!;
      assert.ok(sun.direction[1] < -0.999999, `${sun.direction}`);
      assert.equal(sun.emission.colorTemperature, 4000);
      const rgb = layerstack.blackbodyTemperatureRgb(4000);
      assert.deepEqual(sun.emission.temperatureColor, [...rgb]);
      close(sun.emission.radiance[2], rgb[2]! * 1000);
      assert.equal(sun.emission.sizeFactor, 1);
      assert.ok(sun.shadow?.enable && sun.shadow.distance === -1);
      assert.equal(sun.shaping, null);

      const spot = list.light("/World/Spot")!;
      assert.equal(spot.shaping?.coneAngle, 30);
      assert.equal(spot.shaping?.coneSoftness, 0.25);
      assert.equal(spot.lightLink.includeRoot, false);
      assert.deepEqual(spot.lightLink.includes, ["/World/Rig"]);
      assert.equal(spot.shadowLink.includeRoot, true);

      const sky = list.light("/World/Sky")!.geometry as Extract<LightGeometry, { kind: "dome" }>;
      assert.deepEqual([sky.texture, sky.format, sky.poleAxis], ["sky.exr", "latlong", "scene"]);

      const tube = list.light("/World/Tube")!;
      close(tube.emission.sizeFactor, 2 * Math.PI * 0.75 * 4);
      assert.ok(tube.geometry.kind === "cylinder" && tube.geometry.treatAsLine);

      const eye = list.camera("/World/Eye")!;
      assert.equal(eye.camera!.view[14], -8);
      assert.ok(Object.isFrozen(bulb) && Object.isFrozen(bulb.emission) && Object.isFrozen(eye));
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("read only the light an edit reaches", () => {
    const { stage, list, edit } = fixture();
    try {
      let diff = edit([{ type: "setValue", path: "/World/Rig/Bulb.inputs:intensity", value: { type: "float", value: 16 } }]);
      assert.deepEqual(diff.lightsChanged, ["/World/Rig/Bulb"]);
      assert.equal(diff.work.lightsRead, 1);
      assert.equal(diff.work.camerasRead, 0);
      assert.equal(diff.work.itemsRead, 0);
      close(list.light("/World/Rig/Bulb")!.emission.radiance[0], (16 * 2) / (4 * Math.PI));
      diff = edit([{ type: "setValue", path: "/World/Rig.xformOp:scale", value: { type: "double3", value: [3, 3, 3] } }]);
      assert.deepEqual(diff.lightsChanged, ["/World/Rig/Bulb"]);
      assert.equal(diff.work.lightsRead, 1);
      // A property the light does not read: nothing read.
      diff = edit([{ type: "createAttribute", path: "/World/Rig/Bulb.flicker", typeName: "double", custom: true }]);
      assert.equal(diff.work.lightsRead, 0);
      assert.deepEqual(diff.lightsChanged, []);
      diff = edit([{ type: "definePrim", path: "/World/Lamp", typeName: "SphereLight" }]);
      assert.deepEqual(diff.lightsAdded, ["/World/Lamp"]);
      assert.equal(diff.work.lightsRead, 1);
      diff = edit([{ type: "removePrim", path: "/World/Lamp" }]);
      assert.deepEqual(diff.lightsRemoved, ["/World/Lamp"]);
      assert.equal(list.light("/World/Lamp"), null);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("read at a new time only what might vary", () => {
    const { stage, list } = fixture();
    try {
      const diff = list.setTime(10);
      assert.deepEqual(diff.lightsChanged, ["/World/Sun"]);
      assert.deepEqual(diff.camerasChanged, ["/World/Eye"]);
      assert.deepEqual([diff.work.lightsRead, diff.work.camerasRead], [1, 1]);
      assert.equal(list.light("/World/Sun")!.emission.intensity, 3000);
      assert.equal(list.camera("/World/Eye")!.camera!.view[14], -4);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  // `stagehand_lights/oracle.json`: every light at four times, against
  // what usd-core answers (see `stagehand_lights_oracle.py`).
  it("match OpenUSD's answers", () => {
    const base = new URL("stagehand_lights/", fixtures);
    const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8"));
    const stage = layerstack.open({ identifier: "scene.usda", bytes: readFileSync(new URL("scene.usda", base)) });
    const list = stagehand.drawList(stage);
    try {
      list.refresh();
      assert.equal(oracle.rows.length, 36);
      for (const row of oracle.rows) {
        const time = row.time ?? undefined;
        if (list.time !== time) list.setTime(time);
        const where = `${row.path} ${time}`;
        const light = list.light(row.path)!;
        const inputs = row.inputs;
        const f = (value: number) => Math.fround(value);
        row.world.flat().forEach((value: number, i: number) => close(light.matrix[i]!, value));
        assert.equal(light.hidden, row.visibility === "invisible", where);
        const emission = light.emission;
        assert.deepEqual([...emission.color], inputs.color, where);
        assert.equal(emission.intensity, inputs.intensity, where);
        assert.equal(emission.exposure, inputs.exposure, where);
        assert.equal(emission.normalize, inputs.normalize, where);
        assert.equal(emission.diffuse, inputs.diffuse, where);
        assert.equal(emission.specular, inputs.specular, where);
        if (inputs.enableColorTemperature) {
          assert.equal(emission.colorTemperature, inputs.colorTemperature, where);
          assert.deepEqual([...emission.temperatureColor], row.blackbody.map(f), where);
        } else {
          assert.equal(emission.colorTemperature, null, where);
        }
        const geometry = light.geometry;
        switch (geometry.kind) {
          case "distant":
            assert.equal(geometry.angle, inputs.angle, where);
            break;
          case "sphere":
            assert.deepEqual([geometry.radius, geometry.treatAsPoint], [inputs.radius, row.attributes.treatAsPoint], where);
            break;
          case "rect":
            assert.deepEqual([geometry.width, geometry.height, geometry.texture], [inputs.width, inputs.height, inputs["texture:file"]], where);
            break;
          case "disk":
            assert.equal(geometry.radius, inputs.radius, where);
            break;
          case "cylinder":
            assert.deepEqual(
              [geometry.radius, geometry.length, geometry.treatAsLine],
              [inputs.radius, inputs.length, row.attributes.treatAsLine],
              where,
            );
            break;
          case "dome":
            assert.deepEqual(
              [geometry.texture, geometry.format, geometry.poleAxis],
              [inputs["texture:file"], inputs["texture:format"], row.attributes.poleAxis ?? null],
              where,
            );
            break;
          case "portal":
            assert.deepEqual([geometry.width, geometry.height], [inputs.width, inputs.height], where);
            break;
        }
        assert.equal(light.shaping !== null, row.shaping, where);
        if (light.shaping !== null) {
          const s = light.shaping;
          assert.deepEqual(
            [s.coneAngle, s.coneSoftness, s.focus, [...s.focusTint], s.iesFile, s.iesAngleScale, s.iesNormalize],
            [
              inputs["shaping:cone:angle"],
              inputs["shaping:cone:softness"],
              inputs["shaping:focus"],
              inputs["shaping:focusTint"],
              inputs["shaping:ies:file"],
              inputs["shaping:ies:angleScale"],
              inputs["shaping:ies:normalize"],
            ],
            where,
          );
        }
        assert.equal(light.shadow !== null, row.shadow, where);
        if (light.shadow !== null) {
          const s = light.shadow;
          assert.deepEqual(
            [s.enable, [...s.color], s.distance, s.falloff, s.falloffGamma],
            [
              inputs["shadow:enable"],
              inputs["shadow:color"],
              inputs["shadow:distance"],
              inputs["shadow:falloff"],
              inputs["shadow:falloffGamma"],
            ],
            where,
          );
        }
        for (const [link, name] of [
          [light.lightLink, "lightLink"],
          [light.shadowLink, "shadowLink"],
        ] as const) {
          assert.deepEqual({ ...link, includes: [...link.includes], excludes: [...link.excludes] }, row.links[name], where);
        }
      }
    } finally {
      list.dispose();
      stage.free();
    }
  });

  // `camera_conform.json`: `CameraUtilConformWindow` through OpenUSD's
  // C++, every policy at five aspects.
  it("conform to a viewport as OpenUSD does", () => {
    const stage = layerstack.open({ identifier: "camera.usda", bytes: readFileSync(new URL("camera.usda", fixtures)) });
    try {
      const oracle = JSON.parse(readFileSync(new URL("camera_conform.json", fixtures), "utf8"));
      for (const row of oracle.rows) {
        const camera = stage.camera(row.path, row.time === null ? {} : { time: row.time })!;
        const conformed = conformCamera(camera, row.policy, row.aspect);
        const where = `${row.path} ${row.time} ${row.policy} ${row.aspect}`;
        assert.deepEqual([...conformed.aperture], row.aperture.map(Math.fround), where);
        row.window.forEach((value: number, i: number) => close(conformed.window[i]!, value, 2e-10));
        row.projection.flat().forEach((value: number, i: number) => close(conformed.projectionMatrix[i]!, value, 2e-10));
        assert.deepEqual(conformed.view, camera.view);
        const native = stage.camera(row.path, { ...(row.time === null ? {} : { time: row.time }), conform: { policy: row.policy, aspect: row.aspect } })!;
        assert.deepEqual([...conformed.projectionMatrix], [...native.projectionMatrix], where);
      }
    } finally {
      stage.free();
    }
  });
});
