// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// GPU-driven drawing of meshes with a prebuilt cluster level-of-detail set
// (`clusterFile.ts`): the GPU chooses each instance's clusters every frame
// and draws them indirectly, so neither the triangles a mesh draws nor the
// instances a prototype has cost CPU time per frame.
//
// Resident: one set of storage buffers for every clustered draw (a
// "scene"), packed whole when the clustered meshes or draws change:
//
// - vertices: the editor's own compact vertices, four words each
//   (`snorm16` x and y, `snorm16` z, `snorm8` normal), four more for
//   `float16` display colors and `f32` st, and per mesh its position
//   decode; indices: every DAG level's, made global;
// - clusters: bounds (mesh space) and group, source group, first index,
//   triangles; groups: the bounds and error of their simplified
//   replacement (error -1 for a root or stuck group, never simplified);
// - instances: each draw's instance matrices (without the decode) and its
//   mesh, draw and index within the draw; work units: an instance and up
//   to {@link CHUNK} of its mesh's clusters;
// - per slot (a draw's subset: a material range, as the set's clusters
//   keep them apart): indirect arguments (`drawIndirect`'s four words) and
//   a run of the visible list, as long as the draw's instances times the
//   subset's original clusters (a cut never holds more).
//
// Each frame, before the render, two compute passes run (`prepare`):
// clear every draw's instance count, then one thread per work unit culls
// the unit's instance (its mesh's bounds against the view frustum, and
// against {@link ClusterView.minInstancePixels}: an instance whose
// bounding sphere covers fewer pixels across is dropped) and tests each
// of its clusters: drawn when the error of its group's replacement
// projects above the pixel error and the error of the group it was
// simplified from does not (`layerstack_clusters`'s cut, carried to the
// world by the instance's matrix: centers transformed, radii and errors
// scaled by its largest axis scale), and its bounds touch the frustum.
// Each visible cluster takes a place in its subset's run with an atomic
// add and is written there as an (instance, cluster) pair. A third, tiny
// pass clamps each count to its run.
//
// Instances under a pixel: the element above has 594 k instances of small
// meshes, whose DAGs bottom out at one cluster of about a hundred
// triangles, so at a distance the cut alone still draws 86% of their
// triangles. Most are under a pixel across there, and dropping those
// (rather than drawing a box or a point for each) cuts the triangles drawn
// from 63 M to 1 M with 0.6% of the pixels differing from the full render
// by 16 levels or more (0.3% without dropping). At 2 px, close views lose
// visibly more (1.6%); hence the default of 1.
//
// Drawing: each clustered draw is one `Mesh` (or, for a mesh of several
// subsets, a `Mesh` drawing nothing with a child per subset) whose
// geometry draws indirectly (`drawIndirect`, its count written by the GPU) one instance
// per visible cluster and {@link CLUSTER_VERTICES} vertices each: vertex
// pulling, the vertex index naming the cluster's corner (past the
// cluster's triangles, a degenerate one), the draw instance (with the
// draw's first instance, its run) naming the pair. Pulling rather than
// compacting indices: no output buffer sized for the triangles a frame may
// draw, eight bytes written per visible cluster instead of twelve per
// triangle, and one pass for instances and clusters alike. The draw's own
// node material is kept: its context supplies the pulled attributes
// (`position`, `normal`, `uv`, `color`) and overrides the model matrices and
// `instanceIndex` with the instance's, so lighting, light links, the
// selection highlight and picking (whose id is the draw's first id plus
// `instanceIndex`) work as for any draw.
//
// Not yet: occlusion culling (a depth pyramid), packing by delta (a
// change to a clustered draw packs the scene again), and paging.

import { BufferAttribute, BufferGeometry, Frustum, IndexNode, IndirectStorageBufferAttribute, Matrix4, Mesh, Sphere, StorageBufferAttribute, Vector3, Vector4 } from "three/webgpu";
import type { Camera, Material, WebGPURenderer } from "three/webgpu";
import {
  Fn,
  If,
  Loop,
  Continue,
  Return,
  atomicAdd,
  bitcast,
  context,
  instanceIndex,
  mat3,
  mat4,
  max,
  min,
  modelNormalMatrix,
  modelWorldMatrix,
  modelWorldMatrixInverse,
  overrideNodes,
  select,
  storage,
  uint,
  uniform,
  unpackHalf2x16,
  unpackSnorm2x16,
  unpackSnorm4x8,
  uvec2,
  varying,
  vec3,
  vec4,
  vertexIndex,
} from "three/tsl";
import { attributeValues, toHalf } from "@forest-rs/layerstack_stagehand";
import type { BufferCost, MeshVertices } from "@forest-rs/layerstack_stagehand";
import { NO_GROUP, residentCost } from "./clusterFile.ts";
import type { ClusterSetData } from "./clusterFile.ts";
import { setFixedNode } from "./fixedNodes.ts";

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Node = any;

/** The most triangles in a cluster (`ClusterConfig::max_triangles`'s default). */
export const MAX_CLUSTER_TRIANGLES = 128;

/** Vertices each drawn cluster runs: three per triangle of the largest cluster. */
export const CLUSTER_VERTICES = 3 * MAX_CLUSTER_TRIANGLES;

/** The most clusters one compute thread tests: a word of visibility bits. */
export const CHUNK = 32;

/** Partial sums of the drawn triangles and clusters, spread over this many counters to spare one address the atomics. */
const STAT_LANES = 256;

/** How a cut is chosen; changing it costs nothing but the next frame. */
export interface ClusterView {
  /** The screen-space error a cut may show, in pixels. */
  pixelError: number;
  /** Instances whose bounding sphere is fewer pixels across than this are not drawn (1 by default; see the header). */
  minInstancePixels: number;
  /** Whether clusters and instances outside the view are culled (off for comparing a cut with the library's). */
  frustumCull: boolean;
}

/** What the last read-back frame drew; `null` before one was read. */
export interface ClusterStats {
  readonly triangles: number;
  readonly clusters: number;
}

/** What a clustered draw's scene holds, in bytes and counts. */
export interface ClusterResidency {
  readonly meshes: number;
  readonly draws: number;
  readonly instances: number;
  readonly clusters: number;
  /** Every level's triangles, once per mesh. */
  readonly triangles: number;
  /** Storage buffers uploaded for the clustered draws. */
  readonly gpuBytes: number;
  /** How long the last packing took on the CPU, in milliseconds (every buffer made again: see the header). */
  readonly packMs: number;
}

/** A clustered mesh: its set and the vertices it indexes, copied before the editor's arrays are released. */
interface ClusterMesh {
  readonly set: ClusterSetData;
  /** Four words a vertex ({@link packVertices}). */
  readonly vertices: Uint32Array;
  /** Four words a vertex: `float16` red and green, blue, then `f32` s and t; `null` without colors or st. */
  readonly extras: Uint32Array | null;
  /** Whether the mesh has display colors. */
  readonly colored: boolean;
  readonly decode: readonly [number, number, number, number];
  /** The mesh's bounding sphere (its root group's, which contains every level). */
  readonly bounds: readonly [number, number, number, number];
  /** Clusters of the original triangles: the most a cut of it holds. */
  readonly originalClusters: number;
  /** The original clusters of each subset (material range): the most a cut of the subset holds. */
  readonly originalBySubset: readonly number[];
}

/**
 * A clustered draw: its object, mesh and instance matrices (three.js's
 * column-major order, without the decode). A mesh of one subset is drawn
 * by the object itself; a mesh of several by a child per subset, each
 * with its range's material (`rangeOf`: the index in the object's
 * material array of each subset's), the object drawing nothing itself.
 */
interface ClusterDraw {
  readonly object: Mesh;
  /** The meshes that draw: the object, or its children, one per subset. */
  readonly parts: readonly Mesh[];
  readonly rangeOf: readonly number[];
  mesh: number;
  matrices: ArrayLike<number>;
  instances: number;
}

/** The GPU state of one packing, replaced whole when the clustered meshes or draws change. */
interface Packed {
  readonly argsAttribute: IndirectStorageBufferAttribute;
  readonly statsAttribute: StorageBufferAttribute;
  readonly visibleAttribute: StorageBufferAttribute;
  /** Each draw's first slot (one per subset, in order: a slot is an indirect draw and a run of the visible list). */
  readonly slots: Map<Mesh, number>;
  readonly context: Node;
  readonly pickContext: Node;
  readonly clear: Node;
  readonly cull: Node;
  readonly clamp: Node;
  /** The three passes in order, kept: a frame dispatches them without making a list. */
  readonly passes: Node[];
  readonly residency: ClusterResidency;
  readonly attributes: readonly StorageBufferAttribute[];
}

/**
 * Every clustered draw of a viewport, drawn by GPU culling and cuts (see
 * the header). The viewport registers meshes as their sets arrive and
 * draws as it places them; {@link prepare} runs before each render.
 */
export class ClusterScene {
  readonly #meshes = new Map<number, ClusterMesh>();
  readonly #draws = new Map<Mesh, ClusterDraw>();
  #packed: Packed | null = null;
  #dirty = false;
  /** The storage buffers of the packing before the last, freed at the next frame. */
  readonly #retired: StorageBufferAttribute[] = [];
  /** How cuts are chosen. */
  readonly view: ClusterView = { pixelError: 1, minInstancePixels: 1, frustumCull: true };
  #stats: ClusterStats | null = null;
  #frames = 0;
  #reading = false;
  /** The last frame's camera and frustum, reused: preparing allocates nothing. */
  readonly #frustum = new Frustum();
  readonly #projectionView = new Matrix4();
  readonly #cameraPosition = uniform(new Vector3());
  readonly #planes = [0, 1, 2, 3, 4, 5].map(() => uniform(new Vector4()));
  readonly #projectionScale = uniform(1);
  readonly #near = uniform(0.1);
  readonly #pixelError = uniform(1);
  readonly #minPixels = uniform(0);
  readonly #cullEnabled = uniform(1, "uint");
  /** The world-space view of the last cut, as `layerstack_clusters`'s `CutCamera` takes it (reused). */
  readonly #cutCamera = { position: [0, 0, 0] as [number, number, number], projectionY: 1, viewportHeight: 1, zNear: 0.01 };

  /** The view the last cut was chosen for, in world space: position, `proj[1][1]`, viewport height in pixels, near plane. */
  get cutCamera(): { readonly position: readonly [number, number, number]; readonly projectionY: number; readonly viewportHeight: number; readonly zNear: number } {
    return this.#cutCamera;
  }

  /** The template geometry's attribute: {@link CLUSTER_VERTICES} vertices, never read (the context supplies them). */
  readonly #template = new BufferAttribute(new Float32Array(CLUSTER_VERTICES * 3), 3);

  /** Called when a packing replaced the materials' context and geometries' arguments: the viewport records its bundles again. */
  onRepacked: (() => void) | null = null;

  /** Whether the stagehand buffer `key` draws through its cluster set. */
  hasMesh(key: number): boolean {
    return this.#meshes.has(key);
  }

  /**
   * Registers buffer `key`'s set with its vertices ({@link packVertices},
   * copied before the viewport released the compact arrays) and their
   * decode (scale, then offset). Draws of the buffer added afterwards are
   * clustered.
   */
  addMesh(key: number, set: ClusterSetData, { vertices, extras, colored }: PackedVertices, decode: readonly [number, number, number, number]): void {
    const c = set.clusters;
    let originalClusters = 0;
    const originalBySubset: number[] = [];
    for (let i = 0; i < c.count; i++) {
      const subset = c.subset[i]!;
      while (originalBySubset.length <= subset) originalBySubset.push(0);
      if (c.refined[i] !== NO_GROUP) continue;
      originalClusters++;
      originalBySubset[subset]! += 1;
    }
    // The last group is the deepest level's: its bounds contain every level below.
    const g = set.groups;
    let root = 0;
    for (let i = 0; i < g.count; i++) if (g.depth[i]! >= g.depth[root]!) root = i;
    this.#meshes.set(key, {
      set,
      vertices,
      extras,
      colored,
      decode,
      bounds: [g.bounds[4 * root]!, g.bounds[4 * root + 1]!, g.bounds[4 * root + 2]!, g.bounds[4 * root + 3]!],
      originalClusters,
      originalBySubset,
    });
    this.#dirty = true;
  }

  /**
   * What holding buffer `key`'s set costs, as a draw list's budget weighs
   * it (`DrawList.setResidentCosts`): every level's triangles, and the
   * bytes packed for it (eight words a vertex, a word an index, eight a
   * cluster and a group) and kept in JavaScript (the file and the packed
   * vertices); `null` without a set.
   */
  meshCost(key: number): BufferCost | null {
    const mesh = this.#meshes.get(key);
    if (mesh === undefined) return null;
    const { set } = mesh;
    let triangles = 0;
    for (const level of set.levelTriangles) triangles += level;
    const counts = { vertices: set.vertexCount, indices: set.indices.length, clusters: set.clusters.count, groups: set.groups.count, triangles };
    return residentCost(counts, set.byteLength + mesh.vertices.byteLength + (mesh.extras?.byteLength ?? 0));
  }

  /** The subsets (material ranges) of buffer `key`'s set: a draw of it needs this many non-empty ranges. */
  subsets(key: number): number {
    return this.#meshes.get(key)?.originalBySubset.length ?? 0;
  }

  /** Forgets buffer `key`'s set (its buffer was freed or rebuilt). */
  dropMesh(key: number): void {
    if (this.#meshes.delete(key)) this.#dirty = true;
  }

  /**
   * A clustered `Mesh` for buffer `key` with `material` (one, or one per
   * draw range with `rangeOf` naming each subset's): its geometry draws
   * indirectly, its materials pull the clustered vertices. Placed by
   * {@link place}.
   */
  makeDraw(key: number, material: Material | Material[], rangeOf: readonly number[] = [0]): Mesh {
    const colored = this.#meshes.get(key)?.colored === true;
    const geometry = (drawing: boolean) => {
      const made = new BufferGeometry();
      made.setAttribute("position", this.#template);
      // A normal attribute, so the material reads normals (the context supplies them).
      made.setAttribute("normal", this.#template);
      // And colors, when the mesh has them: a material reads vertex colors only from a geometry that has them.
      if (colored) made.setAttribute("color", this.#template);
      made.boundingSphere = new Sphere(new Vector3(), Infinity);
      if (!drawing) made.setDrawRange(0, 0);
      return made;
    };
    const several = Array.isArray(material);
    const object = new Mesh(geometry(!several), material);
    object.frustumCulled = false;
    object.userData.clusterKey = key;
    const parts: Mesh[] = [];
    if (several) {
      for (const range of rangeOf) {
        const part = new Mesh(geometry(true), material[range]!);
        part.frustumCulled = false;
        part.matrixAutoUpdate = false;
        object.add(part);
        parts.push(part);
      }
    } else {
      parts.push(object);
    }
    this.#draws.set(object, { object, parts, rangeOf, mesh: key, matrices: [], instances: 0 });
    this.style(object);
    this.#dirty = true;
    return object;
  }

  /** The meshes that draw clustered draw `object`: itself, or one child per subset (for swapping their materials while picking). */
  parts(object: Mesh): readonly Mesh[] {
    return this.#draws.get(object)?.parts ?? [];
  }

  /** Whether `object` is a clustered draw. */
  isDraw(object: object): object is Mesh {
    return this.#draws.has(object as Mesh);
  }

  /** The instances `object` draws (for its run of pick ids). */
  instances(object: Mesh): number {
    return this.#draws.get(object)?.instances ?? 0;
  }

  /** Places clustered draw `object`'s instances: `matrices` holds sixteen numbers each, column-major, without the decode. */
  place(object: Mesh, matrices: ArrayLike<number>): void {
    const draw = this.#draws.get(object);
    if (draw === undefined) return;
    draw.matrices = matrices;
    draw.instances = matrices.length / 16;
    this.#dirty = true;
  }

  /** Removes clustered draw `object`; its geometry is disposed, its materials are the viewport's. */
  removeDraw(object: Mesh): void {
    const draw = this.#draws.get(object);
    if (draw === undefined) return;
    this.#draws.delete(object);
    object.geometry.dispose();
    for (const part of draw.parts) if (part !== object) part.geometry.dispose();
    this.#dirty = true;
  }

  /** Gives `object`'s materials the scene's context, a fixed node (`fixedNodes.ts`): a new material of a clustered draw, after a restyle. */
  style(object: Mesh): void {
    const packed = this.#packed;
    const draw = this.#draws.get(object);
    for (const material of [object.material].flat()) {
      setFixedNode(material, "contextNode", packed?.context ?? null);
      material.needsUpdate = true;
    }
    if (draw === undefined) return;
    // Each subset's part takes its range's material.
    if (Array.isArray(object.material)) draw.parts.forEach((part, subset) => (part.material = (object.material as Material[])[draw.rangeOf[subset]!]!));
    if (packed === null) return;
    const slot = packed.slots.get(object);
    if (slot !== undefined) draw.parts.forEach((part, subset) => part.geometry.setIndirect(packed.argsAttribute, 16 * (slot + subset)));
  }

  /** The context a pick material of clustered draws reads its placement and instance from; `null` before the first packing. */
  get pickContext(): Node {
    return this.#packed?.pickContext ?? null;
  }

  /** What the clustered draws hold; zeros before the first packing. */
  get residency(): ClusterResidency {
    return this.#packed?.residency ?? { meshes: 0, draws: 0, instances: 0, clusters: 0, triangles: 0, gpuBytes: 0, packMs: 0 };
  }

  /** What the last frame read back drew (every few frames, asynchronously). */
  get stats(): ClusterStats | null {
    return this.#stats;
  }

  /**
   * Runs the cut for `camera` (its matrices current) at a viewport
   * `height` pixels tall: packs first when something changed, then the
   * clear, cull and clamp passes. Allocates nothing per frame but the
   * occasional read-back of the statistics.
   */
  prepare(renderer: WebGPURenderer, camera: Camera, height: number): void {
    for (const attribute of this.#retired) attribute.dispose();
    this.#retired.length = 0;
    if (this.#dirty) this.#pack();
    const packed = this.#packed;
    if (packed === null || packed.residency.draws === 0) return;
    const view = this.view;
    this.#projectionView.multiplyMatrices(camera.projectionMatrix, camera.matrixWorldInverse);
    this.#frustum.setFromProjectionMatrix(this.#projectionView, camera.coordinateSystem);
    for (let i = 0; i < 6; i++) {
      const plane = this.#frustum.planes[i]!;
      this.#planes[i]!.value.set(plane.normal.x, plane.normal.y, plane.normal.z, plane.constant);
    }
    this.#cameraPosition.value.setFromMatrixPosition(camera.matrixWorld);
    // `CutCamera::projected_error`'s `proj[1][1] / 2 * height`.
    const cut = this.#cutCamera;
    cut.projectionY = camera.projectionMatrix.elements[5]!;
    cut.viewportHeight = height;
    cut.zNear = (camera as { near?: number }).near ?? 0.01;
    cut.position[0] = this.#cameraPosition.value.x;
    cut.position[1] = this.#cameraPosition.value.y;
    cut.position[2] = this.#cameraPosition.value.z;
    this.#projectionScale.value = cut.projectionY * 0.5 * height;
    this.#near.value = cut.zNear;
    this.#pixelError.value = view.pixelError;
    this.#minPixels.value = view.minInstancePixels;
    this.#cullEnabled.value = view.frustumCull ? 1 : 0;
    renderer.compute(packed.passes);
    if (++this.#frames % 8 === 0 && !this.#reading) void this.#readStats(renderer, packed);
  }

  /** Reads back the statistics' partial sums (and, for checking, each draw's visible pairs on request). */
  async #readStats(renderer: WebGPURenderer, packed: Packed): Promise<void> {
    this.#reading = true;
    try {
      const sums = new Uint32Array(await renderer.getArrayBufferAsync(packed.statsAttribute));
      let triangles = 0;
      let clusters = 0;
      for (let i = 0; i < STAT_LANES; i++) {
        triangles += sums[i]!;
        clusters += sums[STAT_LANES + i]!;
      }
      this.#stats = { triangles, clusters };
    } finally {
      this.#reading = false;
    }
  }

  /**
   * The clusters `object` drew in the last cut, each as its instance
   * within the draw and its cluster within the mesh, read back from the
   * GPU: for checking the GPU's cut against the library's.
   */
  async visible(renderer: WebGPURenderer, object: Mesh): Promise<{ instance: number; cluster: number }[]> {
    const packed = this.#packed;
    const slot = packed?.slots.get(object);
    if (packed === undefined || packed === null || slot === undefined) return [];
    const args = new Uint32Array(await renderer.getArrayBufferAsync(packed.argsAttribute));
    const pairs = new Uint32Array(await renderer.getArrayBufferAsync(packed.visibleAttribute));
    const { firstInstance, firstCluster } = this.#offsets(object);
    const out: { instance: number; cluster: number }[] = [];
    for (let s = slot; s < slot + this.#draws.get(object)!.parts.length; s++) {
      const base = args[4 * s + 3]!;
      for (let i = 0; i < args[4 * s + 1]!; i++) out.push({ instance: pairs[2 * (base + i)]! - firstInstance, cluster: pairs[2 * (base + i) + 1]! - firstCluster });
    }
    return out;
  }

  /** Where `object`'s instances and its mesh's clusters start in the packing. */
  #offsets(object: Mesh): { firstInstance: number; firstCluster: number } {
    let firstInstance = 0;
    for (const draw of this.#draws.values()) {
      if (draw.object === object) break;
      if (this.#meshes.has(draw.mesh)) firstInstance += draw.instances;
    }
    let firstCluster = 0;
    const key = this.#draws.get(object)!.mesh;
    for (const [other, mesh] of this.#meshes) {
      if (other === key) break;
      firstCluster += mesh.set.clusters.count;
    }
    return { firstInstance, firstCluster };
  }

  /** Releases the storage buffers; the draws' geometries are disposed with them. */
  dispose(): void {
    for (const attribute of [...this.#retired, ...(this.#packed?.attributes ?? [])]) attribute.dispose();
    this.#retired.length = 0;
    for (const draw of this.#draws.values()) for (const part of new Set([draw.object, ...draw.parts])) part.geometry.dispose();
    this.#draws.clear();
    this.#meshes.clear();
    this.#packed = null;
  }

  /** Packs every mesh and draw into new storage buffers, and the nodes reading them (see the header). */
  #pack(): void {
    const started = performance.now();
    this.#dirty = false;
    // Freed at the next packing's frame: until then a pick or bundle may still bind them.
    this.#retired.push(...(this.#packed?.attributes ?? []));
    // Meshes in insertion order, with their offsets.
    const meshIndex = new Map<number, number>();
    let vertexCount = 0;
    let indexCount = 0;
    let clusterCount = 0;
    let groupCount = 0;
    let triangles = 0;
    for (const [key, mesh] of this.#meshes) {
      meshIndex.set(key, meshIndex.size);
      vertexCount += mesh.set.vertexCount;
      indexCount += mesh.set.indices.length;
      clusterCount += mesh.set.clusters.count;
      groupCount += mesh.set.groups.count;
      for (const level of mesh.set.levelTriangles) triangles += level;
    }
    const vertices = new Uint32Array(4 * Math.max(1, vertexCount));
    const extras = new Uint32Array(4 * Math.max(1, vertexCount));
    const indices = new Uint32Array(Math.max(1, indexCount));
    // Clusters: group, source group, first index, triangles; then the bounds' bits.
    const clusters = new Uint32Array(8 * Math.max(1, clusterCount));
    const clusterFloats = new Float32Array(clusters.buffer);
    // Groups: bounds, then error.
    const groups = new Float32Array(8 * Math.max(1, groupCount));
    // Meshes: first cluster, clusters, original clusters; then the bounds' bits; then the decode's.
    const meshes = new Uint32Array(12 * Math.max(1, meshIndex.size));
    const meshFloats = new Float32Array(meshes.buffer);
    let vertexBase = 0;
    let indexBase = 0;
    let clusterBase = 0;
    let groupBase = 0;
    for (const [key, mesh] of this.#meshes) {
      const m = meshIndex.get(key)!;
      const { set } = mesh;
      vertices.set(mesh.vertices, 4 * vertexBase);
      if (mesh.extras !== null) extras.set(mesh.extras, 4 * vertexBase);
      for (let i = 0; i < set.indices.length; i++) indices[indexBase + i] = set.indices[i]! + vertexBase;
      const c = set.clusters;
      for (let i = 0; i < c.count; i++) {
        const at = 8 * (clusterBase + i);
        clusters[at] = c.group[i]! + groupBase;
        clusters[at + 1] = c.refined[i] === NO_GROUP ? NO_GROUP : c.refined[i]! + groupBase;
        clusters[at + 2] = c.indexStart[i]! + indexBase;
        // Triangles (at most 256) in the low byte, the subset above.
        clusters[at + 3] = c.triangleCount[i]! | (c.subset[i]! << 8);
        clusterFloats.set(c.bounds.subarray(4 * i, 4 * i + 4), at + 4);
      }
      const g = set.groups;
      for (let i = 0; i < g.count; i++) {
        const at = 8 * (groupBase + i);
        groups.set(g.bounds.subarray(4 * i, 4 * i + 4), at);
        // Infinite errors (never simplified further) as -1: WGSL leaves arithmetic on infinities undefined.
        groups[at + 4] = Number.isFinite(g.error[i]!) ? g.error[i]! : -1;
      }
      meshes[12 * m] = clusterBase;
      meshes[12 * m + 1] = c.count;
      meshes[12 * m + 2] = mesh.originalClusters;
      meshFloats.set(mesh.bounds, 12 * m + 4);
      meshFloats.set(mesh.decode, 12 * m + 8);
      vertexBase += set.vertexCount;
      indexBase += set.indices.length;
      clusterBase += c.count;
      groupBase += set.groups.count;
    }
    // Draws: their instances, work units and runs of the visible list.
    const draws = [...this.#draws.values()].filter((draw) => this.#meshes.has(draw.mesh) && draw.parts.length === this.#meshes.get(draw.mesh)!.originalBySubset.length);
    let slotCount = 0;
    for (const draw of draws) slotCount += draw.parts.length;
    let instanceCount = 0;
    let unitCount = 0;
    let visibleCount = 0;
    for (const draw of draws) {
      const mesh = this.#meshes.get(draw.mesh)!;
      instanceCount += draw.instances;
      unitCount += draw.instances * Math.ceil(mesh.set.clusters.count / CHUNK);
      visibleCount += draw.instances * mesh.originalClusters;
    }
    const instanceMatrices = new Float32Array(16 * Math.max(1, instanceCount));
    const instanceInfo = new Uint32Array(4 * Math.max(1, instanceCount));
    // Units: instance, first cluster, mesh, draw.
    const units = new Uint32Array(4 * Math.max(1, unitCount));
    const args = new Uint32Array(4 * Math.max(1, slotCount));
    // Slots: their run's start and length.
    const runs = new Uint32Array(2 * Math.max(1, slotCount));
    let firstSlot = 0;
    const slots = new Map<Mesh, number>();
    let instance = 0;
    let unit = 0;
    let visible = 0;
    for (const draw of draws) {
      const mesh = this.#meshes.get(draw.mesh)!;
      const m = meshIndex.get(draw.mesh)!;
      const slot = firstSlot;
      slots.set(draw.object, slot);
      instanceMatrices.set(Array.prototype.slice.call(draw.matrices, 0, 16 * draw.instances) as number[], 16 * instance);
      const chunks = Math.ceil(mesh.set.clusters.count / CHUNK);
      for (let i = 0; i < draw.instances; i++) {
        const at = 4 * (instance + i);
        instanceInfo[at] = m;
        instanceInfo[at + 1] = slot;
        instanceInfo[at + 2] = i;
        for (let chunk = 0; chunk < chunks; chunk++) {
          units[4 * unit] = instance + i;
          units[4 * unit + 1] = chunk * CHUNK;
          units[4 * unit + 2] = m;
          units[4 * unit + 3] = slot;
          unit++;
        }
      }
      mesh.originalBySubset.forEach((original, subset) => {
        const at = slot + subset;
        args.set([CLUSTER_VERTICES, 0, 0, visible], 4 * at);
        runs[2 * at] = visible;
        runs[2 * at + 1] = draw.instances * original;
        visible += draw.instances * original;
      });
      instance += draw.instances;
      firstSlot += draw.parts.length;
    }
    const attribute = (array: Uint32Array | Float32Array, itemSize: number) => new StorageBufferAttribute(array, itemSize);
    const vertexAttribute = attribute(vertices, 4);
    const extraAttribute = attribute(extras, 4);
    const indexAttribute = attribute(indices, 1);
    const clusterAttribute = attribute(clusters, 4);
    const groupAttribute = attribute(groups, 4);
    const meshAttribute = attribute(meshes, 4);
    const matrixAttribute = attribute(instanceMatrices, 4);
    const instanceInfoAttribute = attribute(instanceInfo, 4);
    const unitAttribute = attribute(units, 4);
    const runAttribute = attribute(runs, 2);
    const visibleAttribute = attribute(new Uint32Array(2 * Math.max(1, visibleCount)), 2);
    const statsAttribute = attribute(new Uint32Array(2 * STAT_LANES), 1);
    const argsAttribute = new IndirectStorageBufferAttribute(args, 4);
    const attributes = [
      vertexAttribute,
      extraAttribute,
      indexAttribute,
      clusterAttribute,
      groupAttribute,
      meshAttribute,
      matrixAttribute,
      instanceInfoAttribute,
      unitAttribute,
      runAttribute,
      visibleAttribute,
      statsAttribute,
      argsAttribute,
    ];
    const gpuBytes = attributes.reduce((sum, a) => sum + a.array.byteLength, 0);

    // The cull: one thread per work unit. Nine storage buffers, within the
    // ten a stage may bind on the adapters the editor meets.
    const read = (a: StorageBufferAttribute, type: string, count: number): Node => (storage(a, type as never, count) as Node).toReadOnly();
    const unitsNode = read(unitAttribute, "uvec4", Math.max(1, unitCount));
    const matrixNode = read(matrixAttribute, "vec4", 4 * Math.max(1, instanceCount));
    const meshNode = read(meshAttribute, "uvec4", 3 * Math.max(1, meshIndex.size));
    const clusterNode = read(clusterAttribute, "uvec4", 2 * Math.max(1, clusterCount));
    const groupNode = read(groupAttribute, "vec4", 2 * Math.max(1, groupCount));
    const runNode = read(runAttribute, "uvec2", Math.max(1, slotCount));
    const argsNode: Node = (storage(argsAttribute, "uint", 4 * Math.max(1, slotCount)) as Node).toAtomic();
    const argsPlain: Node = storage(argsAttribute, "uint", 4 * Math.max(1, slotCount));
    const visibleNode: Node = storage(visibleAttribute, "uvec2", Math.max(1, visibleCount));
    const statsNode: Node = (storage(statsAttribute, "uint", 2 * STAT_LANES) as Node).toAtomic();
    const statsPlain: Node = storage(statsAttribute, "uint", 2 * STAT_LANES);
    const planes = this.#planes;
    const cameraPosition = this.#cameraPosition;
    const projectionScale = this.#projectionScale;
    const near = this.#near;
    const pixelError = this.#pixelError;
    const minPixels = this.#minPixels;
    const cullEnabled = this.#cullEnabled;

    const inFrustum = (center: Node, radius: Node): Node => {
      let inside: Node = cullEnabled.equal(uint(0));
      let all: Node = null;
      for (const plane of planes) {
        const test = plane.xyz.dot(center).add(plane.w).greaterThanEqual(radius.negate());
        all = all === null ? test : all.and(test);
      }
      inside = inside.or(all);
      return inside;
    };

    const clear = Fn(() => {
      const i = instanceIndex;
      If(i.lessThan(uint(slotCount)), () => {
        argsPlain.element(i.mul(4).add(1)).assign(uint(0));
      });
      If(i.lessThan(uint(2 * STAT_LANES)), () => {
        statsPlain.element(i).assign(uint(0));
      });
    })().compute(Math.max(slotCount, 2 * STAT_LANES), [64]);

    const cull = Fn(() => {
      const work = unitsNode.element(instanceIndex).toVar();
      const inst = work.x;
      const first = work.y;
      const slot = work.w;
      const meshData = meshNode.element(work.z.mul(3)).toVar();
      const columns = [0, 1, 2, 3].map((i) => matrixNode.element(inst.mul(4).add(i)).toVar());
      const m: Node = mat4(columns[0], columns[1], columns[2], columns[3]);
      const scale = max(max(columns[0].xyz.length(), columns[1].xyz.length()), columns[2].xyz.length()).toVar();
      const bounds = (bitcast(meshNode.element(work.z.mul(3).add(1)), "vec4") as Node).toVar();
      const center = m.mul(vec4(bounds.xyz, 1)).xyz.toVar();
      const radius = bounds.w.mul(scale).toVar();
      const distance = max(center.sub(cameraPosition).length(), near);
      // The instance: in view, and at least `minInstancePixels` across.
      If(inFrustum(center, radius).not().or(radius.mul(2).div(distance).mul(projectionScale).lessThan(minPixels)), () => {
        Return();
      });
      const above = (group: Node): Node => {
        const g = groupNode.element(group.mul(2));
        const error = groupNode.element(group.mul(2).add(1)).x;
        const d = m.mul(vec4(g.xyz, 1)).xyz.sub(cameraPosition).length().sub(g.w.mul(scale));
        return error.lessThan(0).or(error.mul(scale).div(max(d, near)).mul(projectionScale).greaterThan(pixelError));
      };
      const count = uint(0).toVar();
      const drawn = uint(0).toVar();
      const end = min(first.add(CHUNK), meshData.y);
      Loop({ start: first, end, type: "uint", condition: "<" }, ({ i }: { i: Node }) => {
        const cluster = meshData.x.add(i);
        const c = clusterNode.element(cluster.mul(2)).toVar();
        If(above(c.x).not(), () => {
          Continue();
        });
        If(c.y.notEqual(uint(NO_GROUP)).and(above(c.y)), () => {
          Continue();
        });
        const sphere = (bitcast(clusterNode.element(cluster.mul(2).add(1)), "vec4") as Node).toVar();
        If(inFrustum(m.mul(vec4(sphere.xyz, 1)).xyz, sphere.w.mul(scale)).not(), () => {
          Continue();
        });
        // Its subset's slot: one atomic add per visible cluster.
        const target = slot.add(c.w.shiftRight(8));
        const at = (atomicAdd(argsNode.element(target.mul(4).add(1)), uint(1)) as Node).toVar();
        const run = runNode.element(target);
        If(at.lessThan(run.y), () => {
          visibleNode.element(run.x.add(at)).assign(uvec2(inst, cluster));
        });
        count.addAssign(1);
        drawn.addAssign(c.w.bitAnd(uint(255)));
      });
      If(count.greaterThan(uint(0)), () => {
        const lane = instanceIndex.bitAnd(uint(STAT_LANES - 1));
        atomicAdd(statsNode.element(lane), drawn);
        atomicAdd(statsNode.element(lane.add(STAT_LANES)), count);
      });
    })().compute(Math.max(1, unitCount), [64]);

    // A draw's count past its run (more clusters than its original ones) is cut back to the run.
    const clamp = Fn(() => {
      const i = instanceIndex;
      If(i.lessThan(uint(slotCount)), () => {
        const count = argsPlain.element(i.mul(4).add(1));
        count.assign(min(count, runNode.element(i).y));
      });
    })().compute(Math.max(1, slotCount), [64]);

    // The vertex stage: the pair of the draw instance, the corner of the vertex index.
    const pairs = read(visibleAttribute, "uvec2", Math.max(1, visibleCount));
    const pair: Node = pairs.element(new IndexNode(IndexNode.INSTANCE));
    const source: Node = varying(pair.x, "vClusterInstance");
    source.setInterpolation("flat");
    const cluster = read(clusterAttribute, "uvec4", 2 * Math.max(1, clusterCount)).element(pair.y.mul(2));
    const corner = select(vertexIndex.div(3).lessThan(cluster.w.bitAnd(uint(255))), vertexIndex, uint(0));
    const vertex = read(indexAttribute, "uint", Math.max(1, indexCount)).element(cluster.z.add(corner));
    const raw = read(vertexAttribute, "uvec4", Math.max(1, vertexCount)).element(vertex);
    const extra = read(extraAttribute, "uvec4", Math.max(1, vertexCount)).element(vertex);
    const placement = read(instanceInfoAttribute, "uvec4", Math.max(1, instanceCount)).element(source);
    const decode: Node = bitcast(read(meshAttribute, "uvec4", 3 * Math.max(1, meshIndex.size)).element(placement.x.mul(3).add(2)), "vec4");
    const matrices = read(matrixAttribute, "vec4", 4 * Math.max(1, instanceCount));
    const world: Node = mat4(matrices.element(source.mul(4)), matrices.element(source.mul(4).add(1)), matrices.element(source.mul(4).add(2)), matrices.element(source.mul(4).add(3)));
    const pulled: Record<string, Node> = {
      position: vec3(unpackSnorm2x16(raw.x) as Node, (unpackSnorm2x16(raw.y) as Node).x).mul(decode.x).add(decode.yzw),
      normal: (unpackSnorm4x8(raw.z) as Node).xyz,
      uv: bitcast(extra.zw, "vec2") as Node,
      color: vec3(unpackHalf2x16(extra.x) as Node, (unpackHalf2x16(extra.y) as Node).x),
    };
    const cache = new Map<string, Node>();
    const getAttribute = (name: string) => {
      if (!cache.has(name)) cache.set(name, name in pulled ? varying(pulled[name]) : null);
      return cache.get(name);
    };
    const overrides = (local: Node): never => [
      [instanceIndex, local],
      [modelWorldMatrix, world],
      [modelWorldMatrixInverse, world.inverse()],
      [modelNormalMatrix, mat3(world).inverse().transpose()],
    ] as never;
    const contextNode = overrideNodes(overrides(placement.z), context({ getAttribute }));
    const pickContext = overrideNodes(overrides(placement.z), context({ getAttribute }));

    this.#packed = {
      argsAttribute,
      statsAttribute,
      visibleAttribute,
      slots,
      context: contextNode,
      pickContext,
      clear,
      cull,
      clamp,
      passes: [clear, cull, clamp],
      residency: { meshes: meshIndex.size, draws: draws.length, instances: instanceCount, clusters: clusterCount, triangles, gpuBytes, packMs: 0 },
      attributes,
    };
    this.#packed = { ...this.#packed, residency: { ...this.#packed.residency, packMs: performance.now() - started } };
    for (const draw of this.#draws.values()) this.style(draw.object);
    this.onRepacked?.();
  }
}

/** A mesh's vertices as the cluster scene holds them ({@link packVertices}). */
export interface PackedVertices {
  /** Four words a vertex: position and normal. */
  readonly vertices: Uint32Array;
  /** Four words a vertex: `float16` colors, `f32` st; `null` without either. */
  readonly extras: Uint32Array | null;
  /** Whether {@link extras} holds display colors. */
  readonly colored: boolean;
}

/**
 * A mesh's compact vertices as the cluster scene's four words each:
 * `snorm16` x and y, `snorm16` z (the padding dropped), the `snorm8` x4
 * normal, zero; and when it has display colors or `st`, four more:
 * `float16` red and green, blue, then `st` as `f32` (a half `st` widened,
 * a tiling one kept as it is).
 */
export function packVertices(vertices: MeshVertices): PackedVertices {
  const count = vertices.count;
  const out = new Uint32Array(4 * count);
  const positions = vertices.position.array as Int16Array;
  const position = new Uint32Array(positions.buffer, positions.byteOffset, 2 * count);
  const normals = vertices.normal.array as Int8Array;
  const normal = new Uint32Array(normals.buffer, normals.byteOffset, count);
  for (let v = 0; v < count; v++) {
    out[4 * v] = position[2 * v]!;
    out[4 * v + 1] = position[2 * v + 1]! & 0xffff;
    out[4 * v + 2] = normal[v]!;
  }
  const colors = vertices.color === null ? null : (vertices.color.array as Float32Array);
  const st = vertices.st === null ? null : attributeValues(vertices.st);
  if (colors === null && st === null) return { vertices: out, extras: null, colored: false };
  const extras = new Uint32Array(4 * count);
  if (colors !== null) {
    for (let v = 0; v < count; v++) {
      extras[4 * v] = (toHalf(colors[3 * v]!) | (toHalf(colors[3 * v + 1]!) << 16)) >>> 0;
      extras[4 * v + 1] = toHalf(colors[3 * v + 2]!);
    }
  }
  if (st !== null) {
    const bits = new Uint32Array(st.buffer, st.byteOffset, st.length);
    for (let v = 0; v < count; v++) {
      extras[4 * v + 2] = bits[2 * v]!;
      extras[4 * v + 3] = bits[2 * v + 1]!;
    }
  }
  return { vertices: out, extras, colored: colors !== null };
}

/** Whether a mesh's compact vertices can be clustered as {@link packVertices} packs them: `float32` x3 colors, no texture coordinate sets besides `st`. */
export function clusterable(vertices: MeshVertices): boolean {
  return (
    vertices.format === "compact" &&
    !vertices.released &&
    (vertices.color === null || (vertices.color.type === "float32" && vertices.color.stride === 3)) &&
    vertices.uvSets.length === 0
  );
}
