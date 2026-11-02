// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Prebuilt cluster sets: the key a mesh's set is found by agrees bit for
// bit with the one `cluster_lod --cache` names its file by, and a file is
// read as `layerstack_clusters` writes it, refused when damaged.
import assert from "node:assert/strict";
import { test } from "node:test";
import { encodeVertices } from "@forest-rs/layerstack_stagehand";
import { CLUSTER_FILE_VERSION, NO_GROUP, clusterKey, readClusterFile, selectCut } from "../src/viewport/clusterFile.ts";

test("a known triangle has the key cache.rs gives it", () => {
  const vertices = encodeVertices(
    {
      positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 2, 0.5]),
      normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1]),
      st: null,
      colors: null,
      uvSets: [],
      indices: new Uint32Array([0, 1, 2]),
    },
    "compact",
  );
  // `cache.rs`'s `a_known_mesh_has_a_known_key` expects the same.
  assert.equal(clusterKey(vertices), "71129f20029f413d");
  assert.equal(clusterKey({ ...vertices, indices: new Uint16Array(0) }), null, "released arrays have no key");
});

/** A file of one mesh: two original clusters (group 0) simplified into one (group 1, a root). */
function smallFile(): ArrayBuffer {
  const vertices = 4;
  const indices = [0, 1, 2, 0, 2, 3, 0, 1, 3];
  const words: number[] = [];
  const f32 = (value: number) => new Uint32Array(new Float32Array([value]).buffer)[0]!;
  words.push(CLUSTER_FILE_VERSION, 0, vertices, indices.length, 3, 2, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0);
  for (let i = 0; i < 3 * vertices; i++) words.push(f32(i));
  words.push(...indices);
  // Clusters: index_start, triangle_count, vertex_count, group, refined, subset, bounds.
  words.push(0, 3, 6, 1, 1, 1, 3, 3, 3, 0, 0, 1, NO_GROUP, NO_GROUP, 0, 0, 0, 0);
  for (const sphere of [[0, 0, 0, 1], [1, 0, 0, 1], [0.5, 0, 0, 2]]) words.push(...sphere.map(f32));
  // Groups: depth, bounds, error, cluster_start, cluster_count.
  words.push(0, 1, ...[0.5, 0, 0, 2].map(f32), ...[0.5, 0, 0, 2].map(f32), f32(0.01), f32(Infinity), 0, 2, 2, 1);
  // Nodes (two leaves), the report and its two levels.
  for (let i = 0; i < 16; i++) words.push(0);
  words.push(4, 3, 0, 0, 0, 0);
  words.push(1, 2, 2, 0, 0, 0, 1, 1, 1, 0, 0, 0);
  const bytes = new Uint8Array(8 + 4 * words.length);
  bytes.set([..."LSCLUSTR"].map((c) => c.charCodeAt(0)));
  new Uint32Array(bytes.buffer, 8).set(words);
  return bytes.buffer;
}

test("a cluster file is read column by column, and cut as the library cuts it", () => {
  const set = readClusterFile(smallFile());
  assert.equal(set.clusters.count, 3);
  assert.deepEqual([...set.clusters.triangleCount], [1, 1, 1]);
  assert.deepEqual([...set.indices.subarray(3, 6)], [0, 2, 3]);
  assert.equal(set.groups.error[1], Infinity);
  assert.deepEqual(set.levelTriangles, [2, 1]);
  const view = (distance: number) => ({ position: [0.5, 0, distance] as [number, number, number], projectionY: 1, viewportHeight: 1000, zNear: 0.1 });
  // Near, group 0's error shows: its two clusters; far, the root's one.
  assert.deepEqual(selectCut(set, view(3), 1), [0, 1]);
  assert.deepEqual(selectCut(set, view(1000), 1), [2]);
});

test("a damaged cluster file is refused", () => {
  const file = smallFile();
  assert.throws(() => readClusterFile(file.slice(0, file.byteLength - 4)), /expected/);
  const versioned = new Uint8Array(file.slice(0));
  versioned[8] = CLUSTER_FILE_VERSION + 1;
  assert.throws(() => readClusterFile(versioned.buffer), /version/);
  const outOfRange = new Uint8Array(file.slice(0));
  // The first index, after the header and twelve position floats.
  new Uint32Array(outOfRange.buffer, 8 + 64 + 48, 1)[0] = 99;
  assert.throws(() => readClusterFile(outOfRange.buffer), /out of range/);
});

test("a manifest finds a mesh by its prim's longest listed tail", async () => {
  const { ClusterManifest } = await import("../src/viewport/clusterFile.ts");
  const mesh = (prim: string, key: string) => ({ prim, key, vertices: 3, indices: 3, clusters: 1, groups: 1, triangles: 1 });
  const manifest = new ClusterManifest([{ meshes: [mesh("/rock/geo", "a"), mesh("/geo", "b")] }, { meshes: [mesh("/world/rock/geo", "c")] }]);
  assert.equal(manifest.find("/world/rock/geo")?.key, "c", "the whole path first");
  assert.equal(manifest.find("/scene/rock/geo")?.key, "a", "then its longest tail");
  assert.equal(manifest.find("/scene/stone/geo")?.key, "b");
  assert.equal(manifest.find("/scene/rock/geometry"), null, "whole components only");
  assert.equal(new ClusterManifest([]).find("/rock/geo"), null);
});
