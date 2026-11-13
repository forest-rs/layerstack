// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The store's actions, end to end over the sample scene: what a person
// does in the editor (open, select, edit, undo, choose a variant, load a
// payload, mute a layer, play, rename, cancel), driven through
// `EditorStore` as the panels drive it, with a viewport that records what
// it is told and frames the test moves by hand.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Session } from "../src/model/session.ts";
import { harness, PAYLOAD_ROOT, sampleSource, trap } from "./support/storeHarness.ts";

/** The scalar value of the attribute at `path`; `null` when it has none. */
function valueOf(stage: Session["stage"], path: string, time?: number): unknown {
  const value = stage.attribute(path)?.get(time) ?? null;
  return value?.kind === "scalar" ? value.value : null;
}

/** The variant selected for `set` on the prim at `path`. */
function variantOf(stage: Session["stage"], path: string, set: string): string | null {
  return stage.prim(path)?.variantSets().find((each) => each.name === set)?.selection ?? null;
}

/** The sample's assets behind payloads: a scene whose prims can be renamed whatever the open's payload choice. */
const payloadScene = () => sampleSource("payloads.usda", { "payloads.usda": PAYLOAD_ROOT });

test("opening the sample reaches ready with draws and nothing to report", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  assert.equal(store.status.kind, "ready");
  assert.equal(store.source, source);
  assert.deepEqual(store.notices, []);
  assert.ok(store.scene.items.length > 10, "the sample draws");
  assert.ok(store.scene.items.some((view) => view.item.prim === "/World/Ground"));
  assert.deepEqual(store.range, { start: 0, end: 48, timeCodesPerSecond: 24 });
  assert.equal(store.time, 0);
  assert.equal(store.upAxis, "Y");
  assert.equal(store.selection, null);
  assert.equal(store.opening, null);
  assert.equal(store.openingPhase, null);
  // The open is the first run of the asset loop, and fetched the sublayer and the referenced assets.
  const run = store.session!.loops.runs[0]!;
  assert.equal(run.cause, "open");
  assert.equal(run.error, null);
  assert.ok(source.asked.includes("library.usda"));
  assert.ok(source.asked.includes("assets/robot.usda"));
  // The viewport was reset to the new scene.
  assert.equal(h.view.updates.at(-1)!.update.reset, true);
  assert.equal(h.view.updates.at(-1)!.scene, store.scene);
});

test("a root that does not exist fails the open and keeps nothing", async () => {
  const h = await harness();
  await h.settle(h.store.open(sampleSource("missing.usda")));
  assert.equal(h.store.status.kind, "failed");
  assert.equal(h.store.session, null);
});

test("selecting highlights the prim, and drawing the selection draws it alone", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.select("/World/Robot");
  assert.equal(store.selection, "/World/Robot");
  assert.equal(h.view.updates.at(-1)!.selected, "/World/Robot");
  const whole = store.scene.items.length;

  store.setDrawScope("selection");
  assert.equal(store.focus.kind, "geometry");
  assert.ok(store.scene.items.length > 0 && store.scene.items.length < whole);
  assert.ok(store.scene.items.every((view) => view.item.prim.startsWith("/World/Robot/")));
  // Drawn alone, the whole of it is not highlighted.
  assert.equal(h.view.updates.at(-1)!.selected, null);

  store.select("/World/Top");
  assert.deepEqual(store.scene.items.map((view) => view.item.prim), ["/World/Top"]);

  store.setDrawScope("nothing");
  assert.equal(store.scene.items.length, 0);
  store.setDrawScope("scene");
  assert.equal(store.scene.items.length, whole);

  // Explaining a property keeps it while its prim is selected.
  store.explain("/World/Top.size");
  store.select("/World/Rock");
  assert.equal(store.property, null);
});

test("a value set is read back, undone and redone", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  assert.equal(valueOf(stage, "/World/Top.size"), 0.6);
  store.setValue("/World/Top.size", { type: "double", value: 2 });
  assert.equal(valueOf(stage, "/World/Top.size"), 2);
  assert.equal(store.session!.history.nextUndo?.label, "set /World/Top.size");
  assert.equal(store.session!.changeLog.at(-1)!.cause, "set /World/Top.size");
  // The draw list heard the change: the cube was rebuilt or refilled.
  assert.ok(h.view.updates.at(-1)!.update.records > 0);

  await h.settle(store.undo());
  assert.equal(valueOf(stage, "/World/Top.size"), 0.6);
  assert.equal(store.session!.history.nextUndo, null);
  await h.settle(store.redo());
  assert.equal(valueOf(stage, "/World/Top.size"), 2);
  assert.deepEqual(store.notices, []);
});

test("an edit the stage refuses is a notice, and changes nothing", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.setValue("/World/Top.size", { type: "token", value: "large" });
  assert.equal(store.notices.length, 1);
  assert.equal(store.notices[0]!.action, "set /World/Top.size");
  assert.equal(valueOf(store.session!.stage, "/World/Top.size"), 0.6);
  assert.equal(store.status.kind, "ready");
  store.dismiss(store.notices[0]!.id);
  assert.deepEqual(store.notices, []);
});

test("selecting a variant fetches what it needs as a run of the asset loop", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  assert.ok(!source.asked.includes("assets/chrome.usda"));
  await h.settle(store.selectVariant("/World/Robot", "finish", "chrome"));
  assert.equal(variantOf(session.stage, "/World/Robot", "finish"), "chrome");
  assert.ok(source.asked.includes("assets/chrome.usda"));
  const run = session.loops.runs.at(-1)!;
  assert.equal(run.cause, "finish=chrome");
  assert.equal(run.error, null);
  assert.notEqual(run.endedAt, null);
  assert.deepEqual(run.assets.map((asset) => asset.identifier), ["assets/chrome.usda"]);
  assert.equal(session.loops.current, null);
  assert.deepEqual(store.notices, []);
});

test("payloads opened unloaded load and unload from the store", async () => {
  const h = await harness();
  const source = payloadScene();
  const { store } = h;
  store.setPayloads("none");
  await h.open(source);
  const stage = store.session!.stage;
  assert.equal(stage.prim("/World/Robot/Body"), null);
  assert.ok(!source.asked.includes("assets/robot.usda"));
  assert.equal(store.scene.items.length, 0);

  const loading = store.load("/World/Robot");
  assert.deepEqual(store.status, { kind: "loading", label: "/World/Robot" });
  await h.settle(loading);
  assert.equal(store.status.kind, "ready");
  assert.notEqual(stage.prim("/World/Robot/Body"), null);
  assert.equal(stage.prim("/World/Beacon/Post"), null, "the other payload stays unloaded");
  assert.ok(store.scene.items.some((view) => view.item.prim === "/World/Robot/Body"));
  assert.equal(store.session!.loops.runs.at(-1)!.cause, "load /World/Robot");

  await h.settle(store.unload("/World/Robot"));
  assert.equal(stage.prim("/World/Robot/Body"), null);
  assert.equal(store.scene.items.length, 0);
  assert.ok(store.session!.released.layers > 0, "the payload's layer was released");
  assert.deepEqual(store.notices, []);
});

test("an automatic open loads the payloads that fit", async () => {
  const h = await harness();
  await h.open(payloadScene());
  const stage = h.store.session!.stage;
  assert.notEqual(stage.prim("/World/Robot/Body"), null);
  assert.notEqual(stage.prim("/World/Beacon/Post"), null);
  assert.equal(h.store.payloadNote, null);
});

test("muting a layer recomposes without it, and unmuting fetches it again", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const library = session.layerNamed("library.usda");
  assert.notEqual(session.stage.prim("/World/Lamp"), null);
  const lights = store.scene.lights.length;

  await h.settle(store.setMuted(library, true));
  assert.equal(session.stage.prim("/World/Lamp"), null);
  assert.deepEqual([...session.muted], ["library.usda"]);
  assert.equal(store.scene.lights.length, lights - 1);
  assert.equal(session.history.nextUndo, null, "muting is not an edit");

  await h.settle(store.setMuted("library.usda", false));
  assert.notEqual(session.stage.prim("/World/Lamp"), null);
  assert.equal(source.asked.filter((identifier) => identifier === "library.usda").length, 2);
  assert.equal(store.scene.lights.length, lights);
  assert.deepEqual(store.notices, []);
});

test("playing moves time each frame at the stage's rate, and pausing lands on a frame", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store, frames } = h;
  store.play();
  assert.equal(store.playing, true);
  frames.run(500);
  assert.equal(store.time, 12, "half a second at 24 time codes per second");
  assert.equal(h.view.updates.at(-1)!.update.timeChanged, true);
  frames.run(260);
  assert.ok(Math.abs(store.time! - 18.24) < 1e-9);
  // Past the end it loops to the start.
  frames.run(2000);
  assert.equal(store.time, 0);
  frames.run(260);
  store.pause();
  assert.equal(store.playing, false);
  assert.equal(store.time, 6, "the nearest whole time code");
  assert.equal(frames.waiting, 0, "no frame is left requested");
  frames.run(1000);
  assert.equal(store.time, 6);

  store.step(1);
  assert.equal(store.time, 7);
  store.setTime(1000);
  assert.equal(store.time, 48, "clamped to the range");
  // The animated cube is read at the time.
  assert.equal(valueOf(store.session!.stage, "/World/Top.xformOp:rotateY", store.time), 360);
});

test("defining, renaming and deleting a prim, and undoing each", async () => {
  const h = await harness();
  await h.open(payloadScene());
  const { store } = h;
  const stage = store.session!.stage;

  store.definePrim("/World", "Crate", "Cube");
  assert.equal(store.selection, "/World/Crate");
  assert.equal(stage.prim("/World/Crate")?.typeName, "Cube");
  assert.ok(store.scene.items.some((view) => view.item.prim === "/World/Crate"));

  store.move("/World/Crate", "/World/Box");
  assert.equal(store.selection, "/World/Box");
  assert.equal(stage.prim("/World/Crate"), null);
  assert.notEqual(stage.prim("/World/Box"), null);

  store.move("/World/Box", "/World/Beacon/Box");
  assert.equal(store.selection, "/World/Beacon/Box");
  assert.notEqual(stage.prim("/World/Beacon/Box"), null);

  store.delete("/World/Beacon/Box");
  assert.equal(store.selection, null);
  assert.equal(stage.prim("/World/Beacon/Box"), null);

  // Undone newest first: the delete, the two moves, the definition.
  await h.settle(store.undo());
  assert.notEqual(stage.prim("/World/Beacon/Box"), null);
  await h.settle(store.undo());
  assert.notEqual(stage.prim("/World/Box"), null);
  await h.settle(store.undo());
  assert.notEqual(stage.prim("/World/Crate"), null);
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Crate"), null);
  assert.equal(store.session!.history.nextUndo, null);
  assert.ok(!store.scene.items.some((view) => view.item.prim.endsWith("/Crate") || view.item.prim.endsWith("/Box")));
  assert.deepEqual(store.notices, []);
});

test("cancelling an open keeps the scene the editor had and frees the one being opened", async (t) => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const kept = store.session!;
  store.select("/World/Top");
  const freed = t.mock.method(Session.prototype, "free");

  const second = payloadScene();
  const release = second.hold("assets/robot.usda");
  const opening = store.open(second);
  await h.turn(40);
  assert.deepEqual(store.status, { kind: "opening", label: "payloads.usda" });
  assert.notEqual(store.opening, null);
  assert.ok(second.asked.includes("assets/robot.usda"), "the open is waiting on its payloads");

  store.cancelOpen();
  assert.equal(store.status.kind, "ready");
  assert.equal(store.opening, null);
  assert.equal(store.openingPhase, null);
  release();
  await h.settle(opening);

  assert.equal(store.session, kept);
  assert.equal(store.source?.label, "scene.usda");
  assert.equal(store.selection, "/World/Top");
  assert.equal(valueOf(kept.stage, "/World/Top.size"), 0.6, "the kept stage still answers");
  assert.equal(freed.mock.callCount(), 1);
  assert.notEqual(freed.mock.calls[0]!.this, kept);
  assert.deepEqual(store.notices, []);
});

test("a cancelled open stops fetching", { todo: "the open has no abort signal yet" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const second = sampleSource();
  const release = second.hold("library.usda");
  const opening = store.open(second);
  await h.turn(40);
  assert.deepEqual(second.asked, ["scene.usda", "library.usda"]);

  store.cancelOpen();
  assert.equal(second.signals.get("library.usda")?.aborted, true, "the fetch in flight is told to stop");
  release();
  await h.settle(opening);
  await h.turn(40);
  // The round after the sublayer's (the referenced assets) is never asked for.
  assert.deepEqual(second.asked, ["scene.usda", "library.usda"]);
});

test("an action asked for while another waits on its assets runs after it", { todo: "actions are not serialized yet" }, async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.turn();
  // Asked for while the variant's asset is in flight.
  const undo = store.undo();
  release();
  await h.settle(Promise.all([variant, undo]));

  assert.deepEqual(store.notices, []);
  assert.equal(session.loops.current, null);
  const run = session.loops.runs.find((each) => each.cause === "finish=chrome")!;
  assert.equal(run.error, null);
  assert.equal(run.assets[0]!.outcome, "loaded");
  // The undo ran once the variant had what it needed, and undid it.
  assert.equal(variantOf(session.stage, "/World/Robot", "finish"), "painted");
  assert.equal(session.history.nextUndo, null);
  assert.equal(session.history.redoSteps.length, 1);
});

test("two undos asked for at once undo two steps, one after the other", { todo: "actions are not serialized yet" }, async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const chrome = () => source.asked.filter((identifier) => identifier === "assets/chrome.usda").length;
  await h.settle(store.selectVariant("/World/Robot", "finish", "chrome"));
  store.setValue("/World/Top.size", { type: "double", value: 2 });
  await h.settle(store.selectVariant("/World/Robot", "finish", "painted"));
  // The chrome layer is let go, so undoing back to it fetches it again.
  session.releaseUnusedLayers();
  assert.equal(chrome(), 1);

  const release = source.hold("assets/chrome.usda");
  const first = store.undo();
  const second = store.undo();
  await h.turn();
  release();
  await h.settle(Promise.all([first, second]));

  assert.deepEqual(store.notices, []);
  assert.equal(chrome(), 2, "the layer is fetched once for the undo that wants it");
  assert.equal(session.loops.current, null);
  assert.ok(session.loops.runs.every((run) => run.error === null && run.endedAt !== null));
  assert.equal(variantOf(session.stage, "/World/Robot", "finish"), "chrome");
  assert.equal(valueOf(session.stage, "/World/Top.size"), 0.6);
  assert.equal(session.history.undoSteps.length, 1);
  assert.equal(session.history.redoSteps.length, 2);
});

test("opening another scene while payloads load ends the load and opens it", { todo: "the open frees the session a load still awaits on" }, async () => {
  const h = await harness();
  const first = payloadScene();
  const { store } = h;
  store.setPayloads("none");
  await h.open(first);
  const release = first.hold("assets/robot.usda");
  const loading = store.load("/World");
  await h.turn();
  assert.equal(store.status.kind, "loading");

  const second = sampleSource();
  const opening = store.open(second);
  await h.turn(60);
  release();
  await h.settle(Promise.all([loading, opening]));
  await h.turn(20);

  assert.equal(store.status.kind, "ready");
  assert.equal(store.source, second);
  assert.deepEqual(store.notices, []);
  assert.notEqual(store.session!.stage.prim("/World/Ground"), null);
  // The load of the scene left was told to stop.
  assert.equal(first.signals.get("assets/robot.usda")?.aborted, true);
});

test("a module's trap during an edit loses the module, not just the edit", { todo: "an edit's trap is reported as a notice" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  Object.defineProperty(stage, "apply", {
    value: () => {
      throw trap();
    },
  });
  store.setValue("/World/Top.size", { type: "double", value: 2 });
  assert.equal(store.status.kind, "lost");
  assert.deepEqual(store.notices, []);
});

test("a module's trap during an undo loses the module", { todo: "an undo's trap is reported as a notice" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.setValue("/World/Top.size", { type: "double", value: 2 });
  Object.defineProperty(store.session!.history, "undo", {
    value: () => {
      throw trap();
    },
  });
  await h.settle(store.undo());
  assert.equal(store.status.kind, "lost");
  assert.deepEqual(store.notices, []);
});

test("a module's trap in an open that was superseded still loses the module", { todo: "a superseded open's trap is dropped" }, async () => {
  const h = await harness();
  const { store } = h;
  const first = sampleSource();
  const release = first.hold("library.usda");
  first.fail("library.usda", trap());
  const opening = store.open(first);
  await h.turn(40);
  // Another open supersedes it before its fetch fails with the trap.
  const second = store.open(sampleSource("missing.usda"));
  release();
  await h.settle(Promise.all([opening, second]));
  assert.equal(store.status.kind, "lost");
});

test("the selection follows a renamed ancestor", { todo: "only the renamed prim itself is followed" }, async () => {
  const h = await harness();
  await h.open(payloadScene());
  const { store } = h;
  store.select("/World/Beacon/Post");
  store.explain("/World/Beacon/Post.size");
  store.move("/World/Beacon", "/World/Light");
  assert.deepEqual(store.notices, []);
  assert.equal(store.selection, "/World/Light/Post");
  assert.equal(store.property, "/World/Light/Post.size");
  assert.notEqual(store.session!.stage.prim(store.selection!), null);
  assert.equal(h.view.updates.at(-1)!.selected, "/World/Light/Post");
});

test("the selection follows a rename undone and redone", { todo: "an undo leaves the selection on the old path" }, async () => {
  const h = await harness();
  await h.open(payloadScene());
  const { store } = h;
  store.select("/World/Robot/Head");
  store.move("/World/Robot", "/World/Walker");
  await h.settle(store.undo());
  assert.equal(store.selection, "/World/Robot/Head");
  await h.settle(store.redo());
  assert.equal(store.selection, "/World/Walker/Head");
  assert.deepEqual(store.notices, []);
});

test("a scene without payloads, opened automatically, lets its prims be renamed", { todo: "its load rules are left at none, which refuses a move" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.move("/World/Top", "/World/Spinner");
  assert.deepEqual(store.notices, []);
  assert.notEqual(store.session!.stage.prim("/World/Spinner"), null);
});

test("undoing the definition of the selected prim clears the selection", { todo: "an undo leaves the selection on a prim that is gone" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.definePrim("/World", "Crate", "Cube");
  assert.equal(store.selection, "/World/Crate");
  await h.settle(store.undo());
  assert.equal(store.selection, null);
  assert.equal(h.view.updates.at(-1)!.selected, null);
});

test("the time range and up axis follow the stage's metadata as it is edited and undone", { todo: "they are read once, at open" }, async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.apply("set endTimeCode", [{ type: "setLayerMetadata", key: "endTimeCode", value: { type: "double", value: 96 } }]);
  assert.deepEqual(store.notices, []);
  assert.deepEqual(store.range, { start: 0, end: 96, timeCodesPerSecond: 24 });
  store.apply("set upAxis", [{ type: "setLayerMetadata", key: "upAxis", value: { type: "token", value: "Z" } }]);
  assert.equal(store.upAxis, "Z");
  assert.equal(h.view.upAxes.at(-1), "Z");
  await h.settle(store.undo());
  assert.equal(store.upAxis, "Y");
  assert.equal(h.view.upAxes.at(-1), "Y");
  await h.settle(store.undo());
  assert.equal(store.range!.end, 48);
  // A range that shrinks under the current time brings the time back into it.
  store.setTime(40);
  store.apply("set endTimeCode", [{ type: "setLayerMetadata", key: "endTimeCode", value: { type: "double", value: 24 } }]);
  assert.equal(store.time, 24);
});
