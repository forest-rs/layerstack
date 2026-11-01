// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The skinned draws: which of the draw list's meshes a skeleton deforms
// (OpenUSD's `UsdSkelSkinningQuery`, read through `stage.skinning`), and
// for each, what the viewport skins it with at the current time.
//
// The draw list's buffers hold a skinned mesh's rest points, in vertex
// order; stagehand's `vertexPoints` says which point each vertex came from,
// so the joint influences are gathered into that order once
// (`gatherInfluences`) and the GPU blends them with the joint palette
// (`skinningTransforms`) each frame, as UsdSkelImaging skins for Hydra: a
// time change costs each skinned draw its joint matrices, not its
// vertices. Two kinds of binding the palette cannot express are deformed
// on the CPU instead (`deformedPoints`, `deformedNormals`) and refilled
// each time, and reported: blend shapes, which offset points before
// skinning, and dual-quaternion skinning.
//
// Either way the skinned points are in the skeleton's space
// (`UsdSkelSkinningQuery::ComputeSkinnedPoints`), so the draw is placed by
// the skeleton's world transform, not by the mesh's own: UsdSkel ignores a
// skinned gprim's transform, as UsdSkelImaging draws it.
import type { SkinningQuery, Stage } from "@forest-rs/layerstack";
import type { DrawBuffers, DrawItem, DrawList, PrimvarInput, Stagehand, StagehandMesh, VertexInfluences } from "@forest-rs/layerstack_stagehand";
import { gatherInfluences, readMesh } from "@forest-rs/layerstack_stagehand";

/**
 * Influences a vertex keeps on the GPU: eight, as the viewport's skinning
 * shader reads them (three.js's `skinIndex` and `skinWeight`, then
 * `skinIndex1` and `skinWeight1`). A point weighting more joints keeps its
 * largest weights, and is counted ({@link SkinSummary.reducedBlocks}).
 */
export const SKIN_SLOTS = 8;

/** What a skinned draw is drawn with at the current time. */
export interface SkinDraw {
  /** The bound skeleton's path. */
  readonly skeleton: string;
  /**
   * `gpu`: {@link influences} and the {@link palette}, blended by the
   * vertex shader; `cpu`: {@link positions} and {@link normals} deformed
   * on the CPU, for the {@link reason} the GPU cannot.
   */
  readonly mode: "gpu" | "cpu";
  /** Why a `cpu` draw is: `blend shapes` or `dual quaternion`; `null` on the GPU. */
  readonly reason: string | null;
  /** The joint palette's size: joints in the binding's order. */
  readonly joints: number;
  /** Each vertex's joints and weights ({@link SKIN_SLOTS} each), for `gpu`. */
  readonly influences: VertexInfluences | null;
  /** The geometry's bind transform, 16 numbers row by row. */
  readonly bind: Float64Array;
  /** The joint palette at the time, 16 numbers per joint row by row (`skinningTransforms`), for `gpu`. */
  palette: Float64Array;
  /** The skeleton's local-to-world transform at the time: where the skinned points are placed. */
  world: Float64Array;
  /** The deformed positions and normals in vertex order, in the skeleton's space, for `cpu`. */
  positions: Float32Array | null;
  normals: Float32Array | null;
}

/** What skinning did for one update: the work units the stats show. */
export interface SkinWork {
  /** Draws posed on the GPU: their palettes read. */
  readonly gpuDraws: number;
  /** Joint matrices read for them. */
  readonly joints: number;
  /** Draws deformed on the CPU and refilled. */
  readonly cpuDraws: number;
  /** Points deformed on the CPU. */
  readonly cpuPoints: number;
  /** Vertices whose influences were gathered: draws bound anew. */
  readonly gathered: number;
}

/** The skinned draws' state, as of the last update. */
export interface SkinSummary {
  /** Skinned draws on the GPU, and on the CPU. */
  readonly gpu: number;
  readonly cpu: number;
  /** Points (or rigid prims) weighting more joints than {@link SKIN_SLOTS}: skinned with their largest weights only. */
  readonly reducedBlocks: number;
  /** The largest weight such a point dropped. */
  readonly droppedWeight: number;
  /** Skinned prims drawn unskinned, and why. */
  readonly failed: readonly string[];
}

/** One skinned item's binding: the query, and for the CPU its own plan of the mesh. */
interface Bound {
  readonly query: SkinningQuery;
  readonly draw: SkinDraw;
  /** The mesh planned as the draw list's buffer is, refilled with deformed points, for `cpu`. */
  readonly mesh: StagehandMesh | null;
  /**
   * The normals a CPU refill passes: `deformed`, the `normals` attribute
   * as UsdSkel deforms it (in {@link layout}'s layout); `primvar`,
   * `primvars:normals` as authored ({@link layout}), which UsdSkel does
   * not deform; `computed`, none (the plan computes them from the
   * deformed points).
   */
  readonly normals: "deformed" | "computed" | "primvar";
  readonly layout: PrimvarInput | null;
}

/** Whether `path` is `root` or beneath it. */
function under(path: string, root: string): boolean {
  return root === "/" || path === root || path.startsWith(`${root}/`);
}

/** Whether a change at `touched` may change what is skinned at `path`: one is the other or beneath it. */
function related(path: string, touched: string): boolean {
  return under(path, touched) || under(touched, path);
}

/** A typed array of an `ArrayData`'s numbers, released. */
function copied<T>(array: { copy(): unknown; release(): void }, make: (values: ArrayLike<number>) => T): T {
  try {
    return make(array.copy() as ArrayLike<number>);
  } finally {
    array.release();
  }
}

/**
 * The draw list's skinned draws, kept by item id: bound when an item is
 * added, its buffer rebuilt or refilled, or a change reaches its prim, its
 * skeleton or its animation; posed when the time changes.
 */
export class SkinningModel {
  readonly #stagehand: Stagehand;
  readonly #bound = new Map<number, Bound>();
  readonly #failed = new Map<number, string>();

  constructor(stagehand: Stagehand) {
    this.#stagehand = stagehand;
  }

  /** The skinned draw of item `id`, if it is skinned. */
  draw(id: number): SkinDraw | null {
    return this.#bound.get(id)?.draw ?? null;
  }

  /** The skinned draws in brief. */
  summary(): SkinSummary {
    let gpu = 0;
    let reducedBlocks = 0;
    let droppedWeight = 0;
    for (const { draw } of this.#bound.values()) {
      if (draw.mode !== "gpu") continue;
      gpu++;
      reducedBlocks += draw.influences!.reducedBlocks;
      droppedWeight = Math.max(droppedWeight, draw.influences!.droppedWeight);
    }
    return { gpu, cpu: this.#bound.size - gpu, reducedBlocks, droppedWeight, failed: [...this.#failed.values()].sort() };
  }

  /**
   * Brings the skinned draws up to `list` at `time`: items `reset`, newly
   * `added`, drawing a `rebuffered` buffer or a prim, skeleton or
   * animation under a `touched` path are bound again (and returned as
   * `rebound`: the viewport makes their objects again); every skinned item
   * is posed when `timeChanged`, else only those bound again (`posed`).
   * Items no longer listed are dropped.
   */
  sync(
    stage: Stage,
    list: DrawList,
    change: {
      readonly reset: boolean;
      readonly added: ReadonlySet<number>;
      readonly removed: ReadonlySet<number>;
      readonly rebuffered: ReadonlySet<number>;
      readonly touched: readonly string[];
      readonly timeChanged: boolean;
    },
    time: number | undefined,
  ): { rebound: Set<number>; posed: Set<number>; work: SkinWork } {
    if (change.reset) this.dispose();
    for (const id of change.removed) this.#drop(id);
    const rebound = new Set<number>();
    let gathered = 0;
    for (const item of list.items()) {
      const known = this.#bound.get(item.id);
      const reach = (path: string | null) => path !== null && change.touched.some((touched) => related(path, touched));
      const stale =
        change.reset ||
        change.added.has(item.id) ||
        change.rebuffered.has(item.buffer) ||
        reach(item.prim) ||
        (known !== undefined && (reach(known.draw.skeleton) || reach(known.query.animation)));
      if (!stale) continue;
      const wasSkinned = known !== undefined || this.#failed.has(item.id);
      this.#drop(item.id);
      const bound = this.#bind(stage, item, list.buffers(item.buffer)!, time);
      if (bound !== null) {
        this.#bound.set(item.id, bound);
        gathered += bound.draw.influences === null ? 0 : bound.draw.influences.joints.length / SKIN_SLOTS;
      }
      if (bound !== null || wasSkinned || this.#failed.has(item.id)) rebound.add(item.id);
    }
    const posed = new Set<number>();
    let gpuDraws = 0;
    let joints = 0;
    let cpuDraws = 0;
    let cpuPoints = 0;
    for (const [id, bound] of this.#bound) {
      if (!change.timeChanged && !rebound.has(id)) continue;
      try {
        const points = this.#pose(stage, bound, time);
        posed.add(id);
        if (bound.draw.mode === "gpu") {
          gpuDraws++;
          joints += bound.draw.joints;
        } else {
          cpuDraws++;
          cpuPoints += points;
        }
      } catch (error) {
        // Drawn unskinned from here on, and said so.
        this.#failed.set(id, `${bound.query.path}: ${error instanceof Error ? error.message : String(error)}`);
        this.#drop(id, true);
        rebound.add(id);
      }
    }
    return { rebound, posed, work: { gpuDraws, joints, cpuDraws, cpuPoints, gathered } };
  }

  /** Frees the CPU draws' plans. */
  dispose(): void {
    for (const id of [...this.#bound.keys()]) this.#drop(id);
    this.#failed.clear();
  }

  #drop(id: number, keepFailure = false): void {
    this.#bound.get(id)?.mesh?.dispose();
    this.#bound.delete(id);
    if (!keepFailure) this.#failed.delete(id);
  }

  /**
   * The skinning of `item`, a mesh drawn once outside any instance; `null`
   * when no skeleton binds it, or it cannot be skinned (instanced, or its
   * binding is invalid): the reason is kept for the stats, and it is drawn
   * at rest.
   */
  #bind(stage: Stage, item: DrawItem, buffers: DrawBuffers, time: number | undefined): Bound | null {
    const vertexPoints = buffers.mesh.vertexPoints;
    if (vertexPoints === null || buffers.curves !== null || buffers.points !== null) return null;
    let query: SkinningQuery | null = null;
    try {
      query = stage.skinning(item.prim);
      if (query === null) return null;
      // One draw for many instances shares one pose: not skinned yet.
      if (item.source.kind !== "gprim") throw new Error("a skinned mesh drawn instanced is drawn at rest");
      const reason = query.method === "dualQuaternion" ? "dual quaternion" : query.blendShapes ? "blend shapes" : null;
      const draw: SkinDraw = {
        skeleton: query.skeleton,
        mode: reason === null ? "gpu" : "cpu",
        reason,
        joints: query.jointMapping?.length ?? query.joints.length,
        influences: reason === null ? gatherInfluences(query, vertexPoints, { slots: SKIN_SLOTS, ...(time === undefined ? {} : { time }) }) : null,
        bind: new Float64Array(16),
        palette: new Float64Array(),
        world: new Float64Array(16),
        positions: null,
        normals: null,
      };
      if (reason === null) return { query, draw, mesh: null, normals: "computed", layout: null };
      // The CPU draw's own plan: the same mesh, primvars and texture
      // coordinate sets as the list's buffer, so the same vertices.
      const input = readMesh(stage, item.prim, { ...(time === undefined ? {} : { time }), uvSets: buffers.mesh.uvSets.map((set) => set.name) });
      if (input === null) return null;
      const mesh = this.#stagehand.buildMesh(input);
      if (mesh.positions.length !== buffers.mesh.positions.length) {
        mesh.dispose();
        throw new Error("its CPU plan does not split vertices as the draw list's buffer");
      }
      const layout = input.normals ?? null;
      const value = layout === null ? null : (stage.primvar(item.prim, "normals")?.get(time) ?? null);
      const primvar = value?.kind === "array";
      if (value?.kind === "array") value.array.release();
      return { query, draw, mesh, normals: layout === null ? "computed" : primvar ? "primvar" : "deformed", layout };
    } catch (error) {
      this.#failed.set(item.id, `${item.prim}: ${error instanceof Error ? error.message : String(error)}`);
      return null;
    }
  }

  /** Reads `bound`'s pose at `time` into its draw; the points deformed on the CPU (0 on the GPU). */
  #pose(stage: Stage, bound: Bound, time: number | undefined): number {
    const { query, draw } = bound;
    const at = time === undefined ? {} : { time };
    draw.world = stage.worldTransforms([draw.skeleton], at);
    const transforms = query.skinningTransforms(time);
    draw.bind.set(transforms.geomBindTransform);
    if (draw.mode === "gpu") {
      draw.palette = copied(transforms.transforms, (values) => Float64Array.from(values));
      return 0;
    }
    transforms.transforms.release();
    const mesh = bound.mesh!;
    const points = copied(query.deformedPoints(time), (values) => Float32Array.from(values));
    // `normals` UsdSkel deforms; `primvars:normals` it does not, so the
    // plan's own are kept; computed normals are computed again from the
    // deformed points.
    const normals =
      bound.normals === "deformed" && bound.layout !== null
        ? { ...bound.layout, values: copied(query.deformedNormals(time), (values) => Float32Array.from(values)) }
        : bound.layout;
    mesh.refill(points, normals);
    draw.positions = mesh.positions;
    draw.normals = mesh.normals;
    return points.length / 3;
  }
}
