// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Prebuilt cluster level-of-detail sets: the key the editor finds a mesh's
// set by, and the set's file, read. Apart from three.js so they can be
// tested in Node.
//
// The sets are built natively (`layerstack_clusters`, through the
// `cluster_lod` example's `--cache` mode) and served in development from
// the main checkout's `.local/cluster-cache` (`vite.config.ts`). Each file
// is named by a key of the mesh's content, computed here from what the
// editor holds of it bit for bit: its triangulated indices and the
// `compact` encoding of its positions and normals
// (`@forest-rs/layerstack_stagehand` encodes them in JavaScript as the
// buffers are copied out of WebAssembly, so the editor never sees the
// `f32` positions; `cache.rs` in `layerstack_clusters_examples` encodes
// them again the same way before hashing). So a set is found by content:
// wherever the mesh sits on the stage, and not at all once an edit
// changes it.
//
// The words hashed, in order: `CLUSTER_KEY_VERSION`, `CLUSTER_FILE_VERSION`,
// the vertex and triangle counts; every index; every position's `snorm16`
// x4 as two words (the fourth zero); the decode's scale and offset as
// `f64`, two words each, low first; every normal's `snorm8` x4 as one
// word. Two lanes of MurmurHash3's 32-bit block step (seeds 0 and
// 0x9e3779b9) mix them; the key is both lanes in hex, high lane first.

import type { MeshVertices } from "@forest-rs/layerstack_stagehand";

/** The version of the key and of the configuration sets are built with (`cache.rs`'s `KEY_VERSION`). */
export const CLUSTER_KEY_VERSION = 1;

/** The cluster file version read here (`layerstack_clusters`'s `FILE_VERSION`). */
export const CLUSTER_FILE_VERSION = 1;

/** The group id of "no group": a cluster of the original triangles' `refined`. */
export const NO_GROUP = 0xffffffff;

/** What {@link clusterKey} reads: a mesh's indices and its compact positions and normals. */
export type KeyedVertices = Pick<MeshVertices, "count" | "triangles" | "position" | "decode" | "normal" | "indices">;

/**
 * The cache key of a mesh in the `compact` vertex format: 16 hex digits;
 * `null` when its arrays were released, it is in another format, or it
 * has no triangles.
 */
export function clusterKey(vertices: KeyedVertices): string | null {
  const { position, normal, indices } = vertices;
  if (position.type !== "snorm16" || position.stride !== 4 || normal.type !== "snorm8" || normal.stride !== 4) return null;
  if (vertices.triangles === 0 || indices.length !== vertices.triangles * 3 || position.array.length !== vertices.count * 4) return null;
  const hash = new KeyHasher();
  hash.word(CLUSTER_KEY_VERSION);
  hash.word(CLUSTER_FILE_VERSION);
  hash.word(vertices.count);
  hash.word(vertices.triangles);
  for (let i = 0; i < indices.length; i++) hash.word(indices[i]!);
  const positions = position.array as Int16Array;
  // Two words a vertex: x | y << 16, then z (the fourth component is zero).
  const words = new Uint32Array(positions.buffer, positions.byteOffset, positions.length / 2);
  for (let i = 0; i < words.length; i += 2) {
    hash.word(words[i]!);
    hash.word(words[i + 1]! & 0xffff);
  }
  const bits = new Float64Array(4);
  bits.set([vertices.decode.scale, ...vertices.decode.offset]);
  for (const word of new Uint32Array(bits.buffer)) hash.word(word);
  const normals = normal.array as Int8Array;
  const normalWords = new Uint32Array(normals.buffer, normals.byteOffset, normals.length / 4);
  for (let i = 0; i < normalWords.length; i++) hash.word(normalWords[i]! & 0xffffff);
  return hash.finish();
}

/** Two lanes of MurmurHash3's 32-bit block step over words, as `cache.rs` mixes them. */
class KeyHasher {
  #low = 0;
  #high = 0x9e3779b9;
  #words = 0;

  word(word: number): void {
    let k = Math.imul(word, 0xcc9e2d51);
    k = (k << 15) | (k >>> 17);
    k = Math.imul(k, 0x1b873593);
    let low = this.#low ^ k;
    low = (low << 13) | (low >>> 19);
    this.#low = (Math.imul(low, 5) + 0xe6546b64) | 0;
    let high = this.#high ^ k ^ 0x85ebca6b;
    high = (high << 13) | (high >>> 19);
    this.#high = (Math.imul(high, 5) + 0xe6546b64) | 0;
    this.#words = (this.#words + 1) | 0;
  }

  finish(): string {
    const mix = (lane: number) => {
      let h = lane ^ Math.imul(this.#words, 4);
      h ^= h >>> 16;
      h = Math.imul(h, 0x85ebca6b);
      h ^= h >>> 13;
      h = Math.imul(h, 0xc2b2ae35);
      h ^= h >>> 16;
      return (h >>> 0).toString(16).padStart(8, "0");
    };
    return mix(this.#high) + mix(this.#low);
  }
}

/**
 * A decoded cluster file: its columns as typed arrays over the file's
 * bytes (no copies), as `layerstack_clusters`'s `file` module lays them
 * out. Positions, normals and texture coordinates are in the file too but
 * not read: the editor draws the vertices it already holds, which the key
 * says are the same.
 */
export interface ClusterSetData {
  readonly vertexCount: number;
  readonly levels: number;
  /** Every cluster's vertex indices, three per triangle. */
  readonly indices: Uint32Array;
  readonly clusters: {
    readonly count: number;
    readonly indexStart: Uint32Array;
    readonly triangleCount: Uint32Array;
    readonly group: Uint32Array;
    /** The group the cluster was simplified from, {@link NO_GROUP} for an original one. */
    readonly refined: Uint32Array;
    readonly subset: Uint32Array;
    /** Center and radius, four floats per cluster, in the mesh's space. */
    readonly bounds: Float32Array;
  };
  readonly groups: {
    readonly count: number;
    readonly depth: Uint32Array;
    /** Center and radius of the group's simplified replacement. */
    readonly bounds: Float32Array;
    /** The replacement's error in the mesh's units; infinite for a root or stuck group. */
    readonly error: Float32Array;
  };
  /** Triangles at each DAG level. */
  readonly levelTriangles: readonly number[];
  /** The file's size: what holding it costs. */
  readonly byteLength: number;
}

const MAGIC = "LSCLUSTR";
const HEADER_BYTES = 8 + 16 * 4;

/** Reads a cluster file, or throws saying why it is not one this editor reads. */
export function readClusterFile(buffer: ArrayBuffer): ClusterSetData {
  const bytes = new Uint8Array(buffer);
  if (bytes.length < HEADER_BYTES || String.fromCharCode(...bytes.subarray(0, 8)) !== MAGIC) throw new Error("not a cluster file");
  const header = new Uint32Array(buffer, 8, 16);
  const [version, flags, vertices, indexCount, clusterCount, groupCount, nodeCount, levels, reportLevels] = header as unknown as number[];
  if (version !== CLUSTER_FILE_VERSION) throw new Error(`cluster file version ${version}, expected ${CLUSTER_FILE_VERSION}`);
  const perVertex = 3 + (flags! & 1 ? 3 : 0) + (flags! & 2 ? 2 : 0);
  const expected = HEADER_BYTES + 4 * (vertices! * perVertex + indexCount! + 10 * clusterCount! + 8 * groupCount! + 8 * nodeCount! + 6 + 6 * reportLevels!);
  if (expected !== buffer.byteLength) throw new Error(`cluster file of ${buffer.byteLength} bytes, expected ${expected}`);
  let at = HEADER_BYTES;
  const u32 = (count: number) => {
    const array = new Uint32Array(buffer, at, count);
    at += 4 * count;
    return array;
  };
  const f32 = (count: number) => {
    const array = new Float32Array(buffer, at, count);
    at += 4 * count;
    return array;
  };
  at += 4 * vertices! * perVertex;
  const indices = u32(indexCount!);
  const clusters = {
    count: clusterCount!,
    indexStart: u32(clusterCount!),
    triangleCount: u32(clusterCount!),
    vertexCount: u32(clusterCount!),
    group: u32(clusterCount!),
    refined: u32(clusterCount!),
    subset: u32(clusterCount!),
    bounds: f32(4 * clusterCount!),
  };
  const groups = { count: groupCount!, depth: u32(groupCount!), bounds: f32(4 * groupCount!), error: f32(groupCount!) };
  at += 4 * (2 * groupCount! + 8 * nodeCount! + 6);
  const levelTriangles: number[] = [];
  for (let level = 0; level < reportLevels!; level++) {
    const [, , low, high] = u32(6);
    levelTriangles.push(low! + high! * 2 ** 32);
  }
  for (let i = 0; i < indices.length; i++) if (indices[i]! >= vertices!) throw new Error("a cluster index out of range");
  for (let c = 0; c < clusterCount!; c++) {
    if (clusters.indexStart[c]! + 3 * clusters.triangleCount[c]! > indexCount! || clusters.group[c]! >= groupCount!) throw new Error("a cluster out of range");
    const refined = clusters.refined[c]!;
    if (refined !== NO_GROUP && refined >= groupCount!) throw new Error("a cluster out of range");
  }
  return { vertexCount: vertices!, levels: levels!, indices, clusters, groups, levelTriangles, byteLength: buffer.byteLength };
}

/** What {@link selectCut} measures a cut from, as `layerstack_clusters`'s `CutCamera`: in the mesh's space. */
export interface CutView {
  readonly position: readonly [number, number, number];
  /** `1 / tan(fov_y / 2)`. */
  readonly projectionY: number;
  readonly viewportHeight: number;
  readonly zNear: number;
}

/**
 * The clusters a cut at `pixelError` draws, ascending: `layerstack_clusters`'s
 * rule (`select_cut_linear`), each cluster tested alone as the GPU tests it.
 * Arithmetic in `f32` as the library's, so the two agree but for a sphere
 * whose error projects within rounding of the threshold.
 */
export function selectCut(set: ClusterSetData, view: CutView, pixelError: number): number[] {
  const f = Math.fround;
  const scale = f(f(view.projectionY * 0.5) * view.viewportHeight);
  const projected = (bounds: Float32Array, error: Float32Array, group: number) => {
    const dx = f(bounds[4 * group]! - view.position[0]);
    const dy = f(bounds[4 * group + 1]! - view.position[1]);
    const dz = f(bounds[4 * group + 2]! - view.position[2]);
    const distance = f(f(Math.sqrt(f(f(f(dx * dx) + f(dy * dy)) + f(dz * dz)))) - bounds[4 * group + 3]!);
    return f(f(error[group]! / Math.max(distance, view.zNear)) * scale);
  };
  const out: number[] = [];
  const { bounds, error } = set.groups;
  for (let c = 0; c < set.clusters.count; c++) {
    if (!(projected(bounds, error, set.clusters.group[c]!) > pixelError)) continue;
    const refined = set.clusters.refined[c]!;
    if (refined === NO_GROUP || projected(bounds, error, refined) <= pixelError) out.push(c);
  }
  return out;
}
