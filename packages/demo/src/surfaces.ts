// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// three.js materials from stagehand's `UsdPreviewSurface` records, and the
// textures they read, loaded from the stage's asset bytes.
//
// The mapping, input by input (`MaterialItem` → `MeshStandardMaterial`,
// or `MeshPhysicalMaterial` when clearcoat, `ior`, the specular workflow or
// a `transparent` opacity needs it):
//
// - `diffuseColor` → `color`; a texture's `rgb` → `map` (with `color` its
//   `scale`); the `displayColor` primvar → `vertexColors`.
// - `emissiveColor` → `emissive`; a texture's `rgb` → `emissiveMap`.
// - `metallic` → `metalness`; a texture's `b` → `metalnessMap` (three.js
//   reads blue). `roughness` → `roughness`; a texture's `g` →
//   `roughnessMap` (green). `occlusion`: a texture's `r` → `aoMap` (red).
//   These are the packed occlusion-roughness-metallic channels three.js
//   expects; a lookup of another channel cannot be swizzled without a
//   custom shader, so its input takes the texture's `fallback` channel.
//   The texture's `scale` on that channel multiplies the factor.
// - `useSpecularWorkflow` 1 → `metalness` 0 and `specularColor` (or a
//   texture's `rgb` as `specularColorMap`), `specularIntensity` 1.
// - `clearcoat`, `clearcoatRoughness` → `clearcoat`, `clearcoatRoughness`
//   (maps on `r` and `g`). `ior` → `ior` (three.js clamps it to [1, 2.333]).
// - `normal`: a texture's `rgb` → `normalMap`. The spec has the texture
//   decode tangent-space normals itself, `scale` (2, 2, 2, 1) and `bias`
//   (-1, -1, -1, 0); three.js decodes `* 2 - 1` in its shader, so a map
//   with exactly that scale and bias is used as is, and another is not
//   used.
// - `displacement`: a texture's `r` → `displacementMap` (three.js moves
//   vertices, so it shows on dense meshes only); a constant is ignored.
// - Opacity (`AlphaMode`): `mask` → `alphaTest` at `opacityThreshold`,
//   opaque above it; `blend` with `presence` → `transparent` with
//   `opacity`, which scales the whole response as `presence` does;
//   `blend` with `transparent` (opacity scales only the diffuse response,
//   reflections stay) → `MeshPhysicalMaterial.transmission = 1 - opacity`,
//   three.js's closest model. A textured opacity reads the `a` of the
//   diffuse texture through `map`, or a texture's `g` through `alphaMap`.
//
// Textures: `file`'s identifier is fetched from the stage's asset loop's
// host and decoded by `TextureLoader` from a blob of the bytes; one image
// per identifier, and one `Texture` per use (they share the image), with:
//
// - `sourceColorSpace` `sRGB` → `SRGBColorSpace`, `raw` →
//   `NoColorSpace`; `auto` → sRGB for a color input (`diffuseColor`,
//   `emissiveColor`, `specularColor`), raw otherwise, as Hydra reads an
//   8-bit texture.
// - `wrapS`/`wrapT` `repeat` → `RepeatWrapping`, `mirror` →
//   `MirroredRepeatWrapping`, `clamp` → `ClampToEdgeWrapping`; `black`
//   (three.js has no border color) and `useMetadata` (a PNG has none) →
//   clamp and repeat.
// - `UsdTransform2d` → the texture's `matrix`, set directly
//   (`matrixAutoUpdate` off): `st' = R(rotation) · S(scale) · st +
//   translation`, scale first, then a counter-clockwise rotation, as
//   `UsdTransform2d.osl` computes it; three.js's own offset, repeat and
//   rotation apply rotation before scale and clockwise, so they are not
//   used.
// - Coordinates: stagehand's buffers carry `primvars:st` as the `uv`
//   attribute, and the other sets the materials read (`st1`, `uv`), sorted
//   by name, as `uv1` to `uv3`; a texture reading one samples that
//   channel (`Texture.channel`). The channel is counted among the sets
//   the material itself reads, which are the mesh's when one material
//   draws it.
//   USD's `st` puts (0, 0) at the image's bottom left, as three.js's
//   default `flipY` does.
// - UDIM sets: the tiles the host lists (`udimTiles`) are laid into one
//   atlas canvas, a column per tile number's last digit and a row per ten,
//   and the texture matrix scales `st` into it: tile 1001 + u + 10 v covers
//   [u, u + 1] × [v, v + 1].
// - A texture that cannot be read draws as its `fallback`.
import {
  CanvasTexture,
  ClampToEdgeWrapping,
  Color,
  DataTexture,
  DoubleSide,
  MeshPhysicalMaterial,
  MeshStandardMaterial,
  MirroredRepeatWrapping,
  NoColorSpace,
  RGBAFormat,
  RepeatWrapping,
  SRGBColorSpace,
  Texture,
  TextureLoader,
  UnsignedByteType,
} from "three";
import type { Wrapping } from "three";
import { udimTiles } from "@forest-rs/layerstack_stagehand";
import type { MaterialItem, MaterialTexture, MaterialValue, TextureWrap } from "@forest-rs/layerstack_stagehand";

/** Reads an asset's bytes from the host: `null` when it has none. */
export type AssetReader = (identifier: string) => Promise<Uint8Array | null>;

/** Whether the host's storage has an asset, for a UDIM set's tiles. */
export type AssetExists = (identifier: string) => Promise<boolean>;

/** A decoded image, or a UDIM atlas, and the grid it covers. */
interface Image {
  readonly texture: Texture;
  /** Tile columns and rows of a UDIM atlas; 1 × 1 for one image. */
  readonly columns: number;
  readonly rows: number;
}

/** The textures materials read, one image per identifier, loaded once. */
export class TextureStore {
  readonly #read: AssetReader;
  readonly #exists: AssetExists;
  readonly #loaded: () => void;
  readonly #images = new Map<string, Promise<Image | null>>();
  readonly #ready = new Map<string, Image | null>();

  /** `loaded` is called when an image arrives, to draw again. */
  constructor(read: AssetReader, exists: AssetExists, loaded: () => void) {
    this.#read = read;
    this.#exists = exists;
    this.#loaded = loaded;
  }

  /** The image of `texture`'s file once loaded; `undefined` while loading, `null` when it cannot be read. */
  image(texture: MaterialTexture): Image | null | undefined {
    const identifier = texture.file.identifier;
    if (identifier === null) return null;
    if (this.#ready.has(identifier)) return this.#ready.get(identifier);
    if (!this.#images.has(identifier)) {
      const loading = (texture.file.udim ? this.#atlas(texture) : this.#decode(identifier).then((image) => image && { texture: image, columns: 1, rows: 1 })).catch(
        (error: unknown) => {
          console.error(error);
          return null;
        },
      );
      this.#images.set(identifier, loading);
      void loading.then((image) => {
        this.#ready.set(identifier, image);
        this.#loaded();
      });
    }
    return undefined;
  }

  async #decode(identifier: string): Promise<Texture | null> {
    const bytes = await this.#read(identifier);
    if (bytes === null) return null;
    const url = URL.createObjectURL(new Blob([bytes.slice()]));
    try {
      return await new TextureLoader().loadAsync(url);
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  /** A UDIM set's tiles laid into one canvas, a column per tile's units digit, a row per ten. */
  async #atlas(texture: MaterialTexture): Promise<Image | null> {
    const tiles = await udimTiles(texture.file, this.#exists);
    if (tiles.length === 0) return null;
    const images = await Promise.all(tiles.map((tile) => this.#decode(tile.identifier)));
    const cell = Math.max(...images.map((image) => (image?.image as { width?: number } | undefined)?.width ?? 1));
    const columns = Math.max(...tiles.map((tile) => ((tile.tile - 1001) % 10) + 1));
    const rows = Math.max(...tiles.map((tile) => Math.floor((tile.tile - 1001) / 10) + 1));
    const canvas = document.createElement("canvas");
    canvas.width = cell * columns;
    canvas.height = cell * rows;
    const context = canvas.getContext("2d")!;
    tiles.forEach((tile, i) => {
      const image = images[i]?.image as CanvasImageSource | undefined;
      if (image === undefined) return;
      const u = (tile.tile - 1001) % 10;
      const v = Math.floor((tile.tile - 1001) / 10);
      // Canvas rows run down; `st`'s v runs up.
      context.drawImage(image, u * cell, (rows - 1 - v) * cell, cell, cell);
    });
    return { texture: new CanvasTexture(canvas), columns, rows };
  }

  /** Frees every image. */
  dispose(): void {
    for (const image of this.#ready.values()) image?.texture.dispose();
    this.#ready.clear();
    this.#images.clear();
  }
}

const WRAPS: Record<TextureWrap, Wrapping> = {
  repeat: RepeatWrapping,
  mirror: MirroredRepeatWrapping,
  clamp: ClampToEdgeWrapping,
  black: ClampToEdgeWrapping,
  useMetadata: RepeatWrapping,
};

/** A use of `texture`: its image (or fallback color) with this use's sampling, color space and coordinate transform. */
function use(store: TextureStore, record: MaterialItem, texture: MaterialTexture, color: boolean): Texture {
  const image = store.image(texture);
  if (image === null || image === undefined) {
    // While loading, and when it cannot be read: the fallback.
    const texel = new Uint8Array(texture.fallback.map((value) => Math.round(Math.min(1, Math.max(0, value)) * 255)));
    const fallback = new DataTexture(texel, 1, 1, RGBAFormat, UnsignedByteType);
    fallback.needsUpdate = true;
    return fallback;
  }
  const out = image.texture.clone();
  out.channel = channelOf(record, texture);
  const srgb = texture.colorSpace === "sRGB" || (texture.colorSpace === "auto" && color);
  out.colorSpace = srgb ? SRGBColorSpace : NoColorSpace;
  out.wrapS = WRAPS[texture.wrapS];
  out.wrapT = WRAPS[texture.wrapT];
  // `UsdTransform2d`: scale, then rotate counter-clockwise, then translate;
  // a UDIM atlas then scales the tile grid into [0, 1].
  const transform = texture.coordinates.transform;
  const rotation = ((transform?.rotation ?? 0) * Math.PI) / 180;
  const [sx, sy] = transform?.scale ?? [1, 1];
  const [tx, ty] = transform?.translation ?? [0, 0];
  const [c, s] = [Math.cos(rotation), Math.sin(rotation)];
  const [ax, ay] = [1 / image.columns, 1 / image.rows];
  out.matrixAutoUpdate = false;
  out.matrix.set(ax * c * sx, -ax * s * sy, ax * tx, ay * s * sx, ay * c * sy, ay * ty, 0, 0, 1);
  out.needsUpdate = true;
  return out;
}

/** The three.js uv channel of a texture's coordinates: 0 for `st`, else 1 + the set's place among those the material reads besides `st`, sorted. */
function channelOf(record: MaterialItem, texture: MaterialTexture): number {
  const name = texture.coordinates.primvar;
  if (name === null || name === "st") return 0;
  const sets = [...new Set(record.textures.flatMap((t) => (t.coordinates.primvar === null || t.coordinates.primvar === "st" ? [] : [t.coordinates.primvar])))].sort();
  return Math.min(3, 1 + sets.indexOf(name));
}

/** The constant of a value, else `fallback`. */
function constant<T>(value: MaterialValue<T>, fallback: T): T {
  return value.kind === "constant" ? value.value : fallback;
}

/** The fallback channel of a texture lookup, for a channel three.js cannot read. */
function fallbackChannel(record: MaterialItem, value: MaterialValue<number>, otherwise: number): number {
  if (value.kind !== "texture") return otherwise;
  const texture = record.textures[value.texture]!;
  return value.channel === "rgb" ? otherwise : texture.fallback["rgba".indexOf(value.channel)]!;
}

/** The scale of a texture lookup's channel, else 1. */
function channelScale(record: MaterialItem, value: MaterialValue<number>): number {
  if (value.kind !== "texture" || value.channel === "rgb") return 1;
  return record.textures[value.texture]!.scale["rgba".indexOf(value.channel)]!;
}

/**
 * A three.js material for `record` on a draw (`vertexColors` when the
 * buffer has colors for a `displayColor` primvar), highlighted when
 * selected, as the header of this file maps it.
 */
export function surfaceMaterial(
  record: MaterialItem,
  store: TextureStore,
  options: { readonly vertexColors: boolean; readonly doubleSided: boolean; readonly highlighted: boolean },
): MeshStandardMaterial | MeshPhysicalMaterial {
  const blend = record.alpha.kind === "blend" ? record.alpha : null;
  const transmissive = blend !== null && !blend.presence && record.opacity.kind === "constant";
  const physical =
    transmissive ||
    record.useSpecularWorkflow ||
    constant(record.clearcoat, 0) > 0 ||
    record.clearcoat.kind === "texture" ||
    constant(record.ior, 1.5) !== 1.5;
  const material = physical ? new MeshPhysicalMaterial() : new MeshStandardMaterial();
  const map = (value: MaterialValue<unknown>, color: boolean) =>
    value.kind === "texture" ? use(store, record, record.textures[value.texture]!, color) : null;
  // Diffuse: a constant, a texture's rgb (tinted by its scale), or displayColor.
  const diffuse = record.diffuseColor;
  if (diffuse.kind === "texture") {
    material.map = map(diffuse, true);
    const [r, g, b] = record.textures[diffuse.texture]!.scale;
    material.color.setRGB(r!, g!, b!);
  } else if (diffuse.kind === "primvar") {
    material.vertexColors = options.vertexColors && diffuse.name === "displayColor";
    const [r, g, b] = material.vertexColors ? [1, 1, 1] : diffuse.fallback;
    material.color.setRGB(r, g, b);
  } else {
    material.color.setRGB(...(diffuse.value as [number, number, number]));
  }
  // Emission.
  const emissive = record.emissiveColor;
  if (emissive.kind === "texture") {
    material.emissiveMap = map(emissive, true);
    material.emissive.setRGB(1, 1, 1);
  } else {
    material.emissive.setRGB(...(constant(emissive, [0, 0, 0] as const) as [number, number, number]));
  }
  // Metal and roughness: the channels three.js reads (b, g), else the fallback channel.
  const channel = (value: MaterialValue<number>, wanted: "r" | "g" | "b") =>
    value.kind === "texture" && value.channel === wanted ? map(value, false) : null;
  material.metalnessMap = channel(record.metallic, "b");
  material.metalness = record.useSpecularWorkflow
    ? 0
    : material.metalnessMap !== null
      ? channelScale(record, record.metallic)
      : constant(record.metallic, fallbackChannel(record, record.metallic, 0));
  material.roughnessMap = channel(record.roughness, "g");
  material.roughness =
    material.roughnessMap !== null ? channelScale(record, record.roughness) : constant(record.roughness, fallbackChannel(record, record.roughness, 0.5));
  material.aoMap = channel(record.occlusion, "r");
  if (material.aoMap !== null) material.aoMapIntensity = channelScale(record, record.occlusion);
  // Normals: a map that decodes as three.js does.
  if (record.normal.kind === "texture") {
    const texture = record.textures[record.normal.texture]!;
    const decodes = texture.scale.slice(0, 3).every((v) => v === 2) && texture.bias.slice(0, 3).every((v) => v === -1);
    if (decodes) material.normalMap = use(store, record, texture, false);
  }
  material.displacementMap = channel(record.displacement, "r");
  // Opacity.
  if (record.alpha.kind === "mask") {
    material.alphaTest = record.alpha.threshold;
  } else if (blend !== null && !transmissive) {
    material.transparent = true;
    material.opacity = constant(record.opacity, 1);
    material.depthWrite = false;
  }
  const opacity = record.opacity;
  if (opacity.kind === "texture" && !(diffuse.kind === "texture" && opacity.texture === diffuse.texture && opacity.channel === "a")) {
    material.alphaMap = channel(opacity, "g");
  }
  if (material instanceof MeshPhysicalMaterial) {
    material.ior = Math.min(2.333, Math.max(1, constant(record.ior, 1.5)));
    material.clearcoat = constant(record.clearcoat, record.clearcoat.kind === "texture" ? channelScale(record, record.clearcoat) : 0);
    material.clearcoatMap = channel(record.clearcoat, "r");
    material.clearcoatRoughness = constant(record.clearcoatRoughness, 0.01);
    material.clearcoatRoughnessMap = channel(record.clearcoatRoughness, "g");
    if (record.useSpecularWorkflow) {
      material.specularIntensity = 1;
      const specular = record.specularColor;
      if (specular.kind === "texture") material.specularColorMap = map(specular, true);
      else material.specularColor.setRGB(...(constant(specular, [0, 0, 0] as const) as [number, number, number]));
    }
    if (transmissive) material.transmission = 1 - constant(record.opacity, 1);
  }
  if (options.doubleSided) material.side = DoubleSide;
  if (options.highlighted) {
    material.emissive = new Color(0x2f5fd0);
    material.emissiveIntensity = 0.6;
  }
  return material;
}
