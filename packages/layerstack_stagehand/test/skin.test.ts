// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// GPU skinning over a draw list's buffers: joint influences gathered into
// vertex order through `vertexPoints`, and the linear blend a vertex
// shader computes from them and the joint palette, checked against
// `@forest-rs/layerstack`'s CPU `deformedPoints` and `deformedNormals`
// (which follow OpenUSD's `UsdSkelSkinningQuery`) at several times.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { SkinningQuery, Stage } from "@forest-rs/layerstack";
import { StagehandError, gatherInfluences, loadStagehand } from "../dist/index.js";
import type { DrawBuffers, VertexInfluences } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

const I = "((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0)";

/**
 * A robot: six joints posed over time (the skeleton itself placed in the
 * world), and three meshes, each placed by a transform of its own that
 * skinning does not apply:
 *
 * - `Arm`: two influences per point, vertex normals, and face-varying
 *   `st` that splits every corner into a vertex of its own face;
 * - `Tail`: six influences per point in its own joint order
 *   (`skel:joints`), one point weighting all six;
 * - `Badge`: one block for the whole prim (rigid), under a bind
 *   transform that turns and lifts it.
 */
const ROBOT = `#usda 1.0
def SkelRoot "Robot" (
    prepend apiSchemas = ["SkelBindingAPI"]
) {
    rel skel:skeleton = </Robot/Skel>
    def Skeleton "Skel" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        double3 xformOp:translate = (0, 0, 0.25)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        uniform token[] joints = ["Root", "Root/Spine", "Root/Spine/Neck", "Root/Spine/Arm", "Root/Spine/Arm/Hand", "Root/Tail"]
        uniform matrix4d[] bindTransforms = [${I}, (0, 0, 0, 1)), ${I}, (0, 0, 1, 1)), ${I}, (0, 0, 2, 1)), ${I}, (1, 0, 1.5, 1)), ${I}, (2, 0, 1.5, 1)), ${I}, (0, -1, 0.5, 1))]
        uniform matrix4d[] restTransforms = [${I}, (0, 0, 0, 1)), ${I}, (0, 0, 1, 1)), ${I}, (0, 0, 1, 1)), ${I}, (1, 0, 0.5, 1)), ${I}, (1, 0, 0, 1)), ${I}, (0, -1, 0.5, 1))]
        rel skel:animationSource = </Robot/Skel/Walk>
        def SkelAnimation "Walk" {
            uniform token[] joints = ["Root", "Root/Spine", "Root/Spine/Neck", "Root/Spine/Arm", "Root/Spine/Arm/Hand", "Root/Tail"]
            float3[] translations.timeSamples = {
                1: [(0, 0, 0), (0, 0, 1), (0, 0, 1), (1, 0, 0.5), (1, 0, 0), (0, -1, 0.5)],
                10: [(0.5, 0.25, 0), (0, 0, 1), (0, 0, 1), (1, 0, 0.5), (1, 0, 0), (0, -1, 0.5)],
            }
            quatf[] rotations.timeSamples = {
                1: [(1, 0, 0, 0), (1, 0, 0, 0), (1, 0, 0, 0), (1, 0, 0, 0), (1, 0, 0, 0), (1, 0, 0, 0)],
                10: [(0.9238795, 0, 0, 0.3826834), (0.9659258, 0.258819, 0, 0), (0.8660254, 0, 0.5, 0), (0.7071068, 0, -0.7071068, 0), (0.7071068, 0.7071068, 0, 0), (0.9659258, 0, 0, -0.258819)],
            }
            half3[] scales.timeSamples = {
                1: [(1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1), (1, 1, 1)],
                10: [(1, 1, 1), (1, 1, 1), (1, 1, 1), (1.5, 1.5, 1.5), (1, 1, 1), (1, 1, 1)],
            }
        }
    }
    def Mesh "Arm" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        int[] faceVertexCounts = [4, 4, 4, 4]
        int[] faceVertexIndices = [0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7]
        point3f[] points = [(1, -0.2, 1.3), (1, 0.2, 1.3), (1, 0.2, 1.7), (1, -0.2, 1.7), (2.5, -0.2, 1.3), (2.5, 0.2, 1.3), (2.5, 0.2, 1.7), (2.5, -0.2, 1.7)]
        normal3f[] normals = [(0, -0.7071068, -0.7071068), (0, 0.7071068, -0.7071068), (0, 0.7071068, 0.7071068), (0, -0.7071068, 0.7071068), (0, -0.7071068, -0.7071068), (0, 0.7071068, -0.7071068), (0, 0.7071068, 0.7071068), (0, -0.7071068, 0.7071068)] (
            interpolation = "vertex"
        )
        texCoord2f[] primvars:st = [(0, 0), (0.25, 0), (0.25, 1), (0, 1), (0.25, 0), (0.5, 0), (0.5, 1), (0.25, 1), (0.5, 0), (0.75, 0), (0.75, 1), (0.5, 1), (0.75, 0), (1, 0), (1, 1), (0.75, 1)] (
            interpolation = "faceVarying"
        )
        int[] primvars:skel:jointIndices = [3, 1, 3, 1, 3, 1, 3, 1, 4, 3, 4, 3, 4, 3, 4, 3] (
            elementSize = 2
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [0.7, 0.3, 0.7, 0.3, 0.6, 0.4, 0.6, 0.4, 0.8, 0.2, 0.8, 0.2, 1, 0, 1, 0] (
            elementSize = 2
            interpolation = "vertex"
        )
    }
    def Mesh "Tail" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(-0.2, -1, 0.4), (0, -1.5, 0.4), (0.2, -2, 0.4), (-0.2, -1, 0.6), (0, -1.5, 0.6), (0.2, -2, 0.6)]
        uniform token[] skel:joints = ["Root/Tail", "Root", "Root/Spine", "Root/Spine/Arm", "Root/Spine/Neck", "Root/Spine/Arm/Hand"]
        int[] primvars:skel:jointIndices = [0, 1, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 0, 2, 0, 0, 0, 0] (
            elementSize = 6
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [0.5, 0.5, 0, 0, 0, 0, 0.4, 0.2, 0.15, 0.1, 0.1, 0.05, 1, 0, 0, 0, 0, 0, 0.5, 0.5, 0, 0, 0, 0, 0.3, 0.3, 0.1, 0.1, 0.1, 0.1, 0.75, 0.25, 0, 0, 0, 0] (
            elementSize = 6
            interpolation = "vertex"
        )
    }
    def Mesh "Badge" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        double3 xformOp:translate = (0, 0, -3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-0.1, 0, -0.1), (0.1, 0, -0.1), (0.1, 0, 0.1), (-0.1, 0, 0.1)]
        normal3f[] normals = [(0, -1, 0), (0, -1, 0), (0, -1, 0), (0, -1, 0)] (
            interpolation = "vertex"
        )
        matrix4d primvars:skel:geomBindTransform = ((0, 1, 0, 0), (-1, 0, 0, 0), (0, 0, 1, 0), (0, 0.3, 2, 1))
        int[] primvars:skel:jointIndices = [2, 1] (
            elementSize = 2
            interpolation = "constant"
        )
        float[] primvars:skel:jointWeights = [0.75, 0.25] (
            elementSize = 2
            interpolation = "constant"
        )
    }
}
`;

function openRobot(): Stage {
  return layerstack.open({ identifier: "robot.usda", bytes: new TextEncoder().encode(ROBOT) });
}

/** `p` (three numbers, a point when `w` is 1, a direction when 0) times the row-vector matrix `m` (16 numbers at `at`). */
function times(p: readonly number[], m: ArrayLike<number>, at: number, w: number): number[] {
  return [0, 1, 2].map((j) => p[0]! * m[at + j]! + p[1]! * m[at + 4 + j]! + p[2]! * m[at + 8 + j]! + w * m[at + 12 + j]!);
}

/** The upper-left 3×3 of the row-vector matrix `a` (16 numbers) times that of `b` at `at`. */
function linearProduct(a: ArrayLike<number>, b: ArrayLike<number>, at: number): number[][] {
  return [0, 1, 2].map((i) => [0, 1, 2].map((j) => a[4 * i]! * b[at + j]! + a[4 * i + 1]! * b[at + 4 + j]! + a[4 * i + 2]! * b[at + 8 + j]!));
}

/** The inverse transpose of the 3×3 `m`, by its cofactors; zeros when singular. */
function inverseTranspose(m: number[][]): number[][] {
  const c = (i: number, j: number) => {
    const [r0, r1] = [0, 1, 2].filter((k) => k !== i);
    const [c0, c1] = [0, 1, 2].filter((k) => k !== j);
    return ((i + j) % 2 === 0 ? 1 : -1) * (m[r0!]![c0!]! * m[r1!]![c1!]! - m[r0!]![c1!]! * m[r1!]![c0!]!);
  };
  const cofactors = [0, 1, 2].map((i) => [0, 1, 2].map((j) => c(i, j)));
  const det = m[0]![0]! * cofactors[0]![0]! + m[0]![1]! * cofactors[0]![1]! + m[0]![2]! * cofactors[0]![2]!;
  return cofactors.map((row) => row.map((value) => (det === 0 ? 0 : value / det)));
}

/**
 * What the editor's linear-blend skinning vertex shader computes for each
 * vertex of a buffer (three.js's, each bone matrix the joint's skinning
 * transform and each bone inverse the bind transform, so a bone moves a
 * rest point by the bind transform, then the joint): the position through
 * each slot's matrix, blended by weight; the normal through each slot's
 * matrix's inverse transpose, blended, then normalized, as
 * `UsdSkelSkinNormals` does.
 */
function gpuSkin(
  positions: Float32Array,
  normals: Float32Array,
  influences: VertexInfluences,
  bind: Float64Array,
  palette: Float64Array,
): { positions: Float64Array; normals: Float64Array } {
  const vertices = positions.length / 3;
  const outPositions = new Float64Array(vertices * 3);
  const outNormals = new Float64Array(vertices * 3);
  const joints = palette.length / 16;
  const normalMatrices = Array.from({ length: joints }, (_, joint) => inverseTranspose(linearProduct(bind, palette, 16 * joint)));
  for (let v = 0; v < vertices; v++) {
    const bound = times([...positions.subarray(3 * v, 3 * v + 3)], bind, 0, 1);
    const normal = [...normals.subarray(3 * v, 3 * v + 3)];
    const p = [0, 0, 0];
    const n = [0, 0, 0];
    for (let k = 0; k < influences.slots; k++) {
      const weight = influences.weights[v * influences.slots + k]!;
      const joint = influences.joints[v * influences.slots + k]!;
      const moved = times(bound, palette, 16 * joint, 1);
      const m = normalMatrices[joint]!;
      for (let j = 0; j < 3; j++) {
        p[j]! += weight * moved[j]!;
        n[j]! += weight * (normal[0]! * m[0]![j]! + normal[1]! * m[1]![j]! + normal[2]! * m[2]![j]!);
      }
    }
    const length = Math.hypot(n[0]!, n[1]!, n[2]!) || 1;
    outPositions.set(p, 3 * v);
    outNormals.set(n.map((value) => value / length), 3 * v);
  }
  return { positions: outPositions, normals: outNormals };
}

/** The largest distance between `gpu`'s vertices and the CPU's per-point `cpu` values each vertex's point names. */
function largestError(gpu: Float64Array, cpu: Float32Array, vertexPoints: Uint32Array, skip: ReadonlySet<number> = new Set()): number {
  let error = 0;
  vertexPoints.forEach((point, v) => {
    if (skip.has(point)) return;
    error = Math.max(error, Math.hypot(...[0, 1, 2].map((j) => gpu[3 * v + j]! - cpu[3 * point + j]!)));
  });
  return error;
}

/** The draw list's buffers drawing `path`. */
function buffersOf(list: ReturnType<typeof stagehand.drawList>, path: string): DrawBuffers {
  const item = list.items().find((candidate) => candidate.prim === path);
  assert.ok(item !== undefined, `no item draws ${path}`);
  return list.buffers(item.buffer)!;
}

/** The palette and bind transform of `query` at `time`, copied. */
function paletteAt(query: SkinningQuery, time: number): { bind: Float64Array; palette: Float64Array } {
  const transforms = query.skinningTransforms(time);
  try {
    return { bind: transforms.geomBindTransform, palette: Float64Array.from(transforms.transforms.copy() as ArrayLike<number>) };
  } finally {
    transforms.transforms.release();
  }
}

const TIMES = [1, 3.5, 7, 10];

describe("GPU skinning over a draw list's buffers", () => {
  it("names each vertex's source point, through face-varying splits", () => {
    using stage = openRobot();
    const list = stagehand.drawList(stage, { time: 1 });
    try {
      list.refresh();
      const arm = buffersOf(list, "/Robot/Arm");
      const vertexPoints = arm.mesh.vertexPoints!;
      // The seam where the strip's face-varying `st` wraps splits its two points.
      assert.equal(vertexPoints.length, 10);
      assert.equal(arm.mesh.positions.length, 3 * 10);
      assert.deepEqual([...vertexPoints].sort(), [0, 0, 1, 2, 3, 4, 4, 5, 6, 7]);
      const value = stage.attribute("/Robot/Arm.points")!.get()!;
      assert.equal(value.kind, "array");
      const points = Array.from(value.kind === "array" ? (value.array.copy() as ArrayLike<number>) : []);
      vertexPoints.forEach((point, v) => {
        assert.deepEqual([...arm.mesh.positions.subarray(3 * v, 3 * v + 3)], points.slice(3 * point, 3 * point + 3).map(Math.fround));
      });
      assert.equal(list.buffers(list.items()[0]!.buffer)!.mesh.vertexPoints?.length, list.buffers(list.items()[0]!.buffer)!.mesh.positions.length / 3);
    } finally {
      list.dispose();
    }
  });

  it("reproduces the CPU's deformed points and normals in buffer order at every time", () => {
    using stage = openRobot();
    const list = stagehand.drawList(stage, { time: 1 });
    try {
      list.refresh();
      const errors: Record<string, { points: number; normals: number }> = {};
      for (const path of ["/Robot/Arm", "/Robot/Badge"]) {
        const query = stage.skinning(path)!;
        const buffers = buffersOf(list, path);
        const vertexPoints = buffers.mesh.vertexPoints!;
        const influences = gatherInfluences(query, vertexPoints);
        assert.equal(influences.reducedBlocks, 0);
        assert.equal(influences.rigid, path === "/Robot/Badge");
        let points = 0;
        let normals = 0;
        for (const time of TIMES) {
          const { bind, palette } = paletteAt(query, time);
          const gpu = gpuSkin(buffers.mesh.positions, buffers.mesh.normals, influences, bind, palette);
          using cpuPoints = query.deformedPoints(time);
          using cpuNormals = query.deformedNormals(time);
          points = Math.max(points, largestError(gpu.positions, cpuPoints.view() as Float32Array, vertexPoints));
          normals = Math.max(normals, largestError(gpu.normals, cpuNormals.view() as Float32Array, vertexPoints));
        }
        errors[path] = { points, normals };
      }
      // Single-precision agreement: the CPU skins in f32 points and f64 matrices.
      for (const [path, { points, normals }] of Object.entries(errors)) {
        assert.ok(points < 1e-5, `${path} points off by ${points}`);
        assert.ok(normals < 1e-5, `${path} normals off by ${normals}`);
      }
      // The rest pose at time 1 is the bind pose: the arm stays where it is authored.
      const arm = stage.skinning("/Robot/Arm")!;
      using rest = arm.deformedPoints(1);
      assert.deepEqual([...(rest.view() as Float32Array)].slice(0, 3).map((v) => Math.round(v * 1e5) / 1e5), [1, -0.2, 1.3]);
    } finally {
      list.dispose();
    }
  });

  it("reduces six influences to four explicitly, exact where four suffice", () => {
    using stage = openRobot();
    const list = stagehand.drawList(stage, { time: 1 });
    try {
      list.refresh();
      const query = stage.skinning("/Robot/Tail")!;
      assert.equal(query.elementSize, 6);
      assert.notEqual(query.jointMapping, null);
      const buffers = buffersOf(list, "/Robot/Tail");
      const vertexPoints = buffers.mesh.vertexPoints!;
      const influences = gatherInfluences(query, vertexPoints);
      // Points 1 and 4 weight six joints; four are kept.
      assert.equal(influences.reducedBlocks, 2);
      assert.equal(Math.fround(influences.droppedWeight), Math.fround(0.1));
      const v = vertexPoints.indexOf(1);
      const kept = [...influences.weights.subarray(4 * v, 4 * v + 4)];
      // 0.4, 0.2, 0.15, 0.1 kept, scaled to the block's sum of 1.
      assert.deepEqual(kept.map((w) => Math.round(w * 1e5) / 1e5), [0.4, 0.2, 0.15, 0.1].map((w) => Math.round((w / 0.85) * 1e5) / 1e5));
      const exact = new Set([1, 4]);
      let reduced = 0;
      for (const time of TIMES) {
        const { bind, palette } = paletteAt(query, time);
        const gpu = gpuSkin(buffers.mesh.positions, buffers.mesh.normals, influences, bind, palette);
        using cpu = query.deformedPoints(time);
        assert.ok(largestError(gpu.positions, cpu.view() as Float32Array, vertexPoints, exact) < 1e-5, `time ${time}`);
        reduced = Math.max(reduced, largestError(gpu.positions, cpu.view() as Float32Array, vertexPoints));
      }
      // The reduced points move, posed, away from where the CPU puts them: an approximation, reported.
      assert.ok(reduced > 1e-3, `reduced points off by ${reduced}`);
      // Six slots skin them exactly.
      const six = gatherInfluences(query, vertexPoints, { slots: 6 });
      assert.equal(six.reducedBlocks, 0);
      const { bind, palette } = paletteAt(query, 10);
      using cpu = query.deformedPoints(10);
      const gpu = gpuSkin(buffers.mesh.positions, buffers.mesh.normals, six, bind, palette);
      assert.ok(largestError(gpu.positions, cpu.view() as Float32Array, vertexPoints) < 1e-5);
    } finally {
      list.dispose();
    }
  });

  it("rejects vertex points the binding has no influences for", () => {
    using stage = openRobot();
    const query = stage.skinning("/Robot/Arm")!;
    assert.throws(
      () => gatherInfluences(query, new Uint32Array([0, 8])),
      (error: unknown) => error instanceof StagehandError && error.code === "argument.invalid" && /point 8/.test(error.message),
    );
    assert.throws(() => gatherInfluences(query, new Uint32Array([0]), { slots: 0 }), StagehandError);
  });
});
