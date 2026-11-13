// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Whether what a typed view reads might vary with time: what decides
// whether the light, camera, mesh, instancer and material views render
// again as the timeline plays.
import assert from "node:assert/strict";
import { test } from "node:test";
import { readsMightVary } from "../src/model/timeVarying.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

const EVERY = { attributes: null } as const;

test("the sample's static views hold still; an animated attribute, subset or transform above makes them vary", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const stage = h.store.session!.stage;
  // Static: a light's inputs, a mesh with its subsets, an instancer, a material's network.
  assert.equal(readsMightVary(stage, "/World/Sun", { attributes: ["inputs:intensity", "inputs:color"] }), false);
  assert.equal(readsMightVary(stage, "/World/Ground", { attributes: null, descendants: true }), false);
  assert.equal(readsMightVary(stage, "/World/Stones", EVERY), false);
  assert.equal(readsMightVary(stage, "/World/Looks/Checker", { attributes: null, descendants: true }), false);
  // The spinning top's own rotation.
  assert.equal(readsMightVary(stage, "/World/Top", EVERY), true);
  assert.equal(readsMightVary(stage, "/World/Top", { attributes: ["size"] }), false, "only what the view reads");
  assert.equal(readsMightVary(stage, "/World/Nowhere", EVERY), false);

  stage.apply([
    { type: "definePrim", path: "/World/Top/Child", typeName: "Sphere" },
    { type: "setValue", path: "/World/Sun.inputs:intensity", value: { type: "float", value: 1 }, time: 0 },
    { type: "setValue", path: "/World/Sun.inputs:intensity", value: { type: "float", value: 2 }, time: 10 },
    { type: "setValue", path: "/World/Looks/Checker/Surface.inputs:roughness", value: { type: "float", value: 0 }, time: 0 },
    { type: "setValue", path: "/World/Looks/Checker/Surface.inputs:roughness", value: { type: "float", value: 1 }, time: 10 },
  ]);
  assert.equal(readsMightVary(stage, "/World/Sun", { attributes: ["inputs:intensity", "inputs:color"] }), true);
  assert.equal(readsMightVary(stage, "/World/Looks/Checker", EVERY), false, "the material's own attributes hold still");
  assert.equal(readsMightVary(stage, "/World/Looks/Checker", { attributes: null, descendants: true }), true, "a shader beneath it varies");
  // Beneath the spinning top: its own attributes hold still, its world transform does not.
  assert.equal(readsMightVary(stage, "/World/Top/Child", { attributes: [] }), false);
  assert.equal(readsMightVary(stage, "/World/Top/Child", { attributes: [], worldTransform: true }), true);
});
