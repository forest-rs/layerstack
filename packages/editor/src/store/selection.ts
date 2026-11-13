// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What is selected: a prim, an instance of it when it is a point
// instancer, and the property the explain panel shows. And what the
// viewport needs of a selected instance: where its box is, in the space
// the draws are in. State and reads only: the store decides what a new
// selection does to the draws.
import type { Stage } from "@forest-rs/layerstack";
import type { AssetFocus, DrawScopeChoice } from "../model/assetView.ts";
import { boxCorners, instanceMatrix, type InstancerColumns, invert, multiply, readColumns } from "../model/instancer.ts";
import { primRevision } from "../model/revisions.ts";
import type { Session } from "../model/session.ts";

/** A point instancer's instance: its instancer, and its index in the instancer's arrays. */
export interface InstanceSelection {
  readonly instancer: string;
  readonly index: number;
}

/** The selection. */
export class Selection {
  /** The selected prim's path. */
  prim: string | null = null;
  /**
   * The point instancer instance selected (in its inspector view, or
   * picked in the viewport): its instancer, which is the selected prim;
   * `null` for none.
   */
  instance: InstanceSelection | null = null;
  /** The property whose value the explain panel shows, by path. */
  property: string | null = null;
  /** The point instancer columns last read, and the session, prim revision and time they were read at. */
  #columns: { session: Session; path: string; revision: number; time: number | undefined; columns: InstancerColumns | null } | null = null;

  /** Forgets everything selected: another scene. */
  clear(): void {
    this.prim = null;
    this.instance = null;
    this.property = null;
  }

  /**
   * Selects the prim at `path` (`null` for none): an instance of another
   * instancer is let go, and the explained property is kept while it is
   * the prim's. Returns whether the selected prim changed.
   */
  select(path: string | null): boolean {
    if (path === this.prim) return false;
    this.prim = path;
    if (this.instance !== null && this.instance.instancer !== path) this.instance = null;
    if (this.property !== null && (path === null || !this.property.startsWith(`${path}.`))) this.property = null;
    return true;
  }

  /**
   * The columns of the point instancer at `path` in `session` at `time`,
   * read again only when a change reaches it ({@link primRevision}) or
   * the time moves ({@link readColumns}); `null` when it is not one.
   */
  columns(session: Session, path: string, time: number | undefined): InstancerColumns | null {
    const at = this.#columns;
    const revision = primRevision(session, path);
    if (at !== null && at.session === session && at.path === path && at.revision === revision && at.time === time) return at.columns;
    const columns = session.stage.prim(path)?.isA("PointInstancer") === true ? readColumns(session.stage, path, time) : null;
    this.#columns = { session, path, revision, time, columns };
    return columns;
  }
}

/** The space the viewport draws in: what {@link instanceCorners} places an instance's box in. */
export interface DrawnSpace {
  /** What is drawn: the scene, the selection alone, or nothing. */
  readonly drawScope: DrawScopeChoice;
  /** What the asset view makes of the selection: drawn alone, its root's space is the draws'. */
  readonly focus: AssetFocus;
}

/**
 * The corners of instance `index`'s box (24 numbers), in the space the
 * viewport draws in (the world, or drawing the selection alone, its
 * root's own space): its prototype's local bound through its transform
 * and the instancer's. `null` when nothing is drawn or it has no finite
 * box.
 */
export function instanceCorners(
  stage: Stage,
  instancer: string,
  columns: InstancerColumns | null,
  index: number,
  at: number | undefined,
  space: DrawnSpace,
): Float64Array | null {
  if (space.drawScope === "nothing") return null;
  if (columns === null || index >= columns.count) return null;
  const prototype = columns.prototypes[columns.protoIndices[index]!];
  if (prototype === undefined) return null;
  const time = at === undefined ? {} : { time: at };
  const bounds = stage.localBounds([prototype], { purposes: ["default", "proxy", "render"], ...time });
  const range = bounds.ranges.subarray(0, 6);
  if (!(range[0]! <= range[3]!)) return null;
  let world: ArrayLike<number> = stage.worldTransforms([instancer], time);
  if (space.drawScope === "selection") {
    // Drawn alone, the draws are in the root's space (`DrawScope.space`).
    if (space.focus.kind !== "geometry") return null;
    const root = invert(stage.worldTransforms([space.focus.root], time));
    if (root === null) return null;
    world = multiply(world, root);
  }
  const corners = boxCorners(range, multiply(multiply(bounds.matrices.subarray(0, 16), instanceMatrix(columns, index)), world));
  // An instance whose arrays are shorter than `protoIndices` has no transform to outline or frame.
  return corners.every(Number.isFinite) ? corners : null;
}

/** The axis-aligned box around eight corners (24 numbers, {@link boxCorners}'s). */
export function cornersBox(corners: ArrayLike<number>): number[] {
  const box = [Infinity, Infinity, Infinity, -Infinity, -Infinity, -Infinity];
  for (let corner = 0; corner < 8; corner++) {
    for (let c = 0; c < 3; c++) {
      box[c] = Math.min(box[c]!, corners[corner * 3 + c]!);
      box[c + 3] = Math.max(box[c + 3]!, corners[corner * 3 + c]!);
    }
  }
  return box;
}
