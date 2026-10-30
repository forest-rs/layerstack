// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The geometry a renderer reads: `Xformable` and `Primvar`, over the
// native module's transform, primvar, bounds and instancer reads, and the
// decoders of their records.
import { ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import { decodeValue } from "./stage.js";
import type { Stage } from "./stage.js";
import type {
  ArrayData,
  BoundingBoxes,
  LocalTransform,
  PointInstances,
  PurposeInfo,
  Value,
  XformOp,
  XformProblem,
} from "./types.js";

type Row = readonly unknown[];

/**
 * A prim's transform ops, as read when it was returned
 * ({@link Stage.xformable}): what OpenUSD's `UsdGeomXformable` reports of
 * its `xformOpOrder`. The transform itself comes from
 * {@link Stage.localTransform} and {@link Stage.worldTransforms}.
 */
export class Xformable {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** The prim's path. */
  readonly path: string;
  /** Whether `xformOpOrder` resets the transform stack: the prim ignores its ancestors' transforms (`UsdGeomXformable::GetResetXformStack`). */
  readonly resetsXformStack: boolean;
  /** The ops after any reset, first to last: the last applies first (`GetOrderedXformOps`). */
  readonly ops: readonly XformOp[];
  /** Whether an op might vary over time: two or more samples, or a spline (`TransformMightBeTimeVarying`). */
  readonly transformMightBeTimeVarying: boolean;
  /** The time samples of the ops' attributes, ascending and without repeats (`UsdGeomXformable::GetTimeSamples`). */
  readonly transformTimeSamples: readonly number[];

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [resets, ops, varying, samples] = row;
    this.stage = stage;
    this.path = path;
    this.resetsXformStack = resets as boolean;
    this.ops = Object.freeze(
      (ops as [string, string, string | null, boolean][]).map(([name, attribute, type, inverse]) =>
        Object.freeze({ name, attribute, type, inverse }),
      ),
    );
    this.transformMightBeTimeVarying = varying as boolean;
    this.transformTimeSamples = Object.freeze([...(samples as Float64Array)]);
    Object.freeze(this);
  }
}

/**
 * A primvar: a `primvars:` attribute and how it maps onto its geometry,
 * as read when it was returned ({@link Stage.primvars}). Its methods read
 * the stage now. OpenUSD: `UsdGeomPrimvar`.
 */
export class Primvar {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** The prim whose attribute it is: an ancestor's for an inherited primvar. */
  readonly prim: string;
  /** Its name without `primvars:` (`displayColor`, `st`). */
  readonly name: string;
  /** Its attribute's property path (`/World/Rock.primvars:displayColor`). */
  readonly path: string;
  /** Its attribute's value type name (`color3f[]`). */
  readonly typeName: string;
  /** `constant`, `uniform`, `vertex`, `varying` or `faceVarying`; `constant` when unauthored. */
  readonly interpolation: string;
  /** Values per element; 1 when unauthored. */
  readonly elementSize: number;
  /** Whether it has `:indices`, so {@link Primvar.flattened} expands it. */
  readonly indexed: boolean;
  /** Whether it is an id-target string primvar. */
  readonly idTarget: boolean;
  /** Whether its value or indices might vary over time. */
  readonly mightBeTimeVarying: boolean;

  /** @internal */
  constructor(stage: Stage, row: Row) {
    const [prim, name, typeName, interpolation, elementSize, indexed, idTarget, varying] = row;
    this.stage = stage;
    this.prim = prim as string;
    this.name = name as string;
    this.path = `${this.prim}.primvars:${this.name}`;
    this.typeName = typeName as string;
    this.interpolation = interpolation as string;
    this.elementSize = elementSize as number;
    this.indexed = indexed as boolean;
    this.idTarget = idTarget as boolean;
    this.mightBeTimeVarying = varying as boolean;
    Object.freeze(this);
  }

  /** Its value at `time` as authored, without its indices; `null` when it has none (`UsdGeomPrimvar::Get`). */
  get(time?: number): Value | null {
    return this.#value("get", time, false);
  }

  /**
   * Its value at `time` expanded through its indices, `elementSize` values
   * per index; an unindexed value as it is (`UsdGeomPrimvar::ComputeFlattened`).
   * A numeric array's {@link ArrayData.view} is a typed array over the
   * module's memory.
   *
   * Throws `primvar.indices` for an indexed primvar without readable
   * indices, `primvar.index` for an index past the values.
   */
  flattened(time?: number): Value | null {
    return this.#value("flattened", time, true);
  }

  /** Its `:indices` at `time` as an `int[]` array, or `null` when it is not indexed. Release it when done. */
  indices(time?: number): ArrayData | null {
    const handle = this.stage.native("primvarIndices", (native) =>
      native.primvarIndices(this.prim, this.name, time),
    ) as NativeArrayHandle | null;
    return handle === null ? null : new ArrayHandle(this.stage.layerstack, handle);
  }

  #value(operation: string, time: number | undefined, flattened: boolean): Value | null {
    const record = this.stage.native(operation, (native) =>
      native.primvarValue(this.prim, this.name, time, flattened),
    );
    return decodeValue(this.stage.layerstack, record);
  }
}

/** @internal A local transform record. */
export function decodeLocalTransform(row: Row): LocalTransform {
  const [matrix, resets, problems] = row;
  return Object.freeze({
    matrix: matrix as Float64Array,
    resetsXformStack: resets as boolean,
    problems: Object.freeze(
      (problems as [string, XformProblem["kind"]][]).map(([op, kind]) => Object.freeze({ op, kind })),
    ),
  });
}

/** @internal A purpose record, or `null`. */
export function decodePurpose(row: unknown): PurposeInfo | null {
  if (row === null) return null;
  const [purpose, inheritable, authoredOn] = row as [string, boolean, string | null];
  return Object.freeze({ purpose, inheritable, authoredOn });
}

/** @internal A bounds record. */
export function decodeBounds(row: Row): BoundingBoxes {
  const [ranges, matrices, aligned] = row as [Float64Array, Float64Array, Float64Array];
  return Object.freeze({ count: ranges.length / 6, ranges, matrices, alignedRanges: aligned });
}

/** @internal An instance-transforms record. */
export function decodeInstances(row: Row): PointInstances {
  const [prototypes, matrices, prototypeIndices, ids, indices] = row as [
    string[],
    Float64Array,
    Int32Array,
    BigInt64Array,
    Int32Array,
  ];
  return Object.freeze({
    count: prototypeIndices.length,
    prototypes: Object.freeze([...prototypes]),
    matrices,
    prototypeIndices,
    ids,
    indices,
  });
}
