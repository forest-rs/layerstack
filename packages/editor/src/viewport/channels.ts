// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The texture channel each one-channel map of a three.js material reads.
//
// three.js reads fixed channels, packed as glTF packs occlusion, roughness
// and metalness: roughness `g`, metalness `b`, ambient occlusion `r`,
// alpha `g`, clearcoat `r`, clearcoat roughness `g`, displacement `r`. A
// `UsdPreviewSurface` input names its own (stagehand's
// `MaterialValue.channel`: a float connected to a texture's `rgb` reads
// `r`, as Storm does). So a material whose maps read other channels has
// the chunks reading them patched, in `onBeforeCompile`, to read the
// input's channel instead, and its program keyed by those channels. A
// later patch (the viewport's light links) chains this one.
import { ShaderChunk } from "three";
import type { Material } from "three";

/** One channel of a texture. */
export type MapChannel = "r" | "g" | "b" | "a";

/** The one-channel maps whose channel can be chosen. */
export type ChannelMap = "roughness" | "metalness" | "ao" | "alpha" | "clearcoat" | "clearcoatRoughness" | "displacement";

/** The channel each map reads; a map not named reads three.js's own. */
export type MapChannels = Partial<Record<ChannelMap, MapChannel>>;

/** Each map's chunk, its read there (ending in three.js's channel), and that channel. */
const READS: Record<ChannelMap, { readonly chunk: string; readonly read: string; readonly own: MapChannel }> = {
  roughness: { chunk: "roughnessmap_fragment", read: "texelRoughness.g", own: "g" },
  metalness: { chunk: "metalnessmap_fragment", read: "texelMetalness.b", own: "b" },
  ao: { chunk: "aomap_fragment", read: "texture2D( aoMap, vAoMapUv ).r", own: "r" },
  alpha: { chunk: "alphamap_fragment", read: "texture2D( alphaMap, vAlphaMapUv ).g", own: "g" },
  clearcoat: { chunk: "lights_physical_fragment", read: "texture2D( clearcoatMap, vClearcoatMapUv ).x", own: "r" },
  clearcoatRoughness: { chunk: "lights_physical_fragment", read: "texture2D( clearcoatRoughnessMap, vClearcoatRoughnessMapUv ).y", own: "g" },
  displacement: { chunk: "displacementmap_vertex", read: "texture2D( displacementMap, vDisplacementMapUv ).x", own: "r" },
};

/** Makes `material`'s maps read `channels`; nothing when each is three.js's own. */
export function readChannels(material: Material, channels: MapChannels): void {
  const changed = (Object.entries(channels) as [ChannelMap, MapChannel][]).filter(([map, channel]) => channel !== READS[map].own);
  if (changed.length === 0) return;
  // Each chunk once, with every read in it swizzled.
  const chunks = new Map<string, string>();
  for (const [map, channel] of changed) {
    const { chunk, read } = READS[map];
    const source = chunks.get(chunk) ?? (ShaderChunk as unknown as Record<string, string>)[chunk]!;
    chunks.set(chunk, source.replace(read, `${read.slice(0, read.lastIndexOf(".") + 1)}${channel}`));
  }
  const key = changed.map(([map, channel]) => `${map}.${channel}`).join(" ");
  material.onBeforeCompile = (shader) => {
    for (const [chunk, source] of chunks) {
      const stage = chunk.endsWith("_vertex") ? "vertexShader" : "fragmentShader";
      shader[stage] = shader[stage].replace(`#include <${chunk}>`, source);
    }
  };
  material.customProgramCacheKey = () => `channels ${key}`;
}
