// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// three.js materials from stagehand's `UsdPreviewSurface` records, and the
// textures they read, loaded from the stage's asset bytes.
//
// The mapping, input by input (`MaterialItem` → `MeshStandardNodeMaterial`,
// or `MeshPhysicalNodeMaterial` when clearcoat, `ior`, the specular
// workflow or a `transparent` opacity needs it):
//
// - `diffuseColor` → `color`; a texture's `rgb` → `map`; the
//   `displayColor` primvar → `vertexColors`.
// - `emissiveColor` → `emissive`; a texture's `rgb` → `emissiveMap`.
// - `metallic` → `metalness`, a texture → `metalnessMap`; `roughness` →
//   `roughness`, a texture → `roughnessMap`; `occlusion`: a texture →
//   `aoMap`. Each map reads the input's channel.
// - `useSpecularWorkflow` 1 → `metalness` 0 and `specularColor` (or a
//   texture's `rgb` as `specularColorMap`), `specularIntensity` 1.
// - `clearcoat`, `clearcoatRoughness` → `clearcoat`, `clearcoatRoughness`
//   (maps, any channel). `ior` → `ior` (three.js clamps it to [1, 2.333]).
// - `normal`: a texture's `rgb` → `normalMap`, a tangent-space normal.
// - `displacement`: a texture → `displacementMap` (three.js moves
//   vertices, so it shows on dense meshes only); a constant is ignored.
// - Every texture lookup is `texel * scale + bias`, per channel, as
//   `UsdUVTexture` defines it: which lookups a record draws is
//   `lookups.ts`, and the nodes that draw them, with the scale and bias as
//   uniforms, `channels.ts`.
// - Opacity (`AlphaMode`): `mask` → `alphaTest` at `opacityThreshold`,
//   opaque above it; `blend` with `presence` → `transparent` with
//   `opacity`, which scales the whole response as `presence` does;
//   `blend` with `transparent` (opacity scales only the diffuse response,
//   reflections stay) → `MeshPhysicalNodeMaterial.transmission = 1 - opacity`,
//   three.js's closest model. A textured opacity reads the `a` of the
//   diffuse lookup through `map`, or another lookup through `alphaMap`.
//
// Textures: `file`'s identifier is fetched from the stage's asset loop's
// host and decoded off the main thread (`createImageBitmap` in a worker,
// `imageDecoders.ts`), scaled down to `MAX_TEXTURE_SIZE`; one image per identifier, and one `Texture` per
// distinct use of it (they share the image, and every material making the
// same use shares the texture, so an image is uploaded once per use), with:
//
// - `sourceColorSpace` `sRGB` → `SRGBColorSpace`, `raw` →
//   `NoColorSpace`; `auto` → sRGB for a color input (`diffuseColor`,
//   `emissiveColor`, `specularColor`), raw otherwise, as Hydra reads an
//   8-bit texture.
// - `wrapS`/`wrapT` `repeat` → `RepeatWrapping`, `mirror` →
//   `MirroredRepeatWrapping`, `clamp` → `ClampToEdgeWrapping`; `black`
//   (three.js has no border color) and `useMetadata` (a PNG has none) →
//   clamp and repeat.
// - `UsdTransform2d` → `st' = R(rotation) · S(scale) · st + translation`,
//   scale first, then a counter-clockwise rotation, as `UsdTransform2d.osl`
//   computes it (`coordinateMatrix`, `lookups.ts`): each lookup's matrix
//   (`channels.ts`), and the texture's `matrix`, set directly
//   (`matrixAutoUpdate` off), for the maps three.js samples itself (a
//   plain normal map, a displacement's `r`).
// - Coordinates: stagehand's buffers carry `primvars:st` as the `uv`
//   attribute, and the other sets its materials read (`st1`, `uv`), in
//   the buffer's order (`StagehandMesh.uvSets`), as `uv1` to `uv3`. The
//   mesh's channels (`uvChannels`) map each set's name to its channel, and
//   a texture reading one samples that channel (`Texture.channel`), so
//   subsets' materials reading different sets each find their own; a set
//   past the third, or not drawn, samples `st`.
//   USD's `st` puts (0, 0) at the image's bottom left, as three.js's
//   default `flipY` does.
// - UDIM sets: the tiles the host lists (`udimTiles`) are laid into one
//   atlas canvas, a column per tile number's last digit and a row per ten,
//   and the texture matrix scales `st` into it: tile 1001 + u + 10 v covers
//   [u, u + 1] × [v, v + 1].
// - A texture that cannot be read (or is still loading) draws as its
//   `fallback`, scaled and biased as its texels would be, as Storm does;
//   the fallback is one 8-bit texel, so it is clamped to [0, 1].
import {
  CanvasTexture,
  ClampToEdgeWrapping,
  Color,
  DataTexture,
  DoubleSide,
  MeshPhysicalNodeMaterial,
  MirroredRepeatWrapping,
  NoColorSpace,
  RGBAFormat,
  RepeatWrapping,
  SRGBColorSpace,
  Texture,
  UnsignedByteType,
} from "three/webgpu";
import type { Wrapping } from "three/webgpu";
import { udimTiles } from "@forest-rs/layerstack_stagehand";
import type { MaterialItem, MaterialTexture, MaterialValue, TextureWrap } from "@forest-rs/layerstack_stagehand";
import { drawLookups, SurfacePhysicalMaterial, SurfaceStandardMaterial } from "./channels.ts";
import type { LookupSampling, SurfaceMaterial } from "./channels.ts";
import { imageDecoders } from "./imageDecoders.ts";
import { coordinateMatrix, needsPhysical, surfaceLookups } from "./lookups.ts";

/**
 * Reads an asset's bytes from the host: `null` when it has none. `anchor`
 * is the identifier of the layer that authored the path, against which the
 * host resolves a search path (`textures/wood.png`), as OpenUSD's resolver
 * does; `null` for a fallback.
 */
export type AssetReader = (identifier: string, anchor: string | null) => Promise<Uint8Array | null>;

/** Whether the host's storage has an asset, for a UDIM set's tiles; `anchor` as {@link AssetReader}'s. */
export type AssetExists = (identifier: string, anchor: string | null) => Promise<boolean>;

/** A decoded image, or a UDIM atlas, and the grid it covers. */
interface Image {
  readonly texture: Texture;
  /** Tile columns and rows of a UDIM atlas; 1 × 1 for one image. */
  readonly columns: number;
  readonly rows: number;
}

/**
 * The longest side, in texels, the viewport draws an image at: a larger one
 * is scaled down while it is decoded. A scene's 4096² maps are 64 MB each
 * on the GPU; at 2048 they are 16 MB, and a production set's hundreds of
 * them fit a browser's memory. {@link TextureStore.stats} says how many
 * were.
 */
export const MAX_TEXTURE_SIZE = 2048;

/** What a {@link TextureStore} has decoded. */
export interface TextureStats {
  readonly images: number;
  /** Images scaled down to fit {@link MAX_TEXTURE_SIZE}. */
  readonly capped: number;
  /** {@link MAX_TEXTURE_SIZE}. */
  readonly limit: number;
  /** Images asked for and not yet decoded (or failed). */
  readonly pending: number;
  /** Decodes in the workers now, and how many workers there are ({@link imageDecoders}). */
  readonly decoding: number;
  readonly decoders: number;
}

/** The textures materials read, one image per identifier, loaded once. */
export class TextureStore {
  readonly #read: AssetReader;
  readonly #exists: AssetExists;
  readonly #loaded: () => void;
  readonly #images = new Map<string, Promise<Image | null>>();
  readonly #ready = new Map<string, Image | null>();
  /** Identifiers ready (or failed) since the last {@link takeArrived}. */
  #arrived = new Set<string>();
  #decoded = 0;
  #capped = 0;

  /** `loaded` is called when an image arrives, to draw again. */
  constructor(read: AssetReader, exists: AssetExists, loaded: () => void) {
    this.#read = read;
    this.#exists = exists;
    this.#loaded = loaded;
  }

  /** The image of `texture`'s file once loaded; `undefined` while loading, `null` when it cannot be read. */
  image(texture: Pick<MaterialTexture, "file">): Image | null | undefined {
    const identifier = texture.file.identifier;
    if (identifier === null) return null;
    if (this.#ready.has(identifier)) return this.#ready.get(identifier);
    if (!this.#images.has(identifier)) {
      const anchor = texture.file.layer;
      const loading = (texture.file.udim ? this.#atlas(texture) : this.#decode(identifier, anchor).then((image) => image && { texture: image, columns: 1, rows: 1 })).catch(
        (error: unknown) => {
          console.error(error);
          return null;
        },
      );
      this.#images.set(identifier, loading);
      void loading.then((image) => {
        this.#ready.set(identifier, image);
        this.#arrived.add(identifier);
        this.#loaded();
      });
    }
    return undefined;
  }

  /**
   * The identifiers whose images arrived (or failed) since the last call:
   * only the draws reading one of them need materials made again.
   */
  takeArrived(): ReadonlySet<string> {
    const arrived = this.#arrived;
    this.#arrived = new Set();
    return arrived;
  }

  /** What the store has decoded: images, and how many were scaled down to {@link MAX_TEXTURE_SIZE}. */
  get stats(): TextureStats {
    const decoders = imageDecoders.stats;
    return {
      images: this.#decoded,
      capped: this.#capped,
      limit: MAX_TEXTURE_SIZE,
      pending: this.#images.size - this.#ready.size,
      decoding: decoders.inFlight,
      decoders: decoders.workers,
    };
  }

  /**
   * The image at `identifier`, decoded off the main thread
   * (`createImageBitmap` in a worker, {@link imageDecoders}) so neither
   * decoding, scaling nor its first upload stalls the page, and scaled
   * down while decoding to fit
   * {@link MAX_TEXTURE_SIZE}. Its texels are kept as stored (no color
   * space conversion, no premultiplied alpha): a normal or roughness map is
   * data. `flip` turns it so `st`'s v runs up, as a texture of its own
   * wants; an atlas tile is drawn into a canvas unturned.
   */
  async #decode(identifier: string, anchor: string | null, flip = true): Promise<Texture | null> {
    const bytes = await this.#read(identifier, anchor);
    if (bytes === null) return null;
    const options: ImageBitmapOptions = { imageOrientation: flip ? "flipY" : "from-image", premultiplyAlpha: "none", colorSpaceConversion: "none" };
    // The size from the file's header, when it gives one, so a large image
    // is decoded once, already scaled; otherwise from a first decode.
    const known = imageSize(bytes);
    let bitmap = known === null ? await imageDecoders.decode(bytes, options) : null;
    const { width, height } = known ?? bitmap!;
    const scale = Math.min(1, MAX_TEXTURE_SIZE / Math.max(width, height));
    if (scale < 1) {
      const resizeWidth = Math.max(1, Math.round(width * scale));
      const resizeHeight = Math.max(1, Math.round(height * scale));
      bitmap?.close();
      bitmap = await imageDecoders.decode(bytes, { ...options, resizeWidth, resizeHeight, resizeQuality: "high" });
      this.#capped += 1;
    } else {
      bitmap ??= await imageDecoders.decode(bytes, options);
    }
    this.#decoded += 1;
    const texture = new Texture(bitmap);
    // The bitmap is turned already: three.js cannot flip one on upload.
    texture.flipY = false;
    texture.needsUpdate = true;
    return texture;
  }

  /** A UDIM set's tiles laid into one canvas, a column per tile's units digit, a row per ten. */
  async #atlas(texture: Pick<MaterialTexture, "file">): Promise<Image | null> {
    const anchor = texture.file.layer;
    const tiles = await udimTiles(texture.file, (identifier) => this.#exists(identifier, anchor));
    if (tiles.length === 0) return null;
    const images = await Promise.all(tiles.map((tile) => this.#decode(tile.identifier, anchor, false)));
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

  /** Frees every image and its uses ({@link use}). */
  dispose(): void {
    for (const image of this.#ready.values()) {
      if (image === null || image === undefined) continue;
      for (const kept of USES.get(image.texture)?.values() ?? []) kept.dispose();
      USES.delete(image.texture);
      image.texture.dispose();
    }
    this.#ready.clear();
    this.#images.clear();
    this.#arrived.clear();
  }
}

/**
 * A PNG's or JPEG's size in pixels from its header (PNG's `IHDR`, JPEG's
 * start-of-frame segment); `null` for any other format, a header it
 * cannot read, or a JPEG with EXIF data, whose orientation may turn it.
 */
export function imageSize(bytes: Uint8Array): { width: number; height: number } | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  // PNG: the signature, then `IHDR` with the width and height first.
  if (bytes.length >= 24 && view.getUint32(0) === 0x89504e47 && view.getUint32(12) === 0x49484452) {
    return { width: view.getUint32(16), height: view.getUint32(20) };
  }
  // JPEG: segments after SOI, up to the first start of frame (SOF0-SOF15,
  // except DHT, JPG and DAC).
  if (bytes.length < 4 || view.getUint16(0) !== 0xffd8) return null;
  let at = 2;
  while (at + 9 <= bytes.length) {
    if (bytes[at] !== 0xff) return null;
    const marker = bytes[at + 1]!;
    if (marker === 0xff) {
      at += 1;
      continue;
    }
    const length = view.getUint16(at + 2);
    if (marker === 0xe1) return null;
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      return { width: view.getUint16(at + 7), height: view.getUint16(at + 5) };
    }
    at += 2 + length;
  }
  return null;
}

const WRAPS: Record<TextureWrap, Wrapping> = {
  repeat: RepeatWrapping,
  mirror: MirroredRepeatWrapping,
  clamp: ClampToEdgeWrapping,
  black: ClampToEdgeWrapping,
  useMetadata: RepeatWrapping,
};

/** The 1×1 textures of fallback colors, shared by every use waiting on an image or unable to read one. */
const FALLBACKS = new Map<string, DataTexture>();

/**
 * Each image's uses, by what a use sets (uv channel, color space, wrap,
 * transform): one three.js texture, and so one upload, per distinct use,
 * however many materials share it or how often they are made again (a
 * restyle when an image arrives, a selection).
 */
const USES = new WeakMap<Texture, Map<string, Texture>>();

/**
 * How a lookup of `texture` samples on a mesh of `channels`: its
 * {@link use}, its uv set, and its coordinate matrix (its `UsdTransform2d`,
 * and its UDIM atlas's grid once loaded).
 */
export function sampling(store: TextureStore, channels: UvChannels, texture: MaterialTexture, color: boolean): LookupSampling {
  const image = store.image(texture);
  return {
    texture: use(store, channels, texture, color),
    set: channelOf(texture, channels),
    matrix: coordinateMatrix(texture.coordinates.transform, image?.columns, image?.rows),
  };
}

/** A use of `texture`: its image (or fallback color) with this use's sampling, color space and coordinate transform. */
export function use(store: TextureStore, channels: UvChannels, texture: MaterialTexture, color: boolean): Texture {
  const image = store.image(texture);
  if (image === null || image === undefined) {
    // While loading, and when it cannot be read: the fallback.
    const texel = texture.fallback.map((value) => Math.round(Math.min(1, Math.max(0, value)) * 255));
    const key = texel.join(" ");
    let fallback = FALLBACKS.get(key);
    if (fallback === undefined) {
      fallback = new DataTexture(new Uint8Array(texel), 1, 1, RGBAFormat, UnsignedByteType);
      fallback.needsUpdate = true;
      FALLBACKS.set(key, fallback);
    }
    return fallback;
  }
  const channel = channelOf(texture, channels);
  const srgb = texture.colorSpace === "sRGB" || (texture.colorSpace === "auto" && color);
  // `UsdTransform2d`, then a UDIM atlas's grid, for the maps three.js samples itself.
  const matrix = coordinateMatrix(texture.coordinates.transform, image.columns, image.rows);
  const key = [channel, srgb, texture.wrapS, texture.wrapT, ...matrix].join(" ");
  let uses = USES.get(image.texture);
  if (uses === undefined) {
    uses = new Map();
    USES.set(image.texture, uses);
  }
  const kept = uses.get(key);
  if (kept !== undefined) return kept;
  const out = image.texture.clone();
  out.channel = channel;
  out.colorSpace = srgb ? SRGBColorSpace : NoColorSpace;
  out.wrapS = WRAPS[texture.wrapS];
  out.wrapT = WRAPS[texture.wrapT];
  out.matrixAutoUpdate = false;
  out.matrix.set(...matrix);
  out.needsUpdate = true;
  uses.set(key, out);
  return out;
}

/** A mesh's three.js uv channels besides `uv` (`st`), by the name of the set each draws. */
export type UvChannels = ReadonlyMap<string, number>;

/**
 * The channels of a mesh's buffer: each set it draws besides `st`, in its
 * order, as `uv1` to `uv3`; the viewport sets those attributes from the
 * same map.
 */
export function uvChannels(mesh: { readonly uvSets: readonly { readonly name: string }[] }): UvChannels {
  return new Map(mesh.uvSets.slice(0, 3).map((set, index) => [set.name, index + 1]));
}

/** The three.js uv channel a texture's coordinates sample on a mesh of `channels`: its set's, else 0 (`st`). */
export function channelOf(texture: MaterialTexture, channels: UvChannels): number {
  const name = texture.coordinates.primvar;
  return name === null ? 0 : (channels.get(name) ?? 0);
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

/**
 * A three.js material for `record` on a draw (`vertexColors` when the
 * buffer has colors for a `displayColor` primvar; its textures sampling
 * the buffer's `uvChannels`), highlighted when selected, as the header of
 * this file maps it.
 */
export function surfaceMaterial(
  record: MaterialItem,
  store: TextureStore,
  options: { readonly vertexColors: boolean; readonly doubleSided: boolean; readonly highlighted: boolean; readonly uvChannels: UvChannels },
): SurfaceMaterial {
  const blend = record.alpha.kind === "blend" ? record.alpha : null;
  const transmissive = blend !== null && !blend.presence && record.opacity.kind === "constant";
  const material = needsPhysical(record) ? new SurfacePhysicalMaterial() : new SurfaceStandardMaterial();
  const surface = surfaceLookups(record);
  const drawn = new Set(surface.lookups.map((lookup) => lookup.map));
  // Diffuse: a constant, a texture lookup (`map`), or displayColor.
  const diffuse = record.diffuseColor;
  if (diffuse.kind === "primvar") {
    material.vertexColors = options.vertexColors && diffuse.name === "displayColor";
    const [r, g, b] = material.vertexColors ? [1, 1, 1] : diffuse.fallback;
    material.color.setRGB(r, g, b);
  } else if (diffuse.kind === "constant") {
    material.color.setRGB(...(diffuse.value as [number, number, number]));
  }
  // Emission: a lookup times the emissive color, white but for a selection.
  const emissive = record.emissiveColor;
  if (emissive.kind === "texture") material.emissive.setRGB(1, 1, 1);
  else material.emissive.setRGB(...(constant(emissive, [0, 0, 0] as const) as [number, number, number]));
  // One-channel inputs: a lookup's channel, else the constant (or the
  // fallback of a texture three.js cannot read).
  material.metalness = record.useSpecularWorkflow ? 0 : constant(record.metallic, fallbackChannel(record, record.metallic, 0));
  material.roughness = constant(record.roughness, fallbackChannel(record, record.roughness, 0.5));
  // Opacity.
  if (record.alpha.kind === "mask") {
    material.alphaTest = record.alpha.threshold;
  } else if (blend !== null && !transmissive) {
    material.transparent = true;
    material.opacity = constant(record.opacity, 1);
    material.depthWrite = false;
  }
  if (material instanceof MeshPhysicalNodeMaterial) {
    material.ior = Math.min(2.333, Math.max(1, constant(record.ior, 1.5)));
    // A clearcoat lookup's value is drawn whole (three.js reads clearcoat
    // only where `clearcoat` is above 0).
    material.clearcoat = drawn.has("clearcoatMap") ? 1 : constant(record.clearcoat, fallbackChannel(record, record.clearcoat, 0));
    material.clearcoatRoughness = constant(record.clearcoatRoughness, fallbackChannel(record, record.clearcoatRoughness, 0.01));
    if (record.useSpecularWorkflow) {
      material.specularIntensity = 1;
      const specular = record.specularColor;
      if (specular.kind !== "texture") material.specularColor.setRGB(...(constant(specular, [0, 0, 0] as const) as [number, number, number]));
    }
    if (transmissive) material.transmission = 1 - constant(record.opacity, 1);
  }
  drawLookups(material, surface, (lookup) => sampling(store, options.uvChannels, record.textures[lookup.texture]!, lookup.color));
  if (options.doubleSided) material.side = DoubleSide;
  if (options.highlighted) {
    material.emissive = new Color(0x2f5fd0);
    material.emissiveIntensity = 0.6;
  }
  return material;
}
