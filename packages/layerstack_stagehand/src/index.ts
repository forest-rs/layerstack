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
 * using mesh = stagehand.buildMesh(input!);
 * geometry.setIndex(new BufferAttribute(mesh.indices, 1));
 * geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
 * mesh.report; // { triangles, vertices, concaveFaces, faceNotes, ... }
 * mesh.refill(readPoints(stage, "/World/Rock", { time: next })!); // animation: no new triangulation
 * ```
 *
 * @module
 */
import init, {
  MeshBuilder,
  type StagehandMesh as NativeMesh,
  stagehandAbi,
  tessellateShape,
} from "../wasm/layerstack_stagehand.js";
import type { DrawRange, MeshInput, MeshOptions, MeshReport, PrimvarInput, Shape, Tessellation } from "./types.js";

export type * from "./types.js";
export { readMesh, readPoints, readShape } from "./read.js";

/** The exports' version this wrapper speaks (`layerstack_stagehand_wasm::ABI`). */
const ABI = 2;

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

/**
 * A mesh's (or shape's) buffers, copied out of the module, and, for a
 * mesh, the plan kept in the module for {@link StagehandMesh.refill}.
 * Dispose it (`using`, or {@link StagehandMesh.dispose}) to free the plan.
 */
export class StagehandMesh implements Disposable {
  /** Positions, three per vertex. */
  positions: Float32Array;
  /** Unit normals, three per vertex. */
  normals: Float32Array;
  /** Texture coordinates, two per vertex; `null` when `st` is not drawn. */
  readonly st: Float32Array | null;
  /** Linear RGB colors, three per vertex; `null` when `displayColor` is not drawn. */
  readonly colors: Float32Array | null;
  /** Three vertex indices per triangle, counter-clockwise seen from the front. */
  readonly indices: Uint32Array;
  /** The authored face of each triangle. */
  readonly triangleFaces: Uint32Array;
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
    this.indices = native.indices();
    this.triangleFaces = native.triangleFaces();
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
    const builder = call(
      "argument.invalid",
      "buildMesh",
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
        if (primvar != null) call("argument.invalid", "buildMesh", () => set(primvar));
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
    return new StagehandMesh(builder.build(options.triangulation === "hydraFan", options.normals === "faceOriented"));
  }

  /**
   * The triangles of an implicit shape, centered on the origin as its
   * schema says, at `tessellation` (32 segments around, 16 rings from pole
   * to pole by default). The mesh has one range and no report.
   */
  tessellate(shape: Shape, tessellation: Tessellation = {}): StagehandMesh {
    const [a, b, axis] =
      shape.kind === "Cube"
        ? [shape.size, 0, "Z"]
        : shape.kind === "Sphere"
          ? [shape.radius, 0, "Z"]
          : shape.kind === "Plane"
            ? [shape.width, shape.length, shape.axis]
            : [shape.radius, shape.height, shape.axis];
    const native = call("argument.invalid", "tessellate", () =>
      tessellateShape(shape.kind, a, b, axis, tessellation.segments ?? 32, tessellation.rings ?? 16),
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
