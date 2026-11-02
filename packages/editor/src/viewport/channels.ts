// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The texture channel each one-channel map of a three.js node material
// reads.
//
// three.js reads fixed channels, packed as glTF packs occlusion, roughness
// and metalness: roughness `g`, metalness `b`, ambient occlusion `r`,
// clearcoat `r`, clearcoat roughness `g`, displacement `r` (and its node
// materials an alpha map's `r`, where `WebGLRenderer` read `g`). A
// `UsdPreviewSurface` input names its own (stagehand's
// `MaterialValue.channel`: a float connected to a texture's `rgb` reads
// `r`, as Storm does). So each map a material reads is given as the
// material's input node (`roughnessNode`, `metalnessNode`, `aoNode`,
// `opacityNode`, `clearcoatNode`, `clearcoatRoughnessNode`) the product
// three.js would make, with the map's channel swizzled in. The nodes read
// the material's own properties by reference (`materialReference`), so
// they are made once per map and channel and shared by every material: a
// material's shader is keyed by its nodes, and materials reading the same
// channels share one pipeline.
//
// Displacement moves vertices before instancing in three.js
// (`NodeMaterial.setupPosition`), and a material's `positionNode` replaces
// the position after it; a displacement reading another channel than `r`
// is drawn through `positionNode`, so on an instanced draw its offset is
// in instance space, scaled by the instance's matrix.
import { materialReference, normalLocal, positionLocal, texture } from "three/tsl";
import type { MeshPhysicalNodeMaterial, MeshStandardNodeMaterial } from "three/webgpu";

/** One channel of a texture. */
export type MapChannel = "r" | "g" | "b" | "a";

/** The one-channel maps whose channel can be chosen. */
export type ChannelMap = "roughness" | "metalness" | "ao" | "alpha" | "clearcoat" | "clearcoatRoughness" | "displacement";

/** The channel each map reads; a map not named reads three.js's own. */
export type MapChannels = Partial<Record<ChannelMap, MapChannel>>;

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type FloatNode = any;

/** The texel of a material's map property, by reference, at the map's own coordinates. */
function mapTexel(property: string): FloatNode {
  return materialReference(property, "texture");
}

/** The input node of each map and channel, made at its first use. */
const NODES = new Map<string, FloatNode>();

/** The node three.js would make for `map`'s input, reading `channel`. */
function inputNode(map: Exclude<ChannelMap, "displacement">, channel: MapChannel): FloatNode {
  const key = `${map}.${channel}`;
  let node = NODES.get(key);
  if (node !== undefined) return node;
  const factor = (property: string): FloatNode => materialReference(property, "float");
  switch (map) {
    case "roughness":
      node = factor("roughness").mul(mapTexel("roughnessMap")[channel]);
      break;
    case "metalness":
      node = factor("metalness").mul(mapTexel("metalnessMap")[channel]);
      break;
    case "ao":
      node = mapTexel("aoMap")[channel].sub(1).mul(factor("aoMapIntensity")).add(1);
      break;
    case "alpha":
      node = factor("opacity").mul(mapTexel("alphaMap")[channel]);
      break;
    case "clearcoat":
      node = factor("clearcoat").mul(mapTexel("clearcoatMap")[channel]);
      break;
    case "clearcoatRoughness":
      node = factor("clearcoatRoughness").mul(mapTexel("clearcoatRoughnessMap")[channel]);
      break;
  }
  NODES.set(key, node);
  return node;
}

/**
 * Makes `material`'s maps read `channels`. Set after the maps: a map the
 * material does not have is left to three.js (its node is not set).
 */
export function readChannels(material: MeshStandardNodeMaterial | MeshPhysicalNodeMaterial, channels: MapChannels): void {
  for (const [map, channel] of Object.entries(channels) as [ChannelMap, MapChannel][]) {
    switch (map) {
      case "roughness":
        if (material.roughnessMap !== null) material.roughnessNode = inputNode(map, channel);
        break;
      case "metalness":
        if (material.metalnessMap !== null) material.metalnessNode = inputNode(map, channel);
        break;
      case "ao":
        if (material.aoMap !== null) material.aoNode = inputNode(map, channel);
        break;
      case "alpha":
        if (material.alphaMap !== null) material.opacityNode = inputNode(map, channel);
        break;
      case "clearcoat":
        if ("clearcoatMap" in material && material.clearcoatMap !== null) material.clearcoatNode = inputNode(map, channel);
        break;
      case "clearcoatRoughness":
        if ("clearcoatRoughnessMap" in material && material.clearcoatRoughnessMap !== null) {
          material.clearcoatRoughnessNode = inputNode(map, channel);
        }
        break;
      case "displacement": {
        const displacement = material.displacementMap;
        if (displacement === null || channel === "r") break;
        // Drawn by the position node instead (see the header).
        material.displacementMap = null;
        const offset = (texture(displacement) as FloatNode)[channel].mul(material.displacementScale).add(material.displacementBias);
        material.positionNode = positionLocal.add(normalLocal.normalize().mul(offset));
        break;
      }
    }
  }
}
