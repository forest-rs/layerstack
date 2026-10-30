// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// `SkinningQuery`: a geometry prim's skinning binding, over the native
// module's retained skinning evaluator.
import { ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import type { Stage } from "./stage.js";
import type { ArrayData, SkinningMethod, SkinningTransforms } from "./types.js";

type Row = readonly unknown[];

/**
 * A geometry prim's skinning binding: the skeleton that deforms it and
 * how its joint influences are laid out, as OpenUSD's
 * `UsdSkelSkinningQuery` describes it ({@link Stage.skinning}). The fields
 * are read when it was returned; the methods read the stage at `time`
 * (a time code; the default time without one).
 *
 * Each method returns typed arrays over the module's memory
 * ({@link ArrayData.view}, no copy). The stage keeps a skinning cache that
 * hears its edits, so calling these every frame recomputes only what
 * changes with time.
 *
 * Two ways to skin: on the GPU, upload {@link SkinningQuery.interleavedInfluences}
 * (or {@link SkinningQuery.influences}) once and
 * {@link SkinningQuery.skinningTransforms} each frame; on the CPU, read
 * {@link SkinningQuery.deformedPoints} and
 * {@link SkinningQuery.deformedNormals}, or skin your own arrays with
 * {@link Layerstack.skinPoints}. Points come out in skeleton space: apply
 * the skeleton's world transform after.
 */
export class SkinningQuery {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** The geometry's path. */
  readonly path: string;
  /** The bound skeleton's path. */
  readonly skeleton: string;
  /** The skeleton's joints, in skeleton order. */
  readonly joints: readonly string[];
  /** For each influence index, the skeleton joint it names (`-1` for none); `null` when the binding's joint order is the skeleton's. */
  readonly jointMapping: Int32Array | null;
  /** Influences per point, or for the whole prim when {@link SkinningQuery.rigid}. */
  readonly elementSize: number;
  /** `constant` (one block moves the whole prim) or `vertex` (a block per point). */
  readonly interpolation: "constant" | "vertex";
  /** Whether one block of influences moves the whole prim rigidly (`UsdSkelSkinningQuery::IsRigidlyDeformed`). */
  readonly rigid: boolean;
  /** The skinning method. */
  readonly method: SkinningMethod;
  /** The skeleton's animation source; `null` without one (the rest pose). */
  readonly animation: string | null;
  /** Whether the geometry binds blend shapes, which {@link SkinningQuery.deformedPoints} applies before skinning. */
  readonly blendShapes: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [skeleton, joints, mapping, elementSize, interpolation, method, animation, blendShapes] = row;
    this.stage = stage;
    this.path = path;
    this.skeleton = skeleton as string;
    this.joints = Object.freeze([...(joints as string[])]);
    this.jointMapping = mapping === null ? null : Int32Array.from(mapping as number[]);
    this.elementSize = elementSize as number;
    this.interpolation = interpolation as SkinningQuery["interpolation"];
    this.rigid = this.interpolation === "constant";
    this.method = method as SkinningMethod;
    this.animation = animation as string | null;
    this.blendShapes = blendShapes as boolean;
    Object.freeze(this);
  }

  /**
   * Its joint influences at `time`, flattened: an `int[]` of joint indices
   * and a `float[]` of weights, {@link SkinningQuery.elementSize} per point
   * (`UsdSkelSkinningQuery::ComputeJointInfluences`). Release them when done.
   *
   * Throws `skel.influences` when there are not as many weights as indices
   * or they are not whole blocks, `skel.primvar` when they do not flatten.
   */
  influences(time?: number): { readonly indices: ArrayData; readonly weights: ArrayData } {
    const [indices, weights] = this.#arrays("influences", (native) => native.skinningInfluences(this.path, time, false));
    return { indices: indices as ArrayData, weights: weights as ArrayData };
  }

  /** Its joint influences at `time` as `[index, weight]` pairs, a `float2[]` (`UsdSkelInterleaveInfluences`): the layout GPU skinning reads. Throws as {@link SkinningQuery.influences}. */
  interleavedInfluences(time?: number): ArrayData {
    const [pairs] = this.#arrays("interleavedInfluences", (native) =>
      native.skinningInfluences(this.path, time, true),
    );
    return pairs as ArrayData;
  }

  /**
   * Its joint palette at `time`: per joint, in the order its influence
   * indices address, the inverse bind transform times the animated
   * skeleton-space transform (`UsdSkelSkeletonQuery::ComputeSkinningTransforms`),
   * with the geometry's bind transform and the skinning method.
   *
   * Throws a `skel.*` code when the skeleton cannot be posed
   * (`skel.singular` for a bind transform without an inverse).
   */
  skinningTransforms(time?: number): SkinningTransforms {
    const [transforms, bind, method] = this.stage.native("skinningTransforms", (native) =>
      native.skinningTransforms(this.path, time),
    ) as [NativeArrayHandle, number[], SkinningMethod];
    return {
      transforms: new ArrayHandle(this.stage.layerstack, transforms),
      geomBindTransform: Float64Array.from(bind),
      method,
    };
  }

  /**
   * Its points at `time`, deformed by its blend shapes and then skinned,
   * in skeleton space: a `point3f[]`. Release it when done.
   *
   * Throws `skel.deformation` when the influences do not fit the points
   * (one block per point) or name a joint past the skeleton's, `skel.input`
   * for a prim without points.
   */
  deformedPoints(time?: number): ArrayData {
    const handle = this.stage.native("deformedPoints", (native) => native.deformedPoints(this.path, time));
    return new ArrayHandle(this.stage.layerstack, handle as NativeArrayHandle);
  }

  /** Its normals at `time`, deformed and skinned as {@link SkinningQuery.deformedPoints} deforms points, normalized: a `normal3f[]`. Throws as that does, and `skel.input` for a prim without vertex, varying or face-varying normals. */
  deformedNormals(time?: number): ArrayData {
    const handle = this.stage.native("deformedNormals", (native) => native.deformedNormals(this.path, time));
    return new ArrayHandle(this.stage.layerstack, handle as NativeArrayHandle);
  }

  #arrays(operation: string, f: (native: Parameters<Parameters<Stage["native"]>[1]>[0]) => unknown): ArrayData[] {
    const handles = this.stage.native(operation, f) as NativeArrayHandle[];
    return handles.map((handle) => new ArrayHandle(this.stage.layerstack, handle));
  }
}
