// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Draw mode stand-ins in the draw list: the same models, geometry, diffs
// and work counts as `layerstack_stagehand`'s `standin_tests`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit } from "@forest-rs/layerstack";
import { loadStagehand } from "../dist/index.js";
import type { DrawDiff, DrawList } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

const SCENE = `#usda 1.0
(
    upAxis = "Y"
)
def Xform "Set" (
    kind = "assembly"
    prepend apiSchemas = ["GeomModelAPI"]
) {
    uniform token model:drawMode = "cards"
    def Xform "Rock" (
        kind = "component"
        prepend apiSchemas = ["GeomModelAPI"]
    ) {
        float3[] extentsHint = [(-1, 0, -2), (1, 4, 2)]
        asset model:cardTextureXPos = @rock_x.png@
        def Mesh "Body" {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        }
    }
    def Xform "Lamp" (
        kind = "component"
    ) {
        def Sphere "Bulb" {
            double radius = 0.5
        }
    }
    def Xform "Robot" (
        kind = "component"
        prepend apiSchemas = ["GeomModelAPI"]
    ) {
        uniform bool model:applyDrawMode = false
        def Cube "Chassis" {
            double size = 1
        }
    }
}
def Xform "Crate" (
    prepend apiSchemas = ["GeomModelAPI"]
) {
    uniform token model:drawMode = "bounds"
    uniform bool model:applyDrawMode = true
    float3[] extentsHint = [(0, 0, 0), (2, 1, 1), (-5, -5, -5), (5, 5, 5)]
    def Cube "Box" {
        double size = 2
    }
}
`;

function fixture(): { list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage);
  const first = list.refresh();
  return {
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

function drawn(list: DrawList): string[] {
  return list
    .items()
    .map((item) => `${item.prim}${item.source.kind === "drawMode" ? " standin" : ""}`)
    .sort();
}

function standinOf(list: DrawList, prim: string) {
  const item = list.items().find((candidate) => candidate.prim === prim)!;
  return list.buffers(item.buffer)!.standin!;
}

const token = (path: string, value: string): Edit => ({ type: "setValue", path, value: { type: "token", value } });

describe("draw mode stand-ins", () => {
  it("replace an applying model's subtree", () => {
    const { list, first } = fixture();
    assert.deepEqual(drawn(list), ["/Crate standin", "/Set/Lamp/Bulb", "/Set/Robot/Chassis", "/Set/Rock standin"]);
    assert.equal(first.work.standinsRead, 2);
    const rock = standinOf(list, "/Set/Rock");
    assert.equal(rock.mode, "cards");
    assert.deepEqual(Array.from(rock.input.extent!), [-1, 0, -2, 1, 4, 2]);
    assert.deepEqual(
      rock.cards.map((card) => [card.face, card.texture]),
      [
        ["XPos", "XPos"],
        ["XNeg", "XPos"],
      ],
    );
    assert.deepEqual(Array.from(rock.cards[0]!.uvs.subarray(0, 2)), [1, 1]);
    assert.deepEqual(Array.from(rock.cards[1]!.uvs.subarray(0, 2)), [0, 1]);
    assert.equal(rock.textures[0]!.assetPath, "rock_x.png");
    assert.deepEqual(rock.color, [0.18, 0.18, 0.18].map(Math.fround), "the schema's fallback");
    const crate = standinOf(list, "/Crate");
    assert.equal(crate.mode, "bounds");
    assert.deepEqual(Array.from(crate.input.extent!), [-5, -5, -5, 5, 5, 5]);
    assert.equal(crate.lines.length, 24 * 3);
    assert.deepEqual(Array.from(crate.lines.subarray(0, 6)), [-5, -5, -5, 5, -5, -5]);
  });

  it("rediscover only the subtree a draw mode edit reaches", () => {
    const { list, edit } = fixture();
    let diff = edit([token("/Set.model:drawMode", "default")]);
    assert.equal(diff.work.drawModeRoots, 1);
    assert.equal(diff.removed.length, 1);
    assert.equal(diff.added.length, 1);
    assert.equal(diff.freed.length, 1);
    assert.equal(diff.work.itemsUntouched, 1, "the crate");
    assert.ok(drawn(list).includes("/Set/Rock/Body"));
    diff = edit([token("/Set/Rock.model:drawMode", "origin")]);
    assert.deepEqual([diff.removed.length, diff.added.length], [1, 1]);
    assert.equal(diff.work.itemsUntouched, 3);
    assert.equal(standinOf(list, "/Set/Rock").lines.length, 6 * 3);
    edit([token("/Set.model:drawMode", "cards")]);
    diff = edit([{ type: "setValue", path: "/Set/Robot.model:applyDrawMode", value: { type: "bool", value: true } }]);
    assert.deepEqual([diff.removed.length, diff.added.length], [1, 1]);
    assert.ok(drawn(list).includes("/Set/Robot standin"));
  });

  it("read only the stand-ins a card input edit reaches", () => {
    const { list, edit } = fixture();
    let diff = edit([{ type: "setValue", path: "/Set/Rock.model:drawModeColor", value: { type: "color3f", value: [1, 0, 0] } }]);
    assert.equal(diff.work.drawModeRoots, 0);
    assert.equal(diff.work.standinsRead, 1);
    assert.equal(diff.refilled.length, 1);
    assert.equal(diff.work.itemsUntouched, 3);
    assert.deepEqual(standinOf(list, "/Set/Rock").color, [1, 0, 0]);
    edit([token("/Set/Rock.model:cardGeometry", "box")]);
    assert.ok(standinOf(list, "/Set/Rock").cards.every((card) => card.face !== "XPos" || Array.from(card.corners).filter((_, i) => i % 3 === 0).every((x) => x === 1)));
    diff = edit([token("/Set.model:cardVisibility", "simple")]);
    assert.equal(diff.work.standinsRead, 1);
    assert.equal(diff.work.itemsUntouched, 3);
  });
});
