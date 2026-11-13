// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Keeping a network's drawing still across an edit. Laying a network out
// again after a connection is made can move every node (a new edge
// changes the columns and the sweeps), so while one network is being
// edited, a node keeps the place it was drawn at: what is new goes where
// it was asked for, or where the fresh layout puts it, moved as the kept
// nodes were; and a node that grew (a row shown) pushes down what it now
// overlaps in its column, and nothing else moves.
import { GRAPH, type NetworkLayout, placeEdges, type PlacedNode } from "./network.ts";

/** A place in the drawing. */
export interface Place {
  readonly x: number;
  readonly y: number;
}

/** Whether `a` and `b` overlap, with the gap between nodes kept. */
function overlaps(a: PlacedNode, b: PlacedNode): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height + GRAPH.nodeGap && b.y < a.y + a.height + GRAPH.nodeGap;
}

/**
 * `next` (a fresh layout of the network after an edit) with the nodes
 * `previous` drew kept where they were, as the header of this file
 * describes; `wanted` places a new node's top left by its key. Without a
 * previous drawing, `next` as it is (with `wanted` applied).
 */
export function keepPlaces(previous: NetworkLayout | null, next: NetworkLayout, wanted: ReadonlyMap<string, Place> = new Map()): NetworkLayout {
  const before = new Map((previous?.nodes ?? []).map((placed) => [placed.node.key, placed]));
  // How far the kept nodes moved from where the fresh layout puts them: the new ones move as much.
  let dx = 0;
  let dy = 0;
  let kept = 0;
  for (const placed of next.nodes) {
    const old = before.get(placed.node.key);
    if (old === undefined) continue;
    dx += old.x - placed.x;
    dy += old.y - placed.y;
    kept++;
  }
  if (kept > 0) {
    dx /= kept;
    dy /= kept;
  }
  const settled: PlacedNode[] = [];
  const fresh: PlacedNode[] = [];
  for (const placed of next.nodes) {
    const old = before.get(placed.node.key);
    const want = wanted.get(placed.node.key);
    if (want !== undefined) fresh.push({ ...placed, x: Math.max(0, want.x), y: Math.max(0, want.y) });
    else if (old !== undefined) settled.push({ ...placed, x: old.x, y: old.y, column: old.column });
    else fresh.push({ ...placed, x: Math.max(0, placed.x + dx), y: Math.max(0, placed.y + dy) });
  }
  // A kept node that grew pushes down those below it that it now overlaps, top to bottom.
  settled.sort((a, b) => a.y - b.y);
  for (let i = 0; i < settled.length; i++) {
    for (let moved = true; moved; ) {
      moved = false;
      for (let j = 0; j < i; j++) {
        const above = settled[j]!;
        if (!overlaps(above, settled[i]!)) continue;
        settled[i] = { ...settled[i]!, y: above.y + above.height + GRAPH.nodeGap };
        moved = true;
      }
    }
  }
  // A new node goes down past what it overlaps.
  const placed = [...settled];
  for (let node of fresh) {
    for (let moved = true; moved; ) {
      moved = false;
      for (const other of placed) {
        if (!overlaps(other, node)) continue;
        node = { ...node, y: other.y + other.height + GRAPH.nodeGap };
        moved = true;
      }
    }
    placed.push(node);
  }
  // The nodes in the order the fresh layout has them.
  const order = new Map(next.nodes.map((each, index) => [each.node.key, index]));
  placed.sort((a, b) => order.get(a.node.key)! - order.get(b.node.key)!);
  return {
    nodes: placed,
    edges: placeEdges({ edges: next.edges.map((edge) => edge.edge) }, placed),
    width: Math.max(0, ...placed.map((each) => each.x + each.width)),
    height: Math.max(0, ...placed.map((each) => each.y + each.height)),
  };
}
