// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the viewport draws, read from the stage alone: the drawable prims,
// their world transforms, visibility and purpose, their triangles (a mesh's
// buffers and an implicit shape's tessellation, both built by
// `@forest-rs/layerstack_stagehand`), their colors (the `displayColor`
// primvar, per vertex where it varies, else the bound material's
// `diffuseColor`), their face subsets' draw ranges, the instances of point
// instancers, and the world bounds to frame. Nothing here draws:
// `viewport3d.ts` hands the result to three.js, and
// `scripts/acceptance.ts` checks its `viewportSummary` headless.
import type { Stage } from "@forest-rs/layerstack";
import { readMesh, readShape } from "@forest-rs/layerstack_stagehand";
import type { DrawRange, MeshReport, Stagehand, StagehandMesh } from "@forest-rs/layerstack_stagehand";
import { releaseValue } from "./values.ts";

/** The purposes the viewport draws and frames, as usdview shows them by default. */
export const PURPOSES = ["default", "proxy"] as const;

/** Segments around and rings from pole to pole of curved implicit shapes. */
const TESSELLATION = { segments: 24, rings: 12 } as const;

/** Indexed triangles in the prim's own space, as stagehand built them. */
export interface Triangles {
  readonly positions: Float32Array;
  readonly normals: Float32Array;
  /** Per-vertex linear RGB from `displayColor`; `null` when the prim has none. */
  readonly colors: Float32Array | null;
  /** Three vertex indices per triangle. */
  readonly indices: Uint32Array;
  /** One range per `materialBind` subset, then the faces in no subset. */
  readonly ranges: readonly DrawRange[];
  /** The subsets' paths, by {@link DrawRange.subset}. */
  readonly subsets: readonly string[];
  /** What stagehand made of a mesh; `null` for an implicit shape. */
  readonly report: MeshReport | null;
  /** What makes two prims' triangles the same, so the renderer shares one buffer. */
  readonly key: string;
}

/** One thing the viewport draws: a prim's triangles at one or more world transforms. */
export interface DrawItem {
  /** The prim drawn, or the point instancer whose instances these are. */
  readonly path: string;
  /** The prototype drawn at each instance, for an instancer. */
  readonly prototype: string | null;
  /** `mesh` for a `Mesh`; else the implicit shape's type (`Sphere`, `Cube`, ...). */
  readonly kind: string;
  readonly triangles: Triangles;
  /** Linear RGB. */
  readonly color: readonly [number, number, number];
  /** Where the color comes from. */
  readonly colorSource: "displayColor" | "material" | "fallback";
  /** Each range's color: its subset's bound material's `diffuseColor`, else `null` for the item's. */
  readonly rangeColors: readonly (readonly [number, number, number] | null)[];
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
  /** Each drawn mesh's stagehand report, in brief, sorted: triangles, vertices, concave faces, triangles per range. */
  readonly meshes: readonly string[];
}

const SHAPES = new Set(["Sphere", "Cube", "Cylinder", "Cone", "Capsule", "Plane"]);
const DRAWN_TYPES = new Set(["Mesh", ...SHAPES]);

/** A hash of the bits of `arrays`, for {@link Triangles.key}. */
function hash(...arrays: (Float32Array | Uint32Array | null)[]): string {
  let h = 2166136261;
  for (const array of arrays) {
    if (array === null) continue;
    const words = new Uint32Array(array.buffer, array.byteOffset, array.length);
    for (const word of words) h = Math.imul(h ^ word, 16777619);
    h = Math.imul(h ^ array.length, 16777619);
  }
  return (h >>> 0).toString(16);
}

/** The buffers of a mesh stagehand built, copied before the mesh is disposed. */
function fromMesh(mesh: StagehandMesh, subsets: readonly string[], key: string): Triangles {
  return {
    positions: mesh.positions,
    normals: mesh.normals,
    colors: mesh.colors,
    indices: mesh.indices,
    ranges: mesh.ranges,
    subsets,
    report: mesh.report,
    key: `${key} ${hash(mesh.positions, mesh.indices, mesh.colors)}`,
  };
}

/** A gprim's triangles at `time`; `null` for one with none (a mesh without faces, an invalid topology). */
function triangles(stage: Stage, stagehand: Stagehand, path: string, typeName: string, time: number | undefined): Triangles | null {
  const at = time === undefined ? {} : { time };
  if (typeName === "Mesh") {
    const input = readMesh(stage, path, at);
    if (input === null) return null;
    using mesh = stagehand.buildMesh(input);
    if (mesh.indices.length === 0) return null;
    return fromMesh(mesh, (input.subsets ?? []).map((subset) => subset.path ?? ""), `mesh ${path}`);
  }
  const shape = readShape(stage, path, at);
  if (shape === null) return null;
  using mesh = stagehand.tessellate(shape, TESSELLATION);
  return fromMesh(mesh, [], JSON.stringify(shape));
}

/** The `diffuseColor` of the surface shader of the material at `material`, if it has one. */
function diffuse(stage: Stage, material: string | null, time?: number): [number, number, number] | null {
  const shader = material === null ? null : (stage.material(material)?.surface().shader ?? null);
  const value = shader === null ? null : (stage.shader(shader)?.input("diffuseColor")?.get(time) ?? null);
  if (value?.kind === "tuple" && value.value.length === 3) {
    const [r, g, b] = value.value;
    return [r!, g!, b!];
  }
  return null;
}

/** Each range's color: a subset's bound material's `diffuseColor`, `null` for the item's own. */
function rangeColors(stage: Stage, path: string, geometry: Triangles, time?: number): (readonly [number, number, number] | null)[] {
  if (geometry.subsets.length === 0) return geometry.ranges.map(() => null);
  const bound = new Map<string, string | null>();
  for (const subset of stage.prim(path)?.materialSubsets(time === undefined ? {} : { time }).subsets ?? []) {
    bound.set(subset.path, subset.binding?.material ?? null);
    subset.indices.release();
  }
  return geometry.ranges.map((range) =>
    range.subset === null ? null : diffuse(stage, bound.get(geometry.subsets[range.subset] ?? "") ?? null, time),
  );
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
  const color = diffuse(stage, binding?.material ?? null, time);
  return color === null ? { color: [0.6, 0.6, 0.6], colorSource: "fallback" } : { color, colorSource: "material" };
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
export function buildViewport(stage: Stage, stagehand: Stagehand, time?: number): ViewportScene {
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
    const geometry = triangles(stage, stagehand, path, typeName, time);
    if (geometry === null) return;
    items.push({
      path,
      prototype: null,
      kind: typeName === "Mesh" ? "mesh" : typeName,
      triangles: geometry,
      ...primColor(stage, path, time),
      rangeColors: rangeColors(stage, path, geometry, time),
      matrices: world.slice(16 * i, 16 * i + 16),
    });
  });
  drawnInstancers.forEach((path, j) => {
    const placement = world.subarray(16 * (drawnGprims.length + j), 16 * (drawnGprims.length + j) + 16);
    const instances = stage.pointInstances(path, at);
    instances.prototypes.forEach((prototype, index) => {
      const typeName = stage.prim(prototype)?.typeName ?? null;
      if (typeName === null || !DRAWN_TYPES.has(typeName)) return;
      const geometry = triangles(stage, stagehand, prototype, typeName, time);
      if (geometry === null) return;
      const mine = [...instances.prototypeIndices.keys()].filter((i) => instances.prototypeIndices[i] === index);
      const matrices = new Float64Array(16 * mine.length);
      // An instance's matrix is in the instancer's space: then the instancer's world transform.
      mine.forEach((i, k) => multiply(instances.matrices.subarray(16 * i, 16 * i + 16), placement, matrices, 16 * k));
      items.push({
        path,
        prototype,
        kind: typeName === "Mesh" ? "mesh" : typeName,
        triangles: geometry,
        ...primColor(stage, prototype, time),
        rangeColors: rangeColors(stage, prototype, geometry, time),
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
  const meshes: string[] = [];
  for (const item of scene.items) {
    const placements = item.matrices.length / 16;
    triangles += (item.triangles.indices.length / 3) * placements;
    const report = item.triangles.report;
    if (report !== null) {
      const ranges = item.triangles.ranges.map((range) => range.count / 3).join("+");
      meshes.push(
        `${item.prototype ?? item.path}: ${report.triangles} triangles, ${report.vertices} vertices, ` +
          `${report.concaveFaces} concave (${report.clippedFaces} clipped), ranges ${ranges}`,
      );
    }
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
    meshes: meshes.sort(),
  };
}
