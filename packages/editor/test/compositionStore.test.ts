// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring composition through the store, over the sample scene: adding
// an arc to a layer the stage has not read runs the asset loop with the
// overlay up and composes what it names; removing an arc, adding a
// variant set and a variant, editing inside a variant and removing a set;
// each one undoable step, undone and redone, and each refusal returned
// where it was asked for. Namespace actions an arc or variant target
// cannot take say why before they are tried.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Stage } from "@forest-rs/layerstack";
import { findAuthoring, type NewArc, uncomposedHere } from "../src/model/composition.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

/** A reference to add, as the dialog makes it. */
function reference(asset: string | null, primPath: string | null = null): NewArc {
  return { kind: "reference", asset, primPath, position: "backOfPrepend", offset: 0, scale: 1 };
}

/** The layer identifiers of the prim's composed arcs. */
function arcLayers(stage: Stage, path: string): (string | null)[] {
  return stage.prim(path)!.compositionArcs().map((arc) => arc.layerStack.identifier);
}

/** The sample scene, open. */
async function opened(extra: Readonly<Record<string, string>> = {}) {
  const h = await harness();
  const source = sampleSource("scene.usda", extra);
  await h.open(source);
  return { h, source, store: h.store, stage: h.store.session!.stage, steps: () => h.store.session!.history.undoSteps.length };
}

/** A layer the sample does not have, with prims beneath its default prim. */
const PROPS = `#usda 1.0
(
    defaultPrim = "Props"
)

def Xform "Props"
{
    def Cube "Crate"
    {
        double size = 0.5
    }

    def Sphere "Ball"
    {
    }
}
`;

test("adding a reference to a layer not yet read runs the asset loop, with the overlay, and its prims appear", async () => {
  const { h, source, store, stage, steps } = await opened({ "assets/props.usda": PROPS });
  assert.equal(store.definePrim("/World", "Pile", "Xform"), undefined);
  const before = steps();
  const release = source.hold("assets/props.usda");
  const adding = store.addArc("/World/Pile", reference("./assets/props.usda"));
  assert.equal(adding.refusal, undefined);
  await h.until(() => source.asked.includes("assets/props.usda"), "the referenced layer being asked for");
  assert.equal(store.status.kind, "loading", "the overlay is up while it is fetched");
  assert.equal(store.status.kind === "loading" ? store.status.title : null, "Adding the reference: fetching what it names");
  assert.equal(store.openingPhase?.phase.kind, "fetching");
  release();
  await h.settle(adding.done);
  assert.equal(store.status.kind, "ready");
  assert.deepEqual(stage.children("/World/Pile"), ["/World/Pile/Crate", "/World/Pile/Ball"]);
  assert.equal(steps(), before + 1, "one step");
  assert.equal(store.session!.history.nextUndo?.label, "add reference @./assets/props.usda@ to /World/Pile");
  assert.equal(store.session!.loops.runs.at(-1)?.cause, "add reference @./assets/props.usda@ to /World/Pile");
  assert.deepEqual(store.notices, []);
  // What the inspector offers to show: the root's text, which marks what the edit added.
  assert.equal(store.compositionEdit?.layer, stage.root);
  const text = store.layerText(stage.root);
  assert.ok(typeof text !== "string");
  assert.ok(text.changes.added.some((line) => text.lines[line]!.includes("@./assets/props.usda@")));

  await h.settle(store.undo());
  assert.deepEqual(stage.children("/World/Pile"), []);
  assert.equal(store.compositionEdit, null, "offered only while it is the newest step");
  await h.settle(store.redo());
  assert.deepEqual(stage.children("/World/Pile"), ["/World/Pile/Crate", "/World/Pile/Ball"]);
});

test("a reference to the sample's unread chrome finish, and to a prim of the stage (internal), compose", async () => {
  const { h, source, store, stage } = await opened();
  assert.ok(!source.asked.includes("assets/chrome.usda"));
  await h.settle(store.addArc("/World/Rock", reference("./assets/chrome.usda")).done);
  assert.ok(arcLayers(stage, "/World/Rock").includes("assets/chrome.usda"));
  assert.notEqual(stage.attribute("/World/Rock.roughness"), null, "the chrome prim's opinions reach the rock");
  // An internal reference names a prim of the stage, and fetches nothing.
  const asked = source.asked.length;
  await h.settle(store.addArc("/World/Top", { ...reference(null, "/World/Rock"), position: "backOfAppend" }).done);
  assert.equal(source.asked.length, asked);
  assert.ok(stage.prim("/World/Top")!.compositionArcs().some((arc) => arc.arc === "references" && arc.site === "/World/Rock"));
});

test("a missing asset is a notice and is listed beside the arcs, and removing it there takes it back", async () => {
  const { h, store, stage } = await opened();
  await h.settle(store.addArc("/World/Rock", reference("./missing.usda")).done);
  assert.equal(store.notices.length, 1);
  assert.equal(store.notices[0]!.code, "composition.unresolved_asset");
  assert.match(store.notices[0]!.message, /@\.\/missing\.usda@/);
  const arcs = stage.prim("/World/Rock")!.compositionArcs();
  const read = (layer: Parameters<typeof store.layerSpecs>[0]) => store.layerSpecs(layer);
  const listed = uncomposedHere(arcs, stage.root, "/World/Rock", read);
  assert.deepEqual(listed.map((arc) => arc.asset), ["./missing.usda"]);
  assert.equal(store.removeArc("/World/Rock", listed[0]!), undefined);
  assert.deepEqual(stage.status().compositionErrors, []);
  assert.equal(store.session!.history.nextUndo?.label, "remove reference @./missing.usda@ from /World/Rock");
});

test("an added arc the stage refuses returns why at once and changes nothing", async () => {
  const { store, stage, steps } = await opened();
  const robot = stage.prim("/World/Robot")!.compositionArcs().find((arc) => arc.arc === "references")!;
  store.setTarget(robot.editTarget());
  const before = steps();
  // Through the robot's reference only its namespace maps: the rock is outside it.
  const adding = store.addArc("/World/Robot/Body", { kind: "inherit", asset: null, primPath: "/World/Rock/Pebble", position: "backOfPrepend", offset: 0, scale: 1 });
  assert.match(adding.refusal ?? "", /does not map the target \/World\/Rock\/Pebble/);
  await adding.done;
  assert.equal(steps(), before);
  assert.equal(store.notices.at(-1)?.code, "edit.rejected.unmappable_target");
});

test("cancelling the fetch of an added arc takes the arc back", async () => {
  const { h, source, store, stage, steps } = await opened();
  const before = steps();
  const release = source.hold("assets/chrome.usda");
  const adding = store.addArc("/World/Rock", reference("./assets/chrome.usda"));
  await h.until(() => source.asked.includes("assets/chrome.usda"), "the chrome layer being asked for");
  store.cancel();
  await h.settle(adding.done);
  release();
  assert.equal(store.status.kind, "ready");
  assert.equal(steps(), before, "nothing on the undo stack");
  assert.ok(!arcLayers(stage, "/World/Rock").includes("assets/chrome.usda"));
  assert.deepEqual(stage.unavailable(), []);
});

test("removing an arc the edit layer authors is one step, undone and redone", async () => {
  const { h, store, stage } = await opened();
  const arcs = stage.prim("/World/Rock")!.compositionArcs();
  const source = findAuthoring(arcs, arcs.findIndex((arc) => arc.arc === "inherits"), (layer) => store.layerSpecs(layer))!;
  assert.equal(source.layer, stage.root);
  assert.equal(store.removeArc("/World/Rock", source.item), undefined);
  assert.equal(stage.prim("/World/Rock")!.compositionArcs().some((arc) => arc.arc === "inherits"), false);
  assert.equal(store.compositionEdit?.label, "remove inherit </Shape> from /World/Rock");
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.compositionArcs().some((arc) => arc.arc === "inherits"), true);
  await h.settle(store.redo());
  assert.equal(stage.prim("/World/Rock")!.compositionArcs().some((arc) => arc.arc === "inherits"), false);
});

test("a variant set added with its first variant, a variant added, edited inside, and the set removed", async () => {
  const { h, store, stage, steps } = await opened();
  const rock = () => stage.prim("/World/Rock")!.variantSets();
  assert.equal(store.addVariant("/World/Rock", "size", "small", true), undefined);
  assert.deepEqual(rock(), [{ name: "size", selection: "small", variants: ["small"] }]);
  assert.equal(store.session!.history.nextUndo?.label, "add variant set size (small) to /World/Rock");
  assert.equal(store.addVariant("/World/Rock", "size", "large"), undefined);
  assert.equal(rock()[0]!.selection, "large");
  assert.deepEqual([...rock()[0]!.variants].sort(), ["large", "small"]);
  assert.deepEqual(store.compositionEdit?.variant, { set: "size", variant: "large" });
  // A variant name USD does not take is refused, and says so.
  const before = steps();
  assert.match(store.addVariant("/World/Rock", "size", "two words") ?? "", /not a variant name/);
  assert.equal(steps(), before);

  // Edits inside the variant are written in its branch and show while it is selected.
  store.editInsideVariant("/World/Rock", "size", "large");
  const target = store.session!.target;
  assert.ok(target !== undefined && "kind" in target && target.kind === "variant");
  assert.equal(target.mapPath("/World/Rock"), "/World/Rock{size=large}");
  // `purpose`, which nothing stronger than the variant authors (the inherited class's opinions are stronger: LIVRPS).
  assert.equal(store.setValue("/World/Rock.purpose", { type: "token", value: "proxy" }), undefined);
  const purpose = stage.attribute("/World/Rock.purpose")!.get();
  assert.equal(purpose?.kind === "scalar" ? purpose.value : null, "proxy");
  assert.equal(store.shadow, null);
  assert.ok(store.layerSpecs(stage.root)!.specs.has("/World/Rock{size=large}"));
  store.setTarget(undefined);

  // Removing the set removes its variants and the selection here; the name stays listed, as OpenUSD leaves it.
  assert.equal(store.removeVariantSet("/World/Rock", "size"), undefined);
  assert.deepEqual(rock(), [{ name: "size", selection: null, variants: [] }]);
  assert.equal(store.layerSpecs(stage.root)!.specs.get("/World/Rock")!.variantSets.has("size"), false);
  await h.settle(store.undo());
  assert.equal(rock()[0]!.selection, "large");
  // A layer that holds no such set refuses.
  store.setTarget(stage.layers().find((layer) => layer.identifier === "library.usda"));
  const steps2 = steps();
  assert.match(store.removeVariantSet("/World/Rock", "size") ?? "", /library\.usda holds no variant set size/);
  assert.equal(steps(), steps2);
});

test("edits go back to the edit layer when an undo takes the variant or the arc they went through", async () => {
  const { h, store, stage } = await opened();
  assert.equal(store.addVariant("/World/Rock", "size", "small", true), undefined);
  store.editInsideVariant("/World/Rock", "size", "small");
  assert.ok(store.session!.target !== undefined);
  // A variant that is there and not selected stays where edits go.
  assert.equal(store.addVariant("/World/Rock", "size", "large"), undefined);
  await h.settle(store.undo());
  const inside = store.session!.target;
  assert.ok(inside !== undefined && "kind" in inside && inside.kind === "variant");
  // The undo of the set takes the variant: edits go to the edit layer again, and a notice says so.
  await h.settle(store.undo());
  assert.deepEqual(stage.prim("/World/Rock")!.variantSets(), []);
  assert.equal(store.session!.target, undefined);
  assert.match(store.notices.at(-1)?.message ?? "", /Edits go to scene\.usda again: \/World\/Rock no longer has the variant size=small/);
  // The same through an arc whose prim an undo takes away.
  assert.equal(store.definePrim("/World", "Copy", "Xform"), undefined);
  await h.settle(store.addArc("/World/Copy", reference("./assets/robot.usda")).done);
  const arc = stage.prim("/World/Copy")!.compositionArcs().find((each) => each.arc === "references")!;
  store.setTarget(arc.editTarget());
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Copy"), null);
  assert.equal(store.session!.target, undefined);
});

test("a rename or delete through a variant or an arc says why before it is tried", async () => {
  const { store, stage, steps } = await opened();
  store.editInsideVariant("/World/Robot", "finish", "painted");
  const before = steps();
  assert.match(store.delete("/World/Robot") ?? "", /stage's own layer stack/);
  assert.match(store.move("/World/Robot", "/World/Robot2") ?? "", /stage's own layer stack/);
  assert.equal(steps(), before);
  assert.notEqual(stage.prim("/World/Robot"), null);
  const reference = stage.prim("/World/Robot")!.compositionArcs().find((arc) => arc.arc === "references")!;
  store.setTarget(reference.editTarget());
  assert.match(store.definePrim("/World", "Elsewhere", "Xform") ?? "", /does not map \/World\/Elsewhere/);
  assert.equal(store.definePrim("/World/Robot", "Arm", "Xform"), undefined, "inside the referenced prim it maps");
  assert.equal(store.compositionEdit, null);
});

test("a layer's specs are read once per change of the stage", async () => {
  const { store, stage } = await opened();
  const first = store.layerSpecs(stage.root);
  assert.equal(store.layerSpecs(stage.root), first);
  assert.equal(store.setValue("/World/Rock.radius", { type: "double", value: 3 }), undefined);
  assert.notEqual(store.layerSpecs(stage.root), first);
});

test("the layers an arc can name, and a layer's default and root prims, read or not", async () => {
  const { store } = await opened();
  assert.deepEqual(await store.arcLayers(), ["assets/beacon.usda", "assets/chrome.usda", "assets/robot.usda", "library.usda", "scene.usda"]);
  assert.deepEqual(await store.layerSummary("assets/chrome.usda"), { kind: "read", defaultPrim: "Chrome", roots: [{ name: "Chrome", specifier: "def", typeName: null }] });
  const library = await store.layerSummary("library.usda");
  assert.ok(library.kind === "read");
  assert.equal(library.defaultPrim, null);
  assert.deepEqual(library.roots.map((root) => [root.name, root.specifier]), [["Shape", "class"], ["World", "over"]]);
  assert.deepEqual(await store.layerSummary("assets/nothing.usda"), { kind: "unread", reason: "The scene's source has no assets/nothing.usda." });
});
