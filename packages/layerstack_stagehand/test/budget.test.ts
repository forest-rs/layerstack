// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A draw list under a budget: items admitted in a fixed order, stepped
// down to a stand-in or skipped when they do not fit, reported, promoted,
// and admitted again only as far as a change reaches.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit, Stage } from "@forest-rs/layerstack";
import { loadStagehand } from "../dist/index.js";
import type { BudgetGrant, DrawBudget, DrawDiff, DrawList } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

// By importance (world bound diagonal times instances): the big mesh
// (4 triangles), the field's blades (1 triangle, 4 instances), the small
// mesh (2), the tiny one (1).
const SCENE = `#usda 1.0
def Xform "World" {
    def Mesh "Big" {
        float3[] extent = [(0, 0, 0), (10, 10, 0)]
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 0, 2, 3, 1]
        point3f[] points = [(0, 0, 0), (10, 0, 0), (10, 10, 0), (0, 10, 0)]
    }
    def PointInstancer "Field" {
        rel prototypes = </World/Field/Prototypes/Blade>
        int[] protoIndices = [0, 0, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0)]
        def Scope "Prototypes" {
            def Mesh "Blade" {
                float3[] extent = [(0, 0, 0), (1, 1, 0)]
                int[] faceVertexCounts = [3]
                int[] faceVertexIndices = [0, 1, 2]
                point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0)]
            }
        }
    }
    def Mesh "Small" {
        float3[] extent = [(0, 0, 0), (1, 1, 0)]
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    }
    def Mesh "Tiny" {
        float3[] extent = [(0, 0, 0), (0.1, 0.1, 0)]
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (0.1, 0, 0), (0.1, 0.1, 0)]
    }
}
`;

const BLADE = "/World/Field/Prototypes/Blade";

function fixture(budget: DrawBudget): { stage: Stage; list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff; done(): void } {
  const stage = layerstack.open({ identifier: "budget.usda", bytes: new TextEncoder().encode(SCENE) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { budget });
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
    done() {
      list.dispose();
      stage.free();
    },
  };
}

/** Each item's rung, by prim. */
function rungs(list: DrawList): Record<string, string> {
  const out: Record<string, string> = {};
  for (const item of list.items()) out[item.prim] = list.buffers(item.buffer)!.standin === null ? "full" : "standin";
  for (const grant of list.budgetReport()?.degraded ?? []) if (grant.rung === "skipped") out[grant.prim] = "skipped";
  return out;
}

const brief = (grant: BudgetGrant) => [grant.prim, grant.rung, grant.over.join(",")];

describe("draw budgets", () => {
  it("admit by importance, stepping down what does not fit", () => {
    const { list, first, done } = fixture({ triangles: 7 });
    try {
      // Every item is a box first, then refined by importance: the big
      // mesh fits (4); the field's 4 would make 8: it stays a box; the
      // small (2) and tiny (1) meshes fill it to 7.
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "full" });
      const report = list.budgetReport()!;
      assert.deepEqual(report.budget, { triangles: 7 });
      assert.equal(report.items, 4);
      assert.deepEqual(report.degraded.map(brief), [[BLADE, "standin", "triangles"]]);
      const blade = report.degraded[0]!;
      assert.equal(blade.order, 1);
      assert.deepEqual(blade.requested, { gpuBytes: blade.requested.gpuBytes, triangles: 4, instances: 4 });
      // A point instancer's stand-in is one box over its instances: no triangles.
      assert.deepEqual(blade.granted, { gpuBytes: 288 + 64, triangles: 0, instances: 1 });
      assert.equal(report.requested.triangles, 11);
      assert.equal(report.granted.triangles, 7);
      // The box bounds the blades at their four positions, at the instancer.
      const item = list.item(blade.item!)!;
      assert.equal(item.matrices.length, 16);
      assert.equal(item.ids, null);
      const standin = list.buffers(item.buffer)!.standin!;
      assert.equal(standin.mode, "bounds");
      assert.deepEqual([...standin.input.extent!], [0, 0, 0, 4, 1, 0]);
      // The first diff lists every degradation, and nothing built for it.
      assert.deepEqual(first.budget!.changed.map(brief), [[BLADE, "standin", "triangles"]]);
      assert.equal(first.budget!.degraded, 1);
      assert.equal(first.work.triangulations, 3);
      assert.equal(first.work.itemsWeighed, 4);
    } finally {
      done();
    }
  });

  it("refine most worth per share of the budget first, not the largest first", () => {
    // A huge mesh, three times the big one's size but five times its
    // triangles: by size it would go first and box three others; by worth
    // per triangle it waits, and is the one boxed.
    const quads = Array.from({ length: 10 }, () => "4").join(", ");
    const corners = Array.from({ length: 10 }, () => "0, 1, 2, 3").join(", ");
    const huge = SCENE.replace(
      'def Mesh "Big" {',
      `def Mesh "Huge" {
        float3[] extent = [(0, 0, 0), (30, 30, 0)]
        int[] faceVertexCounts = [${quads}]
        int[] faceVertexIndices = [${corners}]
        point3f[] points = [(0, 0, 0), (30, 0, 0), (30, 30, 0), (0, 30, 0)]
    }
    def Mesh "Big" {`,
    );
    const stage = layerstack.open({ identifier: "huge.usda", bytes: new TextEncoder().encode(huge) });
    const list = stagehand.drawList(stage, { budget: { triangles: 22 } });
    try {
      list.refresh();
      assert.deepEqual(rungs(list), { "/World/Huge": "standin", "/World/Big": "full", [BLADE]: "full", "/World/Small": "full", "/World/Tiny": "full" });
      const report = list.budgetReport()!;
      assert.deepEqual(report.degraded.map(brief), [["/World/Huge", "standin", "triangles"]]);
      assert.equal(report.degraded[0]!.order, 1);
      assert.equal(report.granted.triangles, 11);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("skip what no stand-in fits, and degrade the same way every time", () => {
    const budget = { triangles: 3, instances: 3 };
    const { list, done } = fixture(budget);
    try {
      // Three boxes use the instances up: the tiny mesh's is skipped.
      // Refined, the big mesh's 4 triangles and the field's 4 instances do
      // not fit; the small mesh's 2 triangles do, and the tiny mesh would
      // need a fourth instance.
      assert.deepEqual(rungs(list), { "/World/Big": "standin", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "skipped" });
      const report = list.budgetReport()!;
      assert.deepEqual(report.degraded.map(brief), [
        ["/World/Big", "standin", "triangles"],
        [BLADE, "standin", "triangles,instances"],
        ["/World/Tiny", "skipped", "instances"],
      ]);
      assert.equal(report.degraded[2]!.item, null);
      assert.deepEqual(report.degraded[2]!.granted, { gpuBytes: 0, triangles: 0, instances: 0 });
      assert.equal(list.items().length, 3);
      assert.deepEqual(report.granted, { gpuBytes: report.granted.gpuBytes, triangles: 2, instances: 3 });
      const again = fixture(budget);
      try {
        assert.deepEqual(again.list.budgetReport(), report);
      } finally {
        again.done();
      }
    } finally {
      done();
    }
  });

  it("admit what is promoted first", () => {
    const { list, done } = fixture({ triangles: 7 });
    try {
      const diff = list.promote(["/World/Field"]);
      assert.deepEqual(rungs(list), { "/World/Big": "standin", [BLADE]: "full", "/World/Small": "full", "/World/Tiny": "full" });
      assert.deepEqual(diff.budget!.changed.map((grant) => [grant.prim, grant.rung, grant.promoted]), [
        [BLADE, "full", true],
        ["/World/Big", "standin", false],
      ]);
      // Two items rebound, nothing read but their buffers.
      assert.deepEqual(diff.rebound.length, 2);
      assert.equal(diff.work.itemsWeighed, 0);
      assert.equal(diff.work.itemsRegranted, 2);
      list.promote([]);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "full" });
    } finally {
      done();
    }
  });

  it("weigh again only what a change reaches, and move only what it displaces", () => {
    const { list, edit, done } = fixture({ triangles: 7 });
    try {
      // The small mesh grows to 4 triangles: it no longer fits behind the
      // big mesh, and the tiny one still does.
      const grown = edit([
        { type: "setValue", path: "/World/Small.faceVertexCounts", value: { type: "int[]", value: [4, 4] } },
        { type: "setValue", path: "/World/Small.faceVertexIndices", value: { type: "int[]", value: [0, 1, 2, 3, 0, 2, 3, 1] } },
      ]);
      assert.equal(grown.work.itemsWeighed, 1);
      assert.deepEqual(grown.budget!.changed.map(brief), [["/World/Small", "standin", "triangles"]]);
      assert.equal(grown.work.triangulations, 0);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "standin", "/World/Tiny": "full" });
      // The big mesh shrinks to one triangle: the blades fit again, and
      // the tiny mesh is displaced.
      const shrunk = edit([
        { type: "setValue", path: "/World/Big.faceVertexCounts", value: { type: "int[]", value: [3] } },
        { type: "setValue", path: "/World/Big.faceVertexIndices", value: { type: "int[]", value: [0, 1, 2] } },
      ]);
      assert.equal(shrunk.work.itemsWeighed, 1);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "full", "/World/Small": "standin", "/World/Tiny": "full" });
      assert.deepEqual(shrunk.budget!.changed.map(brief), [[BLADE, "full", ""]]);
      // The big mesh was planned again, the blade built for the first time.
      assert.equal(shrunk.work.triangulations, 2);
      // The big mesh grows back: it displaces the blades, and nothing else.
      const regrown = edit([
        { type: "setValue", path: "/World/Big.faceVertexCounts", value: { type: "int[]", value: [4, 4] } },
        { type: "setValue", path: "/World/Big.faceVertexIndices", value: { type: "int[]", value: [0, 1, 2, 3, 0, 2, 3, 1] } },
      ]);
      assert.equal(regrown.work.itemsWeighed, 1);
      assert.deepEqual(regrown.budget!.changed.map(brief), [[BLADE, "standin", "triangles"]]);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "standin", "/World/Tiny": "full" });
      // A move reaches no cost: nothing weighed.
      const moved = edit([
        { type: "createAttribute", path: "/World/Big.xformOp:translate", typeName: "double3", custom: false },
        { type: "setValue", path: "/World/Big.xformOp:translate", value: { type: "double3", value: [1, 2, 3] } },
        { type: "setValue", path: "/World/Big.xformOpOrder", value: { type: "token[]", value: ["xformOp:translate"] } },
      ]);
      assert.equal(moved.work.itemsWeighed, 0);
      assert.deepEqual(moved.budget!.changed, []);
    } finally {
      done();
    }
  });

  it("list a hidden item only once it draws, and draw everything without a budget", () => {
    const { list, edit, done } = fixture({ triangles: 7 });
    try {
      const hidden = edit([{ type: "setValue", path: "/World/Big.visibility", value: { type: "token", value: "invisible" } }]);
      // Hidden, the big mesh costs nothing: the blades take its place.
      assert.deepEqual(hidden.removed.length, 1);
      assert.deepEqual(rungs(list), { [BLADE]: "full", "/World/Small": "full", "/World/Tiny": "full" });
      assert.deepEqual(hidden.budget!.changed.map(brief), [[BLADE, "full", ""]]);
      const shown = edit([{ type: "setValue", path: "/World/Big.visibility", value: { type: "token", value: "inherited" } }]);
      assert.equal(shown.added.length, 1);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "full" });
      const unlimited = list.setBudget(null);
      assert.equal(unlimited.budget, null);
      assert.equal(list.budgetReport(), null);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "full", "/World/Small": "full", "/World/Tiny": "full" });
      assert.equal(unlimited.rebound.length, 1);
      const limited = list.setBudget({ triangles: 7 });
      assert.equal(limited.work.itemsWeighed, 4);
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "full" });
    } finally {
      done();
    }
  });
  it("weigh again at a new time what might cost otherwise, and drop what a scope leaves out", () => {
    const animated = SCENE.replace('def Mesh "Big" {', 'def Mesh "Big" {\n        token visibility.timeSamples = { 1: "inherited", 2: "invisible" }');
    const stage = layerstack.open({ identifier: "animated.usda", bytes: new TextEncoder().encode(animated) });
    const list = stagehand.drawList(stage, { time: 1, budget: { triangles: 7 } });
    try {
      list.refresh();
      assert.deepEqual(rungs(list), { "/World/Big": "full", [BLADE]: "standin", "/World/Small": "full", "/World/Tiny": "full" });
      // At 2 the big mesh is hidden: only it is weighed again, and the blades take its place.
      const later = list.setTime(2);
      assert.equal(later.work.itemsWeighed, 1);
      assert.deepEqual(later.budget!.changed.map(brief), [[BLADE, "full", ""]]);
      assert.deepEqual(rungs(list), { [BLADE]: "full", "/World/Small": "full", "/World/Tiny": "full" });
      list.setTime(1);
      // A scope of the small mesh alone admits it alone.
      list.setScope({ roots: ["/World/Small"] });
      assert.deepEqual(rungs(list), { "/World/Small": "full" });
      assert.equal(list.budgetReport()!.items, 1);
    } finally {
      list.dispose();
      stage.free();
    }
  });
});
