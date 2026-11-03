// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How a blended surface's opacity is drawn under each `opacityMode`:
// `presence` fades the whole response, `transparent` only the diffuse one,
// composited premultiplied as Storm does, whether the opacity is a
// constant or a texture lookup.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { MaterialItem, MaterialTexture, MaterialValue } from "@forest-rs/layerstack_stagehand";
import { SurfaceStandardMaterial } from "../src/viewport/channels.ts";
import type { SurfaceMaterial } from "../src/viewport/channels.ts";
import { needsPhysical } from "../src/viewport/lookups.ts";
import { surfaceMaterial, TextureStore } from "../src/viewport/surfaces.ts";

const constant = <T>(value: T): MaterialValue<T> => ({ kind: "constant", value });

const RAMP = {
  shader: "/Looks/M/Texture",
  file: { identifier: null, layer: null, udim: false },
  coordinates: { primvar: "st", transform: null },
  wrapS: "repeat",
  wrapT: "repeat",
  fallback: [0.5, 0.5, 0.5, 1],
  scale: [1, 1, 1, 1],
  bias: [0, 0, 0, 0],
  colorSpace: "raw",
} as unknown as MaterialTexture;

/** A blended surface of `opacity` under `presence` or `transparent`. */
function blended(presence: boolean, opacity: MaterialValue<number>): MaterialItem {
  return {
    path: "/Looks/M",
    context: null,
    surface: "/Looks/M/Surface",
    diffuseColor: constant([0.9, 0.9, 0.9]),
    emissiveColor: constant([0, 0, 0]),
    useSpecularWorkflow: false,
    specularColor: constant([0, 0, 0]),
    metallic: constant(0),
    roughness: constant(0.2),
    clearcoat: constant(0),
    clearcoatRoughness: constant(0.01),
    opacity,
    opacityThreshold: 0,
    opacityPresence: presence,
    alpha: { kind: "blend", presence },
    ior: constant(1.5),
    normal: constant([0, 0, 1]),
    displacement: constant(0),
    occlusion: constant(1),
    textures: [RAMP],
    primvars: [],
    notes: [],
  } as unknown as MaterialItem;
}

function material(record: MaterialItem): SurfaceMaterial {
  const store = new TextureStore(
    async () => null,
    async () => false,
    () => {},
  );
  return surfaceMaterial(record, store, { vertexColors: false, doubleSided: false, highlighted: false, uvChannels: new Map() });
}

const OPACITIES: [string, MaterialValue<number>][] = [
  ["a constant", constant(0.4)],
  ["a texture lookup", { kind: "texture", texture: 0, channel: "r" }],
];

for (const [name, opacity] of OPACITIES) {
  test(`${name} opacity under transparent scales the diffuse response and blends premultiplied`, () => {
    const record = blended(false, opacity);
    const drawn = material(record);
    assert.equal(drawn.transparent, true);
    assert.equal(drawn.opacityScalesDiffuse, true);
    assert.equal(drawn.premultipliedAlpha, true);
    assert.equal(drawn.depthWrite, false);
    // No transmission: three.js's tints and blurs what is behind, Storm's does not.
    assert.equal(needsPhysical(record), false);
    assert.equal((drawn as { transmission?: number }).transmission ?? 0, 0);
    if (opacity.kind === "constant") assert.equal(drawn.opacity, 0.4);
  });

  test(`${name} opacity under presence fades the whole response, blended as three.js does`, () => {
    const drawn = material(blended(true, opacity));
    assert.equal(drawn.transparent, true);
    assert.equal(drawn.opacityScalesDiffuse, false);
    assert.equal(drawn.premultipliedAlpha, false);
  });
}

test("the mode is a field three.js keys shaders by and copies with the material", () => {
  const drawn = material(blended(false, constant(0.4)));
  // `RenderObject.getMaterialCacheKey` reads every own enumerable field.
  assert.ok(Object.keys(drawn).includes("opacityScalesDiffuse"));
  const copy = drawn.clone() as SurfaceStandardMaterial;
  assert.equal(copy.opacityScalesDiffuse, true);
  assert.equal(new SurfaceStandardMaterial().opacityScalesDiffuse, false);
});
