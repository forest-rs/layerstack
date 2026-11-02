// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A model's draw mode stand-in: the axes, box or textured cards Hydra draws
// in place of a model whose `model:drawMode` is `origin`, `bounds` or
// `cards`, read off the stage as `layerstack_stagehand` reads it, and built
// by this package's module (`layerstack_stagehand::draw_mode_standin`).
import type { Prim, Stage, Value } from "@forest-rs/layerstack";
import { anchorAssetPath, isUdim } from "./material.js";
import type { TextureFile } from "./material.js";

/** The alternate imaging mode a model is drawn with (`model:drawMode`), when not drawn as itself. */
export type DrawMode = "origin" | "bounds" | "cards";

/** The geometry of a model's cards (`model:cardGeometry`); a token the schema does not list is `cross`. */
export type CardGeometry = "cross" | "box" | "fromTexture";

/** One face of a model's box: a card and its texture (`model:cardTexture{X,Y,Z}{Pos,Neg}`). */
export type CardFace = "XPos" | "YPos" | "ZPos" | "XNeg" | "YNeg" | "ZNeg";

/** Every face, in Hydra's order: the order of {@link StandinBuffers.textures}. */
export const CARD_FACES: readonly CardFace[] = Object.freeze(["XPos", "YPos", "ZPos", "XNeg", "YNeg", "ZNeg"]);

/** What a stand-in is built from: the model's draw mode, extent and card settings. */
export interface StandinInput {
  readonly mode: DrawMode;
  /**
   * The model's extent, `[minX, minY, minZ, maxX, maxY, maxZ]` in its own
   * space; `null` draws the box and cards at the origin with no size, as
   * Hydra does for a model with neither `extent` nor `extentsHint`.
   */
  readonly extent: Float32Array | null;
  readonly cardGeometry: CardGeometry;
  /** Whether each face, in {@link CARD_FACES} order, has a texture: a non-empty asset path authored. */
  readonly textures: readonly boolean[];
  /** The axis whose two cards are hidden: the stage's up axis when `model:cardVisibility` resolves to `simple`. */
  readonly hiddenAxis: "X" | "Y" | "Z" | null;
}

/** One card: a quad in the model's space, drawn one-sided as the triangles `0 1 2` and `0 2 3` of its corners. */
export interface Card {
  readonly face: CardFace;
  /** Its corners, counter-clockwise seen from the side it faces: twelve numbers. */
  readonly corners: Float32Array;
  /** Each corner's texture coordinate (Hydra's `cardsUv`): eight numbers. */
  readonly uvs: Float32Array;
  /** The face whose texture it shows: its own, or the opposite face's, mirrored; `null` when no face has a texture (drawn in the color). */
  readonly texture: CardFace | null;
}

/** A model's stand-in geometry, in its own space. */
export interface Standin {
  readonly mode: DrawMode;
  /** Line segments, two endpoints of three numbers each: the axes of `origin`, the box edges of `bounds`. */
  readonly lines: Float32Array;
  /** The cards of `cards`, in Hydra's order (`+X`, `-X`, `+Y`, `-Y`, `+Z`, `-Z`, those drawn). */
  readonly cards: readonly Card[];
}

/** A draw list's stand-in buffer ({@link DrawBuffers.standin}): the geometry, what it was built from, its color and textures. */
export interface StandinBuffers extends Standin {
  readonly input: StandinInput;
  /** `model:drawModeColor`: the lines' color, and the cards' when no face has a texture or a texture cannot be read. */
  readonly color: readonly [number, number, number];
  /**
   * Each face's texture, in {@link CARD_FACES} order, anchored to the layer
   * authoring it. Hydra draws it with `clamp` wrapping, its color as the
   * diffuse color and its alpha as the opacity, cut out below 0.1.
   */
  readonly textures: readonly (TextureFile | null)[];
}

/** The flat stand-in the module returns, decoded. */
export function decodeStandin(mode: DrawMode, flat: Float32Array): Standin {
  if (mode !== "cards") return Object.freeze({ mode, lines: flat, cards: Object.freeze([]) });
  const cards: Card[] = [];
  for (let at = 0; at + 22 <= flat.length; at += 22) {
    const texture = flat[at + 1]!;
    cards.push(
      Object.freeze({
        face: CARD_FACES[flat[at]!]!,
        texture: texture < 0 ? null : CARD_FACES[texture]!,
        corners: flat.slice(at + 2, at + 14),
        uvs: flat.slice(at + 14, at + 22),
      }),
    );
  }
  return Object.freeze({ mode, lines: new Float32Array(), cards: Object.freeze(cards) });
}

function token(value: Value | null): string | null {
  return value?.kind === "scalar" && typeof value.value === "string" ? value.value : null;
}

/**
 * The draw mode a prim resolves to given `inherited`, its parent's: its own
 * `model:drawMode` when it has `GeomModelAPI` applied and that is not
 * `inherited`, else its parent's, as OpenUSD 26.11's Hydra resolves it
 * (`UsdImagingFlattenedGeomModelDataSourceProvider`).
 */
export function flattenedDrawMode(prim: Prim, inherited: string | null): string | null {
  if (!prim.hasAPI("GeomModelAPI")) return inherited;
  const own = token(prim.attribute("model:drawMode")?.get() ?? null);
  return own === null || own === "" || own === "inherited" ? inherited : own;
}

/** The draw mode the parent of the prim at `path` resolves to: the nearest ancestor's own. */
export function inheritedDrawMode(stage: Stage, path: string): string | null {
  for (let at = parentPath(path); at !== null; at = parentPath(at)) {
    const prim = stage.prim(at);
    const mode = prim === null ? null : flattenedDrawMode(prim, null);
    if (mode !== null) return mode;
  }
  return null;
}

/**
 * The stand-in the prim is drawn by, its draw mode resolving to `mode`:
 * `origin`, `bounds` or `cards` on a prim with `GeomModelAPI` that applies
 * it (`model:applyDrawMode = true`, or none authored and kind
 * `component`), as `layerstack_stagehand` decides it.
 */
export function standinMode(prim: Prim, mode: string | null): DrawMode | null {
  if (mode !== "origin" && mode !== "bounds" && mode !== "cards") return null;
  if (!prim.hasAPI("GeomModelAPI")) return null;
  const apply = prim.attribute("model:applyDrawMode");
  const authored = apply?.authored ? apply.get() : null;
  const applies =
    authored?.kind === "scalar" && typeof authored.value === "boolean" ? authored.value : token(prim.metadata("kind")) === "component";
  return applies ? mode : null;
}

function parentPath(path: string): string | null {
  if (path === "/") return null;
  const at = path.lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}

/** An array value's numbers, three per element; `null` (released) for anything else. */
function vec3s(value: Value | null): Float32Array | null {
  if (value?.kind !== "array") return null;
  try {
    if (value.array.components !== 3 || value.array.scalar === "string") return null;
    const elements = value.array.copy();
    return elements instanceof Float32Array ? elements : Float32Array.from(elements as ArrayLike<number>);
  } finally {
    value.array.release();
  }
}

/**
 * The model's extent as Hydra's stand-in reads it: its authored `extent`
 * when it is a `Boundable`, else the union of its `extentsHint` for the
 * `default`, `render` and `proxy` purposes.
 */
export function standinExtent(prim: Prim, time: number | undefined): Float32Array | null {
  if (prim.isA("Boundable")) {
    const extent = prim.attribute("extent");
    const values = extent?.authored ? vec3s(extent.get(time)) : null;
    if (values !== null && values.length === 6) return values;
  }
  const hint = prim.attribute("extentsHint");
  const values = hint?.authored ? vec3s(hint.get(time)) : null;
  if (values === null) return null;
  const out = Float32Array.of(Infinity, Infinity, Infinity, -Infinity, -Infinity, -Infinity);
  const purposes = Math.min(Math.floor(values.length / 6), 3);
  for (let p = 0; p < purposes; p++) {
    for (let axis = 0; axis < 3; axis++) {
      out[axis] = Math.min(out[axis]!, values[6 * p + axis]!);
      out[3 + axis] = Math.max(out[3 + axis]!, values[6 * p + 3 + axis]!);
    }
  }
  return [0, 1, 2].every((axis) => out[axis]! <= out[3 + axis]!) ? out : null;
}

/** A card texture attribute's file, anchored to the layer authoring it; `null` for none or an empty path. */
function textureOf(stage: Stage, prim: Prim, face: CardFace, time: number | undefined): TextureFile | null {
  const attribute = prim.attribute(`model:cardTexture${face}`);
  if (attribute === null) return null;
  const value = attribute.get(time);
  if (value?.kind !== "scalar" || value.type !== "asset" || typeof value.value !== "string" || value.value === "") return null;
  const explanation = stage.attribute(attribute.path)?.explain(time);
  const layer = explanation?.opinions.find((opinion) => opinion.role === "contributed")?.layer.identifier ?? null;
  return Object.freeze({ assetPath: value.value, layer, identifier: anchorAssetPath(value.value, layer), udim: isUdim(value.value) });
}

/** What the stand-in of the model `prim`, drawn in `mode`, is built from at `time`, with its color and textures. */
export function readStandinInput(
  stage: Stage,
  prim: Prim,
  mode: DrawMode,
  time: number | undefined,
): { input: StandinInput; color: readonly [number, number, number]; textures: readonly (TextureFile | null)[] } {
  const geometry = token(prim.attribute("model:cardGeometry")?.get() ?? null);
  // `model:cardVisibility` is inherited as `model:drawMode` is.
  let visibility: string | null = null;
  for (let at: string | null = prim.path; at !== null && visibility === null; at = parentPath(at)) {
    const above = stage.prim(at);
    if (above === null || !above.hasAPI("GeomModelAPI")) continue;
    const own = token(above.attribute("model:cardVisibility")?.get() ?? null);
    if (own !== null && own !== "inherited") visibility = own;
  }
  const upAxis = token(stage.metadata("upAxis")) ?? "Y";
  const color = prim.attribute("model:drawModeColor")?.get() ?? null;
  const textures = CARD_FACES.map((face) => textureOf(stage, prim, face, time));
  return {
    input: Object.freeze({
      mode,
      extent: standinExtent(prim, time),
      cardGeometry: geometry === "box" || geometry === "fromTexture" ? geometry : "cross",
      textures: Object.freeze(textures.map((texture) => texture !== null)),
      hiddenAxis: visibility === "simple" && (upAxis === "X" || upAxis === "Y" || upAxis === "Z") ? upAxis : null,
    }),
    color: color?.kind === "tuple" && color.value.length === 3 ? [color.value[0]!, color.value[1]!, color.value[2]!] : [0.18, 0.18, 0.18],
    textures: Object.freeze(textures),
  };
}

/** Whether two stand-in readings are the same. */
export function sameStandin(a: StandinBuffers, input: StandinInput, color: readonly number[], textures: readonly (TextureFile | null)[]): boolean {
  const same = (x: unknown, y: unknown) => JSON.stringify(x, (_, v) => (ArrayBuffer.isView(v) ? Array.from(v as Float32Array) : v)) === JSON.stringify(y, (_, v) => (ArrayBuffer.isView(v) ? Array.from(v as Float32Array) : v));
  return same(a.input, input) && same(a.color, color) && same(a.textures, textures);
}
