// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The point instancer view's columns: read once from the stage, the
// `protoIndices` histogram, sorting by a permutation, the mask edits'
// id lists, and one instance's transform as
// `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` composes it.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { boxCorners, histogram, indexOfId, instanceId, instanceMatrix, multiply, ranges, readColumns, sortOrder, toggled } from "../src/model/instancer.ts";

const LAYER = `#usda 1.0
def PointInstancer "Field" (
    inactiveIds = [30]
)
{
    point3f[] positions = [(0, 0, 0), (4, 1, 0), (-2, 0, 3), (1, 1, 1)]
    quath[] orientations = [(1, 0, 0, 0), (0.7071068, 0, 0.7071068, 0), (1, 0, 0, 0), (1, 0, 0, 0)]
    float3[] scales = [(1, 1, 1), (2, 2, 2), (1, 1, 1), (0.5, 0.5, 0.5)]
    int[] protoIndices = [0, 1, 1, 5]
    int64[] ids = [10, 20, 30, 40]
    int64[] invisibleIds = [20]
    rel prototypes = [</Field/A>, </Field/B>]
    def Cube "A"
    {
        double size = 1
    }
    def Sphere "B"
    {
    }
}
`;

// The same instances, each naming a prototype, for OpenUSD's own transforms.
const GROVE = `
def PointInstancer "Grove"
{
    point3f[] positions = [(0, 0, 0), (4, 1, 0), (-2, 0, 3), (1, 1, 1)]
    quath[] orientations = [(1, 0, 0, 0), (0.7071068, 0, 0.7071068, 0), (0.9238795, 0.3826834, 0, 0), (1, 0, 0, 0)]
    float3[] scales = [(1, 1, 1), (2, 2, 2), (1, 3, 1), (0.5, 0.5, 0.5)]
    int[] protoIndices = [0, 1, 1, 0]
    rel prototypes = [</Grove/A>, </Grove/B>]
    def Cube "A"
    {
        double3 xformOp:translate = (0, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Sphere "B"
    {
    }
}
`;

const layerstack = await loadLayerstack();
const stage = layerstack.open({ identifier: "field.usda", bytes: new TextEncoder().encode(LAYER + GROVE) });

describe("point instancer columns", () => {
  const columns = readColumns(stage, "/Field", undefined);

  it("read each authored array once, the mask and the id lists", () => {
    assert.equal(columns.count, 4);
    assert.deepEqual(columns.prototypes, ["/Field/A", "/Field/B"]);
    assert.deepEqual([...columns.protoIndices], [0, 1, 1, 5]);
    assert.deepEqual([...columns.ids!], [10n, 20n, 30n, 40n]);
    assert.deepEqual([...columns.positions!].slice(3, 6), [4, 1, 0]);
    // Half floats decoded, `(r, i, j, k)` authored, `[i, j, k, r]` read.
    assert.ok(Math.abs(columns.orientations![4]! - 0) < 1e-3 && Math.abs(columns.orientations![5]! - 0.7071) < 1e-3, `${[...columns.orientations!]}`);
    assert.deepEqual(columns.invisibleIds, [20n]);
    assert.deepEqual(columns.inactiveIds, [30n]);
    assert.deepEqual([...columns.mask], [1, 0, 0, 1], "20 invisible, 30 inactive");
  });

  it("count instances per prototype, and those naming none", () => {
    const { counts, outOfRange } = histogram(columns.protoIndices, columns.prototypes.length);
    assert.deepEqual([...counts], [1, 2]);
    assert.equal(outOfRange, 1);
  });

  it("sort by an index permutation, ties by index", () => {
    assert.deepEqual([...sortOrder(columns, "index", false)], [0, 1, 2, 3]);
    assert.deepEqual([...sortOrder(columns, "x", false)], [2, 0, 3, 1]);
    assert.deepEqual([...sortOrder(columns, "x", true)], [1, 3, 0, 2]);
    assert.deepEqual([...sortOrder(columns, "prototype", false)], [0, 1, 2, 3]);
    assert.deepEqual([...sortOrder(columns, "scale", false)], [3, 0, 2, 1]);
    assert.deepEqual([...sortOrder(columns, "id", true)], [3, 2, 1, 0]);
  });

  it("sort large columns as a comparison sort would, negatives, ties and all", () => {
    const count = 50000;
    const positions = new Float32Array(count * 3);
    let seed = 7;
    const random = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648) * 200 - 100;
    for (let at = 0; at < positions.length; at++) positions[at] = at % 7 === 0 ? 0 : Math.round(random() * 4) / 4;
    const big = { ...columns, count, positions, protoIndices: new Int32Array(count), ids: null, scales: null };
    for (const descending of [false, true]) {
      const ours = sortOrder(big, "y", descending);
      const sign = descending ? -1 : 1;
      const expected = Uint32Array.from({ length: count }, (_, at) => at).sort((a, b) => sign * (positions[a * 3 + 1]! - positions[b * 3 + 1]!) || a - b);
      assert.deepEqual(ours, expected, descending ? "descending" : "ascending");
    }
  });

  it("name instances by id, and toggle ids in a mask list", () => {
    assert.equal(instanceId(columns, 2), 30n);
    assert.equal(indexOfId(columns, 40n), 3);
    assert.equal(indexOfId(columns, 41n), -1);
    assert.equal(instanceId({ ids: null }, 2), 2n);
    assert.equal(indexOfId({ ids: null, count: 4 }, 3n), 3);
    assert.deepEqual(toggled([20n], 30n), [20n, 30n]);
    assert.deepEqual(toggled([20n, 30n], 20n), [30n]);
  });

  it("summarize positions and scales by axis", () => {
    const spread = ranges(columns.positions, 3)!;
    assert.deepEqual(spread.min, [-2, 0, 0]);
    assert.deepEqual(spread.max, [4, 1, 3]);
    assert.equal(ranges(null, 3), null);
  });

  it("compose an instance's transform as OpenUSD does: scale, orientation, translation", () => {
    // A quarter turn about +y, scale 2, at (4, 1, 0): +x goes to -z, doubled, then moved.
    const m = instanceMatrix(columns, 1);
    const x = [m[0]! + m[12]!, m[1]! + m[13]!, m[2]! + m[14]!];
    assert.ok(Math.abs(x[0]! - 4) < 1e-3 && Math.abs(x[1]! - 1) < 1e-3 && Math.abs(x[2]! + 2) < 1e-3, `${x}`);
    // As OpenUSD composes it, the prototype's own transform first: the stage's instance transforms.
    const grove = readColumns(stage, "/Grove", undefined);
    const native = stage.pointInstances("/Grove", { applyMask: false });
    for (let index = 0; index < grove.count; index++) {
      const prototype = grove.prototypes[grove.protoIndices[index]!]!;
      const ours = multiply(stage.localTransform(prototype).matrix, instanceMatrix(grove, index));
      const theirs = native.matrices.subarray(index * 16, index * 16 + 16);
      ours.forEach((value, at) => assert.ok(Math.abs(value - theirs[at]!) < 1e-3, `${index}[${at}]: ${value} vs ${theirs[at]}`));
    }
    const corners = boxCorners([-1, -1, -1, 1, 1, 1], m);
    assert.equal(corners.length, 24);
  });
});
