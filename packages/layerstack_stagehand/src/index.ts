// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/**
 * Render-ready mesh buffers from a Layerstack stage, over
 * `layerstack_stagehand` compiled to WebAssembly.
 *
 * This package's module is its own: it does not link Layerstack, and a
 * WebAssembly module cannot share Rust objects with another. Data crosses
 * as typed arrays: {@link readMesh} reads a mesh's topology, points,
 * primvars and face subsets through `@forest-rs/layerstack` (a peer
 * dependency) into a plain {@link MeshInput}, and
 * {@link Stagehand.buildMesh} copies it into this module, plans the mesh
 * and copies the buffers out.
 *
 * ```ts
 * const stagehand = await loadStagehand();
 * const input = readMesh(stage, "/World/Rock", { time });
 * const mesh = stagehand.buildMesh(input!);
 * try {
 *   geometry.setIndex(new BufferAttribute(mesh.indices, 1));
 *   geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
 *   mesh.report; // { triangles, vertices, concaveFaces, faceNotes, ... }
 *   mesh.refill(readPoints(stage, "/World/Rock", { time: next })!); // animation: no new triangulation
 * } finally {
 *   mesh.dispose(); // or `using mesh = ...` where the runtime supports it
 * }
 * ```
 *
 * A {@link DrawList} ({@link Stagehand.drawList}) keeps a whole stage's
 * instanced draws and buffers, cameras, lights and materials, and applies
 * the stage's change records as diffs.
 *
 * @module
 */
import init, {
  CurvesBuilder,
  MeshBuilder,
  pointShape,
  type StagehandMesh as NativeMesh,
  stagehandAbi,
  tessellateShape,
} from "../wasm/layerstack_stagehand.js";
import type { Stage } from "@forest-rs/layerstack";
import { StagehandCurves, gatherCurves } from "./curves.js";
import { DrawList } from "./draw.js";
import type { DrawOptions } from "./draw.js";
import type { CurveOptions, CurvesInput, DrawRange, MeshInput, MeshOptions, MeshReport, PrimvarInput, Shape, Tessellation } from "./types.js";

export type * from "./types.js";
export { DrawList } from "./draw.js";
export type { DrawBuffers, DrawDiff, DrawItem, DrawOptions, DrawWork, ItemInstance, ItemSource, PointStyle, Purpose } from "./draw.js";
export type { LinkChange, LinkKind, LinkSet, LinkTarget } from "./links.js";
export { SHAPE_SCHEMAS, readMesh, readPoints, readShape, shapeSchema } from "./read.js";
export { gatherInfluences } from "./skin.js";
export type { VertexInfluences } from "./skin.js";
export {
  CURVE_SCHEMAS,
  StagehandCurves,
  basisDataSizes,
  basisSegmentCounts,
  curvesExtent,
  fillPoints,
  interpolationForSize,
  pointsExtent,
  readCurves,
  readPointSet,
} from "./curves.js";
export { LIGHT_SCHEMAS, lightKey, lightKind, lightSizeFactor, readLight } from "./light.js";
export { conformCamera } from "./camera.js";
export { anchorAssetPath, isUdim, readMaterial, udimTiles } from "./material.js";
export type {
  AlphaMode,
  MaterialItem,
  MaterialNote,
  MaterialNoteKind,
  MaterialReading,
  MaterialTexture,
  MaterialValue,
  TextureChannel,
  TextureColorSpace,
  TextureCoordinates,
  TextureFile,
  TextureTransform,
  TextureWrap,
  UdimTileFile,
} from "./material.js";
export type {
  CameraItem,
  LightEmission,
  LightGeometry,
  LightInstance,
  LightItem,
  LightKind,
  LightLink,
  LightShadow,
  LightShaping,
} from "./light.js";

/** The exports' version this wrapper speaks (`layerstack_stagehand_wasm::ABI`). */
const ABI = 5;

/** A failure, with a stable `code`; its message reads `${operation}: ${message}`. */
export class StagehandError extends Error {
  /** `module.load`, `module.abi`, `argument.invalid`, `plan.mismatch` or `mesh.disposed`. */
  readonly code: "module.load" | "module.abi" | "argument.invalid" | "plan.mismatch" | "mesh.disposed";

  /** @internal */
  constructor(code: StagehandError["code"], operation: string, message: string, options?: ErrorOptions) {
    super(`${operation}: ${message}`, options);
    this.name = "StagehandError";
    this.code = code;
  }
}

/** Calls into the module, turning its thrown text into a {@link StagehandError} of `code`. */
function call<T>(code: StagehandError["code"], operation: string, body: () => T): T {
  try {
    return body();
  } catch (error) {
    if (typeof error === "string") {
      const text = error.startsWith(`${operation}: `) ? error.slice(operation.length + 2) : error;
      throw new StagehandError(code, operation, text);
    }
    throw error;
  }
}

/** Copies `input` into a new {@link MeshBuilder} of the module, for `operation`. */
function gather(operation: string, input: MeshInput): MeshBuilder {
  const builder = call(
    "argument.invalid",
    operation,
    () =>
      new MeshBuilder(
        input.faceVertexCounts,
        input.faceVertexIndices,
        input.holeIndices ?? new Int32Array(),
        input.orientation ?? "rightHanded",
        input.subdivisionScheme ?? "catmullClark",
        input.points,
      ),
  );
  try {
    const primvar = (set: (p: PrimvarInput) => void, primvar: PrimvarInput | null | undefined) => {
      if (primvar != null) call("argument.invalid", operation, () => set(primvar));
    };
    primvar(
      (p) => builder.normals(p.values, p.indices ?? undefined, p.interpolation, p.elementSize ?? 1),
      input.normals,
    );
    primvar((p) => builder.st(p.values, p.indices ?? undefined, p.interpolation, p.elementSize ?? 1), input.st);
    primvar(
      (p) => builder.displayColor(p.values, p.indices ?? undefined, p.interpolation, p.elementSize ?? 1),
      input.displayColor,
    );
    for (const set of input.uvSets ?? []) {
      const p = set.primvar;
      call("argument.invalid", operation, () =>
        builder.uvSet(set.name, p.values, p.indices ?? undefined, p.interpolation, p.elementSize ?? 1),
      );
    }
    const subsets = input.subsets ?? [];
    if (subsets.length > 0) {
      const ends = new Uint32Array(subsets.length);
      let total = 0;
      subsets.forEach((subset, i) => {
        total += subset.faces.length;
        ends[i] = total;
      });
      const faces = new Int32Array(total);
      let at = 0;
      for (const subset of subsets) {
        faces.set(subset.faces, at);
        at += subset.faces.length;
      }
      builder.subsets(faces, ends);
    }
  } catch (error) {
    builder.free();
    throw error;
  }
  return builder;
}

/**
 * A mesh's (or shape's) buffers, copied out of the module, and, for a
 * mesh, the plan kept in the module for {@link StagehandMesh.refill}.
 * Dispose it ({@link StagehandMesh.dispose} in a `finally`, or `using`
 * where the runtime supports it) to free the plan.
 */
export class StagehandMesh implements Disposable {
  /** Positions, three per vertex. */
  positions: Float32Array;
  /** Unit normals, three per vertex. */
  normals: Float32Array;
  /** Texture coordinates, two per vertex; `null` when `st` is not drawn. */
  st: Float32Array | null;
  /** Linear RGB colors, three per vertex; `null` when `displayColor` is not drawn. */
  colors: Float32Array | null;
  /** The texture coordinate sets drawn besides `st` ({@link MeshInput.uvSets}), two numbers per vertex each, in order. */
  uvSets: readonly { readonly name: string; readonly values: Float32Array }[];
  /** Three vertex indices per triangle, counter-clockwise seen from the front. */
  readonly indices: Uint32Array;
  /** The authored face of each triangle. */
  readonly triangleFaces: Uint32Array;
  /**
   * Each vertex's source point, the index into the input's points of the
   * point its corners name; `null` for a shape. A vertex split for a
   * face-varying normal, `st` or color still names one point, so per-point
   * data the buffers do not carry (joint influences, a `vertex` primvar)
   * is gathered into vertex order through it ({@link gatherInfluences}).
   * Fixed by the plan: a refill keeps it.
   */
  readonly vertexPoints: Uint32Array | null;
  /** One range per subset, then the faces in no subset. */
  readonly ranges: readonly DrawRange[];
  /** What the plan made of the mesh; `null` for a shape. */
  readonly report: MeshReport | null;
  #native: NativeMesh | null;

  /** @internal */
  constructor(native: NativeMesh) {
    this.#native = native;
    this.positions = native.positions();
    this.normals = native.normals();
    this.st = native.st() ?? null;
    this.colors = native.colors() ?? null;
    this.uvSets = uvSetsOf(native);
    this.indices = native.indices();
    this.triangleFaces = native.triangleFaces();
    this.vertexPoints = native.vertexPoints() ?? null;
    const ranges: DrawRange[] = [];
    const flat = native.ranges();
    for (let i = 0; i < flat.length; i += 3) {
      ranges.push({ subset: flat[i]! < 0 ? null : flat[i]!, first: flat[i + 1]!, count: flat[i + 2]! });
    }
    this.ranges = ranges;
    const report = native.report();
    this.report = report === undefined ? null : (JSON.parse(report) as MeshReport);
  }

  /**
   * Replaces {@link positions} and {@link normals} for new `points` (as
   * many as the plan's), and new authored `normals` of the same layout
   * when the plan draws authored normals (computed ones are computed
   * again), without triangulating again: for animated points. A topology
   * change needs a new {@link Stagehand.buildMesh}.
   *
   * Throws `plan.mismatch` when the points or normals are not shaped as
   * the plan's, `argument.invalid` for a shape, `mesh.disposed` after
   * {@link dispose}.
   */
  refill(points: Float32Array, normals: PrimvarInput | null = null): void {
    if (this.report === null) throw new StagehandError("argument.invalid", "refill", "a shape has no plan");
    const native = this.#live("refill");
    call("plan.mismatch", "refill", () =>
      native.refill(points, normals?.values, normals?.indices ?? undefined, normals?.interpolation),
    );
    this.positions = native.positions();
    this.normals = native.normals();
  }

  /**
   * Replaces every buffer ({@link positions}, {@link normals}, {@link st},
   * {@link colors}) from a whole `input` of the plan's topology, subsets
   * and primvar layouts, without triangulating again: for new points and
   * primvar values at once.
   *
   * Throws `plan.mismatch` when the topology or layouts differ,
   * `argument.invalid` for a shape or malformed values, `mesh.disposed`
   * after {@link dispose}.
   */
  refillFrom(input: MeshInput): void {
    if (this.report === null) throw new StagehandError("argument.invalid", "refillFrom", "a shape has no plan");
    const native = this.#live("refillFrom");
    const builder = gather("refillFrom", input);
    call("plan.mismatch", "refillFrom", () => native.refillFrom(builder));
    this.positions = native.positions();
    this.normals = native.normals();
    this.st = native.st() ?? null;
    this.colors = native.colors() ?? null;
    this.uvSets = uvSetsOf(native);
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

  #live(operation: string): NativeMesh {
    if (this.#native === null) throw new StagehandError("mesh.disposed", operation, "the mesh was disposed");
    return this.#native;
  }
}

/** The texture coordinate sets a native mesh draws besides `st`. */
function uvSetsOf(native: NativeMesh): { readonly name: string; readonly values: Float32Array }[] {
  return native.uvSetNames().map((name, index) => Object.freeze({ name, values: native.uvSetValues(index) }));
}

/** The loaded module: builds mesh buffers and tessellates implicit shapes. */
export class Stagehand {
  /** @internal */
  constructor() {}

  /**
   * Plans `input` and fills its buffers: Hydra's fan for convex faces,
   * ear clipping for concave ones (or the fan for all, with
   * `triangulation: "hydraFan"`), vertices split where normals, `st` or
   * colors differ, authored or smooth normals (Hydra's, or with
   * `normals: "faceOriented"` turned to the face at reflex corners), one
   * range per subset. An invalid topology draws nothing;
   * {@link MeshReport.topologyProblem} says why.
   *
   * Throws `argument.invalid` for points or primvar values that are not
   * whole elements, or an unknown interpolation.
   */
  buildMesh(input: MeshInput, options: MeshOptions = {}): StagehandMesh {
    const builder = gather("buildMesh", input);
    return new StagehandMesh(builder.build(options.triangulation === "hydraFan", options.normals === "faceOriented"));
  }

  /**
   * Lays `input`'s curves out by OpenUSD's segment rules and fills their
   * buffers, as `layerstack_stagehand::CurvesPlan` does: samples at
   * `options.samples` per segment drawn as line segments, and with
   * `style: "swept"` tubes of `options.sides` sides (ribbons where normals
   * are authored) as {@link StagehandCurves.swept}. Dispose it to free its
   * plan.
   *
   * Throws `argument.invalid` for values that are not whole elements or an
   * unknown interpolation.
   */
  buildCurves(input: CurvesInput, options: CurveOptions = {}): StagehandCurves {
    const builder = call("argument.invalid", "buildCurves", () => gatherCurves(CurvesBuilder, input));
    const native = builder.build(options.style === "swept", options.samples ?? 8, options.sides ?? 8);
    return new StagehandCurves(native, (curves) => {
      const mesh = new StagehandMesh(curves.swept());
      mesh.dispose();
      return mesh;
    });
  }

  /**
   * Fills `curves`' buffers again from `input` by its kept plan: new
   * points, widths, normals or colors, no layout. Returns `false`, filling
   * nothing, when `input` is not of the plan's topology (a new
   * `curveVertexCounts`, `type`, `basis`, `wrap`, or NURBS `order`, `knots`
   * or `ranges`): then {@link buildCurves} lays them out again.
   */
  refillCurves(curves: StagehandCurves, input: CurvesInput): boolean {
    const builder = call("argument.invalid", "refillFrom", () => gatherCurves(CurvesBuilder, input));
    try {
      if (!curves.sameTopology(builder)) return false;
      call("plan.mismatch", "refillFrom", () => curves.refillWith(builder));
      return true;
    } finally {
      builder.free();
    }
  }

  /**
   * The unit shape a point is instanced as, of diameter 1 at the origin:
   * a sphere (at `tessellation`), or a disc facing +Z of
   * `tessellation.segments` segments, as `layerstack_stagehand::point_shape`;
   * an empty mesh for `sprites`.
   */
  pointShape(shape: "spheres" | "discs" | "sprites", tessellation: Tessellation = {}): StagehandMesh {
    const mesh = new StagehandMesh(pointShape(shape, tessellation.segments ?? 32, tessellation.rings ?? 16));
    mesh.dispose();
    return mesh;
  }

  /**
   * A {@link DrawList} of `stage`, empty until its first
   * {@link DrawList.refresh}: the instanced draws a renderer makes of the
   * stage, kept up to date from its change records with diffs that say
   * what changed and count the work done. Dispose it to free its buffers.
   */
  drawList(stage: Stage, options: DrawOptions = {}): DrawList {
    return new DrawList(this, stage, options);
  }

  /**
   * The triangles of an implicit shape, centered on the origin as its
   * schema says, at `tessellation` (32 segments around, 16 rings from pole
   * to pole by default). The mesh has one range and no report.
   */
  tessellate(shape: Shape, tessellation: Tessellation = {}): StagehandMesh {
    const [a, b, c, axis] =
      shape.kind === "Cube"
        ? [shape.size, 0, 0, "Z"]
        : shape.kind === "Sphere"
          ? [shape.radius, 0, 0, "Z"]
          : shape.kind === "Plane"
            ? [shape.width, shape.length, 0, shape.axis]
            : shape.kind === "Cylinder_1" || shape.kind === "Capsule_1"
              ? [shape.radiusTop, shape.height, shape.radiusBottom, shape.axis]
              : [shape.radius, shape.height, 0, shape.axis];
    const native = call("argument.invalid", "tessellate", () =>
      tessellateShape(shape.kind, a, b, c, axis, tessellation.segments ?? 32, tessellation.rings ?? 16),
    );
    const mesh = new StagehandMesh(native);
    mesh.dispose();
    return mesh;
  }
}

let own: Promise<Stagehand> | undefined;

/**
 * Loads this package's module (`wasm/layerstack_stagehand_bg.wasm` beside
 * it: fetched in a browser, read from disk in Node) or the one `source`
 * names, once per JavaScript realm.
 *
 * Throws `module.load` when it cannot be instantiated, `module.abi` when
 * it speaks another ABI.
 */
export function loadStagehand(source?: URL | string | BufferSource | WebAssembly.Module): Promise<Stagehand> {
  own ??= instantiate(source);
  return own;
}

async function instantiate(source: URL | string | BufferSource | WebAssembly.Module | undefined): Promise<Stagehand> {
  let input: URL | string | BufferSource | WebAssembly.Module =
    source ?? new URL("../wasm/layerstack_stagehand_bg.wasm", import.meta.url);
  try {
    if (input instanceof URL && input.protocol === "file:") {
      const fs = globalThis.process?.getBuiltinModule?.("fs/promises");
      if (fs === undefined) throw new Error(`cannot read ${input.href}: no filesystem in this runtime`);
      input = await fs.readFile(input);
    }
    await init({ module_or_path: input });
  } catch (error) {
    own = undefined;
    throw new StagehandError("module.load", "loadStagehand", `could not instantiate the module: ${String(error)}`, {
      cause: error,
    });
  }
  const abi = stagehandAbi();
  if (abi !== ABI) {
    own = undefined;
    throw new StagehandError("module.abi", "loadStagehand", `the module speaks ABI ${abi}, this wrapper ${ABI}`);
  }
  return new Stagehand();
}
