// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the viewport draws, and what the asset view makes of the
// selection. The viewport draws the whole scene, the selection alone (the
// asset view), or nothing; a scene too big to draw opens in the asset view
// and says why. The asset view draws the selected prim's subtree alone,
// framed at the origin (a draw list scope in the prim's own space): a
// gprim, a model or group, a point instancer's instances, a prototype of
// one; a face subset as its mesh, the subset highlighted and the rest
// ghosted. A texture shows as its image and a material on the shader
// ball, drawing nothing.
import type { Stage } from "@forest-rs/layerstack";
import type { CostTotals, DrawScope } from "@forest-rs/layerstack_stagehand";
import { enclosingMaterial, parentPath } from "./network.ts";

/** What the viewport draws: the whole scene, the selection alone, or nothing. */
export type DrawScopeChoice = "scene" | "selection" | "nothing";

/**
 * The most the editor draws without being asked: a scene, or a selection
 * in the asset view, whose estimated draws upload more bytes or draw more
 * triangles is not drawn until chosen. 512 MB of buffers is about what a
 * WebGL context on a laptop holds beside the page before the browser loses
 * it, and the editor keeps a JavaScript copy of as much again; 50 million
 * triangles a frame is about what such a GPU draws at an interactive rate.
 */
export const DRAW_LIMIT = { gpuBytes: 512 * 1024 * 1024, trianglesDrawn: 50_000_000 } as const;

/** Why drawing what `total` estimates would be too much ({@link DRAW_LIMIT}), or `null` when it fits. */
export function overLimit(total: CostTotals): string | null {
  const bytes = (count: number) => (count >= 1024 ** 3 ? `${(count / 1024 ** 3).toFixed(1)} GB` : `${Math.round(count / 1024 ** 2)} MB`);
  if (total.gpuBytes > DRAW_LIMIT.gpuBytes) {
    return `it would upload about ${bytes(total.gpuBytes)} of geometry, over the ${bytes(DRAW_LIMIT.gpuBytes)} the editor draws unasked`;
  }
  if (total.trianglesDrawn > DRAW_LIMIT.trianglesDrawn) {
    return `it would draw about ${(total.trianglesDrawn / 1e6).toFixed(0)} million triangles, over the ${DRAW_LIMIT.trianglesDrawn / 1e6} million the editor draws unasked`;
  }
  return null;
}

/** What the asset view shows for the selection. */
export type AssetFocus =
  /** Nothing selected, or the pseudo-root. */
  | { readonly kind: "none" }
  /**
   * Geometry: `root`'s subtree drawn alone in its own space. `subset` is
   * the selected face subset of the mesh at `root`, highlighted.
   */
  | { readonly kind: "geometry"; readonly root: string; readonly subset: string | null }
  /** An image: the asset the attribute at `attribute` names. */
  | { readonly kind: "texture"; readonly attribute: string }
  /** A material (or a shader inside one), on the shader ball. */
  | { readonly kind: "material"; readonly material: string };

/** File extensions the texture view shows as images. */
const IMAGE = /\.(png|jpe?g|webp|gif|bmp|avif|exr|hdr|tga|tiff?)$/i;

/**
 * What the asset view makes of `selection`, and of `property` (the
 * attribute the explain panel shows) when it is an asset-valued attribute
 * of the selection naming an image.
 */
export function assetFocus(stage: Stage, selection: string | null, property: string | null, time: number | undefined): AssetFocus {
  if (selection === null || selection === "/" || stage.prim(selection) === null) return { kind: "none" };
  if (property !== null && property.startsWith(`${selection}.`) && imageAsset(stage, property, time) !== null) return { kind: "texture", attribute: property };
  const shader = stage.shader(selection);
  if (shader?.id === "UsdUVTexture" || shader?.id === "ND_image_color3" || shader?.id === "ND_image_color4") {
    return { kind: "texture", attribute: `${selection}.inputs:file` };
  }
  const material = enclosingMaterial(stage, selection);
  if (material !== null) return { kind: "material", material };
  const prim = stage.prim(selection)!;
  if (prim.typeName === "GeomSubset") {
    const mesh = parentPath(selection);
    if (mesh !== null) return { kind: "geometry", root: mesh, subset: selection };
  }
  return { kind: "geometry", root: selection, subset: null };
}

/** The asset path the attribute at `path` holds at `time`, when it is an asset naming an image; `null` otherwise. */
export function imageAsset(stage: Stage, path: string, time: number | undefined): string | null {
  const value = stage.attribute(path)?.get(time) ?? null;
  if (value?.kind !== "scalar" || value.type !== "asset" || typeof value.value !== "string") return null;
  return IMAGE.test(value.value.replace(/<UDIM>/g, "1001")) ? value.value : null;
}

/** The draw list scope for `choice`, given the asset view's focus. */
export function scopeFor(choice: DrawScopeChoice, focus: AssetFocus): DrawScope {
  if (choice === "scene") return { roots: ["/"] };
  if (choice === "nothing" || focus.kind !== "geometry") return { roots: [] };
  return { roots: [focus.root], space: focus.root };
}
