// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Reading a mesh or an implicit shape off a stage through
// `@forest-rs/layerstack`'s typed reads, into plain typed arrays.
import type { ArrayData, Prim, Stage, Value } from "@forest-rs/layerstack";
import type { Axis, Interpolation, MeshInput, PrimvarInput, Shape, SubsetInput } from "./types.js";

const INTERPOLATIONS = new Set<string>(["constant", "uniform", "varying", "vertex", "faceVarying"]);

/** The numbers of a numeric array as a new `Float32Array`, releasing it. */
function floats(array: ArrayData): Float32Array {
  try {
    const elements = array.copy();
    return elements instanceof Float32Array ? elements : Float32Array.from(elements as ArrayLike<number>);
  } finally {
    array.release();
  }
}

/** The integers of an `int[]` array as a new `Int32Array`, releasing it. */
function ints(array: ArrayData): Int32Array {
  try {
    const elements = array.copy();
    return elements instanceof Int32Array ? elements : Int32Array.from(elements as ArrayLike<number>);
  } finally {
    array.release();
  }
}

/** An array value's numbers as floats of `components` each; `null` (released) for anything else. */
function arrayOf(value: Value | null, components: number): Float32Array | null {
  if (value?.kind !== "array") return null;
  if (value.array.components !== components || value.array.scalar === "string") {
    value.array.release();
    return null;
  }
  return floats(value.array);
}

/** The primvar `name` of the prim at `path` at `time`, of `components` per element; `null` when absent or of another shape. */
function primvar(
  stage: Stage,
  path: string,
  name: string,
  components: number,
  time: number | undefined,
  inherited: boolean,
): PrimvarInput | null {
  const found = stage.primvar(path, name, { inherited });
  if (found === null || !INTERPOLATIONS.has(found.interpolation)) return null;
  const values = arrayOf(found.get(time), components);
  if (values === null) return null;
  const indices = found.indexed ? found.indices(time) : null;
  return {
    values,
    indices: indices === null ? null : ints(indices),
    interpolation: found.interpolation as Interpolation,
    elementSize: found.elementSize,
  };
}

/**
 * The points of the mesh (or any point-based prim) at `path` at `time`,
 * three numbers each, for {@link StagehandMesh.refill}; `null` when it has
 * none.
 */
export function readPoints(stage: Stage, path: string, options: { readonly time?: number } = {}): Float32Array | null {
  return arrayOf(stage.attribute(`${path}.points`)?.get(options.time) ?? null, 3);
}

/**
 * The mesh at `path` at `time` as a {@link MeshInput}: its topology
 * (`prim.meshTopology`), points, normals, `st`, `displayColor` and
 * `materialBind` face subsets, read through `@forest-rs/layerstack` and
 * copied into typed arrays it owns; `null` when the prim is not a `Mesh`.
 *
 * - normals: the `primvars:normals` primvar when it has a value, else the
 *   `normals` attribute with its `interpolation` (`vertex` when
 *   unauthored), as `UsdGeomPointBased` reads them;
 * - `st`: `primvars:st`; `displayColor`: `primvars:displayColor`, its own or
 *   inherited from an ancestor, as primvars inherit; `uvSets`: the `float2`
 *   primvars `options.uvSets` names besides `st`, those the mesh has;
 * - subsets: the active `GeomSubset` children of family `materialBind` and
 *   element type `face`, in namespace order.
 *
 * A primvar of another shape (not three floats per element for normals
 * and colors, two for `st`) is left out. The topology is read even when
 * invalid: the plan's report then says why it draws nothing.
 */
export function readMesh(
  stage: Stage,
  path: string,
  options: { readonly time?: number; readonly uvSets?: readonly string[] } = {},
): MeshInput | null {
  const { time } = options;
  const prim = stage.prim(path);
  const topology = prim?.meshTopology(time === undefined ? {} : { time }) ?? null;
  if (prim === null || topology === null) return null;
  const points = readPoints(stage, path, options) ?? new Float32Array();
  let normals = primvar(stage, path, "normals", 3, time, false);
  if (normals === null) {
    const attribute = stage.attribute(`${path}.normals`);
    const values = arrayOf(attribute?.get(time) ?? null, 3);
    if (values !== null) {
      const interpolation = attributeInterpolation(stage, path);
      normals = { values, interpolation };
    }
  }
  const subsets: SubsetInput[] = prim
    .geomSubsets({ elementType: "face", familyName: "materialBind", ...(time === undefined ? {} : { time }) })
    .map((subset) => ({
      path: subset.path,
      faces: subset.indices === null ? new Int32Array() : ints(subset.indices),
    }));
  return {
    faceVertexCounts: topology.faceVertexCounts,
    faceVertexIndices: topology.faceVertexIndices,
    holeIndices: topology.holeIndices,
    orientation: topology.orientation,
    subdivisionScheme: topology.subdivisionScheme,
    points,
    normals,
    st: primvar(stage, path, "st", 2, time, false),
    displayColor: primvar(stage, path, "displayColor", 3, time, true),
    uvSets: (options.uvSets ?? []).flatMap((name) => {
      const set = name === "st" ? null : primvar(stage, path, name, 2, time, false);
      return set === null ? [] : [{ name, primvar: set }];
    }),
    subsets,
  };
}

/** The `interpolation` metadata of the `normals` attribute of the prim at `path`, `vertex` when unauthored. */
function attributeInterpolation(stage: Stage, path: string): Interpolation {
  const value = stage.attribute(`${path}.normals`)?.metadata("interpolation") ?? null;
  const token = value?.kind === "scalar" ? String(value.value) : "vertex";
  return INTERPOLATIONS.has(token) ? (token as Interpolation) : "vertex";
}

/** A scalar attribute's number, or `fallback`. */
function number(stage: Stage, path: string, time: number | undefined, fallback: number): number {
  const value = stage.attribute(path)?.get(time) ?? null;
  return value?.kind === "scalar" && typeof value.value === "number" ? value.value : fallback;
}

/** A shape's `axis`, `Z` when unauthored or unknown. */
function axis(stage: Stage, path: string): Axis {
  const value = stage.attribute(`${path}.axis`)?.get() ?? null;
  return value?.kind === "scalar" && (value.value === "X" || value.value === "Y") ? value.value : "Z";
}

/** The implicit shape schemas, in the order a prim is matched against them, as `layerstack_stagehand::read_shape` matches. */
export const SHAPE_SCHEMAS: readonly Shape["kind"][] = [
  "Cube",
  "Sphere",
  "Cylinder",
  "Cone",
  "Capsule",
  "Plane",
  "Cylinder_1",
  "Capsule_1",
];

/**
 * The implicit shape schema `prim` is (`IsA`, {@link Prim.isA}): its own
 * type when that is one, else the first it derives from, so a schema
 * derived from `Sphere` reads as a sphere; `null` for any other prim.
 */
export function shapeSchema(prim: Prim): Shape["kind"] | null {
  const own = SHAPE_SCHEMAS.find((schema) => schema === prim.typeName);
  return own ?? SHAPE_SCHEMAS.find((schema) => prim.isA(schema)) ?? null;
}

/**
 * The implicit shape at `path` at `time` (a prim that is a `Cube`,
 * `Sphere`, `Cylinder`, `Cone`, `Capsule`, `Plane`, `Cylinder_1` or
 * `Capsule_1`, or of a schema derived from one: {@link shapeSchema}) with
 * its size attributes, schema fallbacks included; `null` for any other
 * prim.
 */
export function readShape(stage: Stage, path: string, options: { readonly time?: number } = {}): Shape | null {
  const { time } = options;
  const at = (name: string, fallback: number) => number(stage, `${path}.${name}`, time, fallback);
  const prim = stage.prim(path);
  switch (prim === null ? null : shapeSchema(prim)) {
    case "Cube":
      return { kind: "Cube", size: at("size", 2) };
    case "Sphere":
      return { kind: "Sphere", radius: at("radius", 1) };
    case "Cylinder":
      return { kind: "Cylinder", radius: at("radius", 1), height: at("height", 2), axis: axis(stage, path) };
    case "Cone":
      return { kind: "Cone", radius: at("radius", 1), height: at("height", 2), axis: axis(stage, path) };
    case "Capsule":
      return { kind: "Capsule", radius: at("radius", 0.5), height: at("height", 1), axis: axis(stage, path) };
    case "Plane":
      return { kind: "Plane", width: at("width", 2), length: at("length", 2), axis: axis(stage, path) };
    case "Cylinder_1":
      return {
        kind: "Cylinder_1",
        radiusTop: at("radiusTop", 1),
        radiusBottom: at("radiusBottom", 1),
        height: at("height", 2),
        axis: axis(stage, path),
      };
    case "Capsule_1":
      return {
        kind: "Capsule_1",
        radiusTop: at("radiusTop", 0.5),
        radiusBottom: at("radiusBottom", 0.5),
        height: at("height", 1),
        axis: axis(stage, path),
      };
    default:
      return null;
  }
}
