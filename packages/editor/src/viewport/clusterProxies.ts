// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Instance proxies: a few triangles that stand in for a whole clustered
// mesh where an instance of it is only a few pixels across. Apart from
// three.js so they can be tested in Node.
//
// A set's DAG ends at one cluster per subset (about a hundred triangles
// for the small meshes a production element instances by the hundred
// thousand), and an instance two pixels across still draws it whole: a
// close view of such an element drew 52 M triangles, nearly all of them
// for instances one to four pixels across. A proxy is drawn instead below
// a projected size (`ClusterView.proxyPixels`).
//
// A proxy is made by vertex clustering (Rossignac and Borrel's, as
// meshoptimizer's sloppy simplification is): the subset's original
// triangles' vertices are binned in a grid over their bounds, each cell
// keeps the vertex nearest the mean of its vertices, and the triangles
// whose corners land in three different cells remain, once each. The
// finest grid leaving at most {@link PROXY_TRIANGLES} is taken when it
// leaves at least half as many; otherwise the next finer grid's largest
// triangles, so a proxy keeps what covers the most pixels. Its corners are the mesh's own vertices,
// so it is drawn as a cluster is, with the mesh's normals, colors and
// texture coordinates.

import { NO_GROUP } from "./clusterFile.ts";
import type { ClusterSetData } from "./clusterFile.ts";

/** The most triangles in an instance proxy. */
export const PROXY_TRIANGLES = 16;

/** What {@link buildProxy} reads of a set: its original clusters' triangles and subsets. */
export type ProxySource = Pick<ClusterSetData, "indices"> & {
  readonly clusters: Pick<ClusterSetData["clusters"], "count" | "indexStart" | "triangleCount" | "refined" | "subset">;
};

/** The grids tried, cells along each axis, coarsest first. */
const GRIDS = [2, 3, 4, 5, 6, 8, 10, 12, 16, 24];

/**
 * The instance proxy of `subset` of `set`, three vertex indices per
 * triangle (at most `maxTriangles`), from the subset's original
 * triangles; `positions` holds three numbers per vertex of the set. Empty
 * when the subset has no triangles.
 */
export function buildProxy(set: ProxySource, positions: ArrayLike<number>, subset: number, maxTriangles = PROXY_TRIANGLES): Uint32Array {
  const corners: number[] = [];
  const { clusters } = set;
  for (let c = 0; c < clusters.count; c++) {
    if (clusters.refined[c] !== NO_GROUP || clusters.subset[c] !== subset) continue;
    const start = clusters.indexStart[c]!;
    for (let i = 0; i < 3 * clusters.triangleCount[c]!; i++) corners.push(set.indices[start + i]!);
  }
  if (corners.length === 0) return new Uint32Array(0);
  // The subset's vertices, each once, by a local index.
  const localOf = new Map<number, number>();
  const vertices: number[] = [];
  const local = new Uint32Array(corners.length);
  corners.forEach((v, i) => {
    let at = localOf.get(v);
    if (at === undefined) {
      at = vertices.length;
      localOf.set(v, at);
      vertices.push(v);
    }
    local[i] = at;
  });
  const low = [Infinity, Infinity, Infinity];
  const high = [-Infinity, -Infinity, -Infinity];
  for (const v of vertices) {
    for (let a = 0; a < 3; a++) {
      low[a] = Math.min(low[a]!, positions[3 * v + a]!);
      high[a] = Math.max(high[a]!, positions[3 * v + a]!);
    }
  }
  // The coarsest grid leaving more than `maxTriangles`: the grid before it
  // when that left at least half as many, else its largest triangles.
  let coarser: number[] | null = null;
  for (const grid of GRIDS) {
    const triangles = collapse(local, vertices, positions, low, high, grid);
    if (triangles.length <= 3 * maxTriangles) {
      coarser = triangles;
      continue;
    }
    if (coarser !== null && coarser.length >= (3 * maxTriangles) / 2) return Uint32Array.from(coarser);
    return largest(triangles, positions, maxTriangles);
  }
  return Uint32Array.from(coarser!);
}

/**
 * The triangles of `local` (indices into `vertices`, the set's) with each
 * vertex moved to its cell's representative in a `grid`-cell grid over the
 * bounds, as the set's vertex indices; degenerate and repeated ones dropped.
 */
function collapse(local: Uint32Array, vertices: readonly number[], positions: ArrayLike<number>, low: readonly number[], high: readonly number[], grid: number): number[] {
  const cells = grid * grid * grid;
  const cellOf = new Uint32Array(vertices.length);
  const sums = new Float64Array(4 * cells);
  vertices.forEach((v, i) => {
    let cell = 0;
    for (let a = 0; a < 3; a++) {
      const extent = high[a]! - low[a]!;
      const at = extent > 0 ? Math.min(grid - 1, Math.floor(((positions[3 * v + a]! - low[a]!) / extent) * grid)) : 0;
      cell = cell * grid + at;
    }
    cellOf[i] = cell;
    for (let a = 0; a < 3; a++) sums[4 * cell + a]! += positions[3 * v + a]!;
    sums[4 * cell + 3]! += 1;
  });
  // Each cell's representative: its vertex nearest the mean of its vertices.
  const representative = new Int32Array(cells).fill(-1);
  const nearest = new Float64Array(cells).fill(Infinity);
  vertices.forEach((v, i) => {
    const cell = cellOf[i]!;
    const count = sums[4 * cell + 3]!;
    let distance = 0;
    for (let a = 0; a < 3; a++) distance += (positions[3 * v + a]! - sums[4 * cell + a]! / count) ** 2;
    if (distance < nearest[cell]!) {
      nearest[cell] = distance;
      representative[cell] = v;
    }
  });
  const out: number[] = [];
  const seen = new Set<number>();
  for (let i = 0; i < local.length; i += 3) {
    const a = cellOf[local[i]!]!;
    const b = cellOf[local[i + 1]!]!;
    const c = cellOf[local[i + 2]!]!;
    if (a === b || b === c || a === c) continue;
    const lowest = Math.min(a, b, c);
    const highest = Math.max(a, b, c);
    const key = (lowest * cells + (a + b + c - lowest - highest)) * cells + highest;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(representative[a]!, representative[b]!, representative[c]!);
  }
  return out;
}

/** The `count` triangles of `triangles` of the largest area. */
function largest(triangles: readonly number[], positions: ArrayLike<number>, count: number): Uint32Array {
  const areas: { at: number; area: number }[] = [];
  for (let i = 0; i < triangles.length; i += 3) {
    const p = (k: number, a: number) => positions[3 * triangles[i + k]! + a]!;
    const u = [p(1, 0) - p(0, 0), p(1, 1) - p(0, 1), p(1, 2) - p(0, 2)];
    const w = [p(2, 0) - p(0, 0), p(2, 1) - p(0, 1), p(2, 2) - p(0, 2)];
    const cross = [u[1]! * w[2]! - u[2]! * w[1]!, u[2]! * w[0]! - u[0]! * w[2]!, u[0]! * w[1]! - u[1]! * w[0]!];
    areas.push({ at: i, area: Math.hypot(cross[0]!, cross[1]!, cross[2]!) });
  }
  areas.sort((x, y) => y.area - x.area || x.at - y.at);
  const out = new Uint32Array(3 * Math.min(count, areas.length));
  areas.slice(0, count).forEach(({ at }, k) => out.set([triangles[at]!, triangles[at + 1]!, triangles[at + 2]!], 3 * k));
  return out;
}
