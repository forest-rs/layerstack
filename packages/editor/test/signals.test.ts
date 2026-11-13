// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What each action tells the panels: a panel renders again only when a
// channel it listens to is bumped (`useSyncExternalStore` over
// `Signals.subscribe`), so the channels an action bumps are the panels it
// renders. Playing the timeline must bump the time and nothing the prim
// tree, the toolbar, the layers or the bottom tabs listen to.
import assert from "node:assert/strict";
import { test } from "node:test";
import { CHANNELS, type Channel, Signals } from "../src/store/signals.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

/** The channels whose revisions differ between two snapshots. */
function bumped(before: Readonly<Record<Channel, number>>, after: Readonly<Record<Channel, number>>): Channel[] {
  return CHANNELS.filter((channel) => before[channel] !== after[channel]);
}

test("a bump tells each listener of its channels once, and no other", () => {
  const signals = new Signals();
  const heard: string[] = [];
  const stop = signals.subscribe(["time", "selection"], () => heard.push("both"));
  signals.subscribe(["view"], () => heard.push("view"));
  signals.bump("time", "selection", "time");
  assert.deepEqual(heard, ["both"]);
  assert.equal(signals.revision("time"), 1);
  assert.equal(signals.revision("selection"), 1);
  assert.equal(signals.sum(["time", "selection", "view"]), 2);
  signals.bump("view");
  assert.deepEqual(heard, ["both", "view"]);
  stop();
  signals.bump("time");
  assert.deepEqual(heard, ["both", "view"]);
  assert.equal(signals.listeners("time"), 0);
  signals.bumpAll();
  assert.deepEqual(heard, ["both", "view", "view"]);
});

test("playing bumps the time, once a frame, and nothing else", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store, frames } = h;
  // With the animated cube selected, as the inspector would show it.
  store.select("/World/Top");
  // What the panels that must not render while playing listen to.
  const still: Channel[] = ["status", "composition", "selection", "draws", "view", "history", "target", "notices", "loop"];
  let told = 0;
  store.signals.subscribe(still, () => (told += 1));
  let ticks = 0;
  store.signals.subscribe(["time"], () => (ticks += 1));
  const links = store.scene.unlit;
  const linking = store.scene.linking;
  assert.ok(linking.length > 0, "the sample links a light");

  const before = store.signals.snapshot();
  store.play();
  for (let frame = 0; frame < 60; frame++) frames.run(1000 / 60);
  const after = store.signals.snapshot();

  assert.deepEqual(bumped(before, after), ["time"]);
  assert.equal(told, 0, "no listener of anything but the time was told");
  assert.equal(ticks, 61, "one bump for playing and one a frame");
  assert.ok(Math.abs(store.time! - 24) < 1e-6);
  // The draws moved, and the viewport was told each frame.
  assert.equal(h.view.updates.at(-1)!.update.timeChanged, true);
  assert.ok(h.view.updates.at(-1)!.update.matrices.size > 0);
  // The light links were not evaluated again: nothing they depend on changed.
  assert.equal(store.scene.unlit, links);
  assert.equal(store.scene.linking, linking);

  store.pause();
  assert.deepEqual(bumped(after, store.signals.snapshot()), ["time"]);
});

test("each action bumps the concerns it changed", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const during = (act: () => void): Channel[] => {
    const before = store.signals.snapshot();
    act();
    return bumped(before, store.signals.snapshot());
  };
  store.setBudgetChoice("unlimited");
  assert.deepEqual(during(() => store.select("/World/Top")), ["selection"]);
  assert.deepEqual(during(() => store.select("/World/Top")), [], "selecting the selection changes nothing");
  assert.deepEqual(during(() => store.explain("/World/Top.size")), ["selection"]);
  assert.deepEqual(during(() => store.lookThrough("/World/Camera")), ["view"]);
  assert.deepEqual(during(() => store.setTime(10)), ["time"]);
  assert.deepEqual(during(() => store.setTime(10)), []);
  assert.deepEqual(during(() => store.setValue("/World/Top.size", { type: "double", value: 2 })), ["composition", "draws", "history"]);
  assert.deepEqual(during(() => store.setValue("/World/Top.size", { type: "token", value: "large" })), ["draws", "history", "notices"]);
  assert.deepEqual(during(() => store.dismiss(store.notices[0]!.id)), ["notices"]);
  assert.deepEqual(during(() => store.setTarget(store.session!.layerNamed("library.usda"))), ["target"]);
  assert.deepEqual(during(() => store.setDrawScope("selection")), ["draws", "view"]);
  // Drawing the selection, selecting changes the draws too.
  assert.deepEqual(during(() => store.select("/World/Rock")), ["selection", "draws"]);
  // An edit to a light link is heard by the link tables.
  const links = store.scene.linking;
  store.setDrawScope("scene");
  store.apply("link everything", [{ type: "setValue", path: "/World/Spot.collection:lightLink:includeRoot", value: { type: "bool", value: true } }]);
  assert.deepEqual(store.notices, []);
  assert.notDeepEqual(store.scene.linking, links);

  // An undo may fetch: `status` says an action is running, and then that none is.
  const before = store.signals.snapshot();
  await h.settle(store.undo());
  assert.deepEqual(bumped(before, store.signals.snapshot()), ["status", "composition", "draws", "history", "loop"]);
});

test("opening another scene bumps every concern", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const before = h.store.signals.snapshot();
  await h.open(sampleSource());
  assert.deepEqual(bumped(before, h.store.signals.snapshot()), [...CHANNELS]);
});

test("explaining a property brings its tab forward, and a layer's command the layers", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.showBottomTab("history");
  store.showSceneTab("cameras");
  const before = store.signals.snapshot();
  store.explain("/World/Top.size", false);
  assert.equal(store.bottomTab, "history", "asked not to reveal");
  store.explain("/World/Top.size");
  assert.equal(store.bottomTab, "explain");
  store.showSceneTab("layers");
  assert.equal(store.sceneTab, "layers");
  assert.deepEqual(bumped(before, store.signals.snapshot()), ["selection", "view"]);
  store.showSceneTab("layers");
  assert.deepEqual(bumped(before, store.signals.snapshot()), ["selection", "view"]);
});
