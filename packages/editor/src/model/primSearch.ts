// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Finding prims by name over the composed hierarchy, for the prim tree's
// Find field and the prim picker: a depth-first walk that stops at a
// limit, so a stage of a million prims answers its first matches without
// reading the rest.
import type { Stage } from "@forest-rs/layerstack";

/** What {@link searchPrims} looks for. */
export interface PrimQuery {
  /** Text the prim's name contains, case ignored; every prim with `""`. */
  readonly text: string;
  /** The most matches returned. */
  readonly limit: number;
  /** A schema the prim must be (`Prim.isA`, `Material`) or have applied (`Prim.hasAPI`, `CollectionAPI`); any prim when absent. */
  readonly type?: string;
}

/** The prims matching `query`, in depth-first order: at most `query.limit` paths. */
export function searchPrims(stage: Stage, query: PrimQuery): string[] {
  const needle = query.text.trim().toLowerCase();
  const { limit, type } = query;
  const found: string[] = [];
  const matches = (path: string): boolean => {
    if (needle !== "" && !path.slice(path.lastIndexOf("/") + 1).toLowerCase().includes(needle)) return false;
    if (type === undefined) return true;
    const prim = stage.prim(path);
    return prim !== null && (prim.isA(type) || prim.hasAPI(type));
  };
  const walk = (path: string) => {
    for (const child of stage.children(path)) {
      if (found.length >= limit) return;
      if (matches(child)) found.push(child);
      walk(child);
    }
  };
  walk("/");
  return found;
}
