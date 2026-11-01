// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Joint influences in a mesh buffer's vertex order, for GPU skinning: a
// `SkinningQuery`'s per-point influences gathered through the buffer's
// `vertexPoints`, reduced to the slots a renderer has per vertex.
import type { SkinningQuery } from "@forest-rs/layerstack";
import { StagehandError } from "./index.js";

/**
 * A skinned mesh's joint influences in its buffer's vertex order, at most
 * {@link slots} per vertex ({@link gatherInfluences}): what a GPU skins
 * with, beside the joint palette `SkinningQuery.skinningTransforms` gives
 * each time (three.js: `skinIndex` and `skinWeight`).
 */
export interface VertexInfluences {
  /** Influences per vertex. */
  readonly slots: number;
  /** Each vertex's joints, {@link slots} each: indices into the binding's joint palette (`SkinningQuery.skinningTransforms`). */
  readonly joints: Uint16Array;
  /** Each vertex's weights, {@link slots} each, in step with {@link joints}; 0 in a slot it does not use. */
  readonly weights: Float32Array;
  /** The binding's influences per point (`elementSize`). */
  readonly elementSize: number;
  /** Whether one block moves the whole prim (`constant` interpolation), copied to every vertex. */
  readonly rigid: boolean;
  /**
   * Blocks with more nonzero weights than {@link slots}: points (or the
   * rigid prim's one block) skinned approximately. Each keeps its largest
   * weights, scaled so they sum to what the whole block summed to (as
   * `UsdSkelResizeInfluences` keeps normalized weights normalized); 0 when
   * every block fits and skinning is exact.
   */
  readonly reducedBlocks: number;
  /** The largest weight a reduced block dropped; 0 when none was. */
  readonly droppedWeight: number;
}

/**
 * The joint influences of the skinning binding `query` at `time`, gathered
 * into the vertex order of a mesh buffer whose `vertexPoints` is given
 * (`StagehandMesh.vertexPoints`), `slots` per vertex (4 by default, as
 * three.js skins).
 *
 * Vertex `v` takes the block of point `vertexPoints[v]`, or the prim's one
 * block when it is skinned rigidly (`constant` interpolation), as
 * `UsdSkelSkinningQuery::ComputeSkinnedPoints` reads them; weights are used
 * as authored. A block with more nonzero weights than `slots` is reduced
 * explicitly ({@link VertexInfluences.reducedBlocks}).
 *
 * Throws `argument.invalid` when a vertex names a point the binding has no
 * block for, or a weighted influence names a joint past the binding's; a
 * `skel.*` `LayerstackError` as `SkinningQuery.influences` does.
 */
export function gatherInfluences(
  query: SkinningQuery,
  vertexPoints: Uint32Array,
  options: { readonly slots?: number; readonly time?: number } = {},
): VertexInfluences {
  const slots = options.slots ?? 4;
  if (!Number.isInteger(slots) || slots < 1) {
    throw new StagehandError("argument.invalid", "gatherInfluences", "`slots` must be a positive integer");
  }
  const { indices: indexArray, weights: weightArray } = query.influences(options.time);
  let indices: Int32Array;
  let weights: Float32Array;
  try {
    indices = Int32Array.from(indexArray.copy() as ArrayLike<number>);
    weights = Float32Array.from(weightArray.copy() as ArrayLike<number>);
  } finally {
    indexArray.release();
    weightArray.release();
  }
  const size = query.elementSize;
  const blocks = size === 0 ? 0 : Math.floor(indices.length / size);
  const palette = query.jointMapping?.length ?? query.joints.length;
  // Each block reduced once, then copied to every vertex naming it.
  const blockJoints = new Uint16Array(blocks * slots);
  const blockWeights = new Float32Array(blocks * slots);
  let reducedBlocks = 0;
  let droppedWeight = 0;
  const order: number[] = [];
  for (let block = 0; block < blocks; block++) {
    const first = block * size;
    order.length = 0;
    let sum = 0;
    for (let k = 0; k < size; k++) {
      const weight = weights[first + k]!;
      if (weight === 0) continue;
      const joint = indices[first + k]!;
      if (joint < 0 || joint >= palette) {
        throw new StagehandError(
          "argument.invalid",
          "gatherInfluences",
          `influence ${first + k} of ${query.path} names joint ${joint} of ${palette}`,
        );
      }
      order.push(first + k);
      sum += weight;
    }
    let kept = order;
    let scale = 1;
    if (order.length > slots) {
      // The largest weights, earlier influences first among equals.
      kept = [...order].sort((a, b) => weights[b]! - weights[a]! || a - b).slice(0, slots);
      const keptSum = kept.reduce((total, at) => total + weights[at]!, 0);
      scale = keptSum === 0 ? 1 : sum / keptSum;
      reducedBlocks++;
      for (const at of order) if (!kept.includes(at)) droppedWeight = Math.max(droppedWeight, Math.abs(weights[at]!));
    }
    kept.forEach((at, slot) => {
      blockJoints[block * slots + slot] = indices[at]!;
      blockWeights[block * slots + slot] = weights[at]! * scale;
    });
  }
  const rigid = query.rigid;
  const vertices = vertexPoints.length;
  const joints = new Uint16Array(vertices * slots);
  const vertexWeights = new Float32Array(vertices * slots);
  for (let v = 0; v < vertices; v++) {
    const block = rigid ? 0 : vertexPoints[v]!;
    if (block >= blocks) {
      throw new StagehandError(
        "argument.invalid",
        "gatherInfluences",
        `vertex ${v} of ${query.path} names point ${block}, past its ${blocks} influence blocks`,
      );
    }
    joints.set(blockJoints.subarray(block * slots, (block + 1) * slots), v * slots);
    vertexWeights.set(blockWeights.subarray(block * slots, (block + 1) * slots), v * slots);
  }
  return { slots, joints, weights: vertexWeights, elementSize: size, rigid, reducedBlocks, droppedWeight };
}
