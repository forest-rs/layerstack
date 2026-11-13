// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Whether what a view of a prim reads might vary with time, so the view
// asks to render again as the timeline plays only when it does (`useTime`'s
// `varying`): a light's inputs, a mesh's attributes and subsets and the
// transforms above it, a material's network. Answered by the stage for
// many attributes in one read (`Stage.valuesMightBeTimeVarying`, OpenUSD's
// `UsdAttribute::ValueMightBeTimeVarying`: one sample does not vary), and
// by each transform's ops (`UsdGeomXformable::TransformMightBeTimeVarying`).
import type { Stage } from "@forest-rs/layerstack";

/** What a view of a prim reads at the time. */
export interface TimedReads {
  /** The prim's attributes it reads, by name; `null` for every attribute the prim has. */
  readonly attributes: readonly string[] | null;
  /** Whether it reads every attribute of the prims beneath it too (a material's shaders, a mesh's subsets). */
  readonly descendants?: boolean;
  /** Whether it reads the prim's world transform: its own ops and its ancestors'. */
  readonly worldTransform?: boolean;
}

/** The most prims beneath one whose attributes are asked about: past it, the view is taken to vary. */
const MAX_DESCENDANTS = 2000;

/** The paths of every attribute of the prim at `path`. */
function everyAttribute(stage: Stage, path: string): string[] {
  return stage.prim(path)?.attributeNames().map((name) => `${path}.${name}`) ?? [];
}

/** Whether a value `reads` names, of the prim at `path`, might vary with time; `false` when there is no such prim. */
export function readsMightVary(stage: Stage, path: string, reads: TimedReads): boolean {
  if (stage.prim(path) === null) return false;
  const paths = reads.attributes === null ? everyAttribute(stage, path) : reads.attributes.map((name) => `${path}.${name}`);
  if (reads.descendants === true) {
    const below = [...stage.children(path)];
    for (let at = 0; at < below.length; at++) {
      if (at >= MAX_DESCENDANTS) return true;
      const child = below[at]!;
      paths.push(...everyAttribute(stage, child));
      below.push(...stage.children(child));
    }
  }
  if (paths.length > 0 && stage.valuesMightBeTimeVarying(paths).some(Boolean)) return true;
  if (reads.worldTransform !== true) return false;
  for (let prim = path; prim !== "/" && prim !== ""; prim = prim.slice(0, prim.lastIndexOf("/")) || "/") {
    if (stage.xformable(prim)?.transformMightBeTimeVarying === true) return true;
  }
  return false;
}
