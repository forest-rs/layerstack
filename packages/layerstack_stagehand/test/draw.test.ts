// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The draw list over a stage read through `@forest-rs/layerstack`: the same
// items, diffs and work counts as `layerstack_stagehand::DrawList`'s tests.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit, Stage } from "@forest-rs/layerstack";
import { loadStagehand } from "../dist/index.js";
import type { DrawDiff, DrawList } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

const SCENE = `#usda 1.0
def Xform "World" {
    def Xform "Rocks" {
        def Mesh "Rock" {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points.timeSamples = {
                1: [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)],
                2: [(0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)],
            }
        }
        def Sphere "Pebble" {
            double radius = 0.5
        }
        def Sphere "Stone" {
            double radius = 0.5
        }
    }
    def Xform "Beacons" {
        def Xform "North" (
            instanceable = true
            prepend references = </Beacon>
        ) {
            double3 xformOp:translate = (1, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
        def Xform "South" (
            instanceable = true
            prepend references = </Beacon>
        ) {
            double3 xformOp:translate = (-1, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def PointInstancer "Stones" {
        rel prototypes = </World/Stones/Prototypes/Stone>
        int[] protoIndices = [0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0)]
        def Scope "Prototypes" {
            def Cube "Stone" {
                double size = 0.25
            }
        }
    }
}
class Xform "Beacon" {
    def Cube "Post" {
        double size = 0.5
    }
    def Sphere "Bulb" {
        double radius = 0.25
    }
}
`;

/** A stage, its draw list after a refresh, and the diff of each edit. */
function fixture(): { stage: Stage; list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { time: 1 });
  const first = list.refresh();
  return {
    stage,
    list,
    first,
    edit(edits) {
      stage.apply(edits, stage.root);
      const diffs = heard.splice(0).map((changes) => list.applyChanges(changes));
      assert.equal(diffs.length, 1);
      return diffs[0]!;
    },
  };
}

function itemOf(list: DrawList, prim: string): number {
  const item = list.items().find((item) => item.prim === prim);
  assert.ok(item !== undefined, `no item draws ${prim}`);
  return item.id;
}

describe("draw lists", () => {
  it("group native instances and share equal shapes", () => {
    const { stage, list, first } = fixture();
    try {
      assert.equal(list.items().length, 6);
      assert.equal(first.added.length, 6);
      assert.equal(first.work.triangulations, 1);
      assert.equal(first.work.tessellations, 4);
      const post = list.item(itemOf(list, "/World/Beacons/North/Post"))!;
      assert.deepEqual(post.source, { kind: "prototype", prim: "/World/Beacons/North/Post", material: null });
      assert.deepEqual(post.instances, ["/World/Beacons/North/Post", "/World/Beacons/South/Post"]);
      // Row-vector matrices: the translation is the last row.
      assert.deepEqual([...post.matrices.subarray(12, 15)], [1, 0, 0]);
      assert.deepEqual([...post.matrices.subarray(28, 31)], [-1, 0, 0]);
      const pebble = list.item(itemOf(list, "/World/Rocks/Pebble"))!;
      assert.equal(pebble.buffer, list.item(itemOf(list, "/World/Rocks/Stone"))!.buffer);
      const stones = list.item(itemOf(list, "/World/Stones/Prototypes/Stone"))!;
      assert.deepEqual([...stones.ids!], [0n, 1n, 2n]);
      assert.equal(list.buffers(pebble.buffer)!.shape?.kind, "Sphere");
      const again = list.refresh();
      assert.deepEqual([again.added, again.removed, again.matrices, again.refilled, again.rebuilt], [[], [], [], [], []]);
      assert.equal(again.work.buffersReused, 6);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("touch only the item a property edit reaches", () => {
    const { stage, list, edit } = fixture();
    try {
      const pebble = itemOf(list, "/World/Rocks/Pebble");
      const diff = edit([{ type: "setValue", path: "/World/Rocks/Pebble.radius", value: { type: "double", value: 0.75 } }]);
      assert.deepEqual(diff.rebound, [pebble]);
      assert.equal(diff.buffersAdded.length, 1);
      assert.deepEqual(diff.freed, []);
      assert.equal(diff.work.itemsRead, 1);
      assert.equal(diff.work.itemsUntouched, 5);
      assert.equal(diff.work.tessellations, 1);
      assert.equal(diff.work.triangulations, 0);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("refill an animated mesh at a new time without triangulating", () => {
    const { stage, list } = fixture();
    try {
      const rock = list.item(itemOf(list, "/World/Rocks/Rock"))!;
      const diff = list.setTime(2);
      assert.deepEqual(diff.refilled, [rock.buffer]);
      assert.deepEqual(diff.matrices, []);
      assert.equal(diff.work.refills, 1);
      assert.equal(diff.work.triangulations, 0);
      // Nothing the other items read is animated: none was read.
      assert.equal(diff.work.itemsRead, 1);
      assert.equal(diff.work.itemsUntouched, 5);
      assert.equal(diff.work.itemsTimeInvariant, 5);
      assert.equal(diff.work.buffersReused, 0);
      const buffers = list.buffers(rock.buffer)!;
      assert.equal(buffers.revision, 1);
      assert.ok([...buffers.mesh.positions].every((p, i) => i % 3 !== 2 || p === 1));
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("read at a new time the items an edit animated, and no longer once it is undone", () => {
    const { stage, list, edit } = fixture();
    try {
      const [rock, pebble, stone] = ["Rock", "Pebble", "Stone"].map((name) => itemOf(list, `/World/Rocks/${name}`));
      const readAt = (time: number) => {
        const diff = list.setTime(time);
        assert.equal(diff.work.itemsRead + diff.work.itemsTimeInvariant, 6, "every item read or skipped");
        return diff;
      };
      // A single sample holds at every numeric time: the pebble is larger already.
      const sampled = edit([
        { type: "setValue", path: "/World/Rocks/Pebble.radius", value: { type: "double", value: 2 }, time: 3 },
      ]);
      assert.equal(sampled.work.itemsRead, 1);
      assert.deepEqual(sampled.rebound, [pebble]);
      assert.equal(readAt(3).work.itemsRead, 2, "the rock and the pebble");
      const cleared = edit([{ type: "clearValue", path: "/World/Rocks/Pebble.radius", time: 3 }]);
      assert.deepEqual(cleared.rebound, [pebble], "back to the radius at default");
      assert.equal(readAt(4).work.itemsRead, 1, "the rock alone");

      const moving = edit([
        { type: "setValue", path: "/World/Rocks.xformOpOrder", value: { type: "token[]", value: ["xformOp:translate"] } },
        { type: "createAttribute", path: "/World/Rocks.xformOp:translate", typeName: "double3", custom: false },
        { type: "setValue", path: "/World/Rocks.xformOp:translate", value: { type: "double3", value: [0, 5, 0] }, time: 5 },
        { type: "setValue", path: "/World/Rocks.xformOp:translate", value: { type: "double3", value: [0, 7, 0] }, time: 7 },
      ]);
      assert.equal(moving.work.itemsRead, 3, "the items beneath the rocks");
      const moved = readAt(7);
      assert.equal(moved.work.itemsRead, 3);
      assert.deepEqual(moved.matrices, [rock!, pebble!, stone!].sort((a, b) => a - b));

      edit([
        { type: "setValue", path: "/World/Beacons/North.visibility", value: { type: "token", value: "invisible" }, time: 6 },
      ]);
      const blinking = readAt(8);
      // The rocks (moving) and the beacon's post and bulb (blinking).
      assert.equal(blinking.work.itemsRead, 5);
      assert.equal(blinking.work.itemsTimeInvariant, 1, "the stones");
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("draw prims of schemas derived from Mesh or a shape, as Rust's IsA does", () => {
    const bytes = new TextEncoder().encode(`#usda 1.0
def Xform "Lab" {
    def RockMesh "Rock" {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }
    def Boulder "Boulder" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
    }
    def Lamp "Lamp" {
        double radius = 0.25
    }
    def Robot "Robot" {
    }
}
`);
    const schemas = [
      { name: "RockMesh", base: "Mesh" },
      { name: "Boulder", base: "RockMesh" },
      { name: "Lamp", base: "Sphere" },
      { name: "Robot", base: "Xform" },
    ];
    const stage = layerstack.open({ identifier: "lab.usda", bytes }, { schemas });
    const list = stagehand.drawList(stage);
    try {
      const diff = list.refresh();
      assert.deepEqual(
        list.items().map((item) => item.prim),
        ["/Lab/Rock", "/Lab/Boulder", "/Lab/Lamp"],
        "the meshes and the sphere; the robot is an Xform",
      );
      assert.equal(diff.work.triangulations, 2);
      assert.equal(diff.work.tessellations, 1);
      const lamp = list.item(itemOf(list, "/Lab/Lamp"))!;
      assert.deepEqual(list.buffers(lamp.buffer)!.shape, { kind: "Sphere", radius: 0.25 });
      const boulder = list.item(itemOf(list, "/Lab/Boulder"))!;
      assert.equal(list.buffers(boulder.buffer)!.mesh.indices.length, 3);
    } finally {
      list.dispose();
      stage.free();
    }
    // Unregistered, the types are unknown and nothing is drawn.
    const plain = layerstack.open({ identifier: "lab.usda", bytes });
    const none = stagehand.drawList(plain);
    try {
      none.refresh();
      assert.deepEqual(none.items(), []);
    } finally {
      none.dispose();
      plain.free();
    }
  });

  it("change only matrices for an instancer edit", () => {
    const { stage, list, edit } = fixture();
    try {
      const stones = itemOf(list, "/World/Stones/Prototypes/Stone");
      const diff = edit([
        {
          type: "setValue",
          path: "/World/Stones.positions",
          value: { type: "point3f[]", value: new Float32Array([0, 0, 0, 0, 1, 0, 0, 2, 0]) },
        },
      ]);
      assert.deepEqual(diff.matrices, [stones]);
      assert.deepEqual([diff.added, diff.removed, diff.rebound, diff.buffersAdded, diff.refilled], [[], [], [], [], []]);
      assert.equal(diff.work.triangulations + diff.work.tessellations, 0);
      assert.equal(diff.work.itemsRead, 1);
      assert.equal(diff.work.matricesComputed, 3);
      assert.deepEqual([...list.item(stones)!.matrices.subarray(44, 47)], [0, 2, 0]);
      const hidden = edit([
        { type: "setValue", path: "/World/Stones.invisibleIds", value: { type: "int64[]", value: [1n] } },
      ]);
      assert.deepEqual(hidden.matrices, [stones]);
      assert.deepEqual([...list.item(stones)!.ids!], [0n, 2n]);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("recompute a moved instance's groups' matrices alone", () => {
    const { stage, list, edit } = fixture();
    try {
      const groups = [itemOf(list, "/World/Beacons/North/Post"), itemOf(list, "/World/Beacons/North/Bulb")].sort((a, b) => a - b);
      const diff = edit([
        { type: "setValue", path: "/World/Beacons/North.xformOp:translate", value: { type: "double3", value: [5, 0, 0] } },
      ]);
      assert.deepEqual(diff.matrices, groups);
      assert.equal(diff.work.itemsRead, 2);
      assert.equal(diff.work.localTransforms, 1);
      assert.equal(diff.work.triangulations + diff.work.tessellations, 0);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("hide items without dropping their buffers, and rebuild only a resynced subtree", () => {
    const { stage, list, edit } = fixture();
    try {
      const hidden = edit([
        { type: "setValue", path: "/World/Rocks.visibility", value: { type: "token", value: "invisible" } },
      ]);
      assert.equal(hidden.visibility.length, 3);
      assert.deepEqual(hidden.freed, []);
      assert.equal(hidden.work.itemsUntouched, 3);
      const off = edit([{ type: "setActive", path: "/World/Rocks", active: false }]);
      assert.equal(off.removed.length, 3);
      assert.equal(off.freed.length, 2);
      assert.equal(off.work.itemsUntouched, 3);
      const on = edit([{ type: "setActive", path: "/World/Rocks", active: true }]);
      assert.equal(on.added.length, 3);
      assert.equal(on.work.triangulations, 1);
      assert.equal(on.work.tessellations, 1);
      assert.equal(on.work.itemsUntouched, 3);
    } finally {
      list.dispose();
      stage.free();
    }
  });
});
