// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Curves and points: reading them off a stage into typed arrays, the
// module's curves plans, and points' buffers (filled here: one entry per
// point, nothing to lay out).
import type { ArrayData, Stage, Value } from "@forest-rs/layerstack";
import type { CurvesBuilder, StagehandCurves as NativeCurves } from "../wasm/layerstack_stagehand.js";
import type { StagehandMesh } from "./index.js";
import type {
  ChannelState,
  CurveForm,
  CurvesInput,
  CurvesReport,
  Interpolation,
  PointBuffers,
  PointsInput,
  PointsReport,
  PrimvarInput,
} from "./types.js";

const INTERPOLATIONS = new Set<string>(["constant", "uniform", "varying", "vertex", "faceVarying"]);

/** The schemas drawn as curves, as `layerstack_stagehand`'s draw list matches them. */
export const CURVE_SCHEMAS = ["BasisCurves", "NurbsCurves", "HermiteCurves"] as const;

/** An array value's elements, copied and released; `null` (released) when not numeric of `components` each. */
function elements(value: Value | null, components: number): ArrayLike<number | bigint> | null {
  if (value?.kind !== "array") return null;
  const array: ArrayData = value.array;
  try {
    if (array.components !== components || array.scalar === "string") return null;
    return array.copy() as ArrayLike<number | bigint>;
  } finally {
    array.release();
  }
}

function float32(value: Value | null, components: number): Float32Array | null {
  const found = elements(value, components);
  return found === null ? null : found instanceof Float32Array ? found : Float32Array.from(found as ArrayLike<number>);
}

function float64(value: Value | null, components: number): Float64Array | null {
  const found = elements(value, components);
  return found === null ? null : found instanceof Float64Array ? found : Float64Array.from(found as ArrayLike<number>);
}

function int32(value: Value | null): Int32Array | null {
  const found = elements(value, 1);
  return found === null ? null : found instanceof Int32Array ? found : Int32Array.from(found as ArrayLike<number>);
}

function token(stage: Stage, path: string, fallback: string): string {
  const value = stage.attribute(path)?.get() ?? null;
  return value?.kind === "scalar" && typeof value.value === "string" ? value.value : fallback;
}

/** A primvar of `components` per element, its own or (with `inherited`) a constant ancestor's. */
function primvar(stage: Stage, path: string, name: string, components: number, time: number | undefined, inherited: boolean): PrimvarInput | null {
  const found = stage.primvar(path, name, { inherited });
  if (found === null || !INTERPOLATIONS.has(found.interpolation)) return null;
  const values = float32(found.get(time), components);
  if (values === null) return null;
  const indices = found.indexed ? found.indices(time) : null;
  let flat: Int32Array | null = null;
  if (indices !== null) {
    try {
      const copied = indices.copy();
      flat = copied instanceof Int32Array ? copied : Int32Array.from(copied as ArrayLike<number>);
    } finally {
      indices.release();
    }
  }
  return { values, indices: flat, interpolation: found.interpolation as Interpolation, elementSize: found.elementSize };
}

/** `primvars:<name>` when it has a value, else the attribute with its `interpolation` (`vertex` when unauthored). */
function interpolated(stage: Stage, path: string, name: string, components: number, time: number | undefined): PrimvarInput | null {
  const own = primvar(stage, path, name, components, time, false);
  if (own !== null) return own;
  const attribute = stage.attribute(`${path}.${name}`);
  const values = float32(attribute?.get(time) ?? null, components);
  if (values === null) return null;
  const metadata = attribute?.metadata("interpolation") ?? null;
  const token = metadata?.kind === "scalar" ? String(metadata.value) : "vertex";
  return { values, interpolation: (INTERPOLATIONS.has(token) ? token : "vertex") as Interpolation };
}

/** The curve schema `typeName` or its bases make `path`, as {@link CURVE_SCHEMAS}; `null` for anything else. */
function curveSchema(stage: Stage, path: string): (typeof CURVE_SCHEMAS)[number] | null {
  const prim = stage.prim(path);
  if (prim === null) return null;
  return CURVE_SCHEMAS.find((schema) => prim.typeName === schema) ?? CURVE_SCHEMAS.find((schema) => prim.isA(schema)) ?? null;
}

/**
 * The curves at `path` at `time` as a {@link CurvesInput}, as
 * `layerstack_stagehand::StageCurves` reads them: `curveVertexCounts`,
 * `points`; `type`, `basis` and `wrap` (schema fallbacks included);
 * `order`, `knots`, `ranges`, `pointWeights`; `tangents`; widths and
 * normals (the primvar when it has a value, else the attribute at its
 * `interpolation`, `vertex` when unauthored); `primvars:displayColor` and
 * `primvars:displayOpacity`, inherited when constant. `null` when the
 * prim is not a `BasisCurves`, `NurbsCurves` or `HermiteCurves`.
 */
export function readCurves(stage: Stage, path: string, options: { readonly time?: number } = {}): CurvesInput | null {
  const { time } = options;
  const schema = curveSchema(stage, path);
  if (schema === null) return null;
  const get = (name: string) => stage.attribute(`${path}.${name}`)?.get(time) ?? null;
  let form: CurveForm;
  if (schema === "BasisCurves") {
    form = {
      kind: "basis",
      type: token(stage, `${path}.type`, "cubic"),
      basis: token(stage, `${path}.basis`, "bezier"),
      wrap: token(stage, `${path}.wrap`, "nonperiodic"),
    };
  } else if (schema === "NurbsCurves") {
    form = {
      kind: "nurbs",
      order: int32(get("order")) ?? new Int32Array(),
      knots: float64(get("knots"), 1) ?? new Float64Array(),
      ranges: float64(get("ranges"), 2) ?? new Float64Array(),
      weights: float64(get("pointWeights"), 1) ?? new Float64Array(),
    };
  } else {
    form = { kind: "hermite", tangents: float32(get("tangents"), 3) ?? new Float32Array() };
  }
  return {
    form,
    curveVertexCounts: int32(get("curveVertexCounts")) ?? new Int32Array(),
    points: float32(get("points"), 3) ?? new Float32Array(),
    widths: interpolated(stage, path, "widths", 1, time),
    normals: interpolated(stage, path, "normals", 3, time),
    displayColor: primvar(stage, path, "displayColor", 3, time, true),
    displayOpacity: primvar(stage, path, "displayOpacity", 1, time, true),
  };
}

/**
 * The `Points` at `path` at `time` as a {@link PointsInput}: `points`,
 * widths and normals (as {@link readCurves} reads them), `ids`, and
 * `primvars:displayColor` and `primvars:displayOpacity`; `null` when the
 * prim is not a `Points`.
 */
export function readPointSet(stage: Stage, path: string, options: { readonly time?: number } = {}): PointsInput | null {
  const { time } = options;
  const prim = stage.prim(path);
  if (prim === null || !(prim.typeName === "Points" || prim.isA("Points"))) return null;
  const ids = elements(stage.attribute(`${path}.ids`)?.get(time) ?? null, 1);
  return {
    points: float32(stage.attribute(`${path}.points`)?.get(time) ?? null, 3) ?? new Float32Array(),
    widths: interpolated(stage, path, "widths", 1, time),
    normals: interpolated(stage, path, "normals", 3, time),
    displayColor: primvar(stage, path, "displayColor", 3, time, true),
    displayOpacity: primvar(stage, path, "displayOpacity", 1, time, true),
    ids: ids === null ? null : ids instanceof BigInt64Array ? ids : BigInt64Array.from(Array.from(ids, (id) => BigInt(id))),
  };
}

/** One primvar's elements, `components` numbers each, one per point (`constant` repeated); `null` and its state when dropped. */
function perPoint(primvar: PrimvarInput | null | undefined, n: number, components: number): [Float32Array | null, ChannelState] {
  if (primvar == null) return [null, { state: "absent" }];
  const dropped = (reason: string): [null, ChannelState] => [null, { state: "dropped", reason }];
  const size = primvar.elementSize ?? 1;
  if (size !== 1) return dropped(`element size ${size} is not drawn`);
  const count = primvar.indices != null ? primvar.indices.length : primvar.values.length / components;
  const at = (i: number): number | null => {
    if (primvar.indices == null) return i;
    const index = primvar.indices[i]!;
    return index >= 0 && index < primvar.values.length / components ? index : null;
  };
  if (primvar.indices != null) {
    for (let i = 0; i < primvar.indices.length; i++) {
      if (at(i) === null) return dropped(`the index ${primvar.indices[i]} names no value`);
    }
  }
  if (primvar.interpolation === "faceVarying") return dropped("faceVarying interpolation is not drawn here");
  const each = primvar.interpolation === "vertex" || primvar.interpolation === "varying";
  const expected = each ? n : 1;
  if (count < expected) return dropped(`${count} elements where its interpolation needs ${expected}`);
  const out = new Float32Array(n * components);
  for (let i = 0; i < n; i++) {
    const from = at(each ? i : 0)! * components;
    for (let k = 0; k < components; k++) out[i * components + k] = primvar.values[from + k]!;
  }
  return [out, { state: "used", interpolation: primvar.interpolation, indexed: primvar.indices != null }];
}

/**
 * `UsdGeomPoints::ComputeExtent`: the union of a sphere of diameter
 * `widths[i]` at each point; `null` when widths are given but not one per
 * point, as C++ fails then; without widths, the points' bounds. `null`
 * for no points.
 */
export function pointsExtent(points: Float32Array, widths: Float32Array | null): CurvesReport["extent"] {
  const n = points.length / 3;
  if (n === 0 || (widths !== null && widths.length !== n)) return null;
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  for (let i = 0; i < n; i++) {
    const half = widths === null ? 0 : Math.fround(widths[i]! * 0.5);
    for (let k = 0; k < 3; k++) {
      min[k] = Math.min(min[k]!, points[i * 3 + k]! - half);
      max[k] = Math.max(max[k]!, points[i * 3 + k]! + half);
    }
  }
  return [
    [Math.fround(min[0]!), Math.fround(min[1]!), Math.fround(min[2]!)],
    [Math.fround(max[0]!), Math.fround(max[1]!), Math.fround(max[2]!)],
  ];
}

/**
 * A point set's buffers, as `layerstack_stagehand::fill_points` fills
 * them: every primvar one entry per point (`constant` and `uniform`
 * repeated, `vertex` and `varying` one each), a primvar of another size or
 * interpolation dropped with its reason, normals made unit length (a zero
 * normal faces +Z), `ids` kept when one per point.
 */
export function fillPoints(input: PointsInput): PointBuffers {
  const n = input.points.length / 3;
  const [widths, widthsState] = perPoint(input.widths, n, 1);
  const [normals, normalsState] = perPoint(input.normals, n, 3);
  if (normals !== null) {
    for (let i = 0; i < n; i++) {
      const [x, y, z] = [normals[i * 3]!, normals[i * 3 + 1]!, normals[i * 3 + 2]!];
      const length = Math.fround(Math.sqrt(x * x + y * y + z * z));
      if (length > 0) normals.set([x / length, y / length, z / length], i * 3);
      else normals.set([0, 0, 1], i * 3);
    }
  }
  const [colors, colorsState] = perPoint(input.displayColor, n, 3);
  const [opacities, opacitiesState] = perPoint(input.displayOpacity, n, 1);
  const authored = input.ids ?? new BigInt64Array();
  let ids = new BigInt64Array();
  let idsState: ChannelState = { state: "absent" };
  if (authored.length === n && n > 0) {
    ids = authored.slice();
    idsState = { state: "used", interpolation: "vertex", indexed: false };
  } else if (authored.length > 0) {
    idsState = { state: "dropped", reason: `${authored.length} elements where its interpolation needs ${n}` };
  }
  const report: PointsReport = Object.freeze({
    points: n,
    shape: normals !== null ? "discs" : "spheres",
    channels: Object.freeze({
      widths: widthsState,
      normals: normalsState,
      displayColor: colorsState,
      displayOpacity: opacitiesState,
      ids: idsState,
    }),
    extent: pointsExtent(input.points, input.widths?.values ?? null),
  });
  return Object.freeze({
    positions: input.points.slice(),
    widths: widths ?? new Float32Array(n).fill(1),
    normals,
    colors,
    opacities,
    ids,
    report,
  });
}

/** A native plan's report, with its extent (which leaves as numbers, not JSON). */
function reportOf(native: NativeCurves): CurvesReport {
  const e = native.extent();
  const extent: CurvesReport["extent"] = e === undefined ? null : [[e[0]!, e[1]!, e[2]!], [e[3]!, e[4]!, e[5]!]];
  return Object.freeze({ ...(JSON.parse(native.report()) as Omit<CurvesReport, "extent">), extent });
}

/** `UsdGeomBasisCurves`' `vstep`: 3 for Bezier, 1 otherwise. */
const vstep = (basis: string) => (basis === "bezier" ? 3 : 1);

/**
 * `UsdGeomBasisCurves::ComputeSegmentCounts`, as
 * `layerstack_stagehand::basis_segment_counts`: with C++'s truncating
 * division, so counts below the basis minimum give what C++ gives.
 */
export function basisSegmentCounts(type: "linear" | "cubic", basis: "bezier" | "bspline" | "catmullRom", wrap: "nonperiodic" | "periodic" | "pinned", counts: Int32Array): number[] {
  return Array.from(counts, (n) => {
    if (type === "linear") return wrap === "periodic" ? n : n - 1;
    if (wrap === "periodic") return Math.trunc(n / vstep(basis));
    if (wrap === "pinned" && basis !== "bezier") return n - 1;
    return Math.trunc((n - 4) / vstep(basis)) + 1;
  });
}

/**
 * `UsdGeomBasisCurves::Compute{Uniform,Varying,Vertex}DataSize`, as
 * `layerstack_stagehand::basis_data_sizes`: below four cubic vertices the
 * varying size is the 2 C++ intends (its unsigned arithmetic wraps
 * around); `null` for a negative count.
 */
export function basisDataSizes(type: "linear" | "cubic", basis: "bezier" | "bspline" | "catmullRom", wrap: "nonperiodic" | "periodic" | "pinned", counts: Int32Array): { uniform: number; varying: number; vertex: number } | null {
  let varying = 0;
  let vertex = 0;
  for (const n of counts) {
    if (n < 0) return null;
    vertex += n;
    if (type === "linear") varying += wrap === "periodic" ? n + 1 : n;
    else if (wrap === "periodic") varying += Math.trunc(n / vstep(basis));
    else varying += Math.trunc(Math.max(n - 4, 0) / vstep(basis)) + 2;
  }
  return { uniform: counts.length, varying, vertex };
}

/** `UsdGeomBasisCurves::ComputeInterpolationForSize`: the first of constant, uniform, varying and vertex of `size` elements. */
export function interpolationForSize(sizes: { uniform: number; varying: number; vertex: number }, size: number): Interpolation | null {
  if (size === 1) return "constant";
  if (size === sizes.uniform) return "uniform";
  if (size === sizes.varying) return "varying";
  if (size === sizes.vertex) return "vertex";
  return null;
}

/**
 * `UsdGeomCurves::ComputeExtent`: the points' bounds grown by half the
 * largest width, whatever its interpolation, in `float` as C++ computes
 * it; `null` for no points.
 */
export function curvesExtent(points: Float32Array, widths: Float32Array | null): CurvesReport["extent"] {
  if (points.length < 3) return null;
  const min = [points[0]!, points[1]!, points[2]!];
  const max = [...min];
  for (let i = 3; i < points.length; i += 3) {
    for (let k = 0; k < 3; k++) {
      min[k] = Math.min(min[k]!, points[i + k]!);
      max[k] = Math.max(max[k]!, points[i + k]!);
    }
  }
  let widest = 0;
  if (widths !== null && widths.length > 0) widest = Math.max(...widths);
  const half = Math.fround(widest * 0.5);
  return [
    [Math.fround(min[0]! - half), Math.fround(min[1]! - half), Math.fround(min[2]! - half)],
    [Math.fround(max[0]! + half), Math.fround(max[1]! + half), Math.fround(max[2]! + half)],
  ];
}

/** Copies `input` into a new curves builder of the module. */
export function gatherCurves(Builder: typeof CurvesBuilder, input: CurvesInput): CurvesBuilder {
  const form = input.form;
  const builder =
    form.kind === "basis"
      ? new Builder("basis", form.type, form.basis, form.wrap, input.curveVertexCounts, input.points)
      : new Builder(form.kind, "", "", "", input.curveVertexCounts, input.points);
  try {
    if (form.kind === "nurbs") builder.nurbs(form.order, form.knots, form.ranges, form.weights);
    if (form.kind === "hermite") builder.tangents(form.tangents);
    const set = (primvar: PrimvarInput | null | undefined, apply: (p: PrimvarInput) => void) => {
      if (primvar != null) apply(primvar);
    };
    set(input.widths, (p) => builder.widths(p.values, p.indices ?? undefined, p.interpolation));
    set(input.normals, (p) => builder.normals(p.values, p.indices ?? undefined, p.interpolation));
    set(input.displayColor, (p) => builder.displayColor(p.values, p.indices ?? undefined, p.interpolation));
    set(input.displayOpacity, (p) => builder.displayOpacity(p.values, p.indices ?? undefined, p.interpolation));
  } catch (error) {
    builder.free();
    throw error;
  }
  return builder;
}

/**
 * Curves laid out in the module and filled: samples drawn as line
 * segments with a width, color and opacity each, and with `style: "swept"`
 * their tubes or ribbons as {@link swept}. The plan stays in the module for
 * {@link StagehandCurves.refillFrom}; dispose it to free the plan.
 */
export class StagehandCurves implements Disposable {
  /** Each sample's position, three numbers each. */
  positions: Float32Array;
  /** Each sample's width. */
  widths: Float32Array;
  /** Each sample's normal, when normals are drawn. */
  normals: Float32Array | null;
  /** Each sample's display color, when drawn. */
  colors: Float32Array | null;
  /** Each sample's display opacity, when drawn. */
  opacities: Float32Array | null;
  /** Two sample indices per line segment. */
  readonly lines: Uint32Array;
  /** Each drawn curve's first sample and sample count, two numbers each. */
  readonly curves: Uint32Array;
  /** The swept triangles (empty when drawn as lines), one range. */
  swept: StagehandMesh;
  /** What the plan made of the curves. */
  report: CurvesReport;
  #native: NativeCurves | null;
  readonly #mesh: (native: NativeCurves) => StagehandMesh;

  /** @internal */
  constructor(native: NativeCurves, mesh: (native: NativeCurves) => StagehandMesh) {
    this.#native = native;
    this.#mesh = mesh;
    this.positions = native.positions();
    this.widths = native.widths();
    this.normals = native.normals() ?? null;
    this.colors = native.colors() ?? null;
    this.opacities = native.opacities() ?? null;
    this.lines = native.lines();
    this.curves = native.curves();
    this.swept = mesh(native);
    this.report = reportOf(native);
  }

  /** Whether `builder` holds this plan's topology. @internal */
  sameTopology(builder: CurvesBuilder): boolean {
    return this.#native?.sameTopology(builder) ?? false;
  }

  /**
   * Fills every buffer again from `builder` by the kept plan: new points,
   * widths, normals or colors, no layout. @internal ({@link DrawList} and
   * {@link Stagehand.refillCurves} call it.)
   */
  refillWith(builder: CurvesBuilder): void {
    const native = this.#native;
    if (native === null) throw new Error("refillFrom: the curves were disposed");
    native.refillFrom(builder);
    this.positions = native.positions();
    this.widths = native.widths();
    this.normals = native.normals() ?? null;
    this.colors = native.colors() ?? null;
    this.opacities = native.opacities() ?? null;
    this.swept.dispose();
    this.swept = this.#mesh(native);
    this.report = reportOf(native);
  }

  /** Frees the plan in the module. Idempotent; the buffers stay. */
  dispose(): void {
    this.#native?.free();
    this.#native = null;
  }

  /** Frees the plan, for `using`. */
  [Symbol.dispose](): void {
    this.dispose();
  }
}
