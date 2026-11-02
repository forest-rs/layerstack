// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A point instancer's instances as columns, for its inspector view: each
// authored array read once as a typed array (`positions`, `orientations`,
// `scales`, `protoIndices`, `ids`), so a table of half a million rows
// formats only the rows it shows and sorts an index permutation, never an
// object per instance. Also what the view summarizes: instances per
// prototype, the mask (`invisibleIds`, `inactiveIds`), the positions' and
// scales' ranges, and one instance's transform and box, as
// `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` composes it
// (scale, then orientation, then translation, in USD's row-vector order;
// velocities are not extrapolated here).
import type { Stage } from "@forest-rs/layerstack";
import { fromHalf } from "@forest-rs/layerstack_stagehand";
import { releaseValue } from "./values.ts";

/** A point instancer's authored instance arrays at one time; a missing array is `null`. */
export interface InstancerColumns {
  /** Instances: the length of `protoIndices`. */
  readonly count: number;
  /** The `prototypes` targets, which prototype indices address. */
  readonly prototypes: readonly string[];
  readonly protoIndices: Int32Array;
  /** `ids`, when authored; an instance's id is its index without them. */
  readonly ids: BigInt64Array | null;
  /** Three per instance. */
  readonly positions: Float32Array | null;
  /** Four per instance, `[i, j, k, r]` (`orientations` or `orientationsf`). */
  readonly orientations: Float32Array | null;
  /** Three per instance. */
  readonly scales: Float32Array | null;
  /** `invisibleIds` at the time. */
  readonly invisibleIds: readonly bigint[];
  /** The `inactiveIds` metadata's ids. */
  readonly inactiveIds: readonly bigint[];
  /** One byte per instance, 1 drawn ({@link Stage.instanceMask}); empty when every instance is drawn. */
  readonly mask: Uint8Array;
}

/** An attribute's array at `time` copied as `f32` numbers (half floats decoded); `null` when it has none. */
function floats(stage: Stage, path: string, time: number | undefined): Float32Array | null {
  const array = stage.attribute(path)?.array(time) ?? null;
  if (array === null) return null;
  try {
    const view = array.view();
    if (array.scalar === "f16") {
      const bits = view as Uint16Array;
      const out = new Float32Array(bits.length);
      for (let at = 0; at < bits.length; at++) out[at] = fromHalf(bits[at]!);
      return out;
    }
    return Float32Array.from(view as ArrayLike<number>);
  } finally {
    array.release();
  }
}

function ints(stage: Stage, path: string, time: number | undefined): Int32Array | null {
  const array = stage.attribute(path)?.array(time) ?? null;
  if (array === null) return null;
  try {
    return Int32Array.from(array.view() as ArrayLike<number>);
  } finally {
    array.release();
  }
}

function int64s(stage: Stage, path: string, time: number | undefined): BigInt64Array | null {
  const array = stage.attribute(path)?.array(time) ?? null;
  if (array === null) return null;
  try {
    const view = array.view();
    return view instanceof BigInt64Array ? view.slice() : BigInt64Array.from(view as ArrayLike<number>, (id) => BigInt(id));
  } finally {
    array.release();
  }
}

/** The ids of an `int64` list op metadata field, as the stage reads it (its items). */
function listIds(stage: Stage, path: string, key: string): bigint[] {
  const value = stage.prim(path)?.metadata(key) ?? null;
  try {
    if (value?.kind === "list") return value.value.flatMap((item) => (item?.kind === "scalar" && (typeof item.value === "bigint" || typeof item.value === "number") ? [BigInt(item.value)] : []));
    if (value?.kind === "array") return Array.from(value.array.view() as ArrayLike<number | bigint>, (id) => BigInt(id));
    return [];
  } finally {
    releaseValue(value);
  }
}

/** The instancer at `path`'s columns at `time`. */
export function readColumns(stage: Stage, path: string, time: number | undefined): InstancerColumns {
  const protoIndices = ints(stage, `${path}.protoIndices`, time) ?? new Int32Array();
  const orientations = floats(stage, `${path}.orientations`, time) ?? floats(stage, `${path}.orientationsf`, time);
  const invisible = int64s(stage, `${path}.invisibleIds`, time);
  return {
    count: protoIndices.length,
    prototypes: stage.relationship(`${path}.prototypes`)?.targets() ?? [],
    protoIndices,
    ids: int64s(stage, `${path}.ids`, time),
    positions: floats(stage, `${path}.positions`, time),
    orientations,
    scales: floats(stage, `${path}.scales`, time),
    invisibleIds: invisible === null ? [] : Array.from(invisible),
    inactiveIds: listIds(stage, path, "inactiveIds"),
    mask: stage.instanceMask(path, time === undefined ? {} : { time }),
  };
}

/** Instances per prototype index (`counts[i]` for index `i`), and how many name no prototype. */
export function histogram(protoIndices: ArrayLike<number>, prototypes: number): { readonly counts: Uint32Array; readonly outOfRange: number } {
  const counts = new Uint32Array(prototypes);
  let outOfRange = 0;
  for (let at = 0; at < protoIndices.length; at++) {
    const index = protoIndices[at]!;
    if (index >= 0 && index < prototypes) counts[index]! += 1;
    else outOfRange += 1;
  }
  return { counts, outOfRange };
}

/** An instance's id: its `ids` entry, or its index. */
export function instanceId(columns: Pick<InstancerColumns, "ids">, index: number): bigint {
  return columns.ids?.[index] ?? BigInt(index);
}

/** The index of the instance with `id`; -1 when none has it. */
export function indexOfId(columns: Pick<InstancerColumns, "ids" | "count">, id: bigint): number {
  if (columns.ids === null) return id >= 0n && id < BigInt(columns.count) ? Number(id) : -1;
  return columns.ids.indexOf(id);
}

/** A column a table sorts by. */
export type InstanceColumn = "index" | "id" | "prototype" | "x" | "y" | "z" | "scale";

/**
 * The order of `columns`' instances by `by` (ties by index), descending
 * with `descending`: an index permutation, one `Uint32Array`. A number
 * column is sorted by a stable radix sort of its keys as `f32` (two passes
 * of 16 bits; no comparisons), so half a million rows sort in a few
 * milliseconds; ids, which `f32` would round, by comparison.
 */
export function sortOrder(columns: InstancerColumns, by: InstanceColumn, descending: boolean): Uint32Array {
  const count = columns.count;
  const order = new Uint32Array(count);
  for (let at = 0; at < count; at++) order[at] = at;
  if (by === "index") return descending ? order.reverse() : order;
  if (by === "id") {
    if (columns.ids === null) return descending ? order.reverse() : order;
    const ids = columns.ids;
    const sign = descending ? -1 : 1;
    return order.sort((a, b) => sign * (ids[a]! < ids[b]! ? -1 : ids[a]! > ids[b]! ? 1 : 0) || a - b);
  }
  const keys = new Float32Array(count);
  for (let at = 0; at < count; at++) {
    const key =
      by === "prototype"
        ? columns.protoIndices[at]!
        : by === "scale"
          ? columns.scales === null
            ? 1
            : Math.cbrt(Math.abs(columns.scales[at * 3]! * columns.scales[at * 3 + 1]! * columns.scales[at * 3 + 2]!))
          : (columns.positions?.[at * 3 + "xyz".indexOf(by)] ?? 0);
    // `+ 0` makes -0 a 0, as a comparison sees it.
    keys[at] = (descending ? -key : key) + 0;
  }
  return radixOrder(keys, order);
}

/** `order` (ascending indices) stably sorted by `keys`: their `f32` bits made unsigned-ordered, two 16-bit passes. */
function radixOrder(keys: Float32Array, order: Uint32Array): Uint32Array {
  const bits = new Uint32Array(keys.buffer, keys.byteOffset, keys.length);
  // Negative floats sort reversed and below positives: flip all their bits, or the sign of the rest. NaN sorts last.
  for (let at = 0; at < bits.length; at++) {
    const value = bits[at]!;
    bits[at] = Number.isNaN(keys[at]) ? 0xffffffff : value & 0x80000000 ? ~value >>> 0 : (value | 0x80000000) >>> 0;
  }
  let from: Uint32Array = order;
  let to: Uint32Array = new Uint32Array(order.length);
  const counts = new Uint32Array(65536);
  for (const shift of [0, 16]) {
    counts.fill(0);
    for (let at = 0; at < from.length; at++) counts[(bits[from[at]!]! >>> shift) & 0xffff]! += 1;
    let total = 0;
    for (let bucket = 0; bucket < 65536; bucket++) {
      const here = counts[bucket]!;
      counts[bucket] = total;
      total += here;
    }
    for (let at = 0; at < from.length; at++) {
      const index = from[at]!;
      to[counts[(bits[index]! >>> shift) & 0xffff]!++] = index;
    }
    [from, to] = [to, from];
  }
  return from;
}

/** The range and mean of each of `values`' `components` (positions, scales); `null` for none. */
export function ranges(values: Float32Array | null, components: number): { readonly min: number[]; readonly max: number[]; readonly mean: number[] } | null {
  if (values === null || values.length < components) return null;
  const min = new Array<number>(components).fill(Infinity);
  const max = new Array<number>(components).fill(-Infinity);
  const sum = new Array<number>(components).fill(0);
  const count = Math.floor(values.length / components);
  for (let at = 0; at < count; at++) {
    for (let c = 0; c < components; c++) {
      const value = values[at * components + c]!;
      if (value < min[c]!) min[c] = value;
      if (value > max[c]!) max[c] = value;
      sum[c]! += value;
    }
  }
  return { min, max, mean: sum.map((total) => total / count) };
}

/** `ids` with `id` added, or removed when it holds it: a toggled `invisibleIds` or `inactiveIds`. */
export function toggled(ids: readonly bigint[], id: bigint): bigint[] {
  return ids.includes(id) ? ids.filter((other) => other !== id) : [...ids, id];
}

/**
 * Instance `index`'s transform in the instancer's space, 16 numbers row by
 * row (USD's row vectors: `p' = p · M`): `scale · rotate(orientation) ·
 * translate(position)`.
 */
export function instanceMatrix(columns: InstancerColumns, index: number): Float64Array {
  const s = columns.scales;
  const [sx, sy, sz] = s === null ? [1, 1, 1] : [s[index * 3]!, s[index * 3 + 1]!, s[index * 3 + 2]!];
  const q = columns.orientations;
  let [i, j, k, r] = q === null ? [0, 0, 0, 1] : [q[index * 4]!, q[index * 4 + 1]!, q[index * 4 + 2]!, q[index * 4 + 3]!];
  const length = Math.hypot(i, j, k, r) || 1;
  i /= length;
  j /= length;
  k /= length;
  r /= length;
  // The rotation matrix of a unit quaternion for row vectors (GfMatrix4d::SetRotate).
  const rotation = [
    1 - 2 * (j * j + k * k),
    2 * (i * j + k * r),
    2 * (i * k - j * r),
    2 * (i * j - k * r),
    1 - 2 * (i * i + k * k),
    2 * (j * k + i * r),
    2 * (i * k + j * r),
    2 * (j * k - i * r),
    1 - 2 * (i * i + j * j),
  ];
  const p = columns.positions;
  const [tx, ty, tz] = p === null ? [0, 0, 0] : [p[index * 3]!, p[index * 3 + 1]!, p[index * 3 + 2]!];
  const scale = [sx, sy, sz];
  const m = new Float64Array(16);
  for (let row = 0; row < 3; row++) for (let col = 0; col < 3; col++) m[row * 4 + col] = scale[row]! * rotation[row * 3 + col]!;
  m[12] = tx;
  m[13] = ty;
  m[14] = tz;
  m[15] = 1;
  return m;
}

/** `a · b` of row-major 4×4 matrices. */
export function multiply(a: ArrayLike<number>, b: ArrayLike<number>): Float64Array {
  const out = new Float64Array(16);
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 4; col++) {
      let sum = 0;
      for (let at = 0; at < 4; at++) sum += a[row * 4 + at]! * b[at * 4 + col]!;
      out[row * 4 + col] = sum;
    }
  }
  return out;
}

/** The eight corners of the box `[minX, minY, minZ, maxX, maxY, maxZ]` through `matrix` (row vectors), 24 numbers. */
export function boxCorners(box: ArrayLike<number>, matrix: ArrayLike<number>): Float64Array {
  const out = new Float64Array(24);
  for (let corner = 0; corner < 8; corner++) {
    const x = corner & 1 ? box[3]! : box[0]!;
    const y = corner & 2 ? box[4]! : box[1]!;
    const z = corner & 4 ? box[5]! : box[2]!;
    for (let c = 0; c < 3; c++) out[corner * 3 + c] = x * matrix[c]! + y * matrix[4 + c]! + z * matrix[8 + c]! + matrix[12 + c]!;
  }
  return out;
}
