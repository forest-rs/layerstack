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
// - tables, one buffer of `uvec4` rows (a stage binds ten storage buffers
//   at most): per mesh its clusters and proxies, bounds and position
//   decode; per cluster its bounds (mesh space) and group, source group,
//   first index, triangles; per group the bounds and error of its
//   simplified replacement (error -1 for a root or stuck group, never
//   simplified); per slot its run (below);
// - instances: each draw's instance matrices (without the decode) and its
//   mesh, draw and index within the draw; work units: an instance and up
//   to {@link CHUNK} of its mesh's clusters;
// - per slot (a draw's subset: a material range, as the set's clusters
//   keep them apart): indirect arguments (`drawIndirect`'s four words) and
//   a run of the visible list, as long as the draw's instances times the
//   subset's original clusters (a cut never holds more);
// - for occlusion culling: a word of bits per work unit (what it drew and
//   was not hidden last frame), a depth list of pairs and the depth
//   pyramid (below).
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
// pass clamps each count to its run. Each slot also has a run of proxies
// (one per instance at most), its count in a set of arguments past the
// slots' own, never drawn itself (see below).
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
// Instances a few pixels across: the same DAGs make a close view draw
// 52 M triangles, nearly all of them root clusters of instances one to
// four pixels across. Below {@link ClusterView.proxyPixels} (3 by
// default), an instance draws its mesh's proxies instead, a few of its own
// triangles per subset (`clusterProxies.ts`): its first work unit appends
// it to each subset's proxy run, and every {@link PROXY_PACK}th proxy of a
// run takes a place in the slot's run as a pack (its pair is the first
// proxy and the slot, with {@link PACKED} set), which a draw instance runs
// as that many proxies of {@link PROXY_VERTICES} vertices each. Packs keep
// the draws one per slot, and a proxy's vertex work an eighth of a
// cluster's. At 3 px the close view draws 7 M triangles in 29 ms rather
// than 52 M in 48 ms, and 1.1% of its pixels differ from the full render by
// 16 levels or more (0.4% with clusters alone; mean grey difference 0.76 of
// 255 rather than 0.27); at 2 px, 20 M triangles and 0.7%. Views of the
// whole element, where few instances are that small, are unchanged.
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
// Occlusion culling, in two phases (as three.js's `MeshletDrawingPipeline`
// culls): the first cull takes only what each unit's bits say was drawn
// and not hidden last frame (still tested against the view and the cut),
// and appends the pairs of opaque slots to a depth list as well; the list
// is drawn, depth only, into a target the canvas's size (two indirect
// draws: clusters, then proxies), and reduced into a pyramid of the
// largest depth of each 2x2 texels. The second cull tests the rest: an
// instance whose bounds are behind the pyramid is hidden whole, and of
// the others each cluster in view and of the cut is drawn when it is not
// hidden and the first phase did not draw it; every unit's bits are
// written for the next frame. Both phases append to the same runs, so a
// frame draws each draw once. What the first phase draws is drawn this
// frame whatever the second finds, and what it misses is tested against
// depth this frame's own draws wrote, so a moving camera shows nothing
// missing: a scripted orbit about a close view of the element matches its
// render without occlusion culling pixel for pixel (one frame in 24 within
// 0.001% of pixels). The close view draws 35% fewer clusters (5.0 M
// triangles, 23 ms); the view of the whole element 10% fewer. Where it
// hides under 5% (the far view), it pauses for 60 frames and tries again:
// its depth pass and second cull cost about 1.5 ms of GPU time there.
// `frustumCull: false` turns it off with the rest of the culling.
//
// Not yet: packing by delta (a change to a clustered draw packs the scene
// again), and paging.

import {
  BufferAttribute,
  BufferGeometry,
  DepthTexture,
  Frustum,
  IndexNode,
  IndirectStorageBufferAttribute,
  Matrix4,
  Mesh,
  NodeMaterial,
  RenderTarget,
  Scene,
  Sphere,
  StorageBufferAttribute,
  Vector2,
  Vector3,
  Vector4,
} from "three/webgpu";
import type { Camera, Material, WebGPURenderer } from "three/webgpu";
import {
  Fn,
  If,
  Loop,
  Continue,
  Return,
  atomicAdd,
  bitcast,
  bool,
  ceil,
  clamp,
  float,
  int,
  ivec2,
  log2,
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
  textureLoad,
  uint,
  uniform,
  uniformArray,
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
import { PROXY_TRIANGLES, buildProxy } from "./clusterProxies.ts";
import { setFixedNode } from "./fixedNodes.ts";

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Node = any;

/** The most triangles in a cluster (`ClusterConfig::max_triangles`'s default). */
export const MAX_CLUSTER_TRIANGLES = 128;

/** Vertices each drawn cluster runs: three per triangle of the largest cluster. */
export const CLUSTER_VERTICES = 3 * MAX_CLUSTER_TRIANGLES;

/** Vertices each drawn instance proxy runs (`clusterProxies.ts`). */
export const PROXY_VERTICES = 3 * PROXY_TRIANGLES;

/** Instance proxies one draw instance runs: as many as fill a cluster's vertices. */
export const PROXY_PACK = CLUSTER_VERTICES / PROXY_VERTICES;

/** The bit of a visible pair's second word that makes it a pack of proxies: the first proxy, then the slot. */
const PACKED = 0x80000000;

/** The most clusters one compute thread tests: a word of visibility bits. */
export const CHUNK = 32;

/** Partial sums of the drawn triangles and clusters, spread over this many counters to spare one address the atomics. */
const STAT_LANES = 256;

/** The statistics' words: triangles, clusters and clusters hidden, {@link STAT_LANES} partial sums each. */
const STAT_WORDS = 3 * STAT_LANES;

/** Occlusion culling pauses while it hides less than this share of what is in view (its depth pass and second cull cost more than it saves)... */
const OCCLUSION_MIN_HIDDEN = 0.05;

/** ...for this many frames, then tries again. */
const OCCLUSION_PAUSE_FRAMES = 60;

/** The most levels of the depth pyramid: enough for 65536 pixels across. */
const PYRAMID_LEVELS = 16;

/** How a cut is chosen; changing it costs nothing but the next frame. */
export interface ClusterView {
  /** The screen-space error a cut may show, in pixels. */
  pixelError: number;
  /** Instances whose bounding sphere is fewer pixels across than this are not drawn (1 by default; see the header). */
  minInstancePixels: number;
  /** Instances whose bounding sphere is fewer pixels across than this draw their proxy rather than clusters (`clusterProxies.ts`; 0 never does). */
  proxyPixels: number;
  /** Whether clusters and instances outside the view are culled (off for comparing a cut with the library's, and with it occlusion culling). */
  frustumCull: boolean;
  /** Whether clusters and instances hidden behind what the last frame drew are culled (see the header). */
  occlusionCull: boolean;
}

/** What the last read-back frame drew; `null` before one was read. */
export interface ClusterStats {
  readonly triangles: number;
  readonly clusters: number;
  /** What occlusion culling did not draw: clusters in view and of the cut but hidden, and each instance hidden whole as one. */
  readonly occluded: number;
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
  /** Each subset's instance proxy, three vertex indices per triangle (`clusterProxies.ts`). */
  readonly proxies: readonly Uint32Array[];
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
  /** Where the statistics start in the arguments' words. */
  readonly statsAt: number;
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
  /** With occlusion culling: the clear and the first phase, then (after the depth and the pyramid) the second and the clamp. */
  readonly firstPasses: Node[];
  readonly secondPasses: Node[];
  /** The two depth draws of the first phase's pairs (clusters, proxies), drawn into the pyramid's depth. */
  readonly depthScene: Scene;
  readonly depthMaterial: NodeMaterial;
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
  /** The depth draws' geometries and material of the packing before the last, freed with them. */
  readonly #retiredDepth: { dispose(): void }[] = [];
  /** How cuts are chosen. */
  readonly view: ClusterView = { pixelError: 1, minInstancePixels: 1, proxyPixels: 3, frustumCull: true, occlusionCull: true };
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
  readonly #proxyPixels = uniform(0);
  readonly #cullEnabled = uniform(1, "uint");
  /** The depth pyramid occlusion culling tests against (see {@link Occlusion}). */
  readonly #occlusion = makeOcclusion();
  /** Frames occlusion culling stays paused for, having hidden little ({@link OCCLUSION_MIN_HIDDEN}). */
  #occlusionPaused = 0;

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
    // The proxies are made from the positions as packed: `snorm16`, in the mesh's own frame (the decode is uniform).
    const positions = new Float32Array(3 * set.vertexCount);
    for (let v = 0; v < set.vertexCount; v++) {
      positions[3 * v] = (vertices[4 * v]! << 16) >> 16;
      positions[3 * v + 1] = vertices[4 * v]! >> 16;
      positions[3 * v + 2] = (vertices[4 * v + 1]! << 16) >> 16;
    }
    const proxies = originalBySubset.map((_, subset) => buildProxy(set, positions, subset));
    this.#meshes.set(key, {
      set,
      vertices,
      extras,
      colored,
      decode,
      bounds: [g.bounds[4 * root]!, g.bounds[4 * root + 1]!, g.bounds[4 * root + 2]!, g.bounds[4 * root + 3]!],
      originalClusters,
      originalBySubset,
      proxies,
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
    let proxyIndices = 0;
    for (const proxy of mesh.proxies) proxyIndices += proxy.length;
    const counts = { vertices: set.vertexCount, indices: set.indices.length + proxyIndices, clusters: set.clusters.count + mesh.proxies.length, groups: set.groups.count, triangles };
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
   * `width` by `height` pixels: packs first when something changed, then
   * the clear, cull and clamp passes, or with occlusion culling the clear
   * and first cull, the depth draws, the pyramid, the second cull and the
   * clamp (see the header). Allocates nothing per frame but the
   * occasional read-back of the statistics (and what three.js allocates
   * for the depth draws' render).
   */
  prepare(renderer: WebGPURenderer, camera: Camera, width: number, height: number): void {
    for (const attribute of this.#retired) attribute.dispose();
    this.#retired.length = 0;
    for (const disposable of this.#retiredDepth) disposable.dispose();
    this.#retiredDepth.length = 0;
    if (this.#occlusionPaused > 0) this.#occlusionPaused--;
    const occluding = this.view.frustumCull && this.view.occlusionCull && this.#occlusionPaused === 0;
    // A pyramid outgrown is made again, and the cull that reads it with it.
    if (occluding && resizeOcclusion(this.#occlusion, width, height)) this.#dirty = true;
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
    this.#proxyPixels.value = view.proxyPixels;
    this.#cullEnabled.value = view.frustumCull ? 1 : 0;
    if (!occluding) {
      renderer.compute(packed.passes);
    } else {
      const occlusion = this.#occlusion;
      occlusion.viewProjection.value.copy(this.#projectionView);
      renderer.compute(packed.firstPasses);
      // What the first phase drew, depth only, then its pyramid, then the rest tested against it.
      const target = renderer.getRenderTarget();
      renderer.setRenderTarget(occlusion.target);
      renderer.render(packed.depthScene, camera);
      renderer.setRenderTarget(target);
      renderer.compute(occlusion.passes);
      renderer.compute(packed.secondPasses);
    }
    if (++this.#frames % 8 === 0 && !this.#reading) void this.#readStats(renderer, packed, occluding);
  }

  /** Reads back the statistics' partial sums (and, for checking, each draw's visible pairs on request). */
  async #readStats(renderer: WebGPURenderer, packed: Packed, occluding: boolean): Promise<void> {
    this.#reading = true;
    try {
      // The statistics' words alone: the arguments before them are as many as the slots.
      const sums = new Uint32Array((await renderer.getArrayBufferAsync(packed.argsAttribute, null, 4 * packed.statsAt, 4 * STAT_WORDS)) as ArrayBuffer);
      let triangles = 0;
      let clusters = 0;
      let occluded = 0;
      for (let i = 0; i < STAT_LANES; i++) {
        triangles += sums[i]!;
        clusters += sums[STAT_LANES + i]!;
        occluded += sums[2 * STAT_LANES + i]!;
      }
      this.#stats = { triangles, clusters, occluded };
      if (occluding && occluded < OCCLUSION_MIN_HIDDEN * (clusters + occluded)) this.#occlusionPaused = OCCLUSION_PAUSE_FRAMES;
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
      for (let i = 0; i < args[4 * s + 1]!; i++) {
        // Packs of proxies are not of the cut.
        if (pairs[2 * (base + i) + 1]! >= PACKED) continue;
        out.push({ instance: pairs[2 * (base + i)]! - firstInstance, cluster: pairs[2 * (base + i) + 1]! - firstCluster });
      }
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
    this.#retireDepth();
    for (const disposable of this.#retiredDepth) disposable.dispose();
    this.#retiredDepth.length = 0;
    disposeOcclusion(this.#occlusion);
    for (const draw of this.#draws.values()) for (const part of new Set([draw.object, ...draw.parts])) part.geometry.dispose();
    this.#draws.clear();
    this.#meshes.clear();
    this.#packed = null;
  }

  /** Hands the last packing's depth draws to be freed at the next frame. */
  #retireDepth(): void {
    const packed = this.#packed;
    if (packed === null) return;
    this.#retiredDepth.push(packed.depthMaterial);
    for (const draw of packed.depthScene.children) this.#retiredDepth.push((draw as Mesh).geometry);
  }

  /** Packs every mesh and draw into new storage buffers, and the nodes reading them (see the header). */
  #pack(): void {
    const started = performance.now();
    this.#dirty = false;
    // Freed at the next packing's frame: until then a pick or bundle may still bind them.
    this.#retired.push(...(this.#packed?.attributes ?? []));
    this.#retireDepth();
    // Meshes in insertion order, with their offsets.
    const meshIndex = new Map<number, number>();
    let vertexCount = 0;
    let indexCount = 0;
    let clusterCount = 0;
    let groupCount = 0;
    let triangles = 0;
    let proxyIndexCount = 0;
    let proxyCount = 0;
    for (const [key, mesh] of this.#meshes) {
      meshIndex.set(key, meshIndex.size);
      vertexCount += mesh.set.vertexCount;
      indexCount += mesh.set.indices.length;
      clusterCount += mesh.set.clusters.count;
      groupCount += mesh.set.groups.count;
      for (const level of mesh.set.levelTriangles) triangles += level;
      proxyCount += mesh.proxies.length;
      for (const proxy of mesh.proxies) proxyIndexCount += proxy.length;
    }
    // The proxies follow every set's clusters and indices, a cluster each.
    const proxyClusterBase = clusterCount;
    const proxyIndexBase = indexCount;
    clusterCount += proxyCount;
    indexCount += proxyIndexCount;
    const vertices = new Uint32Array(4 * Math.max(1, vertexCount));
    const extras = new Uint32Array(4 * Math.max(1, vertexCount));
    const indices = new Uint32Array(Math.max(1, indexCount));
    // Clusters: group, source group, first index, triangles; then the bounds' bits.
    const clusters = new Uint32Array(8 * Math.max(1, clusterCount));
    const clusterFloats = new Float32Array(clusters.buffer);
    // Groups: bounds, then error.
    const groups = new Float32Array(8 * Math.max(1, groupCount));
    // Meshes: first cluster, clusters, first proxy, proxies; then the bounds' bits; then the decode's.
    const meshes = new Uint32Array(12 * Math.max(1, meshIndex.size));
    const meshFloats = new Float32Array(meshes.buffer);
    let vertexBase = 0;
    let indexBase = 0;
    let clusterBase = 0;
    let groupBase = 0;
    let proxyCluster = proxyClusterBase;
    let proxyIndex = proxyIndexBase;
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
      meshes[12 * m + 2] = proxyCluster;
      meshes[12 * m + 3] = mesh.proxies.length;
      mesh.proxies.forEach((proxy, subset) => {
        for (let i = 0; i < proxy.length; i++) indices[proxyIndex + i] = proxy[i]! + vertexBase;
        const at = 8 * proxyCluster;
        // Of no group: never cut, only drawn whole for a small instance.
        clusters[at] = 0;
        clusters[at + 1] = NO_GROUP;
        clusters[at + 2] = proxyIndex;
        clusters[at + 3] = proxy.length / 3 | (subset << 8);
        clusterFloats.set(mesh.bounds, at + 4);
        proxyIndex += proxy.length;
        proxyCluster++;
      });
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
    let clusterCapacity = 0;
    for (const draw of draws) {
      const mesh = this.#meshes.get(draw.mesh)!;
      instanceCount += draw.instances;
      unitCount += draw.instances * Math.ceil(mesh.set.clusters.count / CHUNK);
      visibleCount += draw.instances * (mesh.originalClusters + mesh.proxies.length);
      clusterCapacity += draw.instances * mesh.originalClusters;
    }
    // The depth list: as many clusters and proxies as the runs hold, clusters first.
    const proxyCapacity = visibleCount - clusterCapacity;
    const instanceMatrices = new Float32Array(16 * Math.max(1, instanceCount));
    const instanceInfo = new Uint32Array(4 * Math.max(1, instanceCount));
    // Units: instance, first cluster, mesh, draw.
    const units = new Uint32Array(4 * Math.max(1, unitCount));
    // A slot's clusters' arguments and run at its index, its proxies' `slotCount` further.
    const slotArgs = new Uint32Array(8 * Math.max(1, slotCount));
    // Slots' runs: start, length, whether its material hides what is behind it (its pairs drawn into the depth pyramid's depth), 0.
    const runs = new Uint32Array(8 * Math.max(1, slotCount));
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
        const occluder = occludes(draw.parts[subset]!.material as Material) ? 1 : 0;
        slotArgs.set([CLUSTER_VERTICES, 0, 0, visible], 4 * at);
        runs.set([visible, draw.instances * original, occluder], 4 * at);
        visible += draw.instances * original;
        // Its proxies: a counter (in an unused set of arguments) and a run of one per instance at most.
        runs.set([visible, draw.instances, occluder], 4 * (slotCount + at));
        visible += draw.instances;
      });
      instance += draw.instances;
      firstSlot += draw.parts.length;
    }
    // The static tables in one buffer of `uvec4` rows (a stage binds ten
    // storage buffers at most): meshes (three rows each), clusters (two),
    // groups (two, as `f32`) and runs (one).
    const clusterRows = 3 * meshIndex.size;
    const groupRows = clusterRows + 2 * clusterCount;
    const runRows = groupRows + 2 * groupCount;
    const tables = new Uint32Array(4 * (runRows + 2 * Math.max(1, slotCount)));
    tables.set(meshes.subarray(0, 12 * meshIndex.size), 0);
    tables.set(clusters.subarray(0, 8 * clusterCount), 4 * clusterRows);
    tables.set(new Uint32Array(groups.buffer, 0, 8 * groupCount), 4 * groupRows);
    tables.set(runs, 4 * runRows);
    // The arguments: each slot's, its proxies' counters, the two depth
    // draws' (clusters, then proxies past the clusters' list), and the
    // statistics' partial sums.
    const depthArgs = 8 * Math.max(1, slotCount);
    const statsAt = depthArgs + 8;
    const args = new Uint32Array(statsAt + STAT_WORDS);
    args.set(slotArgs, 0);
    args.set([CLUSTER_VERTICES, 0, 0, 0, PROXY_VERTICES, 0, 0, clusterCapacity], depthArgs);
    const attribute = (array: Uint32Array | Float32Array, itemSize: number) => new StorageBufferAttribute(array, itemSize);
    const vertexAttribute = attribute(vertices, 4);
    const extraAttribute = attribute(extras, 4);
    const indexAttribute = attribute(indices, 1);
    const tableAttribute = attribute(tables, 4);
    const matrixAttribute = attribute(instanceMatrices, 4);
    const instanceInfoAttribute = attribute(instanceInfo, 4);
    const unitAttribute = attribute(units, 4);
    const visibleAttribute = attribute(new Uint32Array(2 * Math.max(1, visibleCount)), 2);
    // Each work unit's clusters drawn and not hidden in the last frame, a bit each (an instance's proxies: bit 0 of its first unit).
    const bitsAttribute = attribute(new Uint32Array(Math.max(1, unitCount)), 1);
    // The pairs drawn into the depth pyramid's depth: clusters, then proxies.
    const depthAttribute = attribute(new Uint32Array(2 * Math.max(1, visibleCount)), 2);
    const argsAttribute = new IndirectStorageBufferAttribute(args, 4);
    const attributes = [
      vertexAttribute,
      extraAttribute,
      indexAttribute,
      tableAttribute,
      matrixAttribute,
      instanceInfoAttribute,
      unitAttribute,
      visibleAttribute,
      bitsAttribute,
      depthAttribute,
      argsAttribute,
    ];
    const gpuBytes = attributes.reduce((sum, a) => sum + a.array.byteLength, 0);

    // The cull: one thread per work unit, eight storage buffers.
    const read = (a: StorageBufferAttribute, type: string, count: number): Node => (storage(a, type as never, count) as Node).toReadOnly();
    const unitsNode = read(unitAttribute, "uvec4", Math.max(1, unitCount));
    const matrixNode = read(matrixAttribute, "vec4", 4 * Math.max(1, instanceCount));
    const tableNode = read(tableAttribute, "uvec4", tables.length / 4);
    const meshRow = (m: Node, row: number): Node => tableNode.element(m.mul(3).add(row));
    const clusterRow = (c: Node, row: number): Node => tableNode.element(c.mul(2).add(clusterRows + row));
    const groupRow = (g: Node, row: number): Node => bitcast(tableNode.element(g.mul(2).add(groupRows + row)), "vec4");
    const runRow = (slot: Node): Node => tableNode.element(slot.add(runRows));
    const argsNode: Node = (storage(argsAttribute, "uint", args.length) as Node).toAtomic();
    const argsPlain: Node = storage(argsAttribute, "uint", args.length);
    const visibleNode: Node = storage(visibleAttribute, "uvec2", Math.max(1, visibleCount));
    const bitsNode: Node = storage(bitsAttribute, "uint", Math.max(1, unitCount));
    const depthNode: Node = storage(depthAttribute, "uvec2", Math.max(1, visibleCount));
    const occlusion = this.#occlusion;
    const hzbNode: Node = read(occlusion.pyramid, "float", occlusion.pyramid.array.length);
    const planes = this.#planes;
    const cameraPosition = this.#cameraPosition;
    const projectionScale = this.#projectionScale;
    const near = this.#near;
    const pixelError = this.#pixelError;
    const minPixels = this.#minPixels;
    const proxyPixels = this.#proxyPixels;
    const cullEnabled = this.#cullEnabled;
    const hidden = occlusionTest(hzbNode, occlusion);

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
      If(i.lessThan(uint(2 * slotCount)), () => {
        argsPlain.element(i.mul(4).add(1)).assign(uint(0));
      });
      If(i.lessThan(uint(2)), () => {
        argsPlain.element(i.mul(4).add(depthArgs + 1)).assign(uint(0));
      });
      If(i.lessThan(uint(STAT_WORDS)), () => {
        argsPlain.element(i.add(statsAt)).assign(uint(0));
      });
    })().compute(Math.max(2 * slotCount, STAT_WORDS), [64]);

    /** Appends `pair` to slot `target`'s run (cluster or proxy), and to the depth list when the run's material hides what is behind it and `depth` holds; returns where in the run. */
    const append = (target: Node, pair: Node, depth: boolean): Node => {
      const at = (atomicAdd(argsNode.element(target.mul(4).add(1)), uint(1)) as Node).toVar();
      const run = runRow(target).toVar();
      If(at.lessThan(run.y), () => {
        visibleNode.element(run.x.add(at)).assign(pair);
      });
      if (depth) {
        If(run.z.notEqual(uint(0)), () => {
          // Proxies' runs follow the slots' own: their pairs go to the second depth draw.
          const proxy = target.greaterThanEqual(uint(slotCount));
          const slotOf = select(proxy, uint(depthArgs + 4 + 1), uint(depthArgs + 1));
          const place = (atomicAdd(argsNode.element(slotOf), uint(1)) as Node).toVar();
          If(place.lessThan(select(proxy, uint(proxyCapacity), uint(clusterCapacity))), () => {
            depthNode.element(select(proxy, place.add(uint(clusterCapacity)), place)).assign(pair);
          });
        });
      }
      return at;
    };

    const stat = (block: number, value: Node): void => {
      atomicAdd(argsNode.element(instanceIndex.bitAnd(uint(STAT_LANES - 1)).add(statsAt + block * STAT_LANES)), value);
    };

    /**
     * The cull of one phase: `all` without occlusion; `first` only what
     * was drawn and not hidden last frame (its depth then drawn into the
     * pyramid); `second` the rest, against the pyramid, writing what is
     * drawn and not hidden for the next frame.
     */
    const makeCull = (phase: "all" | "first" | "second"): Node =>
      Fn(() => {
        const unit = instanceIndex;
        const work = unitsNode.element(unit).toVar();
        const inst = work.x;
        const first = work.y;
        const slot = work.w;
        const last = (phase === "first" ? bitsNode.element(unit) : uint(0)).toVar();
        if (phase === "first") {
          If(last.equal(uint(0)), () => {
            Return();
          });
        }
        const meshData = meshRow(work.z, 0).toVar();
        const columns = [0, 1, 2, 3].map((i) => matrixNode.element(inst.mul(4).add(i)).toVar());
        const m: Node = mat4(columns[0], columns[1], columns[2], columns[3]);
        const scale = max(max(columns[0].xyz.length(), columns[1].xyz.length()), columns[2].xyz.length()).toVar();
        const bounds = (bitcast(meshRow(work.z, 1), "vec4") as Node).toVar();
        const center = m.mul(vec4(bounds.xyz, 1)).xyz.toVar();
        const radius = bounds.w.mul(scale).toVar();
        const distance = max(center.sub(cameraPosition).length(), near);
        const pixels = radius.mul(2).div(distance).mul(projectionScale).toVar();
        const wasDrawn = (bit: Node): Node => (phase === "second" ? bitsNode.element(unit).shiftRight(bit).bitAnd(uint(1)).equal(uint(1)) : bool(true));
        // The instance: in view, and at least `minInstancePixels` across.
        If(inFrustum(center, radius).not().or(pixels.lessThan(minPixels)), () => {
          if (phase === "second") bitsNode.element(unit).assign(uint(0));
          Return();
        });
        if (phase === "second") {
          // Hidden as a whole: its clusters untested, the instance counted once among the hidden.
          If(hidden(center, radius), () => {
            If(first.equal(uint(0)), () => {
              stat(2, uint(1));
            });
            bitsNode.element(unit).assign(uint(0));
            Return();
          });
        }
        // Under `proxyPixels`, its proxies instead of clusters (written by its first unit).
        If(pixels.lessThan(proxyPixels).and(meshData.w.greaterThan(uint(0))), () => {
          let mine: Node = first.equal(uint(0));
          if (phase === "first") mine = mine.and(last.bitAnd(uint(1)).equal(uint(1)));
          if (phase === "second") mine = mine.and(wasDrawn(uint(0)).not());
          If(mine, () => {
            const drawn = uint(0).toVar();
            Loop({ start: uint(0), end: meshData.w, type: "uint", condition: "<" }, ({ i }: { i: Node }) => {
              const proxy = meshData.z.add(i);
              const target = slot.add(i);
              const at = append(target.add(uint(slotCount)), uvec2(inst, proxy), phase === "first");
              // The first of each {@link PROXY_PACK} takes a draw instance for them all.
              If(at.mod(uint(PROXY_PACK)).equal(uint(0)), () => {
                append(target, uvec2(at, target.bitOr(uint(PACKED))), false);
              });
              drawn.addAssign(clusterRow(proxy, 0).w.bitAnd(uint(255)));
            });
            stat(0, drawn);
            stat(1, meshData.w);
          });
          if (phase === "second") bitsNode.element(unit).assign(select(first.equal(uint(0)), uint(1), uint(0)));
          Return();
        });
        const above = (group: Node): Node => {
          const g = groupRow(group, 0);
          const error = groupRow(group, 1).x;
          const d = m.mul(vec4(g.xyz, 1)).xyz.sub(cameraPosition).length().sub(g.w.mul(scale));
          return error.lessThan(0).or(error.mul(scale).div(max(d, near)).mul(projectionScale).greaterThan(pixelError));
        };
        const count = uint(0).toVar();
        const drawn = uint(0).toVar();
        const kept = uint(0).toVar();
        const occluded = uint(0).toVar();
        const end = min(first.add(CHUNK), meshData.y);
        Loop({ start: first, end, type: "uint", condition: "<" }, ({ i }: { i: Node }) => {
          const bit = i.sub(first);
          if (phase === "first") {
            If(last.shiftRight(bit).bitAnd(uint(1)).equal(uint(0)), () => {
              Continue();
            });
          }
          const cluster = meshData.x.add(i);
          const c = clusterRow(cluster, 0).toVar();
          If(above(c.x).not(), () => {
            Continue();
          });
          If(c.y.notEqual(uint(NO_GROUP)).and(above(c.y)), () => {
            Continue();
          });
          const sphere = (bitcast(clusterRow(cluster, 1), "vec4") as Node).toVar();
          const sphereCenter = m.mul(vec4(sphere.xyz, 1)).xyz.toVar();
          If(inFrustum(sphereCenter, sphere.w.mul(scale)).not(), () => {
            Continue();
          });
          if (phase === "second") {
            // Hidden: not drawn, and tested again next frame; drawn last frame: drawn already by the first phase.
            If(hidden(sphereCenter, sphere.w.mul(scale)), () => {
              occluded.addAssign(1);
              Continue();
            });
            kept.assign(kept.bitOr(uint(1).shiftLeft(bit)));
            If(wasDrawn(bit), () => {
              Continue();
            });
          }
          // Its subset's slot: one atomic add per visible cluster.
          append(slot.add(c.w.shiftRight(8)), uvec2(inst, cluster), phase === "first");
          count.addAssign(1);
          drawn.addAssign(c.w.bitAnd(uint(255)));
        });
        if (phase === "second") {
          bitsNode.element(unit).assign(kept);
          If(occluded.greaterThan(uint(0)), () => {
            stat(2, occluded);
          });
        }
        If(count.greaterThan(uint(0)), () => {
          stat(0, drawn);
          stat(1, count);
        });
      })().compute(Math.max(1, unitCount), [64]);

    const cull = makeCull("all");
    const cullFirst = makeCull("first");
    const cullSecond = makeCull("second");

    // A draw's count past its run (more clusters than its original ones) is cut back to the run.
    const clamp = Fn(() => {
      const i = instanceIndex;
      If(i.lessThan(uint(2 * slotCount)), () => {
        const count = argsPlain.element(i.mul(4).add(1));
        count.assign(min(count, runRow(i).y));
      });
      If(i.lessThan(uint(2)), () => {
        const count = argsPlain.element(i.mul(4).add(depthArgs + 1));
        count.assign(min(count, select(i.equal(uint(0)), uint(clusterCapacity), uint(proxyCapacity)) as Node));
      });
    })().compute(Math.max(1, 2 * slotCount), [64]);

    // The vertex stage: the pair of the draw instance, the corner of the vertex index.
    // A pack of proxies: each run of {@link PROXY_VERTICES} vertices the
    // pair of one proxy of the slot's, past the slot's count a degenerate one.
    const pairs = read(visibleAttribute, "uvec2", Math.max(1, visibleCount));
    const drawn: Node = pairs.element(new IndexNode(IndexNode.INSTANCE));
    const packedPair = drawn.y.greaterThanEqual(uint(PACKED));
    const proxySlot = drawn.y.bitAnd(uint(PACKED - 1)).add(uint(slotCount));
    const proxyAt = drawn.x.add(vertexIndex.div(PROXY_VERTICES));
    const tablesRead = read(tableAttribute, "uvec4", tables.length / 4);
    const proxyRun = tablesRead.element(proxySlot.add(runRows));
    const proxies = read(argsAttribute, "uint", args.length).element(proxySlot.mul(4).add(1));
    const pair: Node = select(packedPair, pairs.element(proxyRun.x.add(min(proxyAt, proxyRun.y.sub(1)))), drawn);
    const local = select(packedPair, vertexIndex.mod(PROXY_VERTICES), vertexIndex);
    const source: Node = varying(pair.x, "vClusterInstance");
    source.setInterpolation("flat");
    const cluster = tablesRead.element(pair.y.mul(2).add(clusterRows));
    const corner = select(local.div(3).lessThan(cluster.w.bitAnd(uint(255))).and(packedPair.not().or(proxyAt.lessThan(proxies))), local, uint(0));
    const vertex = read(indexAttribute, "uint", Math.max(1, indexCount)).element(cluster.z.add(corner));
    const raw = read(vertexAttribute, "uvec4", Math.max(1, vertexCount)).element(vertex);
    const extra = read(extraAttribute, "uvec4", Math.max(1, vertexCount)).element(vertex);
    const placement = read(instanceInfoAttribute, "uvec4", Math.max(1, instanceCount)).element(source);
    const decode: Node = bitcast(tablesRead.element(placement.x.mul(3).add(2)), "vec4");
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

    // The depth draws: the first phase's pairs, positions only.
    const depthPair: Node = read(depthAttribute, "uvec2", Math.max(1, visibleCount)).element(new IndexNode(IndexNode.INSTANCE));
    const depthCluster = tablesRead.element(depthPair.y.mul(2).add(clusterRows));
    const depthCorner = select(vertexIndex.div(3).lessThan(depthCluster.w.bitAnd(uint(255))), vertexIndex, uint(0));
    const depthRaw = read(vertexAttribute, "uvec4", Math.max(1, vertexCount)).element(read(indexAttribute, "uint", Math.max(1, indexCount)).element(depthCluster.z.add(depthCorner)));
    const depthPlacement = read(instanceInfoAttribute, "uvec4", Math.max(1, instanceCount)).element(depthPair.x);
    const depthDecode: Node = bitcast(tablesRead.element(depthPlacement.x.mul(3).add(2)), "vec4");
    const depthWorld: Node = mat4(matrices.element(depthPair.x.mul(4)), matrices.element(depthPair.x.mul(4).add(1)), matrices.element(depthPair.x.mul(4).add(2)), matrices.element(depthPair.x.mul(4).add(3)));
    const depthPosition = vec3(unpackSnorm2x16(depthRaw.x) as Node, (unpackSnorm2x16(depthRaw.y) as Node).x).mul(depthDecode.x).add(depthDecode.yzw);
    const depthContext = overrideNodes([[modelWorldMatrix, depthWorld]] as never, context({ getAttribute: (name: string) => (name === "position" ? depthPosition : null) }));
    const depthMaterial = new NodeMaterial();
    depthMaterial.colorWrite = false;
    depthMaterial.fog = false;
    setFixedNode(depthMaterial, "contextNode", depthContext);
    const depthScene = new Scene();
    for (const offset of [depthArgs, depthArgs + 4]) {
      const geometry = new BufferGeometry();
      geometry.setAttribute("position", this.#template);
      geometry.boundingSphere = new Sphere(new Vector3(), Infinity);
      geometry.setIndirect(argsAttribute, 4 * offset);
      const draw = new Mesh(geometry, depthMaterial);
      draw.frustumCulled = false;
      depthScene.add(draw);
    }

    this.#packed = {
      argsAttribute,
      statsAt,
      visibleAttribute,
      slots,
      context: contextNode,
      pickContext,
      clear,
      cull,
      clamp,
      passes: [clear, cull, clamp],
      firstPasses: [clear, cullFirst],
      secondPasses: [cullSecond, clamp],
      depthScene,
      depthMaterial,
      residency: { meshes: meshIndex.size, draws: draws.length, instances: instanceCount, clusters: clusterCount, triangles, gpuBytes, packMs: 0 },
      attributes,
    };
    this.#packed = { ...this.#packed, residency: { ...this.#packed.residency, packMs: performance.now() - started } };
    for (const draw of this.#draws.values()) this.style(draw.object);
    this.onRepacked?.();
  }
}

/**
 * Whether a draw in `material` hides what is behind it, so its pairs are
 * drawn into the depth pyramid: opaque, not alpha-tested, cut out by a
 * map or hashed.
 */
function occludes(material: Material): boolean {
  const m = material as Material & { alphaMap?: unknown; transmission?: number; opacityNode?: unknown };
  return !m.transparent && m.alphaTest === 0 && !m.alphaHash && !m.alphaToCoverage && m.alphaMap == null && m.opacityNode == null && (m.transmission ?? 0) === 0 && m.colorWrite && m.depthWrite;
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

/**
 * The depth pyramid of occlusion culling: the first phase's depth (a
 * depth-only target the canvas's size, not multisampled), and levels of
 * the largest depth of 2x2 texels of the level before, the first of 2x2
 * pixels, in one storage buffer (three.js's `MeshletDrawingPipeline` lays
 * them out the same way). The buffer has room for more than the canvas
 * needs; outgrowing it makes it again (and the cull that reads it).
 */
interface Occlusion {
  depth: DepthTexture;
  target: RenderTarget;
  pyramid: StorageBufferAttribute;
  /** Each level's offset in the buffer, width and height. */
  readonly levels: Node;
  readonly levelData: Vector4[];
  readonly levelCount: Node;
  readonly size: Node;
  readonly viewProjection: Node;
  /** The passes that reduce the depth into the levels, made again when the size changes. */
  passes: Node[];
  width: number;
  height: number;
}

function makeOcclusion(): Occlusion {
  const depth = new DepthTexture(1, 1);
  const levelData = Array.from({ length: PYRAMID_LEVELS }, () => new Vector4());
  return {
    depth,
    target: new RenderTarget(1, 1, { depthTexture: depth, count: 0 }),
    pyramid: new StorageBufferAttribute(new Float32Array(1), 1),
    levels: uniformArray(levelData, "vec4"),
    levelData,
    levelCount: uniform(1, "uint"),
    size: uniform(new Vector2(1, 1)),
    viewProjection: uniform(new Matrix4()),
    passes: [],
    width: 0,
    height: 0,
  };
}

/** Sizes `occlusion` for a canvas of `width` by `height` pixels; whether its buffer was made again (the cull reading it must be). */
function resizeOcclusion(occlusion: Occlusion, width: number, height: number): boolean {
  width = Math.max(1, width);
  height = Math.max(1, height);
  if (occlusion.width === width && occlusion.height === height) return false;
  occlusion.width = width;
  occlusion.height = height;
  // Made again rather than resized: a target of no color textures leaves its depth texture's size as it was.
  occlusion.target.dispose();
  occlusion.depth.dispose();
  occlusion.depth = new DepthTexture(width, height);
  occlusion.target = new RenderTarget(width, height, { depthTexture: occlusion.depth, count: 0 });
  const levels: { offset: number; width: number; height: number }[] = [];
  let size = 0;
  let levelWidth = width;
  let levelHeight = height;
  do {
    levelWidth = Math.ceil(levelWidth / 2);
    levelHeight = Math.ceil(levelHeight / 2);
    levels.push({ offset: size, width: levelWidth, height: levelHeight });
    size += levelWidth * levelHeight;
  } while ((levelWidth > 1 || levelHeight > 1) && levels.length < PYRAMID_LEVELS);
  const grown = size > occlusion.pyramid.array.length;
  if (grown) {
    occlusion.pyramid.dispose();
    occlusion.pyramid = new StorageBufferAttribute(new Float32Array(Math.ceil(size * 1.5)), 1);
  }
  levels.forEach((level, i) => occlusion.levelData[i]!.set(level.offset, level.width, level.height, 0));
  for (let i = levels.length; i < PYRAMID_LEVELS; i++) occlusion.levelData[i]!.copy(occlusion.levelData[levels.length - 1]!);
  occlusion.levelCount.value = levels.length;
  (occlusion.size.value as Vector2).set(width, height);
  for (const pass of occlusion.passes) pass.dispose();
  const write: Node = storage(occlusion.pyramid, "float", occlusion.pyramid.array.length);
  // Texels past an odd edge take the edge's: the pyramid stays conservative.
  occlusion.passes = levels.map((level, i) => {
    const source = i > 0 ? levels[i - 1]! : { offset: 0, width, height };
    return Fn(() => {
      If(instanceIndex.lessThan(uint(level.width * level.height)), () => {
        const x = instanceIndex.mod(uint(level.width));
        const y = instanceIndex.div(uint(level.width));
        const deepest = float(0).toVar();
        for (let j = 0; j < 4; j++) {
          const sx: Node = min(x.mul(2).add(j & 1) as Node, uint(source.width - 1) as Node);
          const sy: Node = min(y.mul(2).add(j >> 1) as Node, uint(source.height - 1) as Node);
          const value = i === 0 ? (textureLoad(occlusion.depth, ivec2(sx, sy)) as Node).x : write.element(sy.mul(source.width).add(sx).add(source.offset));
          deepest.assign(max(deepest, value));
        }
        write.element(instanceIndex.add(level.offset)).assign(deepest);
      });
    })().compute(level.width * level.height, [64]);
  });
  return grown;
}

function disposeOcclusion(occlusion: Occlusion): void {
  for (const pass of occlusion.passes) pass.dispose();
  occlusion.passes = [];
  occlusion.target.dispose();
  occlusion.depth.dispose();
  occlusion.pyramid.dispose();
  // Made again at the next frame's size, and the cull with it.
  occlusion.pyramid = new StorageBufferAttribute(new Float32Array(1), 1);
  occlusion.width = 0;
  occlusion.height = 0;
}

/**
 * Whether a sphere (world space) is behind the pyramid's depth: the box
 * about it projected, its nearest depth against the largest of the 2x2
 * texels of the level whose texels its rectangle spans (three.js's
 * `MeshletDrawingPipeline` tests the same way). A sphere crossing the near
 * plane is never hidden.
 */
function occlusionTest(pyramid: Node, occlusion: Occlusion): (center: Node, radius: Node) => Node {
  const { levels, levelCount, size, viewProjection } = occlusion;
  return (center: Node, radius: Node): Node => {
    const minX = float(1e9).toVar();
    const minY = float(1e9).toVar();
    const minZ = float(1e9).toVar();
    const maxX = float(-1e9).toVar();
    const maxY = float(-1e9).toVar();
    const behind = bool(false).toVar();
    for (let i = 0; i < 8; i++) {
      const corner = vec3(i & 1 ? 1 : -1, i & 2 ? 1 : -1, i & 4 ? 1 : -1).mul(radius).add(center);
      const clip = viewProjection.mul(vec4(corner, 1)).toVar();
      If(clip.w.lessThanEqual(0), () => {
        behind.assign(true);
      }).Else(() => {
        const ndc = clip.xyz.div(clip.w);
        minX.assign(min(minX, ndc.x));
        minY.assign(min(minY, ndc.y));
        minZ.assign(min(minZ, ndc.z));
        maxX.assign(max(maxX, ndc.x));
        maxY.assign(max(maxY, ndc.y));
      });
    }
    const hidden = bool(false).toVar();
    If(behind.not(), () => {
      const width = size.x;
      const height = size.y;
      const pixel = (value: Node, extent: Node): Node => int(clamp(value.mul(extent), 0, extent.sub(1)) as Node);
      const x0 = pixel(minX.mul(0.5).add(0.5), width);
      const x1 = pixel(maxX.mul(0.5).add(0.5), width);
      const y0 = pixel(float(0.5).sub(maxY.mul(0.5)), height);
      const y1 = pixel(float(0.5).sub(minY.mul(0.5)), height);
      // The level whose texels cover 2^(level + 1) pixels: the rectangle overlaps 2x2 texels at most.
      const span: Node = (max(x1.sub(x0), y1.sub(y0)) as Node).add(1);
      const level: Node = clamp(int(ceil(log2(float(span)) as Node) as Node).sub(1) as Node, int(0) as Node, int(levelCount).sub(1) as Node);
      const shift = uint(level.add(1));
      const data = levels.element(level);
      const offset = uint(data.x);
      const levelWidth = uint(data.y);
      const tx0 = uint(x0).shiftRight(shift);
      const tx1 = uint(x1).shiftRight(shift);
      const ty0 = uint(y0).shiftRight(shift);
      const ty1 = uint(y1).shiftRight(shift);
      const texel = (tx: Node, ty: Node): Node => pyramid.element(offset.add(ty.mul(levelWidth)).add(tx));
      const deepest = max(max(texel(tx0, ty0), texel(tx1, ty0)), max(texel(tx0, ty1), texel(tx1, ty1)));
      hidden.assign(minZ.greaterThan(deepest));
    });
    return hidden;
  };
}
