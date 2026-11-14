// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The layer stack, the stage's metadata, the stage's health and reading
// layers again from the source, through `EditorStore` as the Layers,
// Stage and Health tabs drive it, over the sample scene and layers a test
// serves beside it.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Layer, Stage } from "@forest-rs/layerstack";
import { layerStack, reloadText } from "../src/model/layerStack.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

/** A layer with one root prim, `name`, to add as a sublayer. */
const layerWith = (...names: string[]) => `#usda 1.0\n${names.map((name) => `def "${name}"\n{\n}\n`).join("\n")}`;

/** The asset paths `layer` lists as its sublayers, with their offsets when not the identity. */
function sublayersOf(stage: Stage, layer: Layer): string[] {
  return stage.sublayers(layer).map(({ asset, layerOffset }) => (layerOffset.offset === 0 && layerOffset.scale === 1 ? asset : `${asset}+${layerOffset.offset}x${layerOffset.scale}`));
}

test("a sublayer is added, made stronger, given an offset and removed, each one undoable step that runs the asset loop", async () => {
  const h = await harness();
  const source = sampleSource("scene.usda", { "extra.usda": layerWith("Extra") });
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  const root = stage.root;
  assert.deepEqual(sublayersOf(stage, root), ["./library.usda"]);

  assert.equal(await h.settle(store.insertSublayer(root, "./extra.usda", 1)), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./library.usda", "./extra.usda"]);
  assert.ok(source.asked.includes("extra.usda"), "the asset loop fetched the new sublayer");
  assert.notEqual(stage.prim("/Extra"), null, "its prims compose");
  assert.equal(session.history.nextUndo!.label, "add sublayer @./extra.usda@ to scene.usda");
  assert.deepEqual(
    session.history.nextUndo!.layers.map((layer) => layer.identifier),
    ["scene.usda"],
  );

  assert.equal(await h.settle(store.moveSublayer(root, 1, -1, "./extra.usda")), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda", "./library.usda"]);
  // The strongest cannot move stronger: said, and nothing recorded.
  const steps = session.history.undoSteps.length;
  assert.match((await h.settle(store.moveSublayer(root, 0, -1, "./extra.usda")))!, /the strongest sublayer of scene\.usda already/);
  assert.equal(session.history.undoSteps.length, steps);

  assert.equal(await h.settle(store.setSublayerOffset(root, 0, { offset: 10, scale: 2 }, "./extra.usda")), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda+10x2", "./library.usda"]);

  assert.equal(await h.settle(store.removeSublayer(root, 0, "./extra.usda")), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./library.usda"]);
  assert.equal(stage.prim("/Extra"), null);

  await h.settle(store.undo());
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda+10x2", "./library.usda"]);
  assert.notEqual(stage.prim("/Extra"), null, "undoing the removal composes the sublayer again");
  await h.settle(store.undo());
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda", "./library.usda"]);
  await h.settle(store.undo());
  assert.deepEqual(sublayersOf(stage, root), ["./library.usda", "./extra.usda"]);
  await h.settle(store.undo());
  assert.deepEqual(sublayersOf(stage, root), ["./library.usda"]);
  assert.equal(stage.prim("/Extra"), null);
  await h.settle(store.redo());
  assert.notEqual(stage.prim("/Extra"), null);
  assert.deepEqual(store.notices.map((notice) => notice.action), ["make @./extra.usda@ stronger in scene.usda"]);
});

test("a sublayer edit queued behind one that changes the list acts on the sublayer its row named, not on what took its index", async () => {
  const h = await harness();
  const source = sampleSource("scene.usda", { "extra.usda": layerWith("Extra"), "more.usda": layerWith("More") });
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  const root = stage.root;
  const authored = () => stage.layerSpec(root, "/").sublayers.map(({ asset, layerOffset }) => [asset, layerOffset.offset, layerOffset.scale]);

  // A sublayer is added in front, and its fetch is slow: the Layers tab still shows `./library.usda` alone, at index 0.
  const release = source.hold("extra.usda");
  const adding = store.insertSublayer(root, "./extra.usda", 0);
  await h.until(() => source.askedFor("extra.usda") > 0, "the new sublayer being asked for");
  // Its row's menu sets its offset, then removes it: both wait their turn behind the insert.
  const offsetting = store.setSublayerOffset(root, 0, { offset: 10, scale: 2 }, "./library.usda");
  const removing = store.removeSublayer(root, 0, "./library.usda");
  release();
  assert.equal(await h.settle(adding), undefined);
  assert.equal(await h.settle(offsetting), undefined);
  assert.equal(await h.settle(removing), undefined);
  assert.deepEqual(authored(), [["./extra.usda", 0, 1]]);
  assert.equal(session.history.nextUndo!.label, "remove sublayer @./library.usda@ from scene.usda");

  // Undone, it is back where it was with its offset; a move named before the undo of the offset still moves it.
  await h.settle(store.undo());
  assert.deepEqual(authored(), [
    ["./extra.usda", 0, 1],
    ["./library.usda", 10, 2],
  ]);
  assert.deepEqual(stage.sublayers(root), stage.layerSpec(root, "/").sublayers);
  assert.equal(await h.settle(store.moveSublayer(root, 0, -1, "./library.usda")), undefined);
  assert.deepEqual(authored(), [
    ["./library.usda", 10, 2],
    ["./extra.usda", 0, 1],
  ]);

  // One that is gone by the time its turn comes is refused, and nothing is recorded or removed in its place.
  const steps = session.history.undoSteps.length;
  const first = store.removeSublayer(root, 0, "./library.usda");
  const second = store.removeSublayer(root, 0, "./library.usda");
  assert.equal(await h.settle(first), undefined);
  assert.match((await h.settle(second))!, /scene\.usda's sublayers changed since: @\.\/library\.usda@ is not one of them now/);
  assert.deepEqual(authored(), [["./extra.usda", 0, 1]]);
  assert.equal(session.history.undoSteps.length, steps + 1);
});

test("a sublayer the stage could not read is a row of its own, never another layer the stage holds by a like name", async () => {
  const h = await harness();
  // `set/root.usda` lists `library.usda`, which is not beside it, and references the `library.usda` a folder up.
  const files = {
    "set/root.usda": `#usda 1.0\n(\n    subLayers = [\n        @library.usda@\n    ]\n)\n\ndef "Use" (\n    references = @../library.usda@</Lib>\n)\n{\n}\n`,
    "library.usda": layerWith("Lib"),
  };
  await h.open(sampleSource("set/root.usda", files));
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  // What the stage says: the sublayer is missing, looked for beside its layer.
  assert.deepEqual(
    store.health!.missing.map((asset) => [asset.assetPath, asset.usedBy, asset.identifier]),
    [["library.usda", "sublayer", "set/library.usda"]],
  );
  const tree = layerStack({ root: stage.root, session: session.sessionLayer, held: stage.layers(), sublayers: (layer) => stage.sublayers(layer), muted: (layer) => stage.isMuted(layer) });
  assert.deepEqual(
    tree.rows.map((row) => [row.identifier, row.state, row.strength, row.layer?.identifier ?? null]),
    [
      ["set/root.usda", "read", 1, "set/root.usda"],
      ["set/library.usda", "unread", 0, null],
    ],
  );
  assert.deepEqual(
    tree.reached.map((layer) => layer.identifier),
    ["library.usda"],
  );
});

test("a sublayer the stage refuses says why; one the source lacks is authored, and the health reports it missing", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const stage = store.session!.stage;
  const root = stage.root;
  assert.equal(store.health!.count, 0);

  const why = await h.settle(store.insertSublayer(root, "./library.usda", 0));
  assert.match(why!, /repeats an earlier one/);
  assert.equal(store.notices.at(-1)!.message, why);

  // Read as a panel reads it, whenever a channel is bumped while the asset loop runs.
  const unsubscribe = store.signals.subscribe(["status", "loop", "history", "composition"], () => void store.health);
  // Read while its fetch is held: wanted, not supplied yet.
  const release = source.hold("nowhere.usda");
  const adding = store.insertSublayer(root, "./nowhere.usda", 1);
  await h.until(() => source.askedFor("nowhere.usda") > 0, "the new sublayer being asked for");
  assert.deepEqual(
    store.health!.waiting.map((asset) => asset.assetPath),
    ["./nowhere.usda"],
  );
  release();
  assert.equal(await h.settle(adding), undefined);
  unsubscribe();
  // Confirmed missing composes nothing new, and the report is read again all the same.
  const health = store.health!;
  assert.deepEqual(health.waiting, []);
  assert.deepEqual(
    health.missing.map((asset) => [asset.assetPath, asset.anchor, asset.usedBy, asset.reason, asset.prims]),
    [["./nowhere.usda", root, "sublayer", "io.not_found", ["/"]]],
  );
  assert.equal(health.groups[0]!.prim, null, "a sublayer's error is the layer stack's");
  assert.ok(health.count > 0);
  // Read once per change: asked again, and as time moves, it is the same report.
  assert.equal(store.health, health);
  store.setTime(12);
  assert.equal(store.health, health);
  await h.settle(store.undo());
  assert.notEqual(store.health, health);
  assert.equal(store.health!.count, 0);
});

test("the health reports a broken reference by its prim, where it is authored, and what a delete left", async () => {
  const h = await harness();
  const broken = `#usda 1.0
def Xform "World"
{
    def "Thing" (
        prepend references = @./assets/nope.usda@
    )
    {
    }

    def "Pointer" (
        variants = {
            string aim = "none"
        }
        prepend variantSets = "aim"
    )
    {
        variantSet "aim" = {
            "none" {
            }
            "thing" {
                rel target = </World/Thing>
            }
        }
    }
}
`;
  await h.open(sampleSource("broken.usda", { "broken.usda": broken }));
  const { store } = h;
  const health = store.health!;
  assert.deepEqual(
    health.groups.map((group) => [group.prim, group.issues.map((issue) => issue.code)]),
    [["/World/Thing", ["composition.unresolved_asset"]]],
  );
  assert.ok(health.errorPrims.has("/World/Thing"));
  assert.equal(health.missing.length, 1);
  const missing = health.missing[0]!;
  assert.equal(missing.anchor.identifier, "broken.usda");
  assert.equal(missing.identifier, "assets/nope.usda");
  assert.deepEqual(missing.prims, ["/World/Thing"], "the loop recorded the prim composed without it");

  // Deleting the prim clears the error; the target in a variant no prim selects is what the delete left.
  assert.equal(store.delete("/World/Thing"), undefined);
  const after = store.health!;
  assert.deepEqual(after.groups, []);
  assert.equal(after.namespaceEdit?.label, "delete /World/Thing");
  assert.deepEqual(
    after.namespaceEdit!.unrepaired.map((each) => [each.spec, each.field, each.path]),
    [["/World/Pointer{aim=thing}.target", "targets", "/World/Thing"]],
  );
  assert.equal(after.count, 1);
});

test("stage metadata is authored in the root whatever layer edits go to, follows the timeline and viewport, and clears", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  const root = stage.root;
  assert.equal(store.stageMetadataSources().get("endTimeCode"), "root");
  assert.equal(store.stageMetadataSources().get("upAxis"), "fallback");
  assert.equal(store.stageMetadataSources().get("documentation"), "root");
  assert.equal(store.metadataPlacement()!.note, null);

  assert.equal(store.setStageMetadata("endTimeCode", { type: "double", value: 96 }), undefined);
  assert.deepEqual(store.range, { start: 0, end: 96, timeCodesPerSecond: 24 });

  // Edits going to the sublayer, the stage's metadata is still the root's to author.
  store.setTarget(session.layerNamed("library.usda"));
  assert.match(store.metadataPlacement()!.note!, /Edits go to library\.usda/);
  assert.equal(store.setStageMetadata("upAxis", { type: "token", value: "Z" }), undefined);
  assert.equal(store.upAxis, "Z");
  assert.equal(h.view.upAxes.at(-1), "Z");
  assert.deepEqual(
    session.history.nextUndo!.layers.map((layer) => layer.identifier),
    ["scene.usda"],
  );
  assert.equal(store.stageMetadataSources().get("upAxis"), "root");

  assert.equal(store.setStageMetadata("timeCodesPerSecond", { type: "double", value: 30 }), undefined);
  assert.equal(store.range!.timeCodesPerSecond, 30);

  assert.equal(store.clearStageMetadata("upAxis", root), undefined);
  assert.equal(store.upAxis, "Y");
  assert.equal(store.stageMetadataSources().get("upAxis"), "fallback");
  // Nothing to clear: refused, and nothing recorded.
  const steps = session.history.undoSteps.length;
  assert.match(store.clearStageMetadata("framesPerSecond", root)!, /scene\.usda has no framesPerSecond to clear/);
  assert.equal(session.history.undoSteps.length, steps);

  await h.settle(store.undo());
  assert.equal(store.upAxis, "Z");
  await h.settle(store.undo());
  assert.equal(store.range!.timeCodesPerSecond, 24);
});

test("which layer holds a stage metadata opinion is read from its own fields, no layer written as text", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  let exports = 0;
  const exportLayer = stage.exportLayer.bind(stage);
  stage.exportLayer = ((layer, format) => {
    exports++;
    return exportLayer(layer, format);
  }) as typeof stage.exportLayer;
  // A key the root's header holds only inside a dictionary is not one it authors.
  assert.equal(store.setStageMetadata("documentation", { type: "string", value: 'upAxis = "Z"; metersPerUnit = 1' }), undefined);
  const sources = store.stageMetadataSources();
  assert.equal(sources.get("documentation"), "root");
  assert.equal(sources.get("upAxis"), "fallback");
  assert.equal(sources.get("metersPerUnit"), "fallback");
  assert.equal(sources.get("endTimeCode"), "root");
  assert.equal(exports, 0);
  assert.deepEqual([...sources.keys()].filter((key) => sources.get(key) === "root").sort(), stage.layerSpec(stage.root, "/").fields.filter((key) => sources.has(key)).sort());
});

test("reloading reads changed files again, and leaves layers with unsaved edits as they are", async () => {
  const h = await harness();
  const files: Record<string, string> = {
    "stacked.usda": `#usda 1.0\n(\n    subLayers = [\n        @./extra.usda@\n    ]\n)\n\ndef "A"\n{\n}\n`,
    "extra.usda": layerWith("B"),
  };
  const source = sampleSource("stacked.usda", files);
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  assert.equal(stage.prim("/C"), null);

  // Changed on disk.
  files["extra.usda"] = layerWith("B", "C");
  const report = await h.settle(store.reload(null));
  assert.equal(report.refusal, null);
  assert.deepEqual(
    report.reloaded.map((layer) => layer.identifier),
    ["extra.usda"],
  );
  assert.notEqual(stage.prim("/C"), null);
  assert.equal(session.history.nextUndo, null, "a reload is not an edit");

  // Nothing changed: nothing read.
  assert.deepEqual((await h.settle(store.reload(null))).reloaded, []);

  // An edit not saved keeps its layer from being read again.
  const extra = session.layerNamed("extra.usda");
  store.setTarget(extra);
  assert.equal(store.definePrim("/", "D", null), undefined);
  files["extra.usda"] = layerWith("B");
  const refused = await h.settle(store.reload(extra));
  assert.match(refused.refusal!, /extra\.usda has unsaved edits/);
  assert.notEqual(stage.prim("/C"), null);
  const all = await h.settle(store.reload(null));
  assert.equal(all.refusal, null);
  assert.deepEqual(all.skipped, [extra]);
  assert.match(store.notices.at(-1)!.message, /not read again, having unsaved edits: extra\.usda/);
  assert.notEqual(stage.prim("/D"), null, "the edit is kept");
});

test("the default prim is authored in the root layer with edits going to a session layer, and refused when the root is read-only", async () => {
  const h = await harness();
  const files = {
    "top.usda": `#usda 1.0\n(\n    defaultPrim = "A"\n)\n\ndef "A"\n{\n    def "Inner"\n    {\n    }\n}\n\ndef "B"\n{\n}\n`,
    // What reads the default prim: a reference without a prim path.
    "user.usda": `#usda 1.0\ndef "R" (\n    references = @./top.usda@\n)\n{\n}\n`,
  };
  const { store } = h;
  store.setEditLayerChoice("session");
  await h.open(sampleSource("top.usda", files));
  const session = store.session!;
  const stage = session.stage;
  const sessionLayer = session.sessionLayer!;
  assert.equal(store.metadataPlacement()!.layer, sessionLayer);

  // Another field goes where edits go; the default prim goes to the root, which is where a reference reads it.
  assert.equal(store.setStageMetadata("upAxis", { type: "token", value: "Z" }), undefined);
  assert.deepEqual(stage.layerSpec(sessionLayer, "/").fields, ["upAxis"]);
  assert.equal(store.setStageMetadata("defaultPrim", { type: "token", value: "B" }), undefined);
  assert.equal(stage.layerSpec(stage.root, "/").defaultPrim, "B");
  assert.equal(stage.layerSpec(sessionLayer, "/").defaultPrim, null);
  assert.equal(session.history.nextUndo!.label, "set defaultPrim in top.usda");
  assert.equal(store.stageMetadataSources().get("defaultPrim"), "root");
  await h.settle(store.undo());
  assert.equal(stage.layerSpec(stage.root, "/").defaultPrim, "A");
  await h.settle(store.redo());
  assert.equal(stage.layerSpec(stage.root, "/").defaultPrim, "B");

  // A path names a prim beneath a root prim: what a reference to the layer then brings in.
  assert.equal(store.setStageMetadata("defaultPrim", { type: "token", value: "A/Inner" }), undefined);
  const saved = session.save();
  const rootText = new TextDecoder().decode(saved.written.find((file) => file.identifier === "top.usda")!.bytes);
  await h.open(sampleSource("user.usda", { ...files, "top.usda": rootText }));
  assert.deepEqual(store.session!.stage.children("/R"), [], "/R is /A/Inner, which has no children");
  assert.equal(store.health!.count, 0);

  // A package's root takes no edits: the default prim is refused, not authored in the session layer where it would name nothing.
  store.setEditLayerChoice("root");
  await h.open(sampleSource());
  const packaged = await h.settle(store.exportPackage());
  const bytes = packaged!.bytes!;
  await h.open({ label: "sample.usdz", root: "sample.usdz", fetch: async (identifier) => (identifier === "sample.usdz" ? bytes : null), exists: async (identifier) => identifier === "sample.usdz", size: async () => null });
  const packagedSession = store.session!;
  assert.ok(packagedSession.stage.root.readOnly);
  const before = packagedSession.stage.layerSpec(packagedSession.sessionLayer!, "/").fields;
  assert.match(store.setStageMetadata("defaultPrim", { type: "token", value: "World" })!, /defaultPrim is read from the root layer alone, and sample\.usdz is read-only/);
  assert.deepEqual(packagedSession.stage.layerSpec(packagedSession.sessionLayer!, "/").fields, before);
  assert.equal(packagedSession.history.nextUndo, null);
  // Other fields still go to the session layer in front of it.
  assert.equal(store.setStageMetadata("metersPerUnit", { type: "double", value: 1 }), undefined);
  assert.ok(packagedSession.stage.layerSpec(packagedSession.sessionLayer!, "/").fields.includes("metersPerUnit"));
});

test("reloading a layer the undo history changed forgets the history, which could not be undone past it, and keeps unsaved layers edited", async () => {
  const h = await harness();
  const files: Record<string, string> = {
    "stacked.usda": `#usda 1.0\n(\n    subLayers = [\n        @./extra.usda@\n    ]\n)\n\ndef "A"\n{\n}\n`,
    "extra.usda": layerWith("B"),
  };
  await h.open(sampleSource("stacked.usda", files));
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  const extra = session.layerNamed("extra.usda");

  // An edit of the sublayer, saved; then one of the root, not saved.
  store.setTarget(extra);
  assert.equal(store.definePrim("/", "D", null), undefined);
  session.save();
  store.setTarget(stage.root);
  assert.equal(store.definePrim("/", "R", null), undefined);
  assert.equal(store.isEdited(extra), false);
  assert.equal(store.isEdited(stage.root), true);

  // The sublayer's file changes on disk (what the save wrote is not what is there now) and is read again.
  files["extra.usda"] = layerWith("B", "C");
  const report = await h.settle(store.reload(extra));
  assert.equal(report.refusal, null);
  assert.deepEqual(report.reloaded, [extra]);
  assert.equal(stage.prim("/D"), null, "the layer is the file's now");
  assert.notEqual(stage.prim("/C"), null);

  // "define /D" restores what the file no longer has: undo could never get past it. The history is forgotten, and said to be.
  assert.equal(report.forgotten, 2);
  assert.deepEqual(session.history.undoSteps, []);
  assert.deepEqual(session.history.redoSteps, []);
  assert.match(store.notices.at(-1)!.message, /the undo history \(2 steps\) was forgotten/);
  const notices = store.notices.length;
  await h.settle(store.undo());
  assert.equal(store.notices.slice(notices).some((notice) => /stale/.test(notice.message)), false, "no undo is refused as stale for ever");
  // The root's edit is still there, and still to save.
  assert.notEqual(stage.prim("/R"), null);
  assert.equal(store.isEdited(stage.root), true);
  assert.equal(store.isEdited(extra), false);
  assert.deepEqual(store.dirtyLayers(), [stage.root]);

  // Edits made after it undo as ever, back to where the reload left the stage.
  assert.equal(store.definePrim("/", "S", null), undefined);
  await h.settle(store.undo());
  assert.equal(stage.prim("/S"), null);
  assert.equal(store.isEdited(stage.root), true);
  session.save();
  assert.equal(store.isEdited(stage.root), false);

  // A reload that reads no layer a step changed keeps the history.
  assert.equal(store.definePrim("/", "T", null), undefined);
  files["extra.usda"] = layerWith("B");
  const kept = await h.settle(store.reload(extra));
  assert.equal(kept.forgotten, 0);
  assert.equal(session.history.nextUndo!.label, "define typeless /T");
});

test("reloading every layer reads a file that was missing and is there now, and says which it could not read or no longer finds", async () => {
  const h = await harness();
  const files: Record<string, string | undefined> = {
    "top.usda": `#usda 1.0\n(\n    subLayers = [\n        @./late.usda@,\n        @./extra.usda@\n    ]\n)\n\ndef "Use" (\n    references = @./asset.usda@</Asset>\n)\n{\n}\n`,
    "extra.usda": layerWith("B"),
  };
  const source = sampleSource("top.usda", files as Record<string, string>);
  await h.open(source);
  const { store } = h;
  const session = store.session!;
  const stage = session.stage;
  assert.deepEqual(
    store.health!.missing.map((asset) => asset.identifier),
    ["late.usda", "asset.usda"],
  );
  assert.ok(store.health!.errorPrims.has("/Use"));

  // Both files are made; no layer the stage holds changed.
  files["late.usda"] = layerWith("Late");
  files["asset.usda"] = `#usda 1.0\ndef "Asset"\n{\n    def "Part"\n    {\n    }\n}\n`;
  const report = await h.settle(store.reload(null));
  assert.equal(report.refusal, null);
  assert.ok(source.asked.filter((identifier) => identifier === "late.usda").length >= 2, "the missing sublayer is fetched again");
  assert.notEqual(stage.prim("/Late"), null);
  assert.notEqual(stage.prim("/Use/Part"), null);
  assert.deepEqual(store.health!.missing, []);
  assert.equal(store.health!.count, 0);
  assert.equal(session.history.nextUndo, null);

  // One file becomes malformed and another goes: neither is "nothing changed".
  files["extra.usda"] = `#usda 1.0\ndef "B" {`;
  delete files["late.usda"];
  const troubled = await h.settle(store.reload(null));
  assert.deepEqual(troubled.failed, ["extra.usda"]);
  assert.deepEqual(troubled.gone, ["late.usda"]);
  assert.deepEqual(troubled.reloaded, []);
  assert.notEqual(stage.prim("/B"), null, "kept as it was read");
  assert.notEqual(stage.prim("/Late"), null, "kept as it was read");
  assert.equal(
    reloadText(troubled, (identifier) => identifier ?? ""),
    "Could not be read, kept as before: extra.usda; no longer in the source, kept as read: late.usda.",
  );
  assert.ok(store.notices.some((notice) => /late\.usda is no longer in the source/.test(notice.message)));
  assert.equal(reloadText({ reloaded: [], skipped: [], failed: [], gone: [], forgotten: 0 }, String), "Nothing changed in the source.");
  assert.equal(
    reloadText({ reloaded: [{ identifier: "a.usda" }], skipped: [1], failed: [], gone: [], forgotten: 1 }, String),
    "Read again: a.usda; 1 with unsaved edits left as they are; the undo history was forgotten (1 step).",
  );
});
