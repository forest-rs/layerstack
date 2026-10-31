// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Light and shadow linking: against usd-core's answers
// (`stagehand_links_oracle.py`), with the same categories, diffs and work
// counts as `layerstack_stagehand`'s link tests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit, Stage } from "@forest-rs/layerstack";
import { loadStagehand } from "../dist/index.js";
import type { DrawDiff, DrawList, LinkKind, LinkTarget } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();
const base = new URL("../../../layerstack_conformance/fixtures/stagehand_links/", import.meta.url);
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8")) as {
  targets: { path: string }[];
  links: { light: string; link: string; trivial: boolean; shares: number; included: string[] }[];
};

function fixture(): { stage: Stage; list: DrawList; first: DrawDiff; edit(edits: Edit[]): DrawDiff } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: readFileSync(new URL("scene.usda", base)) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { time: 0 });
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

function targets(list: DrawList): Map<string, LinkTarget> {
  const all = new Map<string, LinkTarget>();
  for (const item of list.items()) for (const path of item.linkPaths) all.set(path, { item: item.id, path });
  return all;
}

function linked(list: DrawList, light: string, kind: LinkKind): string[] {
  return [...targets(list).values()]
    .filter((target) => list.isLinked(light, kind, target))
    .map((target) => target.path)
    .sort();
}

describe("light links", () => {
  it("match OpenUSD's membership for every light, link and target", () => {
    const { stage, list } = fixture();
    try {
      const all = targets(list);
      assert.deepEqual([...all.keys()].sort(), oracle.targets.map((target) => target.path).sort());
      const categories: (number | null)[] = [];
      for (const link of oracle.links) {
        const light = list.light(link.light)!;
        const kind: LinkKind = link.link === "lightLink" ? "light" : "shadow";
        const category = (kind === "light" ? light.lightLink : light.shadowLink).category;
        assert.equal(category === null, link.trivial, `${link.light} ${link.link}`);
        for (const [path, target] of all) {
          assert.equal(list.isLinked(link.light, kind, target), link.included.includes(path), `${link.light} ${link.link} ${path}`);
          if (category !== null) assert.equal(list.linkCategories(target).includes(category), link.included.includes(path));
        }
        categories.push(category);
      }
      // Links share a category exactly when OpenUSD's queries are equal.
      oracle.links.forEach((link, i) =>
        oracle.links.forEach((other, j) => assert.equal(categories[i] === categories[j], link.shares === other.shares, `${link.light} / ${other.light}`)),
      );
      const distinct = new Set(oracle.links.filter((link) => !link.trivial).map((link) => link.shares));
      assert.equal(list.linkSets().length, distinct.size);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("evaluate only the link an edit reaches", () => {
    const { stage, list, first, edit } = fixture();
    try {
      const count = [...targets(list).keys()].length;
      assert.equal(first.work.linksEvaluated, 2 * list.lights().length);
      assert.deepEqual(first.linksChanged, [], "new lights are not listed");
      const shared = list.light("/World/Lights/Spot")!.lightLink.category;
      assert.notEqual(shared, null);
      assert.equal(list.light("/World/Lights/Twin")!.lightLink.category, shared);

      // The spot links the rocks too: a membership of its own.
      let diff = edit([{ type: "setTargets", path: "/World/Lights/Spot.collection:lightLink:includes", targets: ["/World/Robot", "/World/Rocks"] }]);
      assert.equal(diff.work.linksEvaluated, 1);
      assert.equal(diff.work.linksClassified, count);
      assert.equal(diff.work.itemsRead, 0);
      assert.deepEqual(diff.lightsChanged, ["/World/Lights/Spot"]);
      assert.equal(diff.linksChanged.length, 1);
      assert.deepEqual(
        [diff.linksChanged[0]!.light, diff.linksChanged[0]!.kind, diff.linksChanged[0]!.added.map((target) => target.path).sort(), diff.linksChanged[0]!.removed],
        ["/World/Lights/Spot", "light", ["/World/Rocks/Pebble", "/World/Rocks/Rock"], []],
      );
      const own = list.light("/World/Lights/Spot")!.lightLink.category;
      assert.ok(own !== null && own !== shared);
      assert.equal(list.light("/World/Lights/Twin")!.lightLink.category, shared);
      assert.deepEqual(linked(list, "/World/Lights/Spot", "light"), ["/World/Robot/Body", "/World/Robot/Head", "/World/Rocks/Pebble", "/World/Rocks/Rock"]);

      // Back to the twin's membership: it joins the twin's category.
      diff = edit([{ type: "setTargets", path: "/World/Lights/Spot.collection:lightLink:includes", targets: ["/World/Robot"] }]);
      assert.equal(diff.work.linksEvaluated, 1);
      assert.equal(diff.work.linksClassified, 0);
      assert.equal(list.light("/World/Lights/Spot")!.lightLink.category, shared);
      assert.equal(diff.linksChanged[0]!.removed.length, 2);
      assert.equal(list.linkSet(own!), null, "unused, dropped");

      // A shadow link edit evaluates the shadow link alone.
      diff = edit([{ type: "setValue", path: "/World/Lights/Spot.collection:shadowLink:includeRoot", value: { type: "bool", value: false } }]);
      assert.equal(diff.work.linksEvaluated, 1);
      assert.equal(diff.linksChanged.length, 1);
      assert.equal(diff.linksChanged[0]!.kind, "shadow");
      assert.equal(diff.linksChanged[0]!.removed.length, count);
      assert.deepEqual(linked(list, "/World/Lights/Spot", "shadow"), []);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("evaluate again the links including an edited collection", () => {
    const { stage, list, edit } = fixture();
    try {
      const diff = edit([{ type: "setTargets", path: "/World.collection:robots:includes", targets: ["/World/Rocks"] }]);
      assert.equal(diff.work.linksEvaluated, 1);
      assert.equal(diff.linksChanged.length, 1);
      assert.equal(diff.linksChanged[0]!.light, "/World/Lights/Collected");
      assert.deepEqual([diff.linksChanged[0]!.added.length, diff.linksChanged[0]!.removed.length], [2, 2]);
      assert.deepEqual(linked(list, "/World/Lights/Collected", "light"), ["/World/Rocks/Pebble", "/World/Rocks/Rock"]);
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("classify a new item once against each category", () => {
    const { stage, list, edit } = fixture();
    try {
      const categories = list.linkSets().length;
      let diff = edit([{ type: "definePrim", path: "/World/Robot/Arm", typeName: "Cube" }]);
      assert.equal(diff.added.length, 1);
      assert.equal(diff.work.linksEvaluated, 0);
      assert.equal(diff.work.linksClassified, categories);
      assert.deepEqual(diff.linksChanged, [], "a new item's links are read from the list");
      const arm = targets(list).get("/World/Robot/Arm")!;
      assert.ok(list.isLinked("/World/Lights/Spot", "light", arm));
      assert.ok(!list.isLinked("/World/Lights/Rocky", "light", arm));
      assert.ok(list.isLinked("/World/Lights/Everything", "light", arm));
      diff = edit([{ type: "removePrim", path: "/World/Robot/Arm" }]);
      assert.equal(diff.removed.length, 1);
      assert.deepEqual(diff.linksChanged, []);
      assert.equal(diff.work.linksClassified, 0);
      assert.ok(!list.isLinked("/World/Lights/Spot", "light", arm));
      assert.ok(list.linkSets().every((set) => set.members.every((member) => member.path !== "/World/Robot/Arm")));
    } finally {
      list.dispose();
      stage.free();
    }
  });

  it("do not vary with time", () => {
    const { stage, list } = fixture();
    try {
      const before = list.linkSets();
      // The rock is invisible at 10: hidden, still linked.
      const diff = list.setTime(10);
      assert.ok(diff.visibility.length > 0);
      assert.equal(diff.work.linksEvaluated + diff.work.linksClassified, 0);
      assert.deepEqual(diff.linksChanged, []);
      assert.deepEqual(list.linkSets(), before);
    } finally {
      list.dispose();
      stage.free();
    }
  });
});
