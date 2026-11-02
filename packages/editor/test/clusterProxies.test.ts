// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Instance proxies: a few of a mesh's own triangles per subset, standing
// in for it where an instance is a few pixels across.
import assert from "node:assert/strict";
import { test } from "node:test";
import { NO_GROUP } from "../src/viewport/clusterFile.ts";
import { PROXY_TRIANGLES, buildProxy } from "../src/viewport/clusterProxies.ts";
import type { ProxySource } from "../src/viewport/clusterProxies.ts";

/** A wavy `n` x `n` grid of quads in one original cluster of subset 0, then a second subset of one triangle, and a simplified cluster that is not original. */
function grid(n: number): { set: ProxySource; positions: Float32Array } {
  const positions: number[] = [];
  for (let y = 0; y <= n; y++) for (let x = 0; x <= n; x++) positions.push(x / n, y / n, 0.1 * Math.sin(x) * Math.cos(y));
  const indices: number[] = [];
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      const i = y * (n + 1) + x;
      indices.push(i, i + 1, i + n + 2, i, i + n + 2, i + n + 1);
    }
  }
  const quads = indices.length / 3;
  indices.push(0, 1, n + 1, 0, n, (n + 1) * (n + 1) - 1);
  return {
    set: {
      indices: Uint32Array.from(indices),
      clusters: {
        count: 3,
        indexStart: Uint32Array.from([0, 3 * quads, 3 * quads + 3]),
        triangleCount: Uint32Array.from([quads, 1, 1]),
        refined: Uint32Array.from([NO_GROUP, NO_GROUP, 0]),
        subset: Uint32Array.from([0, 1, 0]),
      },
    },
    positions: Float32Array.from(positions),
  };
}

test("a proxy is a few of the subset's own vertices' triangles", () => {
  const { set, positions } = grid(24);
  const proxy = buildProxy(set, positions, 0);
  assert.equal(proxy.length % 3, 0);
  assert.ok(proxy.length / 3 <= PROXY_TRIANGLES, `${proxy.length / 3} triangles`);
  assert.ok(proxy.length / 3 >= PROXY_TRIANGLES / 2, `${proxy.length / 3} triangles`);
  for (let i = 0; i < proxy.length; i += 3) {
    assert.ok(proxy[i]! < positions.length / 3);
    assert.ok(proxy[i] !== proxy[i + 1] && proxy[i + 1] !== proxy[i + 2] && proxy[i] !== proxy[i + 2], "no degenerate triangle");
  }
  // Spread over the whole grid, as the grid's cells are.
  const xs = [...proxy].map((v) => positions[3 * v]!);
  assert.ok(Math.min(...xs) < 0.25 && Math.max(...xs) > 0.75);
});

test("a subset's proxy is of its triangles only; one of no triangles is empty", () => {
  const { set, positions } = grid(4);
  assert.deepEqual([...buildProxy(set, positions, 1)], [0, 1, 5]);
  assert.equal(buildProxy(set, positions, 2).length, 0);
});

test("a mesh already within the budget keeps its triangles", () => {
  const { set, positions } = grid(2);
  // Eight triangles: a grid fine enough to keep them all.
  assert.equal(buildProxy(set, positions, 0).length / 3, 8);
});
