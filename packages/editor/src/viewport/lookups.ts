// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The texture lookups a `UsdPreviewSurface` record draws, and the three.js
// map each one is drawn through.
//
// A `UsdUVTexture` lookup is `texel * scale + bias`, per channel
// (UsdPreviewSurface specification, `UsdUVTexture` inputs `scale` and
// `bias`). Storm reads the texel, or the `fallback` in its place when the
// file cannot be read, and then applies the scale and the bias
// (`HdSt_CodeGen`'s `HdGet_<name>_applyScaleAndBias`), so a fallback is
// scaled and biased too. Every input reads the lookup's value as it is:
// `opacity`, `roughness`, `metallic`, `occlusion`, `clearcoat` and
// `clearcoatRoughness` their channel of it, `diffuseColor`,
// `emissiveColor` and `specularColor` its `rgb`, and `normal` its `rgb` as
// a tangent-space normal, normalised (`previewSurface.glslfx`,
// `perturbFragmentNormal`).
//
// A lookup reads its texture at its `st`, moved by the `UsdTransform2d`
// feeding the texture's `st` input, if any: scaled, then rotated
// counter-clockwise by `rotation` degrees, then translated
// (UsdPreviewSurface specification, `UsdTransform2d`; `UsdTransform2d.osl`
// computes `R(rotation) · S(scale) · st + translation` in that order).
// Each texture has its own: textures reading one primvar reader through
// different transforms each read their own coordinates.
//
// This module says which lookups a record draws, with which scale and
// bias, and the matrix its coordinates are moved by
// ({@link coordinateMatrix}); `channels.ts` builds the three.js nodes that
// compute them.

import type { MaterialItem, MaterialValue } from "@forest-rs/layerstack_stagehand";

/** A 3×3 matrix, row by row, as three.js's `Matrix3.set` takes it. */
export type Mat3 = readonly [number, number, number, number, number, number, number, number, number];

/** A `UsdTransform2d`'s inputs (stagehand's `TextureTransform`). */
export interface Transform2d {
  /** Degrees, counter-clockwise. */
  readonly rotation: number;
  readonly scale: readonly [number, number];
  readonly translation: readonly [number, number];
}

/**
 * The matrix taking a lookup's `st` to the coordinates it samples its
 * image at, as `(u, v, 1)`: `transform` (scale, then rotate
 * counter-clockwise, then translate, as the header says; identity for
 * none), then, for a UDIM set drawn as an atlas of `columns` × `rows`
 * tiles, the tile grid scaled into [0, 1] (tile 1001 + u + 10 v covers
 * [u, u + 1] × [v, v + 1]). three.js's own offset, repeat and rotation
 * rotate before scaling, and clockwise, so they are not used.
 */
export function coordinateMatrix(transform: Transform2d | null, columns = 1, rows = 1): Mat3 {
  const rotation = ((transform?.rotation ?? 0) * Math.PI) / 180;
  const [sx, sy] = transform?.scale ?? [1, 1];
  const [tx, ty] = transform?.translation ?? [0, 0];
  const [c, s] = [Math.cos(rotation), Math.sin(rotation)];
  const [ax, ay] = [1 / columns, 1 / rows];
  return [ax * c * sx, -ax * s * sy, ax * tx, ay * s * sx, ay * c * sy, ay * ty, 0, 0, 1];
}

/** Four channels, `r`, `g`, `b`, `a`. */
export type Vec4 = readonly [number, number, number, number];

/** The channel a lookup's input reads: `rgb` for a color or a normal, else one channel. */
export type LookupChannel = "rgb" | "r" | "g" | "b" | "a";

/**
 * The three.js material property each lookup's texture is set as. The
 * material carries the lookup's scale, bias and coordinate matrix beside
 * it, as `<map>Scale`, `<map>Bias` and `<map>Transform` (`channels.ts`).
 */
export type LookupMap =
  | "map"
  | "emissiveMap"
  | "specularColorMap"
  | "metalnessMap"
  | "roughnessMap"
  | "aoMap"
  | "alphaMap"
  | "clearcoatMap"
  | "clearcoatRoughnessMap"
  | "normalMap"
  | "displacementMap";

/** Every {@link LookupMap}. */
export const LOOKUP_MAPS: readonly LookupMap[] = [
  "map",
  "emissiveMap",
  "specularColorMap",
  "metalnessMap",
  "roughnessMap",
  "aoMap",
  "alphaMap",
  "clearcoatMap",
  "clearcoatRoughnessMap",
  "normalMap",
  "displacementMap",
];

/** One texture lookup a material draws. */
export interface TextureLookup {
  /** The material property its texture is set as. */
  readonly map: LookupMap;
  /** Its texture, an index into the record's `textures`. */
  readonly texture: number;
  /** The channel its input reads. */
  readonly channel: LookupChannel;
  /** Whether its input is a color, for a texture whose `sourceColorSpace` is `auto`. */
  readonly color: boolean;
  /** Multiplies the texel, per channel. */
  readonly scale: Vec4;
  /** Added to the scaled texel, per channel. */
  readonly bias: Vec4;
}

/** The lookups a record draws ({@link surfaceLookups}). */
export interface SurfaceLookups {
  readonly lookups: readonly TextureLookup[];
  /**
   * Whether `opacity` reads the `a` of the diffuse lookup: the `map`
   * lookup then carries opacity as its `a`, and there is no `alphaMap`.
   */
  readonly opacityInMap: boolean;
  /**
   * Whether the `normalMap` lookup is exactly a normal map's decoding,
   * `scale` (2, 2, 2) and `bias` (-1, -1, -1) on `rgb`: three.js's own
   * normal map decodes `* 2 - 1` in its shader, so such a lookup is drawn
   * as a plain normal map, and any other as its scaled and biased value.
   */
  readonly normalDecodes: boolean;
}

/**
 * Whether a record is drawn with `MeshPhysicalNodeMaterial`: for
 * clearcoat, an `ior` other than 1.5, the specular workflow, or a
 * `transparent` blend of constant opacity (drawn as transmission).
 */
export function needsPhysical(record: MaterialItem): boolean {
  const blend = record.alpha.kind === "blend" ? record.alpha : null;
  const transmissive = blend !== null && !blend.presence && record.opacity.kind === "constant";
  const constant = <T>(value: MaterialValue<T>, fallback: T): T => (value.kind === "constant" ? value.value : fallback);
  return transmissive || record.useSpecularWorkflow || constant(record.clearcoat, 0) > 0 || record.clearcoat.kind === "texture" || constant(record.ior, 1.5) !== 1.5;
}

/** Whether a texture's scale and bias are exactly a normal map's decoding on `rgb`. */
export function decodesNormal(texture: { readonly scale: Vec4; readonly bias: Vec4 }): boolean {
  return texture.scale.slice(0, 3).every((v) => v === 2) && texture.bias.slice(0, 3).every((v) => v === -1);
}

/**
 * The texture lookups `record` draws, each with the map it is set as and
 * its texture's scale and bias, as the header of this file describes. A
 * number read from a texture's `rgb` is not a lookup three.js can draw (it
 * is drawn as the input's fallback); metallic is not drawn under the
 * specular workflow, nor clearcoat and specular color without the physical
 * material ({@link needsPhysical}).
 */
export function surfaceLookups(record: MaterialItem): SurfaceLookups {
  const lookups: TextureLookup[] = [];
  const add = (map: LookupMap, value: MaterialValue<unknown>, color: boolean, oneChannel: boolean) => {
    if (value.kind !== "texture") return;
    if (oneChannel && value.channel === "rgb") return;
    const texture = record.textures[value.texture]!;
    lookups.push({ map, texture: value.texture, channel: oneChannel ? value.channel : "rgb", color, scale: texture.scale, bias: texture.bias });
  };
  const physical = needsPhysical(record);
  const diffuse = record.diffuseColor;
  const opacity = record.opacity;
  const opacityInMap = diffuse.kind === "texture" && opacity.kind === "texture" && opacity.texture === diffuse.texture && opacity.channel === "a";
  add("map", diffuse, true, false);
  add("emissiveMap", record.emissiveColor, true, false);
  if (physical && record.useSpecularWorkflow) add("specularColorMap", record.specularColor, true, false);
  if (!record.useSpecularWorkflow) add("metalnessMap", record.metallic, false, true);
  add("roughnessMap", record.roughness, false, true);
  add("aoMap", record.occlusion, false, true);
  if (!opacityInMap) add("alphaMap", opacity, false, true);
  if (physical) {
    add("clearcoatMap", record.clearcoat, false, true);
    add("clearcoatRoughnessMap", record.clearcoatRoughness, false, true);
  }
  add("normalMap", record.normal, false, false);
  add("displacementMap", record.displacement, false, true);
  const normal = record.normal.kind === "texture" ? record.textures[record.normal.texture]! : null;
  return { lookups, opacityInMap, normalDecodes: normal !== null && decodesNormal(normal) };
}
