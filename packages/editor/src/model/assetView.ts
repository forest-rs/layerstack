// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the viewport draws, and what the asset view makes of the
// selection. The viewport draws the whole scene, the selection alone (the
// asset view), or nothing, always under the draw budget: what does not
// fit is drawn as boxes or skipped, and said. The asset view draws the
// selected prim's subtree alone, framed at the origin (a draw list scope
// in the prim's own space): a gprim, a model or group, a point instancer's
// instances, a prototype of one; a face subset as its mesh, the subset
// highlighted and the rest ghosted. A texture shows as its image and a
// material on the shader ball, drawing nothing.
import type { Stage } from "@forest-rs/layerstack";
import type { BudgetReport, DrawBudget, DrawScope } from "@forest-rs/layerstack_stagehand";
import { enclosingMaterial, parentPath } from "./network.ts";

/** What the viewport draws: the whole scene, the selection alone, or nothing. */
export type DrawScopeChoice = "scene" | "selection" | "nothing";

/**
 * The most the editor draws unless raised: 1 GiB uploaded and 50 million
 * triangles drawn, instanced draws counted in full. A gigabyte of buffers
 * is about what a WebGL context on a laptop holds beside the page, and
 * the editor keeps a JavaScript copy of as much again; 50 million
 * triangles a frame is about what such a GPU draws at an interactive
 * rate. Fixed, not sized to the device, so a scene degrades the same way
 * everywhere.
 */
export const DRAW_BUDGET: DrawBudget = Object.freeze({ gpuBytes: 1024 ** 3, triangles: 50_000_000 });

/** The draw budgets a viewer can choose: the default, raised two or four times, or none. */
export type BudgetChoice = "default" | "double" | "quadruple" | "unlimited";

/** Each choice, in order, with its label. */
export const BUDGET_CHOICES: readonly (readonly [BudgetChoice, string])[] = [
  ["default", "1 GiB"],
  ["double", "2 GiB"],
  ["quadruple", "4 GiB"],
  ["unlimited", "Unlimited"],
];

/** The budget a choice draws under: {@link DRAW_BUDGET}, its bytes and triangles raised together, or none. */
export function budgetFor(choice: BudgetChoice): DrawBudget | null {
  const scale = choice === "default" ? 1 : choice === "double" ? 2 : choice === "quadruple" ? 4 : null;
  if (scale === null) return null;
  return { gpuBytes: DRAW_BUDGET.gpuBytes! * scale, triangles: DRAW_BUDGET.triangles! * scale };
}

/** Where a viewer's budget choice is remembered. */
const BUDGET_KEY = "layerstack-editor:draw-budget";

/** The budget choice this viewer made last; the default when none is remembered or storage is unavailable. */
export function rememberedBudget(): BudgetChoice {
  try {
    const value = globalThis.localStorage?.getItem(BUDGET_KEY) ?? null;
    return BUDGET_CHOICES.some(([choice]) => choice === value) ? (value as BudgetChoice) : "default";
  } catch {
    return "default";
  }
}

/** Remembers `choice` for this viewer, when storage is available. */
export function rememberBudget(choice: BudgetChoice): void {
  try {
    globalThis.localStorage?.setItem(BUDGET_KEY, choice);
  } catch {
    // Storage blocked: the choice lasts this page.
  }
}

/** What fitting the budget did, in a sentence; `null` when nothing was simplified. */
export function budgetNote(report: BudgetReport | null): string | null {
  if (report === null || report.degraded.length === 0) return null;
  const boxes = report.degraded.filter((grant) => grant.rung === "standin").length;
  const skipped = report.degraded.length - boxes;
  const parts = [boxes > 0 && `${boxes.toLocaleString()} as boxes`, skipped > 0 && `${skipped.toLocaleString()} skipped`].filter(Boolean).join(", ");
  const count = report.degraded.length;
  return `${count.toLocaleString()} ${count === 1 ? "item" : "items"} simplified to fit the budget (${parts}).`;
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
