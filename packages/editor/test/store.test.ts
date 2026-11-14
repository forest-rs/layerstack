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
import { sceneChoice } from "../src/model/sources.ts";
import { harness, NESTED_LAYERS, PAYLOAD_ROOT, sampleSource, trap } from "./support/storeHarness.ts";

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

test("a ?scene= that names no layer is a failed open, not the sample opened unasked", async () => {
  assert.deepEqual(sceneChoice(null, "Sample", true), { kind: "sample" });
  assert.deepEqual(sceneChoice("Sample", "Sample", false), { kind: "sample" });
  assert.deepEqual(sceneChoice("set/root.usda", "Sample", true), { kind: "local", path: "set/root.usda" });
  const refused = sceneChoice("rocks", "Sample", true);
  assert.equal(refused.kind, "refused");
  assert.match(refused.kind === "refused" ? refused.message : "", /"rocks" is neither/);
  assert.equal(sceneChoice("set/root.usda", "Sample", false).kind, "refused", "a build serves no local scenes");
  // What the page does with it: the failed-open card, nothing opened.
  const h = await harness();
  h.store.failOpen("rocks", refused.kind === "refused" ? refused.message : "");
  assert.deepEqual(h.store.status, { kind: "failed", label: "rocks", message: refused.kind === "refused" ? refused.message : "" });
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

test("an edit the stage refuses returns why, is a notice, and changes nothing", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  assert.equal(store.setValue("/World/Top.size", { type: "double", value: 1 }), undefined, "an edit the stage takes returns nothing");
  await h.settle(store.undo());
  const refusal = store.setValue("/World/Top.size", { type: "token", value: "large" });
  assert.equal(typeof refusal, "string");
  assert.equal(store.notices.length, 1);
  assert.equal(store.notices[0]!.message, refusal);
  assert.equal(store.apply("nothing there", [{ type: "setValue", path: "/World/Nowhere.size", value: { type: "double", value: 1 } }]), store.notices[1]!.message);
  store.dismiss(store.notices[1]!.id);
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
  // The muted layer was released: a panel still holding it (the Layers tab, between the mute and its next read) asks of a layer the stage no longer has.
  assert.equal(store.isEdited(library), false);

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
  await h.until(() => second.askedFor("assets/robot.usda") > 0, "the open asking for its payloads");
  assert.deepEqual(store.status, { kind: "opening", label: "payloads.usda" });
  assert.notEqual(store.opening, null);
  assert.ok(second.asked.includes("assets/robot.usda"), "the open is waiting on its payloads");

  store.cancel();
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

test("a cancelled open stops fetching", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  // Its first round fetches a layer whose reference is the second round's.
  const second = sampleSource("nested.usda", NESTED_LAYERS);
  const release = second.hold("middle.usda");
  const opening = store.open(second);
  await h.until(() => second.askedFor("middle.usda") > 0, "the open's first round");
  assert.deepEqual(second.asked, ["nested.usda", "middle.usda"]);

  store.cancel();
  assert.equal(second.signals.get("middle.usda")?.aborted, true, "the fetch in flight is told to stop");
  release();
  await h.settle(opening);
  await h.turn(40);
  // The round in flight is not read, and the round after it is never asked for.
  assert.deepEqual(second.asked, ["nested.usda", "middle.usda"]);
  assert.equal(store.status.kind, "ready");
});

test("an action asked for while another waits on its assets runs after it", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  // Asked for while the variant's asset is in flight.
  const undo = store.undo();
  release();
  await h.settle(Promise.all([variant, undo]));

  assert.deepEqual(store.notices, []);
  assert.equal(session.loops.current, null);
  const run = session.loops.runs.find((each) => each.cause === "finish=chrome")!;
  assert.equal(run.error, null);
  assert.equal(run.assets[0]!.outcome, "read");
  // The undo ran once the variant had what it needed, and undid it.
  assert.equal(variantOf(session.stage, "/World/Robot", "finish"), "painted");
  assert.equal(session.history.nextUndo, null);
  assert.equal(session.history.redoSteps.length, 1);
});

test("two undos asked for at once undo two steps, one after the other", async () => {
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
  await h.until(() => chrome() === 2, "the first undo's fetch");
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

test("opening another scene while payloads load ends the load and opens it", async () => {
  const h = await harness();
  const first = payloadScene();
  const { store } = h;
  store.setPayloads("none");
  await h.open(first);
  const release = first.hold("assets/robot.usda");
  const loading = store.load("/World");
  await h.until(() => first.askedFor("assets/robot.usda") > 0, "the load's fetch");
  assert.equal(store.status.kind, "loading");

  // The open ends the load itself: the held fetch is let go only once it has.
  const second = sampleSource();
  await h.settle(store.open(second));
  await h.settle(loading);
  release();
  await h.turn(20);

  assert.equal(store.status.kind, "ready");
  assert.equal(store.source, second);
  assert.deepEqual(store.notices, []);
  assert.notEqual(store.session!.stage.prim("/World/Ground"), null);
  // The load of the scene left was told to stop.
  assert.equal(first.signals.get("assets/robot.usda")?.aborted, true);
});

test("a module's trap during an edit loses the module, not just the edit", async () => {
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

test("a module's trap during an undo loses the module", async () => {
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

test("a module's trap in an open that was superseded still loses the module", async (t) => {
  const h = await harness();
  const { store } = h;
  const first = sampleSource();
  const release = first.hold("library.usda");
  const opening = store.open(first);
  // The open has made its stage by the time it asks for a sublayer: superseded before, it would have nothing to free.
  await h.until(() => first.askedFor("library.usda") > 0, "the open's first round");
  // Letting go of what the superseded open made is a call into the module, and it traps.
  t.mock.method(Session.prototype, "free", () => {
    throw trap();
  });
  const second = store.open(sampleSource("missing.usda"));
  release();
  await h.settle(Promise.all([opening, second]));
  assert.equal(store.status.kind, "lost");
  // Lost is for good: an open asked for afterwards does nothing.
  await h.settle(store.open(sampleSource()));
  assert.equal(store.status.kind, "lost");
});

test("the selection follows a renamed ancestor", async () => {
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

test("the selection follows a rename undone and redone", async () => {
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

test("a scene without payloads, opened automatically, lets its prims be renamed", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.move("/World/Top", "/World/Spinner");
  assert.deepEqual(store.notices, []);
  assert.notEqual(store.session!.stage.prim("/World/Spinner"), null);
});

test("undoing the definition of the selected prim clears the selection", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.definePrim("/World", "Crate", "Cube");
  assert.equal(store.selection, "/World/Crate");
  await h.settle(store.undo());
  assert.equal(store.selection, null);
  assert.equal(h.view.updates.at(-1)!.selected, null);
});

test("the time range and up axis follow the stage's metadata as it is edited and undone", async () => {
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

test("cancelling a payload load stops its fetches and leaves the payloads as they were", async () => {
  const h = await harness();
  const source = payloadScene();
  const { store } = h;
  store.setPayloads("none");
  await h.open(source);
  const session = store.session!;
  await h.settle(store.load("/World/Beacon"));
  const rules = session.stage.loadRules();

  const release = source.hold("assets/robot.usda");
  const loading = store.load("/World");
  await h.until(() => source.askedFor("assets/robot.usda") > 0, "the load's fetch");
  assert.equal(store.status.kind, "loading");
  assert.equal(store.running, "load /World");
  assert.equal(store.cancellable, true);
  store.cancel();
  await h.settle(loading);
  release();
  await h.turn(20);

  assert.equal(store.status.kind, "ready");
  assert.equal(store.running, null);
  assert.equal(source.signals.get("assets/robot.usda")?.aborted, true);
  assert.deepEqual(session.stage.loadRules(), rules);
  assert.equal(session.stage.prim("/World/Robot/Body"), null);
  assert.notEqual(session.stage.prim("/World/Beacon/Post"), null, "what was loaded before stays loaded");
  assert.equal(session.loops.runs.at(-1)!.error, "cancelled");
  assert.equal(session.loops.current, null);
  assert.deepEqual(store.notices, []);
  // Loading it again, uncancelled, fetches it and loads it.
  await h.settle(store.load("/World/Robot"));
  assert.notEqual(session.stage.prim("/World/Robot/Body"), null);
  assert.deepEqual(store.notices, []);
});

test("an edit asked for while an action fetches runs after it, and a refusal is then a notice", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  assert.equal(store.running, "select finish=chrome");
  assert.equal(store.queued, 0);
  assert.equal(store.cancellable, false, "a variant's fetch is not taken back");
  const status = store.signals.revision("status");
  assert.equal(store.setValue("/World/Top.size", { type: "double", value: 3 }), undefined);
  assert.equal(store.setValue("/World/Top.size", { type: "token", value: "large" }), undefined, "not refused yet: it has not run");
  assert.equal(store.queued, 2, "what the toolbar counts behind it");
  assert.ok(store.signals.revision("status") > status, "the toolbar hears what queues");
  assert.equal(valueOf(store.session!.stage, "/World/Top.size"), 0.6, "the edit waits its turn");
  release();
  await h.settle(variant);
  await h.until(() => store.running === null && store.notices.length > 0, "the edits behind the variant");
  assert.equal(store.running, null);
  assert.equal(store.queued, 0);
  assert.equal(valueOf(store.session!.stage, "/World/Top.size"), 3);
  assert.equal(store.notices.length, 1);
  // Each recomposition is logged under the action that caused it.
  assert.deepEqual(
    store.session!.changeLog.slice(-2).map((entry) => entry.cause),
    ["finish=chrome", "set /World/Top.size"],
  );
});

test("edits asked for while an action fetches are written where edits went when they were asked for", async () => {
  const h = await harness();
  const source = sampleSource("scene.usda", { "extra.usda": '#usda 1.0\n\ndef Scope "Extra"\n{\n}\n' });
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  const root = stage.root;
  const library = session.layerNamed("library.usda");
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");

  // Asked for with edits going to the root: a value, a prim, a shader, a sublayer, a save.
  assert.equal(store.setValue("/World/Top.size", { type: "double", value: 3 }), undefined);
  assert.equal(store.setValue("/World/Top.size", { type: "double", value: 4 }, 12), undefined);
  assert.equal(store.definePrim("/World", "Late", "Scope"), undefined);
  assert.equal(store.addShader("/World/Looks/Checker/Reader2", "UsdPrimvarReader_float2"), undefined);
  const sublayer = store.insertSublayer(root, "./extra.usda", 1);
  const save = store.save();
  assert.equal(store.queued, 6);
  // Then the toolbar is pointed at the sublayer, which takes effect at once.
  store.setTarget(library);
  assert.equal(session.target, library);
  // And one more, asked for with edits going there.
  assert.equal(store.definePrim("/World", "Later", "Scope"), undefined);
  release();
  await h.settle(variant);
  await h.settle(sublayer);
  const saved = await h.settle(save);
  await h.until(() => store.running === null && store.queued === 0, "the edits behind the variant");
  assert.deepEqual(store.notices, []);

  const authors = (path: string) => [root, library].filter((layer) => stage.layerSpec(layer, path) !== null).map((layer) => layer.identifier);
  assert.equal(valueOf(stage, "/World/Top.size"), 3, "the root's opinion, which shows");
  const size = stage.layerSpec(root, "/World/Top.size");
  assert.deepEqual(size?.type === "attribute" ? size.timeSamples : null, [12], "at the time it was asked at");
  assert.deepEqual(authors("/World/Top.size"), ["scene.usda"]);
  assert.deepEqual(authors("/World/Late"), ["scene.usda"]);
  assert.deepEqual(authors("/World/Looks/Checker/Reader2"), ["scene.usda"]);
  assert.deepEqual(authors("/World/Later"), ["library.usda"], "asked for after the change, so written there");
  assert.deepEqual(stage.sublayers(root).map((each) => each.asset), ["./library.usda", "./extra.usda"], "a sublayer edit names its layer");
  assert.deepEqual(saved?.written.map((layer) => layer.identifier), ["scene.usda"], "the save took what was asked for before it, not what came after");
  assert.equal(session.target, library, "where edits go stays as chosen");
  assert.equal(store.shadow, null, "no note names the wrong layer");
  // Each is one step, undone in the layer it was written to.
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Later"), null);
  assert.equal(session.target, library);
});

test("jumping between keys pauses and lands on the selected prim's keys", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  assert.equal(store.jumpToKey(1), null, "nothing is selected");
  store.select("/World/Top");
  store.setTime(10);
  assert.equal(store.jumpToKey(1), 48);
  assert.equal(store.time, 48);
  assert.equal(store.jumpToKey(1), null, "no key after the last, and the time stays");
  assert.equal(store.time, 48);
  assert.equal(store.jumpToKey(-1), 0);
  assert.equal(store.jumpToKey(-1), null);

  // A key authored between them is walked through once the composition has it.
  assert.equal(store.setValue("/World/Top.xformOp:rotateY", { type: "double", value: 180 }, 24), undefined);
  assert.equal(store.jumpToKey(1), 24);
  assert.equal(store.jumpToKey(1), 48);

  // Playing is paused by a jump.
  store.setTime(0);
  store.play();
  assert.equal(store.playing, true);
  assert.equal(store.jumpToKey(1), 24);
  assert.equal(store.playing, false);
});

test("seek pauses and clamps to the range", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.play();
  store.seek(30);
  assert.equal(store.playing, false);
  assert.equal(store.time, 30);
  store.seek(-4);
  assert.equal(store.time, 0);
  store.seek(500);
  assert.equal(store.time, 48);
});

test("the loop setting loops playback at the end, or stops it there", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store, frames } = h;
  assert.equal(store.loop, true, "looping is the default");

  store.setLoop(false);
  assert.equal(store.loop, false);
  store.setTime(47);
  store.play();
  frames.run(20);
  assert.equal(store.playing, true);
  frames.run(2000);
  assert.equal(store.time, 48, "stopped on the last frame");
  assert.equal(store.playing, false);
  assert.equal(frames.waiting, 0, "no frame is left requested");

  // Playing from the end starts over at the start.
  store.play();
  assert.equal(store.time, 0);
  assert.equal(store.playing, true);
  store.pause();

  store.setLoop(true);
  store.setTime(47);
  store.play();
  frames.run(20);
  frames.run(2000);
  assert.equal(store.playing, true, "still playing past the end");
  assert.ok(store.time! < 48);
  store.pause();
});
