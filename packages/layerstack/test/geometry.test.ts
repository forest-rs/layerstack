// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The reads a renderer makes each frame: transforms, visibility and
// purpose and primvars, checked against the
// answers OpenUSD 26.11 gives for the conformance fixtures
// (`layerstack_conformance/fixtures`), and the retained caches behind
// them hearing edits.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, Primvar, Xformable, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();
const fixtures = new URL("../../../layerstack_conformance/fixtures/", import.meta.url);

/** Opens the fixture `root` under `dir`, supplying what it references from the same directory. */
async function openFixture(dir: string, root: string): Promise<Stage> {
  const base = new URL(dir, fixtures);
  const stage = layerstack.open({ identifier: root, bytes: readFileSync(new URL(root, base)) });
  await stage.resolve({
    fetch: async (identifier) => {
      try {
        return readFileSync(new URL(identifier, base));
      } catch {
        return null;
      }
    },
  });
  return stage;
}

function oracle(path: string): any {
  return JSON.parse(readFileSync(new URL(path, fixtures), "utf8"));
}

/** Whether `actual` is `expected` to within `tolerance` of its scale, number by number. */
function close(actual: ArrayLike<number>, expected: readonly number[], tolerance = 1e-9): boolean {
  if (actual.length !== expected.length) return false;
  const scale = Math.max(1, ...expected.map(Math.abs));
  return expected.every((value, i) => Math.abs((actual[i] as number) - value) <= tolerance * scale);
}

function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, String(error));
    return error;
  }
  assert.fail("expected a LayerstackError");
}

/** `{ time }`, or no time at all for the default time. */
const at = (time: number | undefined): { time?: number } => (time === undefined ? {} : { time });

describe("transforms, visibility and purpose", () => {
  it("agree with OpenUSD for every prim of the transform fixture", async () => {
    const stage = await openFixture("xform/", "scene.usda");
    const { prims } = oracle("xform/oracle.json");
    const paths = Object.keys(prims);
    // One batched read per time, as a renderer makes each frame.
    for (const [label, time] of [["default", undefined], ...[0, 1.5, 2, 3].map((t) => [`linear@${t.toFixed(1)}`, t])] as [
      string,
      number | undefined,
    ][]) {
      const world = stage.worldTransforms(paths, at(time));
      assert.equal(world.length, 16 * paths.length);
      const visibility = stage.visibilities(paths, at(time));
      paths.forEach((path, i) => {
        const sample = prims[path].samples[label];
        assert.ok(sample, `${path} ${label}`);
        assert.ok(close(world.subarray(16 * i, 16 * i + 16), sample.world.flat(), 1e-12), `${path} world at ${label}`);
        if (sample.local !== undefined) {
          const local = stage.localTransform(path, at(time));
          assert.ok(close(local.matrix, sample.local.flat(), 1e-12), `${path} local at ${label}`);
          assert.equal(local.resetsXformStack, sample.resets_xform_stack, `${path} reset at ${label}`);
        }
        assert.equal(visibility[i], sample.visibility ?? null, `${path} visibility at ${label}`);
        if (sample.effective_visibility !== undefined) {
          for (const [purpose, expected] of Object.entries(sample.effective_visibility)) {
            assert.equal(stage.visibilities([path], { ...at(time), purpose })[0], expected, `${path} ${purpose} at ${label}`);
          }
        }
      });
    }
    const purposes = stage.purposeInfos(paths);
    paths.forEach((path, i) => {
      const expected = prims[path].purpose;
      if (expected === undefined) {
        assert.equal(purposes[i], null, path);
      } else {
        assert.deepEqual(
          purposes[i],
          { purpose: expected.purpose, inheritable: expected.inheritable, authoredOn: expected.authored_on },
          path,
        );
      }
    });
  });

  it("report a prim's ops, reset, samples and parent", () => {
    const stage = layerstack.open({
      identifier: "robot.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Xform "Robot" {
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Xform "Arm" {
        float xformOp:rotateZ.timeSamples = { 0: 0, 10: 90 }
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:rotateZ", "xformOp:scale"]
    }
}
def "Loose" {
}
`),
    });
    const arm = stage.xformable("/Robot/Arm");
    assert.ok(arm instanceof Xformable);
    assert.equal(arm.resetsXformStack, true);
    assert.deepEqual(arm.ops, [{ name: "xformOp:rotateZ", attribute: "xformOp:rotateZ", type: "rotateZ", inverse: false }]);
    assert.equal(arm.transformMightBeTimeVarying, true);
    assert.deepEqual(arm.transformTimeSamples, [0, 10]);
    assert.equal(stage.xformable("/Loose"), null);
    const local = stage.localTransform("/Robot/Arm", { time: 10 });
    assert.equal(local.resetsXformStack, true);
    assert.deepEqual(local.problems, [{ op: "xformOp:scale", kind: "noAttribute" }]);
    // Reset: the arm's world transform is its local one; its parent's is the robot's.
    assert.deepEqual([...stage.worldTransforms(["/Robot/Arm"], { time: 10 })], [...local.matrix]);
    assert.deepEqual([...stage.parentToWorldTransforms(["/Robot/Arm"]).subarray(12)], [1, 2, 3, 1]);
    assert.deepEqual([...stage.prim("/Robot")!.worldTransform().subarray(12)], [1, 2, 3, 1]);
    assert.deepEqual([...stage.worldTransforms(["/"])], [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
    assert.equal(stage.prim("/Loose")!.visibility(), null);
    assert.equal(failure(() => stage.worldTransforms(["/Robot", "/Gone"])).code, "prim.not_found");
  });
});

describe("the retained transform cache", () => {
  it("hears edits and undos as OpenUSD's cache recomputes them", async () => {
    // `retained_queries_oracle.py`: OpenUSD's `XformCache` after each edit
    // of the sequence; here one stage's cache hears them.
    const stage = await openFixture("", "retained_queries.usda");
    const { records } = oracle("retained_queries.json");
    const byName = new Map(records.map((record: any) => [record.name, record]));
    const check = (name: string, time?: number) => {
      const expected = byName.get(name) as any;
      const world = stage.worldTransforms(["/World/Shape"], at(time));
      assert.ok(close(world, expected.matrix.flat()), `${name} matrix`);
    };
    const set = (path: string, type: string, value: unknown) =>
      stage.apply([{ type: "setValue", path, value: { type, value } as never }]);
    check("initial");
    set("/World.inputs:unrelated", "double", 2);
    check("unrelated");
    const before = stage.transformCache();
    const translated = set("/World.xformOp:translate", "double3", [4, 0, 0]);
    check("translated");
    const after = stage.transformCache();
    assert.ok(after.changeBatches > before.changeBatches, "the edit was heard");
    assert.ok(after.localComputed > before.localComputed, "the edited transform was recomputed");
    translated.undo();
    check("undo_translate");
    check("time_1", 1);
    const sized = set("/World/Shape.size", "double", 4);
    check("size_4", 1);
    sized.undo();
    check("undo_size", 1);
    assert.equal(stage.transformCache().resets, 0);
  });

  it("reuses every transform an edit did not touch", () => {
    const stage = layerstack.open({
      identifier: "row.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Xform "Row" {
    def Xform "A" {
        double3 xformOp:translate = (1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "B" {
        double3 xformOp:translate = (2, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
`),
    });
    const paths = ["/Row/A", "/Row/B"];
    stage.worldTransforms(paths);
    const warm = stage.transformCache();
    stage.worldTransforms(paths);
    const reused = stage.transformCache();
    assert.equal(reused.localComputed, warm.localComputed, "a second frame computes nothing");
    assert.ok(reused.hits > warm.hits);
    stage.apply([{ type: "setValue", path: "/Row/B.xformOp:translate", value: { type: "double3", value: [5, 0, 0] } }]);
    const moved = stage.worldTransforms(paths);
    assert.deepEqual([...moved.subarray(28, 31)], [5, 0, 0]);
    assert.equal(stage.transformCache().localComputed, reused.localComputed + 1, "only B's local transform is recomputed");
  });
});

describe("primvars", () => {
  it("list, inherit and flatten as OpenUSD does", async () => {
    const stage = await openFixture("", "primvar_behavior.usda");
    const expected = oracle("primvar_behavior.json");
    const mesh = "/Root/Group/Mesh";
    for (const name of ["packed", "scalar", "animated", "blockedIndices", "empty"]) {
      const primvar = stage.primvar(mesh, name);
      assert.ok(primvar instanceof Primvar, name);
      assert.equal(primvar.interpolation, expected[name].interpolation, name);
      assert.equal(primvar.elementSize, expected[name].elementSize, name);
      assert.equal(primvar.indexed, expected[name].indexed, name);
      const value = primvar.flattened();
      const flat = (v: typeof value) =>
        v === null
          ? null
          : v.kind === "array"
            ? Array.from(v.array.copy() as ArrayLike<number>)
            : v.kind === "scalar"
              ? v.value
              : v.kind === "tuple"
                ? [...v.value]
                : v.kind;
      const want = expected[name].default;
      const got = flat(value);
      if (Array.isArray(want)) {
        assert.ok(Array.isArray(got) && close(got, want.flat()), `${name}: ${JSON.stringify(got)}`);
      } else {
        assert.deepEqual(got, want, name);
      }
    }
    const animated = stage.primvar(mesh, "animated")!.flattened(1);
    assert.ok(animated?.kind === "array" && close(animated.array.copy() as Float32Array, expected.animated.at1.flat()));
    assert.deepEqual(
      stage.primvars(mesh, { inherited: true }).map((primvar) => primvar.name),
      expected.inheritedNames,
    );
  });

  it("inherit from the ancestors OpenUSD finds", async () => {
    const stage = await openFixture("", "primvar_queries.usda");
    const { inheritance } = oracle("primvar_queries.json");
    for (const [path, expected] of Object.entries(inheritance) as [string, any][]) {
      const found = stage
        .primvars(path, { inherited: true })
        .map((primvar) => [primvar.name, primvar.prim])
        .sort();
      assert.deepEqual(found, expected.all, path);
    }
  });

  it("view indices and flattened values as typed arrays", () => {
    const stage = layerstack.open({
      identifier: "rock.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Mesh "Rock" {
    texCoord2f[] primvars:st = [(0, 0), (1, 1)] (
        interpolation = "faceVarying"
    )
    int[] primvars:st:indices = [0, 1, 1]
}
`),
    });
    const st = stage.prim("/Rock")!.primvar("st")!;
    assert.equal(st.typeName, "texCoord2f[]");
    assert.equal(st.path, "/Rock.primvars:st");
    using indices = st.indices()!;
    assert.deepEqual([...(indices.view() as Int32Array)], [0, 1, 1]);
    const flat = st.flattened();
    assert.ok(flat?.kind === "array");
    assert.ok(flat.array.view() instanceof Float32Array);
    assert.deepEqual([...(flat.array.view() as Float32Array)], [0, 0, 1, 1, 1, 1]);
    flat.array.release();
    assert.equal(stage.primvar("/Rock", "nothing"), null);
  });
});
