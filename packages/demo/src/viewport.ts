// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the viewport draws, read from the stage alone: the drawable prims,
// their world transforms, visibility and purpose, their triangles (a mesh's
// points and faces, a sphere's radius, a cube's size), their colors (the
// `displayColor` primvar, else the bound material's `diffuseColor`), the
// instances of point instancers, and the world bounds to frame. Nothing
// here draws: `viewport3d.ts` hands the result to three.js, and
// `scripts/acceptance.ts` checks its `viewportSummary` headless.
import type { Stage, Value } from "@forest-rs/layerstack";
import { releaseValue } from "./values.ts";

/** The purposes the viewport draws and frames, as usdview shows them by default. */
export const PURPOSES = ["default", "proxy"] as const;

/** Segments around and rings from pole to pole of a sphere's triangles. */
const SPHERE_SEGMENTS = 24;
const SPHERE_RINGS = 12;

/** Triangles in the prim's own space: positions and normals per corner, three corners per triangle. */
export interface Triangles {
  readonly positions: Float32Array;
  readonly normals: Float32Array;
  /** What makes two prims' triangles the same, so the renderer shares one buffer. */
  readonly key: string;
}

/** One thing the viewport draws: a prim's triangles at one or more world transforms. */
export interface DrawItem {
  /** The prim drawn, or the point instancer whose instances these are. */
  readonly path: string;
  /** The prototype drawn at each instance, for an instancer. */
  readonly prototype: string | null;
  readonly kind: "mesh" | "sphere" | "cube";
  readonly triangles: Triangles;
  /** Linear RGB. */
  readonly color: readonly [number, number, number];
  /** Where the color comes from. */
  readonly colorSource: "displayColor" | "material" | "fallback";
  /** 16 numbers per placement, row by row (OpenUSD's row-vector matrices; column-major for three.js). */
  readonly matrices: Float64Array;
}

/** The viewport's contents at one time. */
export interface ViewportScene {
  readonly items: readonly DrawItem[];
  /** The gprims and instancers left out because they are invisible, or of a purpose not drawn. */
  readonly hidden: readonly string[];
  /** The aligned world bounds of everything drawable, `[minX, minY, minZ, maxX, maxY, maxZ]`; `null` when empty. */
  readonly bounds: readonly number[] | null;
}

/** What the acceptance script checks of the viewport, independent of pixels. */
export interface ViewportSummary {
  /** The prims drawn, sorted; an instancer as `path ×count`. */
  readonly drawn: readonly string[];
  readonly hidden: readonly string[];
  readonly triangles: number;
  readonly instances: number;
  /** The framed world bounds, to 3 decimals. */
  readonly bounds: readonly number[] | null;
}

const DRAWN_TYPES = new Set(["Mesh", "Sphere", "Cube"]);

/** The scalar number of an attribute value, or `fallback`. */
function number(value: Value | null, fallback: number): number {
  return value?.kind === "scalar" && typeof value.value === "number" ? value.value : fallback;
}

/** The numbers of an attribute's array, copied out of the module; `null` when it has none. */
function numbers(stage: Stage, path: string, time: number | undefined): Float64Array | null {
  const value = stage.attribute(path)?.get(time) ?? null;
  if (value?.kind !== "array") {
    releaseValue(value);
    return null;
  }
  const copy = Float64Array.from(value.array.copy() as ArrayLike<number>);
  releaseValue(value);
  return copy;
}

/** A sphere of `radius`: a latitude-longitude grid, two triangles per cell. */
export function sphere(radius: number): Triangles {
  const positions: number[] = [];
  const normals: number[] = [];
  const at = (ring: number, segment: number): [number, number, number] => {
    const theta = (ring / SPHERE_RINGS) * Math.PI;
    const phi = (segment / SPHERE_SEGMENTS) * 2 * Math.PI;
    return [Math.sin(theta) * Math.cos(phi), Math.cos(theta), -Math.sin(theta) * Math.sin(phi)];
  };
  for (let ring = 0; ring < SPHERE_RINGS; ring++) {
    for (let segment = 0; segment < SPHERE_SEGMENTS; segment++) {
      const a = at(ring, segment);
      const b = at(ring + 1, segment);
      const c = at(ring + 1, segment + 1);
      const d = at(ring, segment + 1);
      for (const corner of [a, b, c, a, c, d]) {
        normals.push(...corner);
        positions.push(...corner.map((v) => v * radius));
      }
    }
  }
  return { positions: new Float32Array(positions), normals: new Float32Array(normals), key: `sphere ${radius}` };
}

/** A cube of edge `size`, centered on the origin, as `UsdGeomCube` is. */
export function cube(size: number): Triangles {
  const h = size / 2;
  const positions: number[] = [];
  const normals: number[] = [];
  // Each face: its normal axis and sign, and two axes spanning it, counterclockwise seen from outside.
  for (const [axis, sign] of [[0, 1], [0, -1], [1, 1], [1, -1], [2, 1], [2, -1]] as const) {
    const u = (axis + (sign > 0 ? 1 : 2)) % 3;
    const v = (axis + (sign > 0 ? 2 : 1)) % 3;
    const corner = (du: number, dv: number) => {
      const p = [0, 0, 0];
      p[axis] = sign * h;
      p[u] = du * h;
      p[v] = dv * h;
      return p;
    };
    const quad = [corner(-1, -1), corner(1, -1), corner(1, 1), corner(-1, 1)];
    for (const index of [0, 1, 2, 0, 2, 3]) {
      positions.push(...quad[index]!);
      const n = [0, 0, 0];
      n[axis] = sign;
      normals.push(...n);
    }
  }
  return { positions: new Float32Array(positions), normals: new Float32Array(normals), key: `cube ${size}` };
}

/**
 * A polygon mesh's faces as triangles: each face fanned from its first
 * vertex, wound as `orientation` says (`rightHanded` counterclockwise),
 * with one flat normal per face. Faces with fewer than three vertices or
 * an index past the points are skipped, as renderers skip them.
 */
export function meshTriangles(
  path: string,
  points: Float64Array,
  counts: Float64Array,
  indices: Float64Array,
  leftHanded: boolean,
): Triangles {
  const positions: number[] = [];
  const normals: number[] = [];
  const vertices = points.length / 3;
  let start = 0;
  let checksum = 0;
  for (const count of counts) {
    const face = Array.from(indices.subarray(start, start + count));
    start += count;
    if (count < 3 || face.some((index) => index < 0 || index >= vertices)) continue;
    const p = (index: number) => [points[3 * index]!, points[3 * index + 1]!, points[3 * index + 2]!] as const;
    for (let k = 1; k + 1 < count; k++) {
      const corners = leftHanded ? [face[0]!, face[k + 1]!, face[k]!] : [face[0]!, face[k]!, face[k + 1]!];
      const [a, b, c] = corners.map(p) as unknown as [readonly number[], readonly number[], readonly number[]];
      const e1 = [b[0]! - a[0]!, b[1]! - a[1]!, b[2]! - a[2]!];
      const e2 = [c[0]! - a[0]!, c[1]! - a[1]!, c[2]! - a[2]!];
      const n = [e1[1]! * e2[2]! - e1[2]! * e2[1]!, e1[2]! * e2[0]! - e1[0]! * e2[2]!, e1[0]! * e2[1]! - e1[1]! * e2[0]!];
      const length = Math.hypot(n[0]!, n[1]!, n[2]!) || 1;
      for (const corner of [a, b, c]) {
        positions.push(...corner);
        normals.push(n[0]! / length, n[1]! / length, n[2]! / length);
        checksum = (checksum * 31 + corner[0]! * 7 + corner[1]! * 13 + corner[2]! * 17) % 1e9;
      }
    }
  }
  return {
    positions: new Float32Array(positions),
    normals: new Float32Array(normals),
    key: `mesh ${path} ${positions.length} ${checksum}`,
  };
}

/** A gprim's triangles from its attributes at `time`; `null` for a mesh without points or faces. */
function triangles(stage: Stage, path: string, typeName: string, time: number | undefined): Triangles | null {
  switch (typeName) {
    case "Sphere":
      return sphere(number(stage.attribute(`${path}.radius`)?.get(time) ?? null, 1));
    case "Cube":
      return cube(number(stage.attribute(`${path}.size`)?.get(time) ?? null, 2));
    case "Mesh": {
      const points = numbers(stage, `${path}.points`, time);
      const counts = numbers(stage, `${path}.faceVertexCounts`, time);
      const indices = numbers(stage, `${path}.faceVertexIndices`, time);
      if (points === null || counts === null || indices === null) return null;
      const orientation = stage.attribute(`${path}.orientation`)?.get(time) ?? null;
      const leftHanded = orientation?.kind === "scalar" && orientation.value === "leftHanded";
      return meshTriangles(path, points, counts, indices, leftHanded);
    }
    default:
      return null;
  }
}

/**
 * The prim's color: its `displayColor` primvar, its own or inherited (the
 * first value: the viewport draws one color per prim), else its bound
 * material's surface `diffuseColor`, else a neutral gray.
 */
export function primColor(stage: Stage, path: string, time?: number): Pick<DrawItem, "color" | "colorSource"> {
  const primvar = stage.primvar(path, "displayColor", { inherited: true });
  const value = primvar?.flattened(time) ?? null;
  if (value?.kind === "array" && value.array.length > 0) {
    const [r, g, b] = Array.from(value.array.copy() as ArrayLike<number>);
    releaseValue(value);
    return { color: [r!, g!, b!], colorSource: "displayColor" };
  }
  releaseValue(value);
  const binding = stage.prim(path)?.boundMaterial() ?? null;
  const shader = binding?.material == null ? null : stage.material(binding.material)?.surface().shader ?? null;
  const diffuse = shader === null ? null : stage.shader(shader)?.input("diffuseColor")?.get(time) ?? null;
  if (diffuse?.kind === "tuple" && diffuse.value.length === 3) {
    const [r, g, b] = diffuse.value;
    return { color: [r!, g!, b!], colorSource: "material" };
  }
  return { color: [0.6, 0.6, 0.6], colorSource: "fallback" };
}

/** `a * b`, row-vector 4x4 matrices of 16 numbers, row by row. */
function multiply(a: ArrayLike<number>, b: ArrayLike<number>, out: Float64Array, at: number): void {
  for (let row = 0; row < 4; row++) {
    for (let column = 0; column < 4; column++) {
      let sum = 0;
      for (let k = 0; k < 4; k++) sum += a[4 * row + k]! * b[4 * k + column]!;
      out[at + 4 * row + column] = sum;
    }
  }
}

/** The drawable prims under `root`: gprims the viewport draws, and point instancers (whose prototypes it draws through them). */
function drawable(stage: Stage, root: string, gprims: [string, string][], instancers: string[]): void {
  for (const path of stage.children(root)) {
    const prim = stage.prim(path);
    if (prim === null || !prim.active || !prim.defined || prim.abstract) continue;
    if (prim.typeName === "PointInstancer") {
      instancers.push(path);
      continue;
    }
    if (prim.typeName !== null && DRAWN_TYPES.has(prim.typeName)) gprims.push([path, prim.typeName]);
    drawable(stage, path, gprims, instancers);
  }
}

/**
 * The viewport's contents at `time`: one batched read of the drawable
 * prims' visibility and purpose and one of their world transforms, the
 * geometry and color of each, each point instancer's instances placed by
 * its own world transform, and the world bounds of the drawn purposes.
 */
export function buildViewport(stage: Stage, time?: number): ViewportScene {
  const at = time === undefined ? {} : { time };
  const gprims: [string, string][] = [];
  const instancers: string[] = [];
  drawable(stage, "/", gprims, instancers);
  const candidates = [...gprims.map(([path]) => path), ...instancers];
  const visibility = stage.visibilities(candidates, at);
  const purposes = stage.purposeInfos(candidates);
  const shown = (i: number) =>
    visibility[i] !== "invisible" && (PURPOSES as readonly string[]).includes(purposes[i]?.purpose ?? "default");
  const hidden = candidates.filter((_, i) => !shown(i));
  const drawnGprims = gprims.filter((_, i) => shown(i));
  const drawnInstancers = instancers.filter((_, i) => shown(gprims.length + i));
  const world = stage.worldTransforms([...drawnGprims.map(([path]) => path), ...drawnInstancers], at);
  const items: DrawItem[] = [];
  drawnGprims.forEach(([path, typeName], i) => {
    const geometry = triangles(stage, path, typeName, time);
    if (geometry === null) return;
    items.push({
      path,
      prototype: null,
      kind: typeName === "Mesh" ? "mesh" : typeName === "Sphere" ? "sphere" : "cube",
      triangles: geometry,
      ...primColor(stage, path, time),
      matrices: world.slice(16 * i, 16 * i + 16),
    });
  });
  drawnInstancers.forEach((path, j) => {
    const placement = world.subarray(16 * (drawnGprims.length + j), 16 * (drawnGprims.length + j) + 16);
    const instances = stage.pointInstances(path, at);
    instances.prototypes.forEach((prototype, index) => {
      const typeName = stage.prim(prototype)?.typeName ?? null;
      if (typeName === null || !DRAWN_TYPES.has(typeName)) return;
      const geometry = triangles(stage, prototype, typeName, time);
      if (geometry === null) return;
      const mine = [...instances.prototypeIndices.keys()].filter((i) => instances.prototypeIndices[i] === index);
      const matrices = new Float64Array(16 * mine.length);
      // An instance's matrix is in the instancer's space: then the instancer's world transform.
      mine.forEach((i, k) => multiply(instances.matrices.subarray(16 * i, 16 * i + 16), placement, matrices, 16 * k));
      items.push({
        path,
        prototype,
        kind: typeName === "Mesh" ? "mesh" : typeName === "Sphere" ? "sphere" : "cube",
        triangles: geometry,
        ...primColor(stage, prototype, time),
        matrices,
      });
    });
  });
  const bounds = stage.worldBounds(["/"], { ...at, purposes: [...PURPOSES] }).alignedRanges;
  const empty = bounds[0]! > bounds[3]!;
  return { items, hidden, bounds: empty ? null : Array.from(bounds) };
}

/** The viewport's contents as the acceptance script checks them: no pixels. */
export function viewportSummary(scene: ViewportScene): ViewportSummary {
  let triangles = 0;
  let instances = 0;
  const drawn: string[] = [];
  for (const item of scene.items) {
    const placements = item.matrices.length / 16;
    triangles += (item.triangles.positions.length / 9) * placements;
    if (item.prototype === null) {
      drawn.push(item.path);
    } else {
      instances += placements;
      drawn.push(`${item.path} ×${placements}`);
    }
  }
  return {
    drawn: drawn.sort(),
    hidden: [...scene.hidden].sort(),
    triangles,
    instances,
    bounds: scene.bounds === null ? null : scene.bounds.map((value) => Math.round(value * 1000) / 1000),
  };
}
