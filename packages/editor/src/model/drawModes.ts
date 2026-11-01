// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Models drawn by their draw mode (`UsdGeomModelAPI`'s `model:drawMode`):
// what the viewport draws them as, the bounds they add to what is framed,
// and the draw mode a prim has and resolves to, for the inspector to show
// and author. Stagehand decides which prims are drawn so, as OpenUSD 26.11's
// Hydra does; nothing here draws.
import type { Prim, Stage } from "@forest-rs/layerstack";
import type { StandinBuffers } from "@forest-rs/layerstack_stagehand";
import type { ViewItem } from "./draws.ts";

/** The draw modes a model may author, in the schema's order. */
export const DRAW_MODES = ["inherited", "default", "origin", "bounds", "cards"] as const;

/** A `model:drawMode` token. */
export type DrawModeToken = (typeof DRAW_MODES)[number];

/** What the viewport draws a stand-in as: line segments, or cards. */
export function standinKind(standin: StandinBuffers): "standinLines" | "cards" {
  return standin.mode === "cards" ? "cards" : "standinLines";
}

/**
 * `bounds` (`[minX, minY, minZ, maxX, maxY, maxZ]`) grown by each shown
 * stand-in's extent at its placements: a stage of models with
 * `extentsHint` and no geometry frames what its stand-ins draw.
 */
export function withStandins(bounds: ArrayLike<number>, items: readonly ViewItem[]): number[] {
  const out = Array.from(bounds);
  for (const { item, buffers } of items) {
    const extent = buffers.standin?.input.extent;
    if (extent == null) continue;
    for (let at = 0; at + 16 <= item.matrices.length; at += 16) {
      const m = item.matrices.subarray(at, at + 16);
      for (let corner = 0; corner < 8; corner++) {
        const p = [extent[corner & 4 ? 3 : 0]!, extent[corner & 2 ? 4 : 1]!, extent[corner & 1 ? 5 : 2]!];
        for (let axis = 0; axis < 3; axis++) {
          // Row vectors: the point times the matrix.
          const world = p[0]! * m[axis]! + p[1]! * m[4 + axis]! + p[2]! * m[8 + axis]! + m[12 + axis]!;
          out[axis] = Math.min(out[axis]!, world);
          out[3 + axis] = Math.max(out[3 + axis]!, world);
        }
      }
    }
  }
  return out;
}

/** A prim's draw mode as the inspector shows it. */
export interface DrawModeState {
  /** Whether it has `GeomModelAPI` applied: only then does its draw mode count. */
  readonly applied: boolean;
  /** Its own `model:drawMode` (the schema's fallback, `inherited`, when unauthored). */
  readonly own: string;
  /** The mode it resolves to, inheriting from the nearest prim above with the API that authors one; `default` when none does. */
  readonly resolved: string;
  /** The prim it resolves from; `null` for its own or none. */
  readonly from: string | null;
  /** Whether it applies its draw mode: `model:applyDrawMode`, else a component's implicit `true`. */
  readonly applies: boolean;
}

function token(prim: Prim, name: string): string | null {
  const value = prim.attribute(name)?.get() ?? null;
  return value?.kind === "scalar" && typeof value.value === "string" ? value.value : null;
}

/** The draw mode of `prim`, as stagehand resolves it. */
export function drawModeState(stage: Stage, prim: Prim): DrawModeState {
  const applied = prim.hasAPI("GeomModelAPI");
  const own = applied ? (token(prim, "model:drawMode") ?? "inherited") : "inherited";
  let resolved = "default";
  let from: string | null = null;
  for (let at: Prim | null = prim; at !== null; ) {
    const mode = at.hasAPI("GeomModelAPI") ? token(at, "model:drawMode") : null;
    if (mode !== null && mode !== "" && mode !== "inherited") {
      resolved = mode;
      from = at.path === prim.path ? null : at.path;
      break;
    }
    const parent: string | null = at.path === "/" ? null : at.path.slice(0, Math.max(1, at.path.lastIndexOf("/")));
    at = parent === null || parent === at.path ? null : stage.prim(parent);
  }
  const apply = prim.attribute("model:applyDrawMode");
  const authored = apply?.authored ? apply.get() : null;
  const kind = prim.metadata("kind");
  const applies =
    authored?.kind === "scalar" && typeof authored.value === "boolean" ? authored.value : kind?.kind === "scalar" && kind.value === "component";
  return { applied, own, resolved, from, applies };
}
