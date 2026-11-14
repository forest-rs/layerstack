// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What leaves the page, through the store as the File menu and the Text
// tab drive it: a save writes the dirty layers and clears them, exports
// return the bytes of their format, a flatten says what it lost, a
// package fetches what the stage never read, a USDZ root opens editable
// in a session layer, and the page knows when leaving would lose edits.
import assert from "node:assert/strict";
import { test } from "node:test";
import { flattenConfirmation, packageConfirmation } from "../src/model/files.ts";
import type { SceneSource } from "../src/model/sources.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);
const HIDE_ROBOT = { type: "token", value: "invisible" } as const;

test("a save writes the edited layer, clears it, and says so on the history channel", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  assert.deepEqual(store.dirtyLayers(), []);
  assert.equal(store.unsaved, false, "nothing to lose on open");
  assert.deepEqual(store.savePlan(), []);

  assert.equal(store.setValue("/World/Robot.visibility", HIDE_ROBOT), undefined);
  assert.deepEqual(store.dirtyLayers().map((layer) => layer.identifier), ["scene.usda"]);
  assert.equal(store.unsaved, true, "leaving would lose the edit");
  assert.deepEqual(
    store.savePlan().map((entry) => [entry.layer.identifier, entry.name, entry.disposition]),
    [["scene.usda", "scene.usda", "write"]],
  );

  const before = store.signals.revision("history");
  const result = await h.settle(store.save());
  assert.ok(result !== null);
  assert.deepEqual(result.written.map((layer) => layer.identifier), ["scene.usda"]);
  assert.match(text(result.written[0]!.bytes), /token visibility = "invisible"/);
  assert.deepEqual(result.failures, []);
  assert.deepEqual(store.dirtyLayers(), []);
  assert.equal(store.unsaved, false, "saved, nothing to lose");
  assert.ok(store.signals.revision("history") > before, "the File menu and the Layers tab hear the layers are clean");
  assert.deepEqual(store.session!.saved.map((file) => file.identifier), ["scene.usda"]);
});

test("an edit undone back to the save leaves nothing unsaved, though the stage still reads the layer dirty", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const root = store.session!.stage.root;
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  await h.settle(store.undo());
  assert.ok(store.session!.stage.isDirty(root), "dirty means changed since read, as in OpenUSD");
  assert.deepEqual(store.dirtyLayers(), []);
  assert.equal(store.unsaved, false, "nothing to lose on leaving");
  await h.settle(store.redo());
  assert.deepEqual(store.dirtyLayers(), [root]);

  // Saved with the edit, then undone: the layer differs from what was saved.
  await h.settle(store.save());
  assert.deepEqual(store.dirtyLayers(), []);
  await h.settle(store.undo());
  assert.deepEqual(store.dirtyLayers(), [root], "the undo changed it from the save");
  await h.settle(store.redo());
  assert.deepEqual(store.dirtyLayers(), [], "back at the save");
  // Undone past the save and edited anew: the save's step is gone, and the layer counts.
  await h.settle(store.undo());
  store.setValue("/World/Robot.visibility", { type: "token", value: "inherited" });
  assert.deepEqual(store.dirtyLayers(), [root]);
});

test("each layer counts as unsaved only while a step since its save writes it", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const session = store.session!;
  const library = session.layerNamed("library.usda");
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  store.setTarget(library);
  assert.equal(store.setValue("/World/Robot.visibility", { type: "token", value: "inherited" }), undefined);
  assert.deepEqual(new Set(store.dirtyLayers().map((layer) => layer.identifier)), new Set(["scene.usda", "library.usda"]));
  await h.settle(store.undo());
  assert.deepEqual(store.dirtyLayers().map((layer) => layer.identifier), ["scene.usda"], "only the library's step was undone");
  await h.settle(store.undo());
  assert.deepEqual(store.dirtyLayers(), []);
});

test("a save writes only the layers its confirmation lists, not one whose edits were undone", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const session = store.session!;
  const root = session.stage.root;
  // The root is edited and undone: dirty to the stage, with nothing to save.
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  await h.settle(store.undo());
  store.setTarget(session.layerNamed("library.usda"));
  assert.equal(store.setValue("/World/Robot.visibility", { type: "token", value: "inherited" }), undefined);
  assert.ok(session.stage.isDirty(root));
  assert.deepEqual(store.savePlan().map((entry) => entry.layer.identifier), ["library.usda"], "what the confirmation lists");

  const result = await h.settle(store.save());
  assert.deepEqual(result?.written.map((layer) => layer.identifier), ["library.usda"], "the root's file is not replaced");
  assert.equal(result?.bytes, result?.written[0]!.bytes.byteLength);
  assert.deepEqual(session.saved.map((file) => file.identifier), ["library.usda"]);
  assert.deepEqual(store.dirtyLayers(), []);
  // The undone edit, redone, is unsaved again; the library's is not.
  store.setTarget(undefined);
  await h.settle(store.undo());
  assert.deepEqual(store.dirtyLayers().map((layer) => layer.identifier), ["library.usda"]);
  await h.settle(store.redo());
  assert.deepEqual(store.dirtyLayers(), []);
});

test("the page has something to lose until a save's bytes are handed on, not only until the stage wrote them", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  // A write into a folder that has not finished: the stage counts the layer saved, and no file holds it yet.
  let written = () => {};
  const writing = new Promise<void>((resolve) => (written = resolve));
  const saving = store.persisting(async () => {
    const result = await store.save();
    await writing;
    return result;
  });
  await h.turn();
  assert.deepEqual(store.dirtyLayers(), [], "saved, to the stage");
  assert.equal(store.unsaved, true, "leaving now would lose the bytes");
  written();
  assert.deepEqual((await h.settle(saving))?.written.map((layer) => layer.identifier), ["scene.usda"]);
  assert.equal(store.unsaved, false);
  // A write that fails ends the wait all the same.
  await assert.rejects(store.persisting(() => Promise.reject(new Error("the disk is full"))));
  assert.equal(store.unsaved, false);
});

test("a layer whose edits were undone is read again from the source, with nothing to save first", async () => {
  const h = await harness();
  const files: Record<string, string> = { "plain.usda": '#usda 1.0\ndef Xform "World"\n{\n    double size = 1\n}\n' };
  await h.open(sampleSource("plain.usda", files));
  const { store } = h;
  const stage = store.session!.stage;
  const root = stage.root;
  assert.equal(store.setValue("/World.size", { type: "double", value: 2 }), undefined);
  // With the edit, it is refused: reading it again would lose the edit.
  assert.match((await h.settle(store.reload(root))).refusal ?? "", /has unsaved edits/);
  await h.settle(store.undo());
  assert.ok(stage.isDirty(root), "dirty to the stage, with nothing to save");
  assert.deepEqual(store.savePlan(), []);
  // Another program writes the file.
  files["plain.usda"] = '#usda 1.0\ndef Xform "World"\n{\n    double size = 5\n}\n';
  const report = await h.settle(store.reload(root));
  assert.equal(report.refusal, null);
  assert.deepEqual(report.reloaded, [root]);
  assert.deepEqual(stage.attribute("/World.size")?.get(), { kind: "scalar", type: "double", value: 5 });
  assert.deepEqual(store.dirtyLayers(), []);
  // The undone edit could be redone into a layer that is no longer what it changed: the history is forgotten, and says so.
  assert.equal(report.forgotten, 1);
  assert.equal(store.session!.history.canRedo, false);
  assert.deepEqual(
    store.notices.map((notice) => notice.message.slice(0, 40)),
    ["plain.usda has unsaved edits: save or un", "the undo history (1 step) was forgotten:"],
  );
  // Every layer read again leaves none out for edits that were undone.
  store.setValue("/World.size", { type: "double", value: 3 });
  await h.settle(store.undo());
  files["plain.usda"] = '#usda 1.0\ndef Xform "World"\n{\n    double size = 7\n}\n';
  const all = await h.settle(store.reload(null));
  assert.deepEqual(all.skipped, []);
  assert.deepEqual(stage.attribute("/World.size")?.get(), { kind: "scalar", type: "double", value: 7 });
});

test("a layer exports as USDA text or USDC binary, and stays edited", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  const root = store.session!.stage.root;
  const usda = store.exportLayer(root, "usda");
  assert.ok(usda !== null);
  assert.match(text(usda), /^#usda 1\.0/);
  assert.match(text(usda), /token visibility = "invisible"/);
  const usdc = store.exportLayer(root, "usdc");
  assert.ok(usdc !== null);
  assert.equal(text(usdc.subarray(0, 8)), "PXR-USDC");
  assert.ok(store.session!.stage.isDirty(root), "an export saves nothing");
  assert.deepEqual(store.notices, []);
});

/** A root whose prim reads value clips from a layer that is not there: a flatten cannot bake them. */
const MISSING_CLIP = `#usda 1.0
def Xform "World" (
    clips = {
        dictionary default = {
            asset[] assetPaths = [@./missing_clip.usda@]
            string primPath = "/Clip"
            double2[] active = [(0, 0)]
        }
    }
)
{
    double size = 1
}
`;

test("a flatten of the sample is lossless; one that cannot bake value clips says what it lost", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const lossless = h.store.exportFlattened("usda");
  assert.ok(lossless !== null);
  assert.equal(lossless.lossless, true);
  assert.deepEqual(lossless.lost, []);
  assert.doesNotMatch(text(lossless.bytes), /@\.\/assets\/|subLayers/, "no arcs to other layers");
  // Lossless, it is still confirmed first, as a save is: what will be written, then one button.
  assert.deepEqual(flattenConfirmation(lossless, "scene.flat.usda", "12.0 KB"), {
    title: "Export the stage flattened as scene.flat.usda",
    what: "scene.flat.usda, 12.0 KB: every composed prim and value written into one layer with no arcs (Stage.exportFlattened). It loses nothing.",
    confirm: "Download scene.flat.usda",
  });

  await h.open(sampleSource("clips.usda", { "clips.usda": MISSING_CLIP }));
  const flat = h.store.exportFlattened("usdc");
  assert.ok(flat !== null);
  assert.equal(flat.lossless, false);
  assert.deepEqual(flat.lost, [{ path: "/World", message: "lost: value clips" }]);
  assert.equal(text(flat.bytes.subarray(0, 8)), "PXR-USDC", "written all the same, for the person to decide");
  const lossy = flattenConfirmation(flat, "clips.flat.usdc", "1.0 KB");
  assert.equal(lossy.title, "The flattened stage loses one thing");
  assert.equal(lossy.confirm, "Download clips.flat.usdc anyway");
});

test("a save leaves the session layer, and says it is the session layer", async () => {
  const h = await harness();
  h.store.setEditLayerChoice("session");
  await h.open(sampleSource());
  const { store } = h;
  const sessionLayer = store.session!.sessionLayer!;
  assert.ok(sessionLayer.anonymous);
  assert.equal(store.session!.sessionNote, null, "asked for, not forced");
  // A new session layer is clean until an edit writes it.
  assert.equal(store.session!.stage.isDirty(sessionLayer), false);
  assert.deepEqual(store.dirtyLayers(), []);
  assert.equal(store.unsaved, false);
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  assert.deepEqual(store.dirtyLayers(), [sessionLayer]);
  assert.equal(store.unsaved, true, "the session layer's edits are lost with the page");
  assert.deepEqual(
    store.savePlan().map((entry) => [entry.name, entry.disposition]),
    [["session.usda", "session"]],
  );
  const result = await h.settle(store.save());
  assert.deepEqual(result?.written, []);
  assert.equal(store.unsaved, true, "still unsaved: only an export keeps it");
  const exported = store.exportLayer(sessionLayer, "usda");
  assert.ok(exported !== null);
  assert.match(text(exported), /over "World"[\s\S]*over "Robot"[\s\S]*token visibility = "invisible"/);
  // Undone, the session layer holds nothing to lose.
  await h.settle(store.undo());
  assert.equal(store.unsaved, false);
});

test("a save leaves a sublayer of the session layer, and its confirmation says so", async () => {
  const h = await harness();
  h.store.setEditLayerChoice("session");
  await h.open(sampleSource("scene.usda", { "patch.usda": '#usda 1.0\nover "World"\n{\n}\n' }));
  const { store } = h;
  const session = store.session!;
  const sessionLayer = session.sessionLayer!;
  assert.equal(await h.settle(store.insertSublayer(sessionLayer, "patch.usda", 0)), undefined);
  const patch = session.layerNamed("patch.usda");
  store.setTarget(patch);
  assert.equal(store.setValue("/World/Robot.visibility", HIDE_ROBOT), undefined);
  // The session layer lists the patch; the patch holds the edit.
  assert.deepEqual(
    store.savePlan().map((entry) => [entry.name, entry.disposition]),
    [
      ["session.usda", "session"],
      ["patch.usda", "sessionSublayer"],
    ],
    "not listed as a file the save writes",
  );
  const result = await h.settle(store.save());
  assert.deepEqual(result?.written, [], "as UsdStage::Save leaves the session layer's stack");
  assert.deepEqual(result?.skipped, []);
  assert.deepEqual(new Set(store.dirtyLayers()), new Set([sessionLayer, patch]), "both still have edits to lose");
  assert.equal(store.unsaved, true);
  // Exported, it has the edit.
  assert.match(text(store.exportLayer(patch, "usda")!), /token visibility = "invisible"/);
});

/** A root whose variant not selected references a layer that names a texture. */
const VARIANT_ROOT = `#usda 1.0
def Xform "World" (
    variants = {
        string look = "plain"
    }
    prepend variantSets = "look"
)
{
    variantSet "look" = {
        "plain" {
        }
        "fancy" (
            references = @./assets/fancy.usda@
        ) {
        }
    }
}
`;
const fancy = (texture: string) => `#usda 1.0
(
    defaultPrim = "Fancy"
)
def Xform "Fancy"
{
    asset texture = @${texture}@
}
`;

test("a package fetches what the layer of a variant not selected names, or says which layer wants it", async () => {
  const h = await harness();
  const source = sampleSource("variant.usda", { "variant.usda": VARIANT_ROOT, "assets/fancy.usda": fancy("../textures/checker.png") });
  await h.open(source);
  const { store } = h;
  assert.ok(!source.asked.includes("assets/fancy.usda"), "the stage never read the variant's layer");
  const packaged = await h.settle(store.exportPackage());
  assert.deepEqual(store.notices, []);
  assert.ok(packaged?.bytes != null, JSON.stringify(packaged?.unresolved.map((each) => each.reason)));
  assert.ok(source.asked.includes("assets/fancy.usda") && source.asked.includes("textures/checker.png"), "the layer, then the texture it names");
  assert.equal(packaged.layers, 2);
  assert.equal(packaged.assets, 1);
  assert.ok(!store.session!.stage.layers().some((layer) => layer.identifier === "assets/fancy.usda"), "the stage still does not hold it");
  assert.equal(store.session!.history.undoSteps.length, 0);

  // A texture that is not there is listed with the layer that names it, a layer the stage does not hold.
  await h.open(sampleSource("variant.usda", { "variant.usda": VARIANT_ROOT, "assets/fancy.usda": fancy("./gone.png") }));
  const failed = await h.settle(store.exportPackage());
  assert.deepEqual(store.notices, []);
  assert.equal(failed?.bytes, null);
  assert.deepEqual(
    failed?.unresolved.map((each) => [each.layer, each.identifier, each.assetPath]),
    [[null, "assets/fancy.usda", "./gone.png"]],
  );
});

/** A root referencing a model whose textures are beside the model. */
const MODEL_ROOT = `#usda 1.0
def Xform "World"
{
    def "Model" (
        references = @./assets/model.usda@
    )
    {
    }
}
`;
const MODEL = `#usda 1.0
(
    defaultPrim = "Model"
)
def Xform "Model"
{
    asset bark = @./textures/bark.png@
    asset leaf = @./textures/leaf.png@
}
`;

test("a flatten says which asset path of the session layer it could not anchor, and anchors the others", async () => {
  const h = await harness();
  h.store.setEditLayerChoice("session");
  await h.open(sampleSource("model.usda", { "model.usda": MODEL_ROOT, "assets/model.usda": MODEL }));
  const { store } = h;
  assert.equal(store.exportFlattened("usda")?.lossless, true);
  // A path relative to the session layer, which is nowhere: the flatten cannot say where it points.
  assert.equal(store.setValue("/World/Model.leaf", { type: "asset", value: "./other.png" }), undefined);
  const flat = store.exportFlattened("usda");
  assert.ok(flat !== null);
  assert.equal(flat.lossless, false, "held for the person to read, not downloaded at once");
  assert.deepEqual(flat.lost.map((loss) => loss.path), ["/World/Model.leaf"]);
  assert.match(text(flat.bytes), /asset bark = @\.\/assets\/textures\/bark\.png@/, "the model's own path still points at its texture");
  assert.equal(store.session!.history.undoSteps.length, 1, "an export is no step");
  // Undone, nothing is lost again.
  await h.settle(store.undo());
  assert.equal(store.exportFlattened("usda")?.lossless, true);
});

test("an edit asked for while a package is being made goes where edits went when it was asked for", { todo: "queued edits read the target when they run: owned by review-property-ops" }, async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const library = session.layerNamed("library.usda");
  const release = source.hold("textures/checker.png");
  const packing = store.exportPackage();
  await h.until(() => source.asked.includes("textures/checker.png"), "the package asking for a texture");
  // Asked for with edits going to the root; the target changes before the package is made.
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  store.setTarget(library);
  release();
  await h.settle(packing);
  await h.turn();
  assert.deepEqual(store.dirtyLayers().map((layer) => layer.identifier), ["scene.usda"]);
  assert.equal(session.stage.layerSpec(library, "/World/Robot.visibility"), null);
});

/** A source serving one USDZ package as its root. */
function packageSource(name: string, bytes: Uint8Array): SceneSource {
  return {
    label: name,
    root: name,
    fetch: async (identifier) => (identifier === name ? bytes : null),
    exists: async (identifier) => identifier === name,
    size: async (identifier) => (identifier === name ? bytes.byteLength : null),
  };
}

test("a package fetches what the stage never read, and a USDZ root opens editable in a session layer", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  // Neither the chrome variant's layer nor the textures were read by the stage: the package fetches them.
  const packaged = await h.settle(store.exportPackage());
  assert.ok(packaged !== null);
  assert.ok(packaged.bytes !== null, JSON.stringify(packaged.unresolved.map((each) => each.reason)));
  assert.deepEqual(packaged.unresolved, []);
  assert.deepEqual(packaged.omitted, [], "no session layer");
  assert.equal(packaged.layers, 5, "the root, its sublayer, and the robot, the chrome finish and the beacon");
  assert.equal(packaged.assets, 3, "the checker and the two UDIM tiles");
  assert.ok(source.asked.includes("textures/tile.1002.png"), "every tile of the UDIM set");
  assert.ok(store.dirtyLayers().length === 1, "packaging saves nothing");
  // Made, then confirmed: what it holds first, then one button that downloads it.
  assert.deepEqual(packageConfirmation(packaged, "scene.usdz", "1.0 MB", false), {
    title: "Export the stage as scene.usdz",
    what: "A package holds the root layer and everything it depends on, with their unsaved edits (Stage.exportPackage). scene.usdz: 5 layers and 3 other assets, 1.0 MB.",
    confirm: "Download scene.usdz",
  });
  assert.equal(packageConfirmation({ ...packaged, bytes: null }, "scene.usdz", null, false).confirm, null, "nothing to download");
  // The variant whose layer the package fetched still composes when chosen.
  await h.settle(store.selectVariant("/World/Robot", "finish", "chrome"));
  assert.deepEqual(store.notices, []);
  assert.ok(store.session!.stage.layers().some((layer) => layer.identifier === "assets/chrome.usda"));

  // Opened as the root, the package is read-only: edits go to a session layer, and the scene says why.
  assert.equal(store.editLayerChoice, "root");
  await h.open(packageSource("sample.usdz", packaged.bytes));
  const session = store.session!;
  assert.match(session.sessionNote ?? "", /\.usdz package, which is read-only/);
  const sessionLayer = session.sessionLayer;
  assert.ok(sessionLayer !== null && sessionLayer.anonymous);
  assert.equal(session.stage.editLayer, sessionLayer);
  assert.deepEqual(store.dirtyLayers().map((layer) => layer.identifier), []);
  assert.equal(store.unsaved, false);
  assert.deepEqual(session.stage.attribute("/World/Robot.visibility")?.get(), { kind: "scalar", type: "token", value: "invisible" }, "the package holds the unsaved edit");
  // An edit applies, where edits go.
  assert.equal(store.setValue("/World/Beacons/North.visibility", HIDE_ROBOT), undefined);
  assert.deepEqual(store.notices, []);
  assert.deepEqual(store.dirtyLayers(), [sessionLayer]);
  assert.deepEqual(store.savePlan().map((entry) => entry.disposition), ["session"]);
  // A package of it leaves the session layer out, and says so.
  const again = await h.settle(store.exportPackage());
  assert.ok(again?.bytes != null);
  assert.deepEqual(again.omitted, [sessionLayer]);
  assert.deepEqual(
    (({ title, confirm }) => [title, confirm])(packageConfirmation(again, "sample.usdz", "1.0 MB", true)),
    ["The package leaves out the session layer", "Download the package without them"],
  );
});

test("the Text tab reads a layer as USDA and marks the lines the last edit changed", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const root = store.session!.stage.root;
  assert.equal(store.textLayer, root, "the edit layer by default");
  const first = store.layerText(root);
  assert.ok(typeof first !== "string");
  assert.deepEqual(first.changes.added, [], "nothing to compare the first read with");
  assert.equal(first.truncated, false);

  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  const after = store.layerText(root);
  assert.ok(typeof after !== "string");
  const changed = after.changes.added.map((line) => after.lines[line]!.trim());
  assert.ok(changed.includes('token visibility = "invisible"'), JSON.stringify(changed));
  assert.ok(changed.length < 4, `only what the edit authored: ${JSON.stringify(changed)}`);
  // An edit elsewhere leaves the text as it was, and what the last edit here showed.
  const again = store.layerText(root);
  assert.ok(typeof again !== "string");
  assert.deepEqual(again.changes, after.changes);

  // Undone, the line is gone, and the text says where.
  await h.settle(store.undo());
  const undone = store.layerText(root);
  assert.ok(typeof undone !== "string");
  assert.deepEqual(undone.changes.added, []);
  assert.ok(undone.changes.removed >= 1 && undone.changes.removedBefore.length > 0);

  assert.equal(after.steps, 1, "what the last change did");
  assert.equal(undone.steps, 1);

  // Two edits with no read between them (the tab not shown) are marked as two changes, not as the last one.
  store.setValue("/World/Robot.visibility", HIDE_ROBOT);
  store.setValue("/World/Beacons/North.visibility", HIDE_ROBOT);
  // An edit of another layer between the reads is none of this layer's.
  store.setTarget(store.session!.layerNamed("library.usda"));
  store.setValue("/World/Robot.visibility", { type: "token", value: "inherited" });
  store.setTarget(undefined);
  const both = store.layerText(root);
  assert.ok(typeof both !== "string");
  assert.ok(both.changes.added.length >= 2);
  assert.equal(both.steps, 2, "the marks are two changes' lines");
  // Read again with nothing changed, they are still those two.
  const kept = store.layerText(root);
  assert.ok(typeof kept !== "string");
  assert.equal(kept.steps, 2);

  // Showing another layer brings the tab forward.
  const library = store.session!.stage.layers().find((layer) => layer.identifier === "library.usda")!;
  store.showBottomTab("explain");
  store.showLayerText(library);
  assert.equal(store.textLayer, library);
  assert.equal(store.bottomTab, "text");
});
