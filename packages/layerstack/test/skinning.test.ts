// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The trace an engine takes to skin a mesh each frame: the binding, its
// influences and joint palette as typed arrays, or the deformed points
// and normals, and the skinning functions over its own arrays.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, SkinningQuery, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();

/** A robot whose arm joint moves +2 in x from time 0 to 10, a mesh whose last two points follow it, and a mesh with one weight for two joint indices. */
const robot = `#usda 1.0
def SkelRoot "Robot" (
    prepend apiSchemas = ["SkelBindingAPI"]
) {
    rel skel:skeleton = </Robot/Skel>
    def Skeleton "Skel" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        uniform token[] joints = ["Root", "Root/Arm"]
        uniform matrix4d[] bindTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1))]
        uniform matrix4d[] restTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1))]
        rel skel:animationSource = </Robot/Skel/Wave>
        def SkelAnimation "Wave" {
            uniform token[] joints = ["Root/Arm"]
            float3[] translations.timeSamples = {
                0: [(0, 1, 0)],
                10: [(2, 1, 0)],
            }
            quatf[] rotations = [(1, 0, 0, 0)]
            half3[] scales = [(1, 1, 1)]
        }
    }
    def Mesh "Arm" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0, 1, 0), (1, 1, 0)]
        normal3f[] normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1)] (
            interpolation = "vertex"
        )
        int[] primvars:skel:jointIndices = [0, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
    def Mesh "Bad" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0, 1, 0), (1, 1, 0)]
        int[] primvars:skel:jointIndices = [0, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
}
def Mesh "Loose" {
}
`;

function openRobot(): Stage {
  return layerstack.open({ identifier: "robot.usda", bytes: new TextEncoder().encode(robot) });
}

function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
const moved = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 2, 0, 0, 1];

describe("an engine skinning a mesh", () => {
  it("reads the binding, its influences and palette, and the deformed points each frame", () => {
    using stage = openRobot();
    const skin = stage.skinning("/Robot/Arm");
    assert.ok(skin instanceof SkinningQuery);
    assert.equal(skin.skeleton, "/Robot/Skel");
    assert.deepEqual(skin.joints, ["Root", "Root/Arm"]);
    assert.equal(skin.jointMapping, null);
    assert.equal(skin.elementSize, 1);
    assert.equal(skin.interpolation, "vertex");
    assert.equal(skin.rigid, false);
    assert.equal(skin.method, "classicLinear");
    assert.equal(skin.animation, "/Robot/Skel/Wave");
    assert.equal(stage.skinning("/Loose"), null);
    assert.equal(stage.prim("/Robot/Arm")?.skinning()?.path, "/Robot/Arm");

    const { indices, weights } = skin.influences();
    assert.ok(indices.view() instanceof Int32Array);
    assert.deepEqual([...(indices.copy() as Int32Array)], [0, 1, 1]);
    assert.deepEqual([...(weights.copy() as Float32Array)], [1, 1, 1]);
    const pairs = skin.interleavedInfluences();
    assert.equal(pairs.type, "float2[]");
    assert.deepEqual([...(pairs.copy() as Float32Array)], [0, 1, 1, 1, 1, 1]);

    const palette = skin.skinningTransforms(10);
    assert.equal(palette.transforms.type, "matrix4d[]");
    assert.ok(palette.transforms.view() instanceof Float64Array);
    assert.deepEqual([...(palette.transforms.copy() as Float64Array)], [...identity, ...moved]);
    assert.deepEqual([...palette.geomBindTransform], identity);

    for (const [time, x] of [
      [0, 0],
      [5, 1],
      [10, 2],
    ] as const) {
      using points = skin.deformedPoints(time);
      assert.ok(points.view() instanceof Float32Array);
      assert.deepEqual([...(points.view() as Float32Array)], [0, 0, 0, x, 1, 0, x + 1, 1, 0], `time ${time}`);
    }
    assert.deepEqual([...(skin.deformedNormals(5).copy() as Float32Array)], [0, 0, 1, 0, 0, 1, 0, 0, 1]);

    // CPU skinning of the engine's own arrays matches, with either influence layout.
    const points = new Float32Array([0, 0, 0, 0, 1, 0, 1, 1, 0]);
    const jointTransforms = palette.transforms.copy() as Float64Array;
    const separate = layerstack.skinPoints({
      geomBindTransform: palette.geomBindTransform,
      jointTransforms,
      influences: { indices: indices.copy() as Int32Array, weights: weights.copy() as Float32Array, elementSize: 1 },
      points,
    });
    assert.deepEqual([...separate], [0, 0, 0, 2, 1, 0, 3, 1, 0]);
    const interleaved = layerstack.skinPoints({
      geomBindTransform: identity,
      jointTransforms,
      influences: { interleaved: pairs.copy() as Float32Array, elementSize: 1 },
      points,
    });
    assert.deepEqual([...interleaved], [...separate]);
    assert.deepEqual(
      [
        ...layerstack.skinNormals({
          method: "dualQuaternion",
          geomBindTransform: identity,
          jointTransforms,
          influences: { interleaved: pairs.copy() as Float32Array, elementSize: 1 },
          normals: new Float32Array([0, 0, 1, 0, 0, 1, 0, 0, 1]),
        }),
      ],
      [0, 0, 1, 0, 0, 1, 0, 0, 1],
    );
    const rigid = layerstack.skinTransform({
      geomBindTransform: identity,
      jointTransforms,
      influences: { indices: new Int32Array([1]), weights: new Float32Array([1]), elementSize: 1, constant: true },
    });
    assert.deepEqual([...rigid], moved);
  });

  it("hears an edit of the animation before the next frame", () => {
    using stage = openRobot();
    const skin = stage.skinning("/Robot/Arm")!;
    assert.deepEqual([...(skin.deformedPoints(10).copy() as Float32Array)], [0, 0, 0, 2, 1, 0, 3, 1, 0]);
    const first = stage.skinningCache();
    assert.equal(first.misses, 1);
    assert.equal(first.bindings, 1);
    skin.deformedPoints(10).release();
    const again = stage.skinningCache();
    assert.equal(again.hits, first.hits + 1, "the same frame is reused");
    assert.equal(again.pointVertices, first.pointVertices, "nothing skinned again");
    const receipt = stage.apply([
      {
        type: "setValue",
        path: "/Robot/Skel/Wave.translations",
        time: 10,
        value: { type: "float3[]", value: new Float32Array([4, 1, 0]) },
      },
    ]);
    assert.deepEqual([...(skin.deformedPoints(10).copy() as Float32Array)], [0, 0, 0, 4, 1, 0, 5, 1, 0]);
    assert.ok(stage.skinningCache().invalidations > again.invalidations, "the edit invalidated the arm");
    receipt.undo();
    assert.deepEqual([...(skin.deformedPoints(10).copy() as Float32Array)], [0, 0, 0, 2, 1, 0, 3, 1, 0]);
  });

  it("refuses mismatched influences and unbound prims with their codes", () => {
    using stage = openRobot();
    const bad = stage.skinning("/Robot/Bad")!;
    const error = failure(() => bad.influences());
    assert.equal(error.code, "skel.influences");
    assert.equal(error.path, "/Robot/Bad");
    assert.match(error.message, /\/Robot\/Bad/);
    assert.equal(failure(() => bad.deformedPoints()).code, "skel.deformation");
    assert.equal(failure(() => stage.skinning("/Robot/Gone")).code, "prim.not_found");
    const twoIndicesOneWeight = failure(() =>
      layerstack.skinPoints({
        geomBindTransform: identity,
        jointTransforms: [...identity, ...moved],
        influences: { indices: new Int32Array([0, 1]), weights: new Float32Array([1]), elementSize: 1 },
        points: new Float32Array([0, 0, 0, 0, 1, 0]),
      }),
    );
    assert.equal(twoIndicesOneWeight.code, "skel.deformation");
    assert.equal(twoIndicesOneWeight.operation, "skinPoints");
    assert.equal(
      failure(() =>
        layerstack.skinPoints({
          geomBindTransform: identity.slice(1),
          jointTransforms: identity,
          influences: { indices: new Int32Array([0]), weights: new Float32Array([1]), elementSize: 1 },
          points: new Float32Array([0, 0, 0]),
        }),
      ).code,
      "argument.invalid",
    );
  });
});
