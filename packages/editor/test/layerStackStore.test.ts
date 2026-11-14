// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The layer stack, the stage's metadata, the stage's health and reading
// layers again from the source, through `EditorStore` as the Layers,
// Stage and Health tabs drive it, over the sample scene and layers a test
// serves beside it.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Layer, Stage } from "@forest-rs/layerstack";
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

  assert.equal(await h.settle(store.moveSublayer(root, 1, -1)), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda", "./library.usda"]);
  // The strongest cannot move stronger: said, and nothing recorded.
  const steps = session.history.undoSteps.length;
  assert.match((await h.settle(store.moveSublayer(root, 0, -1)))!, /the strongest sublayer of scene\.usda already/);
  assert.equal(session.history.undoSteps.length, steps);

  assert.equal(await h.settle(store.setSublayerOffset(root, 0, { offset: 10, scale: 2 })), undefined);
  assert.deepEqual(sublayersOf(stage, root), ["./extra.usda+10x2", "./library.usda"]);

  assert.equal(await h.settle(store.removeSublayer(root, 0)), undefined);
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
