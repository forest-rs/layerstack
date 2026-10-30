// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A stage read through `@forest-rs/layerstack`, handed to this package's
// own module as typed arrays, and the buffers and reports that come back.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Stage } from "@forest-rs/layerstack";
import { StagehandError, loadStagehand, readMesh, readPoints, readShape } from "../dist/index.js";
import type { StagehandMesh } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

const SCENE = `#usda 1.0
def Xform "World" {
    color3f[] primvars:displayColor = [(0.25, 0.5, 0.75)]

    def Mesh "Rock" {
        int[] faceVertexCounts = [6, 4, 4, 2]
        int[] faceVertexIndices = [2, 3, 4, 5, 0, 1, 1, 6, 7, 2, 5, 4, 8, 9, 0, 1]
        int[] holeIndices = [2]
        point3f[] points = [(0, 0, 0), (2, 0, 0), (2, 1, 0), (1, 1, 0), (1, 2, 0), (0, 2, 0), (3, 0, 0), (3, 1, 0), (1, 3, 0), (0, 3, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
            interpolation = "faceVarying"
        )
        int[] primvars:st:indices = [0, 1, 2, 2, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1]
        uniform token subdivisionScheme = "catmullClark"

        def GeomSubset "Moss" {
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [1]
        }
    }

    def Mesh "Robot" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        uniform token orientation = "leftHanded"
        uniform token subdivisionScheme = "none"
        point3f[] points.timeSamples = {
            1: [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)],
            2: [(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)],
        }
    }

    def Mesh "Broken" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 9]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    }

    def Capsule "Lamp" {
        double radius = 0.25
        uniform token axis = "Y"
    }

    def Cylinder_1 "Post" {
        double radiusTop = 0.5
    }

    def Capsule_1 "Bulb" {
        double radiusTop = 0.25
        uniform token axis = "X"
    }
}
`;

function open(): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
}

/** Twice the area of each triangle, signed by its facing along +Z. */
function areas(mesh: StagehandMesh): number[] {
  const out: number[] = [];
  for (let t = 0; t < mesh.indices.length; t += 3) {
    const [a, b, c] = [0, 1, 2].map((k) => 3 * mesh.indices[t + k]!);
    const p = mesh.positions;
    const ux = p[b!]! - p[a!]!;
    const uy = p[b! + 1]! - p[a! + 1]!;
    const vx = p[c!]! - p[a!]!;
    const vy = p[c! + 1]! - p[a! + 1]!;
    out.push(ux * vy - uy * vx);
  }
  return out;
}

describe("meshes", () => {
  it("reads a mesh off the stage and builds its buffers, ranges and report", () => {
    using stage = open();
    const input = readMesh(stage, "/World/Rock")!;
    assert.deepEqual([...input.faceVertexCounts], [6, 4, 4, 2]);
    assert.equal(input.st?.interpolation, "faceVarying");
    assert.equal(input.displayColor?.interpolation, "constant", "inherited from /World");
    assert.deepEqual(input.subsets?.map((s) => s.path), ["/World/Rock/Moss"]);
    using mesh = stagehand.buildMesh(input);
    const report = mesh.report!;
    assert.equal(report.faces, 4);
    assert.equal(report.drawnFaces, 2);
    assert.equal(report.holeFaces, 1);
    assert.equal(report.degenerateFaces, 1);
    assert.equal(report.concaveFaces, 1);
    assert.equal(report.clippedFaces, 1);
    assert.equal(report.triangles, 6);
    assert.equal(report.subdivisionScheme, "catmullClark");
    assert.equal(report.refined, false);
    assert.deepEqual(report.channels.st, { state: "used", interpolation: "faceVarying", indexed: true });
    assert.deepEqual(
      report.faceNotes.map((note) => [note.face, note.kind]),
      [[0, "concaveClipped"], [2, "hole"], [3, "degenerate"]],
    );
    // The subset's face (1) first, then the L (0).
    assert.deepEqual([...mesh.triangleFaces], [1, 1, 0, 0, 0, 0]);
    assert.deepEqual(mesh.ranges, [
      { subset: 0, first: 0, count: 6 },
      { subset: null, first: 6, count: 12 },
    ]);
    // The L's triangles cover its area, 3, and none folds back.
    const doubled = areas(mesh);
    assert.ok(doubled.every((area) => area >= 0));
    assert.equal(doubled.slice(2).reduce((sum, area) => sum + area, 0) / 2, 3);
    assert.equal(mesh.colors!.length, (mesh.positions.length / 3) * 3);
    assert.deepEqual([...mesh.colors!.subarray(0, 3)], [0.25, 0.5, 0.75]);
    assert.equal(mesh.st!.length, (mesh.positions.length / 3) * 2);
    assert.equal(report.vertices, mesh.positions.length / 3);
  });

  it("fans every face as Hydra does when asked", () => {
    using stage = open();
    using mesh = stagehand.buildMesh(readMesh(stage, "/World/Rock")!, { triangulation: "hydraFan" });
    assert.equal(mesh.report!.triangulation, "hydraFan");
    assert.deepEqual(
      mesh.report!.faceNotes.map((note) => note.kind),
      ["concaveFanned", "hole", "degenerate"],
    );
    // The fan of the L from its first corner, (2, 1), folds a triangle back.
    assert.ok(areas(mesh).some((area) => area < 0));
  });

  it("flips a left-handed mesh and refills its points without triangulating again", () => {
    using stage = open();
    using mesh = stagehand.buildMesh(readMesh(stage, "/World/Robot", { time: 1 })!);
    assert.equal(mesh.report!.orientation, "leftHanded");
    assert.ok(areas(mesh).every((area) => area < 0), "clockwise seen from +Z");
    assert.ok([...mesh.normals].every((n, i) => i % 3 !== 2 || n === -1));
    const indices = [...mesh.indices];
    mesh.refill(readPoints(stage, "/World/Robot", { time: 2 })!);
    assert.deepEqual([...mesh.indices], indices);
    assert.ok([...mesh.positions].every((p, i) => i % 3 !== 2 || p === 1));
    const error = (() => {
      try {
        mesh.refill(new Float32Array(3));
      } catch (error) {
        return error as StagehandError;
      }
      return null;
    })();
    assert.equal(error?.code, "plan.mismatch");
    assert.match(error!.message, /^refill: the plan has 4 points, the input 1$/);
  });

  it("computes Hydra's smooth normals by default, and face-oriented ones on request", () => {
    // A lone L: its reflex corner (point 3) turns the other way.
    const input = {
      faceVertexCounts: new Int32Array([6]),
      faceVertexIndices: new Int32Array([0, 1, 2, 3, 4, 5]),
      points: new Float32Array([0, 0, 0, 2, 0, 0, 2, 1, 0, 1, 1, 0, 1, 2, 0, 0, 2, 0]),
    };
    const zOfPoint3 = (mesh: StagehandMesh) => {
      for (let v = 0; v < mesh.positions.length / 3; v++) {
        if (mesh.positions[3 * v] === 1 && mesh.positions[3 * v + 1] === 1) return mesh.normals[3 * v + 2];
      }
      return undefined;
    };
    using hydra = stagehand.buildMesh(input);
    assert.equal(hydra.report!.smoothNormals, "hydra");
    assert.equal(zOfPoint3(hydra), -1, "Hydra's: facing away at the reflex corner");
    using oriented = stagehand.buildMesh(input, { normals: "faceOriented" });
    assert.equal(oriented.report!.smoothNormals, "faceOriented");
    assert.ok([...oriented.normals].every((n, i) => n === (i % 3 === 2 ? 1 : 0)), "every normal faces +Z");
    // A refill keeps the mode.
    oriented.refill(input.points);
    assert.equal(zOfPoint3(oriented), 1);
  });

  it("reports an invalid topology and draws nothing", () => {
    using stage = open();
    const input = readMesh(stage, "/World/Broken")!;
    using mesh = stagehand.buildMesh(input);
    assert.equal(mesh.report!.topologyProblem?.code, "vertexIndex");
    assert.equal(mesh.indices.length, 0);
    assert.equal(readMesh(stage, "/World/Lamp"), null);
  });

  it("throws for values that are not whole elements, and after disposal", () => {
    const input = {
      faceVertexCounts: new Int32Array([3]),
      faceVertexIndices: new Int32Array([0, 1, 2]),
      points: new Float32Array(8),
    };
    assert.throws(() => stagehand.buildMesh(input), { code: "argument.invalid" });
    const mesh = stagehand.buildMesh({ ...input, points: new Float32Array(9) });
    mesh.dispose();
    assert.throws(() => mesh.refill(new Float32Array(9)), { code: "mesh.disposed" });
  });
});

describe("implicit shapes", () => {
  it("reads a shape off the stage and tessellates it at the given segments", () => {
    using stage = open();
    const shape = readShape(stage, "/World/Lamp")!;
    assert.deepEqual(shape, { kind: "Capsule", radius: 0.25, height: 1, axis: "Y" });
    const mesh = stagehand.tessellate(shape, { segments: 8, rings: 4 });
    assert.equal(mesh.report, null);
    assert.equal(mesh.positions.length / 3, (2 * 2 + 2) * 9);
    assert.equal(mesh.indices.length / 3, 2 * 8 * 4);
    // Along Y: half the height plus the radius.
    const ys = [...mesh.positions].filter((_, i) => i % 3 === 1);
    assert.ok(Math.abs(Math.max(...ys) - 0.75) < 1e-6);
    // The `_1` revisions: a radius per end.
    const post = readShape(stage, "/World/Post")!;
    assert.deepEqual(post, { kind: "Cylinder_1", radiusTop: 0.5, radiusBottom: 1, height: 2, axis: "Z" });
    const frustum = stagehand.tessellate(post, { segments: 8 });
    assert.equal(frustum.positions.length / 3, 4 * 9 + 2);
    assert.equal(frustum.indices.length / 3, 4 * 8);
    const bulb = readShape(stage, "/World/Bulb")!;
    assert.deepEqual(bulb, { kind: "Capsule_1", radiusTop: 0.25, radiusBottom: 0.5, height: 1, axis: "X" });
    const capsule = stagehand.tessellate(bulb, { segments: 8, rings: 4 });
    assert.equal(capsule.indices.length / 3, 2 * 8 * 4);
    // Along X, the poles reach OpenUSD's extent (`capsule_1.cpp`): the
    // spheres sit inward by slope * radius, slope (0.5 - 0.25) / 1.
    const xs = [...capsule.positions].filter((_, i) => i % 3 === 0);
    const secant = Math.hypot(1, 0.25);
    assert.ok(Math.abs(Math.max(...xs) - (0.5 - 0.0625 + 0.25 * secant)) < 1e-6);
    assert.ok(Math.abs(Math.min(...xs) - (-0.5 - 0.125 - 0.5 * secant)) < 1e-6);
    const cube = stagehand.tessellate({ kind: "Cube", size: 2 });
    assert.equal(cube.indices.length, 36);
    assert.throws(() => cube.refill(cube.positions), { code: "argument.invalid" });
  });
});
