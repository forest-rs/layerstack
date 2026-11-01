// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A model drawn by its draw mode (`model:drawMode`), as three.js draws
// stagehand's stand-in: the axes of `origin` and the box edges of `bounds`
// as line segments in `model:drawModeColor`; the cards of `cards` as quads,
// one-sided as Hydra culls their backs, each lit with its texture
// (`model:cardTexture*`, clamped, its alpha cut out below 0.1 as Hydra's
// card material does) or, without one or until it loads, the color.
import { BufferAttribute, BufferGeometry, ClampToEdgeWrapping, Color, FrontSide, LineBasicMaterial, MeshStandardMaterial, SRGBColorSpace } from "three";
import type { Texture } from "three";
import type { CardFace, StandinBuffers } from "@forest-rs/layerstack_stagehand";
import { CARD_FACES } from "@forest-rs/layerstack_stagehand";
import type { TextureStore } from "./surfaces.ts";

/** The selection's highlight, as the other draws show it. */
const HIGHLIGHT = 0x6f9fff;

/** The geometry of a stand-in: its line segments, or its cards with a group each. */
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
    geometry.addGroup(6 * i, 6, i);
  });
  geometry.setIndex(new BufferAttribute(indices, 1));
  geometry.setAttribute("position", new BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new BufferAttribute(normals, 3));
  geometry.setAttribute("uv", new BufferAttribute(uvs, 2));
  geometry.computeBoundingSphere();
  return geometry;
}

/** The materials of a stand-in: its lines' color, or one per card. */
export function standinMaterials(standin: StandinBuffers, highlighted: boolean, textures: TextureStore): LineBasicMaterial | MeshStandardMaterial[] {
  const color = new Color().setRGB(...(standin.color as [number, number, number]));
  if (standin.mode !== "cards") return new LineBasicMaterial({ color: highlighted ? new Color(HIGHLIGHT) : color });
  const emissive = highlighted ? { emissive: new Color(0x2f5fd0), emissiveIntensity: 0.6 } : {};
  return standin.cards.map((card) => {
    const image = card.texture === null ? null : imageOf(standin, card.texture, textures);
    if (image == null) return new MeshStandardMaterial({ color, roughness: 1, side: FrontSide, ...emissive });
    // UsdPreviewSurface's fallbacks besides the card's color and cutout.
    return new MeshStandardMaterial({ map: cardMap(image.texture), alphaTest: 0.1, roughness: 0.5, side: FrontSide, ...emissive });
  });
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

/** The loaded image of the card texture `face`; `undefined` while loading, `null` when it has none or cannot be read. */
function imageOf(standin: StandinBuffers, face: CardFace, textures: TextureStore) {
  const file = standin.textures[CARD_FACES.indexOf(face)] ?? null;
  return file === null ? null : textures.image({ file });
}

/** Whether a stand-in reads textures: a cards stand-in with at least one. */
export function standinTextured(standin: StandinBuffers | null): boolean {
  return standin !== null && standin.mode === "cards" && standin.textures.some((texture) => texture !== null);
}
