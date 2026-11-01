// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A prim's collections as the inspector shows them: each one's membership,
// asked of every prim of the stage's tree.
import type { Stage } from "@forest-rs/layerstack";

/** One collection of a prim, and what it includes. */
export interface CollectionRow {
  /** The collection's name: `lightLink` for `collection:lightLink`. */
  readonly name: string;
  /**
   * Whether it includes every prim, as a light's link collections do by
   * their `includeRoot = true` fallback.
   */
  readonly everything: boolean;
  /** The prims of the tree it includes, in tree order. */
  readonly included: readonly string[];
  /** How many prims were asked: the whole tree. */
  readonly of: number;
}

/** Every prim below `path`, depth first, instance proxies included: the prim tree's rows. */
export function treePaths(stage: Stage, path = "/"): string[] {
  return stage.children(path).flatMap((child) => [child, ...treePaths(stage, child)]);
}

/**
 * The collections of the prim at `path`, by the `collection:<name>:includes`
 * relationships it has (a light's `lightLink` and `shadowLink`, which its
 * schema gives it), each asked of every prim of the tree with
 * `Stage.collectionMembership`.
 */
export function collectionRows(stage: Stage, path: string): CollectionRow[] {
  const prim = stage.prim(path);
  if (prim === null) return [];
  const names = prim.relationshipNames().flatMap((name) => /^collection:([^:]+):includes$/.exec(name)?.[1] ?? []);
  if (names.length === 0) return [];
  const paths = treePaths(stage);
  return names.map((name) => {
    const membership = stage.collectionMembership(path, name, paths);
    return {
      name,
      everything: membership.everything,
      included: paths.filter((_, index) => membership.included[index] === true),
      of: paths.length,
    };
  });
}
