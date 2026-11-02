// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A skeleton's joints as the skeleton view lists them. UsdSkel names each
// joint by its path from the root joint (`Hips/Torso/Chest`), and orders
// parents before children, so the tree is each joint's depth and name.

/** Joint paths as a tree: each with its depth and its own name, in the order given (parents precede children in UsdSkel). */
export function jointTree(joints: readonly string[]): { readonly path: string; readonly name: string; readonly depth: number }[] {
  return joints.map((path) => {
    const parts = path.split("/").filter((part) => part !== "");
    return { path, name: parts.at(-1) ?? path, depth: Math.max(0, parts.length - 1) };
  });
}
