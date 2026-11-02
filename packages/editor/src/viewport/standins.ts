// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A model drawn by its draw mode (`model:drawMode`), as three.js draws
// stagehand's stand-in: the axes of `origin` and the box edges of `bounds`
// as line segments in `model:drawModeColor`; the cards of `cards` as quads,
// one-sided as Hydra culls their backs, each lit with its texture
// (`model:cardTexture*`, clamped, its alpha cut out below 0.1 as Hydra's
// card material does) or, without one or until it loads, the color.
//
// A model's cards are one draw, as Hydra's are (one mesh whose
// `cardsTexAssign` primvar picks each card's texture,
// `pxr/usdImaging/usdImaging/drawModeStandin.cpp`): one material holds the
// six faces' textures, each card's vertices carry the face whose texture
// it shows (`cardFace`), and the shader picks that face's. A draw per card
// cost six times the draws, and the GPU's cost is per draw: a production
// set of 1,280 card models, 7,680 cards, took 28 ms a frame drawn card by
// card and 5 ms model by model. Every card material builds the same shader
// (its nodes are shared and read the material's own textures and looks by
// reference, as `channels.ts`'s do), so every card model is one pipeline,
// and its nodes are fixed (`fixedNodes.ts`): three.js does not refresh each
// card draw in full every frame for them.
import {
  BufferAttribute,
  BufferGeometry,
  ClampToEdgeWrapping,
  Color,
  DataTexture,
  FrontSide,
  LinearFilter,
  LinearMipmapLinearFilter,
  LineBasicNodeMaterial,
  MeshStandardNodeMaterial,
  RGBAFormat,
  SRGBColorSpace,
  UnsignedByteType,
  Vector4,
} from "three/webgpu";
import type { Texture } from "three/webgpu";
import { attribute, materialReference, texture, vec4 } from "three/tsl";
import type { StandinBuffers } from "@forest-rs/layerstack_stagehand";
import { CARD_FACES } from "@forest-rs/layerstack_stagehand";
import { setFixedNode } from "./fixedNodes.ts";
import type { TextureStore } from "./surfaces.ts";

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Node = any;

/** The selection's highlight, as the other draws show it. */
const HIGHLIGHT = 0x6f9fff;

/** The geometry of a stand-in: its line segments, or its cards, each card's vertices carrying the face whose texture it shows ({@link cardFace}). */
export function standinGeometry(standin: StandinBuffers): BufferGeometry {
  const geometry = new BufferGeometry();
  if (standin.mode !== "cards") {
    geometry.setAttribute("position", new BufferAttribute(standin.lines.slice(), 3));
    geometry.computeBoundingSphere();
    return geometry;
  }
  const count = standin.cards.length;
  const positions = new Float32Array(12 * count);
  const uvs = new Float32Array(8 * count);
  const normals = new Float32Array(12 * count);
  const faces = new Float32Array(4 * count);
  const indices = new Uint32Array(6 * count);
  standin.cards.forEach((card, i) => {
    positions.set(card.corners, 12 * i);
    uvs.set(card.uvs, 8 * i);
    const [a, b, c] = [0, 1, 2].map((k) => card.corners.subarray(3 * k, 3 * k + 3)) as [Float32Array, Float32Array, Float32Array];
    const u = [b[0]! - a[0]!, b[1]! - a[1]!, b[2]! - a[2]!];
    const v = [c[0]! - a[0]!, c[1]! - a[1]!, c[2]! - a[2]!];
    const n = [u[1]! * v[2]! - u[2]! * v[1]!, u[2]! * v[0]! - u[0]! * v[2]!, u[0]! * v[1]! - u[1]! * v[0]!];
    const length = Math.hypot(n[0]!, n[1]!, n[2]!) || 1;
    for (let k = 0; k < 4; k++) normals.set([n[0]! / length, n[1]! / length, n[2]! / length], 12 * i + 3 * k);
    indices.set([0, 1, 2, 0, 2, 3].map((k) => 4 * i + k), 6 * i);
    faces.fill(cardFace(card.texture), 4 * i, 4 * i + 4);
  });
  geometry.setIndex(new BufferAttribute(indices, 1));
  geometry.setAttribute("position", new BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new BufferAttribute(normals, 3));
  geometry.setAttribute("uv", new BufferAttribute(uvs, 2));
  geometry.setAttribute("cardFace", new BufferAttribute(faces, 1));
  geometry.computeBoundingSphere();
  return geometry;
}

/** The slot of a card's texture in its material: its face's, in {@link CARD_FACES} order; 0 for a card in the color (every slot is then the color). */
function cardFace(texture: StandinBuffers["cards"][number]["texture"]): number {
  return texture === null ? 0 : CARD_FACES.indexOf(texture);
}

/** The material of a stand-in: its lines' color, or one for all its cards ({@link CardMaterial}). */
export function standinMaterials(standin: StandinBuffers, highlighted: boolean, textures: TextureStore): LineBasicNodeMaterial | MeshStandardNodeMaterial {
  const color = new Color().setRGB(...(standin.color as [number, number, number]));
  if (standin.mode !== "cards") return new LineBasicNodeMaterial({ color: highlighted ? new Color(HIGHLIGHT) : color });
  const material = new CardMaterial();
  if (highlighted) {
    material.emissive = new Color(0x2f5fd0);
    material.emissiveIntensity = 0.6;
  }
  const slots = material as unknown as Record<string, Texture | Vector4>;
  // Each face in turn: its texture once loaded, with UsdPreviewSurface's
  // fallbacks besides the card's color and cutout; while loading, without
  // one, or unreadable, the color.
  CARD_FACES.forEach((_, slot) => {
    const file = standin.textures[slot] ?? null;
    const image = file === null ? null : textures.image({ file });
    slots[`cardMap${slot}`] = image == null ? white() : cardMap(image.texture);
    slots[`cardLook${slot}`] = image == null ? new Vector4(color.r, color.g, color.b, 1) : new Vector4(1, 1, 1, 0.5);
  });
  return material;
}

/**
 * A model's cards' material: the six faces' textures (`cardMap0` to
 * `cardMap5`, in {@link CARD_FACES} order) and looks (`cardLook0` to
 * `cardLook5`: the color the texture is multiplied by, and the roughness),
 * the card's face picking one. Its color and roughness nodes are shared by
 * every card material and fixed, a copy's too.
 */
export class CardMaterial extends MeshStandardNodeMaterial {
  constructor() {
    super();
    this.side = FrontSide;
    // Hydra's card cutout. A card in the color has an opaque white texture:
    // every card material keeps the same cutout, and so the same shader.
    this.alphaTest = 0.1;
    this.#fix();
  }

  override copy(source: CardMaterial): this {
    super.copy(source);
    for (let slot = 0; slot < CARD_FACES.length; slot++) {
      const from = source as unknown as Record<string, Texture | Vector4>;
      const to = this as unknown as Record<string, Texture | Vector4>;
      to[`cardMap${slot}`] = from[`cardMap${slot}`]!;
      to[`cardLook${slot}`] = (from[`cardLook${slot}`] as Vector4).clone();
    }
    this.#fix();
    return this;
  }

  #fix(): void {
    const nodes = cardNodes();
    setFixedNode(this, "colorNode", nodes.color);
    setFixedNode(this, "roughnessNode", nodes.roughness);
  }
}

/** The card materials' shared nodes, made at the first card material. */
let CARD_NODES: { color: Node; roughness: Node } | null = null;

/** The color of the card's face's texture times its look, and its look's roughness. */
function cardNodes(): { color: Node; roughness: Node } {
  if (CARD_NODES !== null) return CARD_NODES;
  // Every vertex of a card carries the same slot: no fraction survives
  // interpolation that the half-way comparisons would not absorb.
  const face = attribute("cardFace", "float") as Node;
  const look = (slot: number): Node => materialReference(`cardLook${slot}`, "vec4");
  const texel = (slot: number): Node => slotTexture(slot).mul(vec4(look(slot).xyz, 1));
  let color: Node = texel(CARD_FACES.length - 1);
  let roughness: Node = look(CARD_FACES.length - 1).w;
  for (let slot = CARD_FACES.length - 2; slot >= 0; slot--) {
    const here = face.lessThan(slot + 0.5);
    color = here.select(texel(slot), color);
    roughness = here.select(look(slot).w, roughness);
  }
  CARD_NODES = { color, roughness };
  return CARD_NODES;
}

/**
 * The texture in slot `slot` of the material drawn: its `cardMap` texture,
 * read for each draw. Not merged with another slot holding the same
 * texture, as three.js merges texture uniforms by their texture when a
 * shader is built: the shader every card material shares would otherwise
 * read one texture for every slot that held it in the first card material
 * built (six white ones while its images load).
 */
function slotTexture(slot: number): Node {
  const node = texture(white()) as Node;
  node.mergeable = false;
  return node.onObjectUpdate(({ material }: { material: Record<string, Texture> }) => material[`cardMap${slot}`]);
}

/**
 * The texture of a face in the color: one opaque white texel, sampled and
 * colored as a card's image is (clamped, filtered, sRGB), so every card
 * material builds the same shader whichever of its faces have images.
 */
let WHITE: DataTexture | null = null;

function white(): DataTexture {
  if (WHITE !== null) return WHITE;
  WHITE = new DataTexture(new Uint8Array([255, 255, 255, 255]), 1, 1, RGBAFormat, UnsignedByteType);
  WHITE.colorSpace = SRGBColorSpace;
  WHITE.wrapS = ClampToEdgeWrapping;
  WHITE.wrapT = ClampToEdgeWrapping;
  WHITE.magFilter = LinearFilter;
  WHITE.minFilter = LinearMipmapLinearFilter;
  WHITE.needsUpdate = true;
  return WHITE;
}

/** Each loaded image's card use (clamped, sRGB), shared by every card showing it: one upload per image. */
const MAPS = new WeakMap<Texture, Texture>();

function cardMap(texture: Texture): Texture {
  let map = MAPS.get(texture);
  if (map === undefined) {
    map = texture.clone();
    map.colorSpace = SRGBColorSpace;
    map.wrapS = ClampToEdgeWrapping;
    map.wrapT = ClampToEdgeWrapping;
    MAPS.set(texture, map);
  }
  return map;
}
