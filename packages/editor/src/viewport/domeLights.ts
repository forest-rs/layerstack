// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A `DomeLight`'s texture, as the viewport lights with it: a three.js
// `HemisphereLight` whose sky and ground colors are the texture's.
//
// A dome of radiance `L(ω)` (`inputs:texture:file` times the light's
// radiance) gives a surface of normal `n` the irradiance `∫ L(ω) n·ω dω`
// over the hemisphere around `n` (`UsdLux` `DomeLight`; Storm lights a
// `UsdPreviewSurface` with the dome's irradiance map, `HdSt` dome light
// textures). A `HemisphereLight` adds `mix(ground, sky, 0.5 + 0.5 n·up)`:
// exactly that irradiance for a surface facing the pole or the horizon
// when `sky` and `ground` are the cosine-weighted mean radiance of the
// upper and lower half of the dome (`π L` for a surface facing a uniform
// half of radiance `L`, the intensity being `π` × the light's), and close
// between them. Its specular reflections are not drawn: three.js reflects
// nothing of a `HemisphereLight`.
//
// The texture is read as a `latlong` map (`inputs:texture:format`
// `latlong`, or `automatic`, which Storm reads as one): its top row is the
// pole, its middle row the horizon. 8-bit texels are sRGB-encoded, as
// Storm reads an 8-bit color texture without metadata (`HioImage`'s
// `auto` color space). Other formats, and a file the browser cannot
// decode (`.exr`, `.hdr`: not fetched), leave the dome one color.
import type { HemisphereLight } from "three/webgpu";
import type { LightItem, TextureFile } from "@forest-rs/layerstack_stagehand";
import type { TextureStore } from "./surfaces.ts";

/** A linear RGB color. */
export type Rgb = readonly [number, number, number];

/** The colors of a dome's halves: the cosine-weighted mean radiance (linear) of each. */
export interface Hemisphere {
  readonly sky: Rgb;
  readonly ground: Rgb;
}

/** The sRGB transfer function, inverted: an encoded value in [0, 1] to linear. */
function decode(value: number): number {
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

/**
 * The sky and ground colors of a `latlong` image: RGBA texels in rows,
 * `topFirst` when the first row is the image's top (the pole), linear or
 * sRGB-encoded (`srgb`). Each row is weighted by its solid angle and its
 * cosine to the pole (`|sin φ| cos φ` at elevation `φ`), so a uniform half
 * of radiance `L` has the color `L`.
 */
export function latlongHemisphere(texels: ArrayLike<number>, width: number, height: number, srgb: boolean, topFirst: boolean): Hemisphere {
  const sums = [
    [0, 0, 0, 0],
    [0, 0, 0, 0],
  ];
  const lookup = Array.from({ length: 256 }, (_, value) => (srgb ? decode(value / 255) : value / 255));
  for (let row = 0; row < height; row++) {
    // The elevation of the row's center: π/2 at the top of the image, -π/2 at its bottom.
    const top = topFirst ? row : height - 1 - row;
    const elevation = Math.PI / 2 - (Math.PI * (top + 0.5)) / height;
    const weight = Math.abs(Math.sin(elevation)) * Math.cos(elevation);
    const sum = sums[elevation >= 0 ? 0 : 1]!;
    for (let column = 0; column < width; column++) {
      const at = 4 * (row * width + column);
      for (let channel = 0; channel < 3; channel++) sum[channel]! += weight * lookup[texels[at + channel]!]!;
      sum[3]! += weight;
    }
  }
  const mean = (sum: number[]): Rgb => (sum[3]! > 0 ? [sum[0]! / sum[3]!, sum[1]! / sum[3]!, sum[2]! / sum[3]!] : [1, 1, 1]);
  return { sky: mean(sums[0]!), ground: mean(sums[1]!) };
}

/** The image files a browser decodes (`createImageBitmap`). */
const BROWSER_IMAGE = /\.(png|jpe?g|webp|avif|gif|bmp)$/i;

/**
 * The file of a dome's texture the viewport reads: a `latlong` (or
 * `automatic`) image a browser decodes, by its asset path's extension (an
 * identifier may end in a package's `]`). `null` for none, a UDIM set,
 * another format, or a file such as an `.exr` or `.hdr`, which is not
 * fetched: it can be hundreds of megabytes, and would not decode.
 */
export function domeImage(geometry: LightItem["geometry"]): (TextureFile & { readonly identifier: string }) | null {
  if (geometry.kind !== "dome") return null;
  const file = geometry.file;
  if (file === null || file.identifier === null || file.udim || (geometry.format !== "latlong" && geometry.format !== "automatic")) return null;
  return BROWSER_IMAGE.test(file.assetPath) ? (file as TextureFile & { readonly identifier: string }) : null;
}

/** The texel grid a dome's image is read at: enough for its halves' means, small enough to read at once. */
const READ_WIDTH = 128;
const READ_HEIGHT = 64;

/**
 * The domes' `HemisphereLight`s, colored by their textures once loaded:
 * the colors set when the image is in the store, or when it arrives
 * ({@link arrived}). The light keeps its program: only its colors change.
 */
export class DomeTints {
  readonly #textures: TextureStore;
  readonly #tinted: () => void;
  /** Each identifier's colors; `null` for one that cannot be read. */
  readonly #colors = new Map<string, Hemisphere | null>();
  /** Lights whose texture is still loading, and the light item each draws. */
  readonly #waiting = new Map<HemisphereLight, LightItem>();

  /** `tinted` is called when a light's colors change after it was added, to draw again. */
  constructor(textures: TextureStore, tinted: () => void) {
    this.#textures = textures;
    this.#tinted = tinted;
  }

  /** Colors `hemisphere`, `light`'s, by its texture: now when it is loaded, else when it arrives. */
  tint(hemisphere: HemisphereLight, light: LightItem): void {
    if (!this.#apply(hemisphere, light)) this.#waiting.set(hemisphere, light);
  }

  /** Forgets every light's colors, and the lights waiting: the textures they were read from are gone. */
  clear(): void {
    this.#colors.clear();
    this.#waiting.clear();
  }

  /** Forgets `hemisphere`: its light was dropped. */
  drop(hemisphere: HemisphereLight): void {
    this.#waiting.delete(hemisphere);
  }

  /** Colors the waiting lights whose images arrived. */
  arrived(): void {
    let changed = false;
    for (const [hemisphere, light] of [...this.#waiting]) {
      if (!this.#apply(hemisphere, light)) continue;
      this.#waiting.delete(hemisphere);
      changed = true;
    }
    if (changed) this.#tinted();
  }

  /** Sets `hemisphere`'s colors from its texture; `false` while the image is loading. */
  #apply(hemisphere: HemisphereLight, light: LightItem): boolean {
    const file = domeImage(light.geometry);
    if (file === null) return true;
    let colors = this.#colors.get(file.identifier);
    if (colors === undefined) {
      const image = this.#textures.image({ file });
      if (image === undefined) return false;
      colors = image === null ? null : read(image.texture.image as CanvasImageSource | null);
      this.#colors.set(file.identifier, colors);
    }
    if (colors !== null) {
      // The light's color (its radiance over its peak) times the texture's.
      const base = hemisphere.color.clone();
      hemisphere.color.setRGB(base.r * colors.sky[0], base.g * colors.sky[1], base.b * colors.sky[2]);
      hemisphere.groundColor.setRGB(base.r * colors.ground[0], base.g * colors.ground[1], base.b * colors.ground[2]);
    }
    return true;
  }
}

/** The colors of a decoded `latlong` image (as the texture store keeps it: turned so its first row is the bottom). */
function read(image: CanvasImageSource | null): Hemisphere | null {
  if (image === null || typeof OffscreenCanvas === "undefined") return null;
  const canvas = new OffscreenCanvas(READ_WIDTH, READ_HEIGHT);
  const context = canvas.getContext("2d", { colorSpace: "srgb" });
  if (context === null) return null;
  context.drawImage(image, 0, 0, READ_WIDTH, READ_HEIGHT);
  const texels = context.getImageData(0, 0, READ_WIDTH, READ_HEIGHT).data;
  // The store keeps 8-bit images, read as stored: sRGB-encoded, the bitmap turned (`flipY`).
  return latlongHemisphere(texels, READ_WIDTH, READ_HEIGHT, true, false);
}
