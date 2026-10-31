// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The reads a renderer makes each frame: transforms, visibility and
// purpose, primvars, bounds and point instances, checked against the
// answers OpenUSD 26.11 gives for the conformance fixtures
// (`layerstack_conformance/fixtures`), and the retained caches behind
// them hearing edits.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, Primvar, Xformable, loadLayerstack } from "../dist/index.js";
import type { BoundingBoxes, Layerstack, Stage } from "../dist/index.js";

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

const box = (bounds: BoundingBoxes, i: number) => ({
  range: [...bounds.ranges.subarray(6 * i, 6 * i + 6)],
  matrix: [...bounds.matrices.subarray(16 * i, 16 * i + 16)],
});

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

  it("say which transforms and attributes might vary with time", () => {
    const stage = layerstack.open({
      identifier: "robot.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Xform "Robot" {
    double3 xformOp:translate.timeSamples = { 1: (1, 2, 3) }
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Xform "Arm" {
        def Xform "Hand" {
            uniform token[] xformOpOrder = ["!resetXformStack!"]
            double size = 2
        }
    }
}
def Xform "Base" {
    float radius.timeSamples = { 0: 1, 10: 2 }
}
`),
    });
    // A single sample makes the robot and its arm vary (a numeric time
    // reads it, the default time does not); the hand resets the stack.
    assert.deepEqual(stage.worldTransformsMightVary(["/Robot", "/Robot/Arm", "/Robot/Arm/Hand", "/Base", "/"]), [
      true,
      true,
      false,
      false,
      false,
    ]);
    assert.deepEqual(
      stage.valueSourcesMightBeTimeVarying(["/Base.radius", "/Robot/Arm/Hand.size", "/Robot.xformOp:translate", "/Base.none"]),
      [true, false, true, false],
    );
    assert.equal(stage.attribute("/Base.radius")?.valueSourceMightBeTimeVarying(), true);
    assert.equal(failure(() => stage.worldTransformsMightVary(["/Gone"])).code, "prim.not_found");
    assert.equal(failure(() => stage.valueSourcesMightBeTimeVarying(["/Base"])).code, "path.invalid");
  });

  it("call an attribute value clips may supply varying while their metadata is not loaded", () => {
    // Whether clips supply `points` is the manifest's to say, and it is
    // not loaded: unknown counts as varying, never as a silent `false`.
    const stage = layerstack.open({
      identifier: "plate.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Mesh "Plate" (
    clips = {
        dictionary default = {
            asset[] assetPaths = [@./motion.usda@]
            asset manifestAssetPath = @./manifest.usda@
            string primPath = "/Motion"
            double2[] active = [(0, 0)]
        }
    }
    prepend references = </Base>
) {
}
class Mesh "Base" {
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
}
def Mesh "Still" {
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
}
`),
    });
    assert.deepEqual(stage.valueSourcesMightBeTimeVarying(["/Plate.points", "/Still.points"]), [true, false]);
  });
});

describe("the retained caches", () => {
  it("hear edits and undos as OpenUSD's caches recompute them", async () => {
    // `retained_queries_oracle.py`: OpenUSD's `XformCache` and `BBoxCache`
    // after each edit of the sequence; here one stage's caches hear them.
    const stage = await openFixture("", "retained_queries.usda");
    const { records } = oracle("retained_queries.json");
    const byName = new Map(records.map((record: any) => [record.name, record]));
    const check = (name: string, time?: number) => {
      const expected = byName.get(name) as any;
      const world = stage.worldTransforms(["/World/Shape"], at(time));
      assert.ok(close(world, expected.matrix.flat()), `${name} matrix`);
      const bounds = stage.worldBounds(["/World"], at(time));
      assert.ok(close(bounds.alignedRanges, [...expected.min, ...expected.max]), `${name} bounds`);
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
    const boundsBefore = stage.boundsCache();
    sized.undo();
    check("undo_size", 1);
    assert.ok(stage.boundsCache().invalidated > boundsBefore.invalidated, "the undo evicted the shape's bound");
    assert.equal(stage.transformCache().resets, 0);
  });

  it("reuse every transform an edit did not touch", () => {
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

  it("list every prim's primvars as OpenUSD's GetPrimvars does", async () => {
    // `primvar_listing_oracle.py`: name, type, interpolation, element size,
    // indexed, id target and indices of every primvar, schema-declared
    // ones included, and `FindPrimvarsWithInheritance`.
    const { scenes } = oracle("primvar_listing.json");
    let checked = 0;
    for (const [scene, prims] of Object.entries(scenes) as [string, Record<string, any>][]) {
      const stage = await openFixture("", scene);
      for (const [path, expected] of Object.entries(prims)) {
        const listed = stage.primvars(path).map((primvar) => {
          // OpenUSD's `GetIndices` is an empty array where there are none at the time.
          let indices: number[] | null = null;
          if (primvar.indexed) {
            const array = primvar.indices();
            indices = array === null ? [] : Array.from(array.copy() as Int32Array);
            array?.release();
          }
          return [primvar.name, primvar.typeName, primvar.interpolation, primvar.elementSize, primvar.indexed, primvar.idTarget, indices];
        });
        assert.deepEqual(listed, expected.primvars, `${scene} ${path}`);
        const inherited = stage
          .primvars(path, { inherited: true })
          .map((primvar) => [primvar.name, primvar.prim])
          .sort();
        assert.deepEqual(inherited, expected.inherited, `${scene} ${path} inherited`);
        checked += listed.length;
      }
    }
    assert.ok(checked > 40, `${checked} primvars checked`);
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

describe("bounds", () => {
  it("agree with OpenUSD's BBoxCache for every option, purpose and prim", async () => {
    const stage = await openFixture("bounds/", "scene.usda");
    const { records } = oracle("bounds/oracle.json");
    const spaces = {
      world: (paths: string[], options: object) => stage.worldBounds(paths, options),
      local: (paths: string[], options: object) => stage.localBounds(paths, options),
      untransformed: (paths: string[], options: object) => stage.untransformedBounds(paths, options),
    };
    let checked = 0;
    for (const record of records) {
      if (record.relative_to !== undefined) continue;
      const options = {
        ...at(record.time ?? undefined),
        purposes: record.purposes,
        useExtentsHint: record.hints,
        ignoreVisibility: record.ignore,
      };
      for (const [space, read] of Object.entries(spaces)) {
        const expected = record.bounds[space];
        const got = box(read([record.path], options), 0);
        const label = `${record.path} ${space} ${JSON.stringify(options)}`;
        // An empty box is empty whatever its range and matrix: OpenUSD
        // keeps a float extent's empty range and the prim's matrix, this
        // `GfRange3d()` and the identity.
        const empty = (range: readonly number[]) => [0, 1, 2].some((i) => range[i]! > range[i + 3]!);
        const want = [...expected.min, ...expected.max];
        assert.equal(empty(got.range), empty(want), `${label}: ${got.range}`);
        if (empty(want)) continue;
        assert.ok(close(got.range, want, 1e-12), `${label}: ${got.range}`);
        assert.ok(close(got.matrix, expected.matrix.flat(), 1e-12), `${label} matrix`);
        checked += 1;
      }
    }
    assert.ok(checked > 300, `${checked} bounds checked`);
    assert.ok(stage.boundsCache().caches <= 4);
  });

  it("compute a model's extentsHint as OpenUSD does", async () => {
    const stage = await openFixture("extents_hint/", "scene.usda");
    const { computed } = oracle("extents_hint/oracle.json");
    let checked = 0;
    for (const [path, byOptions] of Object.entries(computed) as [string, Record<string, number[][]>][]) {
      for (const [key, expected] of Object.entries(byOptions)) {
        const [time, hints] = key.split("_");
        const hint = stage.extentsHint(path, {
          ...at(time === "default" ? undefined : Number(time)),
          useExtentsHint: hints === "hints",
        });
        assert.ok(close(hint, expected.flat(), 1e-6), `${path} ${key}: ${hint}`);
        checked += 1;
      }
    }
    assert.ok(checked > 40, `${checked} hints checked`);
  });

  it("frame a batch, with the root counted even when invisible", () => {
    const stage = layerstack.open({
      identifier: "lamp.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def Xform "Lamp" {
    token visibility = "invisible"
    double3 xformOp:translate = (0, 0, 10)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Sphere "Bulb" {
        double radius = 2
    }
}
def "Empty" {
}
`),
    });
    const bounds = stage.worldBounds(["/Lamp", "/Lamp/Bulb", "/Empty"]);
    assert.equal(bounds.count, 3);
    assert.deepEqual([...bounds.alignedRanges.subarray(0, 6)], [-2, -2, 8, 2, 2, 12]);
    assert.deepEqual([...bounds.alignedRanges.subarray(6, 12)], [-2, -2, 8, 2, 2, 12]);
    assert.ok(bounds.alignedRanges[12]! > bounds.alignedRanges[15]!, "an empty box's minimum is above its maximum");
    assert.equal(failure(() => stage.worldBounds(["/Gone"])).code, "prim.not_found");
  });
});

describe("point instancers", () => {
  it("compute instance transforms as OpenUSD does", async () => {
    const stage = await openFixture("point_instancer_transforms/", "scene.usda");
    const { cases } = oracle("point_instancer_transforms/oracle.json");
    const options: Record<string, { includePrototypeTransform: boolean; applyMask: boolean }> = {
      include_mask: { includePrototypeTransform: true, applyMask: true },
      include_all: { includePrototypeTransform: true, applyMask: false },
      exclude_mask: { includePrototypeTransform: false, applyMask: true },
      exclude_all: { includePrototypeTransform: false, applyMask: false },
    };
    for (const { case: [path, time, base], transforms } of cases) {
      for (const [label, expected] of Object.entries(transforms) as [string, number[][][] | null][]) {
        const read = () =>
          stage.pointInstances(path, {
            ...at(time ?? undefined),
            ...(base === null ? {} : { baseTime: base }),
            ...options[label]!,
          });
        const context = `${path} ${time} ${base} ${label}`;
        // OpenUSD returns no transforms for a refused computation.
        if (expected === null || expected.length === 0) {
          let count = 0;
          try {
            count = read().count;
          } catch (error) {
            assert.ok(error instanceof LayerstackError, context);
          }
          assert.equal(count, 0, context);
          continue;
        }
        const instances = read();
        assert.equal(instances.count, expected.length, context);
        assert.ok(close(instances.matrices, expected.flat(2), 1e-9), context);
      }
    }
  });

  it("mask and bound instances as OpenUSD does", async () => {
    const stage = await openFixture("", "point_instancer_behavior.usda");
    const expected = oracle("point_instancer_behavior.json");
    const bools = (mask: Uint8Array) => [...mask].map((drawn) => drawn === 1);
    assert.deepEqual(bools(stage.instanceMask("/World/Instances")), expected.mask);
    assert.deepEqual(bools(stage.instanceMask("/World/ImplicitIds")), expected.implicitMask);
    const motion = stage.pointInstances("/World/Instances", { time: 2, baseTime: 0, applyMask: false });
    assert.ok(close(motion.matrices, expected.motion.flat(2)));
    for (const [label, time] of [["boundsDefault", undefined], ["boundsAt2", 2]] as const) {
      const bounds = stage.worldBounds(["/World/Instances"], at(time));
      assert.ok(close(bounds.alignedRanges, expected[label].flat()), `${label}: ${bounds.alignedRanges}`);
    }
  });

  it("report prototypes, indices, ids and the mask", () => {
    const stage = layerstack.open({
      identifier: "rocks.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
def PointInstancer "Rocks" {
    rel prototypes = [</Rocks/Pebble>, </Rocks/Boulder>]
    int[] protoIndices = [0, 1, 0]
    point3f[] positions = [(0, 0, 0), (2, 0, 0), (4, 0, 0)]
    int64[] ids = [10, 11, 12]
    int64[] invisibleIds = [11]
    def Cube "Pebble" {
    }
    def Cube "Boulder" {
    }
}
def "Loose" {
}
`),
    });
    const visible = stage.pointInstances("/Rocks");
    assert.deepEqual(visible.prototypes, ["/Rocks/Pebble", "/Rocks/Boulder"]);
    assert.equal(visible.count, 2);
    assert.deepEqual([...visible.prototypeIndices], [0, 0]);
    assert.deepEqual([...visible.ids], [10n, 12n]);
    assert.deepEqual([...visible.indices], [0, 2]);
    assert.deepEqual([...visible.matrices.subarray(28, 31)], [4, 0, 0]);
    const all = stage.pointInstances("/Rocks", { applyMask: false });
    assert.deepEqual([...all.prototypeIndices], [0, 1, 0]);
    assert.deepEqual([...stage.instanceMask("/Rocks")], [1, 0, 1]);
    assert.equal(failure(() => stage.pointInstances("/Loose")).code, "instancer.not_instancer");
  });
});

describe("mesh topology", () => {
  it("reads each mesh's topology and its validation as OpenUSD 26.11 does", async () => {
    const stage = await openFixture("mesh_topology/", "scene.usda");
    const expected = oracle("mesh_topology/oracle.json");
    for (const read of expected.meshes) {
      const label = `${read.mesh} at ${read.time}`;
      const time = read.time === "default" ? {} : { time: Number(read.time) };
      const topology = stage.prim(read.mesh)!.meshTopology(time)!;
      assert.deepEqual([...topology.faceVertexCounts], read.face_vertex_counts ?? [], label);
      assert.deepEqual([...topology.faceVertexIndices], read.face_vertex_indices ?? [], label);
      assert.deepEqual([...topology.holeIndices], read.hole_indices ?? [], label);
      assert.equal(topology.orientation, read.orientation, label);
      assert.equal(topology.subdivisionScheme, read.subdivision_scheme, label);
      assert.equal(topology.pointCount, read.points, label);
      assert.equal(topology.valid, read.valid, label);
      if (read.valid) {
        assert.equal(topology.problem, null, label);
      } else {
        const code = read.reason.startsWith("Sum of") ? "sizeMismatch" : "vertexIndex";
        assert.equal(topology.problem?.code, code, label);
      }
    }
    // Not a mesh.
    const other = layerstack.open({
      identifier: "lamp.usda",
      bytes: new TextEncoder().encode('#usda 1.0\ndef Xform "Lamp" {\n}\n'),
    });
    assert.equal(other.prim("/Lamp")!.meshTopology(), null);
  });
});

describe("native instancing", () => {
  it("lists each prototype's instance roots", () => {
    using stage = layerstack.open({
      identifier: "scene.usda",
      bytes: new TextEncoder().encode(`#usda 1.0
class "Beacon" {
    def Sphere "Bulb" {
    }
}
class "Lamp" {
    def Cube "Base" {
    }
}
def "South" (
    instanceable = true
    prepend references = </Beacon>
) {
}
def "North" (
    instanceable = true
    prepend references = </Beacon>
) {
}
def "Desk" (
    instanceable = true
    prepend references = </Lamp>
) {
}
def "Loose" (
    prepend references = </Beacon>
) {
}
`),
    });
    // In traversal order; the loose reference is no instance.
    assert.deepEqual(stage.prototypes(), [{ instances: ["/South", "/North"] }, { instances: ["/Desk"] }]);
    assert.equal(stage.prim("/North/Bulb")?.instanceProxy, true);
  });
});

describe("cameras and lights", () => {
  // `camera.json`: `UsdGeomCamera::GetCamera` and its frustum, every
  // camera at the default time and three numeric times, linear and held.
  it("computes cameras as OpenUSD does", async () => {
    const stage = await openFixture("", "camera.usda");
    for (const row of oracle("camera.json").rows) {
      if (row.interpolation !== "linear") continue;
      const camera = stage.camera(row.path, at(row.time ?? undefined))!;
      const where = `${row.path} ${row.time}`;
      assert.ok(close(camera.view, row.view.flat(), 2e-10), where);
      assert.ok(close(camera.projectionMatrix, row.projection.flat(), 2e-10), where);
      assert.ok(close(camera.cameraToWorld, row.transform.flat(), 2e-10), where);
      assert.ok(close(camera.window, row.window, 2e-10), where);
      assert.deepEqual([...camera.clippingRange], row.clipping_range, where);
      assert.deepEqual([...camera.aperture], row.aperture, where);
      assert.deepEqual([...camera.apertureOffset], row.aperture_offset, where);
      assert.equal(camera.focalLength, row.focal_length, where);
      assert.equal(camera.fStop, row.f_stop, where);
      assert.equal(camera.focusDistance, row.focus_distance, where);
      assert.deepEqual([...camera.shutter], row.shutter, where);
      assert.deepEqual([...camera.clippingPlanes], row.clipping_planes.flat(), where);
    }
    assert.equal(stage.camera("/Rig"), null, "not a camera");
    const inverted = layerstack.open({
      identifier: "inverted.usda",
      bytes: new TextEncoder().encode('#usda 1.0\ndef Camera "Eye" {\n    float2 clippingRange = (5, 1)\n}\n'),
    });
    assert.equal(failure(() => inverted.camera("/Eye")).code, "camera.invalid");
    assert.equal(
      failure(() => stage.camera("/Defaults", { conform: { policy: "squash" as "fit", aspect: 1 } })).code,
      "argument.invalid",
    );
  });

  // `camera_conform.json`: `CameraUtilConformWindow`, every policy at five
  // aspects, through OpenUSD's C++.
  it("conforms a camera's window as OpenUSD does", async () => {
    const stage = await openFixture("", "camera.usda");
    for (const row of oracle("camera_conform.json").rows) {
      const camera = stage.camera(row.path, { ...at(row.time ?? undefined), conform: { policy: row.policy, aspect: row.aspect } })!;
      const where = `${row.path} ${row.time} ${row.policy} ${row.aspect}`;
      assert.deepEqual([...camera.aperture], row.aperture.map(Math.fround), where);
      assert.ok(close(camera.window, row.window, 2e-10), where);
      assert.ok(close(camera.projectionMatrix, row.projection.flat(), 2e-10), where);
      assert.ok(close(camera.view, row.view.flat(), 2e-10), where);
    }
  });

  // `camera_exposure.json`: the exposure controls and
  // `UsdGeomCamera::ComputeLinearExposureScale`.
  it("reads a camera's exposure as OpenUSD does", async () => {
    const stage = await openFixture("", "camera_exposure.usda");
    for (const row of oracle("camera_exposure.json").rows) {
      const camera = stage.camera(row.path, at(row.time ?? undefined))!;
      const where = `${row.path} ${row.time}`;
      assert.equal(camera.exposure.exposure, row.exposure ?? 0, where);
      assert.equal(camera.exposure.fStop, row.f_stop ?? 1, where);
      assert.equal(camera.exposure.iso, row.iso ?? 100, where);
      assert.equal(camera.exposure.responsivity, row.responsivity ?? 1, where);
      assert.equal(camera.exposure.time, row.exposure_time ?? 1, where);
      assert.equal(camera.linearExposureScale, row.linear_exposure_scale, where);
    }
  });

  // `lux_helpers/oracle.json`: `UsdLuxBlackbodyTemperatureAsRgb`.
  it("converts color temperatures as OpenUSD does", () => {
    for (const { kelvin, rgb } of oracle("lux_helpers/oracle.json").blackbody) {
      if (!Number.isFinite(Math.fround(kelvin))) {
        assert.equal(failure(() => layerstack.blackbodyTemperatureRgb(kelvin)).code, "argument.invalid");
        continue;
      }
      assert.deepEqual([...layerstack.blackbodyTemperatureRgb(kelvin)], rgb, `${kelvin}`);
    }
  });

  // `stagehand_lights/oracle.json`: which lights carry `ShapingAPI` and
  // `ShadowAPI`, as `UsdPrim::HasAPI` answers.
  it("says which API schemas a prim has", async () => {
    const stage = await openFixture("stagehand_lights/", "scene.usda");
    for (const row of oracle("stagehand_lights/oracle.json").rows) {
      const prim = stage.prim(row.path)!;
      assert.equal(prim.hasAPI("ShapingAPI"), row.shaping, row.path);
      assert.equal(prim.hasAPI("ShadowAPI"), row.shadow, row.path);
      assert.ok(prim.hasAPI("LightAPI"), "built in");
      assert.ok(prim.hasAPI("CollectionAPI", "lightLink"), "built in, multiple-apply");
      assert.ok(!prim.hasAPI("CollectionAPI", "other"));
    }
  });
});
