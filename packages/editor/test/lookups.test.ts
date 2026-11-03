// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The texture lookups a surface draws: every one `texel * scale + bias`,
// with its texture's scale and bias, whichever input reads it.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { MaterialItem, MaterialTexture, MaterialValue } from "@forest-rs/layerstack_stagehand";
import { decodesNormal, surfaceLookups } from "../src/viewport/lookups.ts";
import type { Vec4 } from "../src/viewport/lookups.ts";

/** A texture of `scale` and `bias`. */
function texture(scale: Vec4 = [1, 1, 1, 1], bias: Vec4 = [0, 0, 0, 0]): MaterialTexture {
  return {
    shader: "/Looks/M/Texture",
    file: { identifier: "image.png", layer: null, udim: false },
    coordinates: { primvar: "st", transform: null },
    wrapS: "repeat",
    wrapT: "repeat",
    fallback: [0, 0, 0, 1],
    scale,
    bias,
    colorSpace: "auto",
  } as unknown as MaterialTexture;
}

const constant = <T>(value: T): MaterialValue<T> => ({ kind: "constant", value });
const lookup = <T>(index: number, channel: "rgb" | "r" | "g" | "b" | "a"): MaterialValue<T> => ({ kind: "texture", texture: index, channel });

/** A surface of constants, with `inputs` and `textures` over them. */
function record(inputs: Partial<MaterialItem>, textures: MaterialTexture[]): MaterialItem {
  return {
    path: "/Looks/M",
    context: null,
    surface: "/Looks/M/Surface",
    diffuseColor: constant([0.18, 0.18, 0.18]),
    emissiveColor: constant([0, 0, 0]),
    useSpecularWorkflow: false,
    specularColor: constant([0, 0, 0]),
    metallic: constant(0),
    roughness: constant(0.5),
    clearcoat: constant(0),
    clearcoatRoughness: constant(0.01),
    opacity: constant(1),
    opacityThreshold: 0,
    opacityPresence: true,
    alpha: { kind: "opaque" },
    ior: constant(1.5),
    normal: constant([0, 0, 1]),
    displacement: constant(0),
    occlusion: constant(1),
    textures,
    primvars: [],
    notes: [],
    ...inputs,
  } as MaterialItem;
}

test("every textured input is a lookup with its texture's scale and bias", () => {
  const colors = texture([0.5, 1, 0.5, 1], [0, 0, 0.4, 0]);
  const packed = texture([-1, 2, 1, 1], [1, 0, 0.25, 0]);
  const surface = surfaceLookups(
    record(
      {
        diffuseColor: lookup(0, "rgb"),
        emissiveColor: lookup(0, "rgb"),
        occlusion: lookup(1, "r"),
        roughness: lookup(1, "g"),
        metallic: lookup(1, "b"),
        opacity: lookup(1, "a"),
        alpha: { kind: "blend", presence: true },
        displacement: lookup(1, "r"),
      },
      [colors, packed],
    ),
  );
  const by = new Map(surface.lookups.map((each) => [each.map, each]));
  assert.deepEqual([...by.keys()].sort(), ["alphaMap", "aoMap", "displacementMap", "emissiveMap", "map", "metalnessMap", "roughnessMap"]);
  assert.deepEqual(by.get("map"), { map: "map", texture: 0, channel: "rgb", color: true, scale: colors.scale, bias: colors.bias });
  assert.deepEqual(by.get("emissiveMap")?.bias, [0, 0, 0.4, 0]);
  assert.deepEqual(by.get("roughnessMap"), { map: "roughnessMap", texture: 1, channel: "g", color: false, scale: packed.scale, bias: packed.bias });
  assert.equal(by.get("metalnessMap")?.channel, "b");
  assert.equal(by.get("aoMap")?.channel, "r");
  assert.equal(by.get("alphaMap")?.channel, "a");
  assert.equal(surface.opacityInMap, false);
});

test("a bias alone still makes a lookup, and a constant none", () => {
  const surface = surfaceLookups(record({ diffuseColor: lookup(0, "rgb") }, [texture([1, 1, 1, 1], [0.3, 0, 0, 0])]));
  assert.equal(surface.lookups.length, 1);
  assert.deepEqual(surface.lookups[0]!.bias, [0.3, 0, 0, 0]);
  assert.deepEqual(surfaceLookups(record({}, [])).lookups, []);
});

test("opacity from the diffuse texture's alpha is the diffuse lookup's a", () => {
  const surface = surfaceLookups(record({ diffuseColor: lookup(0, "rgb"), opacity: lookup(0, "a"), alpha: { kind: "mask", threshold: 0.5 } }, [texture()]));
  assert.equal(surface.opacityInMap, true);
  assert.deepEqual(
    surface.lookups.map((each) => each.map),
    ["map"],
  );
  // Another channel of it is a lookup of its own.
  const other = surfaceLookups(record({ diffuseColor: lookup(0, "rgb"), opacity: lookup(0, "r") }, [texture()]));
  assert.equal(other.opacityInMap, false);
  assert.deepEqual(
    other.lookups.map((each) => each.map),
    ["map", "alphaMap"],
  );
});

test("a normal map's decoding is drawn by three.js's normal map, another scale and bias as its value", () => {
  assert.equal(decodesNormal(texture([2, 2, 2, 1], [-1, -1, -1, 0])), true);
  assert.equal(decodesNormal(texture([2, 2, 2, 7], [-1, -1, -1, 3])), true, "alpha does not matter");
  assert.equal(decodesNormal(texture([6, 6, 2, 1], [-3, -3, -1, 0])), false);
  assert.equal(decodesNormal(texture()), false);
  const decoded = surfaceLookups(record({ normal: lookup(0, "rgb") }, [texture([2, 2, 2, 1], [-1, -1, -1, 0])]));
  assert.equal(decoded.normalDecodes, true);
  assert.deepEqual(decoded.lookups[0], { map: "normalMap", texture: 0, channel: "rgb", color: false, scale: [2, 2, 2, 1], bias: [-1, -1, -1, 0] });
  const strong = surfaceLookups(record({ normal: lookup(0, "rgb") }, [texture([6, 6, 2, 1], [-3, -3, -1, 0])]));
  assert.equal(strong.normalDecodes, false);
  assert.equal(strong.lookups[0]?.map, "normalMap", "drawn, not dropped");
});

test("inputs the material does not draw are not lookups", () => {
  // Metallic under the specular workflow; the specular color and clearcoat with it.
  const specular = surfaceLookups(
    record(
      { useSpecularWorkflow: true, metallic: lookup(0, "r"), specularColor: lookup(0, "rgb"), clearcoat: lookup(0, "g"), clearcoatRoughness: lookup(0, "b") },
      [texture()],
    ),
  );
  assert.deepEqual(specular.lookups.map((each) => each.map).sort(), ["clearcoatMap", "clearcoatRoughnessMap", "specularColorMap"]);
  assert.equal(specular.lookups.find((each) => each.map === "specularColorMap")?.color, true);
  // Without the physical material, no specular color or clearcoat roughness.
  const standard = surfaceLookups(record({ specularColor: lookup(0, "rgb"), clearcoatRoughness: lookup(0, "g") }, [texture()]));
  assert.deepEqual(standard.lookups, []);
  // A number read from a texture's rgb is drawn as its fallback.
  assert.deepEqual(surfaceLookups(record({ roughness: lookup(0, "rgb") }, [texture()])).lookups, []);
});
