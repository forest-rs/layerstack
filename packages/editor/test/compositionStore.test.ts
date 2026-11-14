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
import type { AuthoredReference, Stage } from "@forest-rs/layerstack";
import { authoredHere, findAuthoring, mapping, type NewArc, newArcUsda, selectionTarget, strongerSelection, uncomposedHere, uncomposedWhy, variantSetSource } from "../src/model/composition.ts";
import { targetSite } from "../src/model/propertyOps.ts";
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
  assert.equal(adding.unchanged, false);
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
  const read = store.specsOf;
  const listed = uncomposedHere(arcs, stage.root, "/World/Rock", read);
  assert.deepEqual(listed.map((arc) => arc.asset), ["./missing.usda"]);
  assert.equal(store.removeArc("/World/Rock", listed[0]!), undefined);
  assert.deepEqual(stage.status().compositionErrors, []);
  assert.equal(store.session!.history.nextUndo?.label, "remove reference @./missing.usda@ from /World/Rock");
});

test("an arc that makes a cycle is listed as making one, matched to its error by its site and asset", async () => {
  const { h, store, stage } = await opened();
  const why = (prim: string) => {
    const issues = stage.status().compositionErrors.filter((issue) => issue.prim === prim);
    const [item, ...rest] = uncomposedHere(stage.prim(prim)!.compositionArcs(), stage.root, prim, store.specsOf);
    assert.ok(item !== undefined && rest.length === 0, `one arc of ${prim} composes nothing`);
    return uncomposedWhy(item, stage.root.identifier, issues, false);
  };
  // An internal reference to its own ancestor, and a reference through the layer's own file to itself.
  await h.settle(store.addArc("/World/Rock", reference(null, "/World")).done);
  const internal = why("/World/Rock");
  assert.equal(internal.kind, "cycle");
  assert.match(internal.text, /^It makes a cycle, so the stage ignores it: \/World\/Rock in @scene\.usda@ -> reference \/World in @scene\.usda@\.$/);
  await h.settle(store.addArc("/World/Top", reference("./scene.usda", "/World/Top")).done);
  assert.equal(why("/World/Top").kind, "cycle", "its asset is authored ./scene.usda, named @scene.usda@ in the error");
  // An inherit of its own descendant.
  await h.settle(store.addArc("/World/Robot", { kind: "inherit", asset: null, primPath: "/World/Robot/Body", position: "backOfPrepend", offset: 0, scale: 1 }).done);
  assert.equal(why("/World/Robot").kind, "cycle");
  // A cycle error of another arc does not explain this one.
  assert.equal(uncomposedWhy({ field: "references", asset: null, primPath: "/World/Other" }, "scene.usda", stage.status().compositionErrors, false).kind, "unknown");
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
  const source = findAuthoring(arcs, arcs.findIndex((arc) => arc.arc === "inherits"), store.specsOf)!;
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
  assert.notEqual(store.authoredSpec(stage.root, "/World/Rock{size=large}"), null);
  store.setTarget(undefined);

  // Removing the set removes its variants and the selection here; the name stays listed, as OpenUSD leaves it.
  assert.equal(store.removeVariantSet("/World/Rock", "size"), undefined);
  assert.deepEqual(rock(), [{ name: "size", selection: null, variants: [] }]);
  assert.deepEqual(store.authoredSpec(stage.root, "/World/Rock")!.spec.variantSets, []);
  await h.settle(store.undo());
  assert.equal(rock()[0]!.selection, "large");
  // A layer that holds no such set refuses.
  store.setTarget(stage.layers().find((layer) => layer.identifier === "library.usda"));
  const steps2 = steps();
  assert.match(store.removeVariantSet("/World/Rock", "size") ?? "", /library\.usda holds no variant set size/);
  assert.equal(steps(), steps2);
});

test("a variant added in a weaker layer is not said to be selected while a stronger layer's selection wins, and is selected there when asked", async () => {
  const { h, store, stage } = await opened();
  const robot = () => stage.prim("/World/Robot")!.variantSets().find((set) => set.name === "finish")!;
  assert.equal(robot().selection, "painted", "scene.usda selects it");
  const library = store.session!.layerNamed("library.usda");
  store.setTarget(library);
  const target = targetSite(stage, store.session!.target, "/World/Robot");
  // The dialog says so before: the root's selection is stronger than the sublayer's.
  assert.deepEqual(strongerSelection(stage.prim("/World/Robot")!.compositionArcs(), "finish", target, store.specsOf), { layer: stage.root, variant: "painted", arc: 0 });
  assert.equal(store.addVariant("/World/Robot", "finish", "matte"), undefined);
  assert.equal(robot().selection, "painted", "the stage still selects the root's");
  const sub = stage.layerSpec(library, "/World/Robot");
  assert.ok(sub !== null && sub.type === "prim");
  assert.equal(sub.variantSelections?.get("finish"), "matte", "its selection is authored where it went");
  const edit = store.compositionEdit!;
  assert.equal(edit.variant?.variant, "matte");
  assert.deepEqual(edit.variant?.outranked && [edit.variant.outranked.layer, edit.variant.outranked.variant], [stage.root, "painted"]);
  // Selecting it where the winning selection is authored.
  const where = selectionTarget(stage.prim("/World/Robot")!, edit.variant!.outranked!);
  assert.equal(where, stage.root);
  await h.settle(store.selectVariant("/World/Robot", "finish", "matte", where!));
  assert.equal(robot().selection, "matte");
  const root = stage.layerSpec(stage.root, "/World/Robot");
  assert.ok(root !== null && root.type === "prim");
  assert.equal(root.variantSelections?.get("finish"), "matte");
  // Where edits go wins: nothing outranks it.
  store.setTarget(undefined);
  assert.equal(store.addVariant("/World/Robot", "finish", "satin"), undefined);
  assert.equal(robot().selection, "satin");
  assert.equal(store.compositionEdit?.variant?.outranked, undefined);
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

test("a layer's spec is read once per change of the stage, never on a time change, and no layer is exported for it", async () => {
  const { store, stage } = await opened();
  const reads: string[] = [];
  const layerSpec = stage.layerSpec.bind(stage);
  stage.layerSpec = ((layer, path) => {
    reads.push(path);
    return layerSpec(layer, path);
  }) as typeof stage.layerSpec;
  let exports = 0;
  const exportLayer = stage.exportLayer.bind(stage);
  stage.exportLayer = ((layer, format) => {
    exports++;
    return exportLayer(layer, format);
  }) as typeof stage.exportLayer;
  const first = store.authoredSpec(stage.root, "/World/Robot");
  assert.equal(first?.spec.path, "/World/Robot");
  assert.equal(store.authoredSpec(stage.root, "/World/Robot"), first);
  store.setTime(12);
  assert.equal(store.authoredSpec(stage.root, "/World/Robot"), first, "a time change reads nothing again");
  assert.deepEqual(reads, ["/World/Robot"], "one spec read, of the one spec asked for");
  // The section's reads of the robot: each arc's authoring, what composes nothing, what is deleted.
  const arcs = stage.prim("/World/Robot")!.compositionArcs();
  arcs.forEach((_, index) => findAuthoring(arcs, index, store.specsOf));
  uncomposedHere(arcs, stage.root, "/World/Robot", store.specsOf);
  assert.equal(exports, 0, "no layer is written as text for it");
  assert.equal(store.setValue("/World/Rock.radius", { type: "double", value: 3 }), undefined);
  assert.notEqual(store.authoredSpec(stage.root, "/World/Robot"), first, "read again after an edit");
  assert.equal(exports, 0);
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

/** The layer offsets of the arcs the root authors on `/World/Pile` in `field`: its prepended items, and its deleted ones. */
function pileOffsets(stage: Stage, field: "references" | "payloads") {
  const spec = stage.layerSpec(stage.root, "/World/Pile");
  assert.ok(spec !== null && spec.type === "prim");
  const offsets = (items: readonly AuthoredReference[] | undefined) => (items ?? []).map((item) => [item.layerOffset.offset, item.layerOffset.scale]);
  return { prepended: offsets(spec[field]?.prepended), deleted: offsets(spec[field]?.deleted) };
}

// OpenUSD: an `SdfReference` is its asset path, prim path and layer offset together
// (`SdfReference::operator==`), and `UsdReferences::RemoveReference` removes the item equal to the one given.
test("of two arcs to one asset and prim that differ by layer offset, the one removed is the one asked for", async () => {
  for (const kind of ["reference", "payload"] as const) {
    const field = kind === "reference" ? "references" : "payloads";
    for (const [offset, scale] of [[24, 1], [0, 2]] as const) {
      for (const second of [true, false]) {
        const { h, store, stage } = await opened({ "assets/props.usda": PROPS });
        assert.equal(store.definePrim("/World", "Pile", "Xform"), undefined);
        const arc: NewArc = { kind, asset: "./assets/props.usda", primPath: "/Props", position: "backOfPrepend", offset: 0, scale: 1 };
        await h.settle(store.addArc("/World/Pile", arc).done);
        await h.settle(store.addArc("/World/Pile", { ...arc, offset, scale }).done);
        const both = { prepended: [[0, 1], [offset, scale]], deleted: [] };
        assert.deepEqual(pileOffsets(stage, field), both);

        // Each composed arc is read as the item that authors it: the one at its own offset.
        const read = store.specsOf;
        const arcs = stage.prim("/World/Pile")!.compositionArcs();
        const composed = arcs.flatMap((each, index) => (each.arc === "local" ? [] : [{ arc: each, source: findAuthoring(arcs, index, read) }]));
        assert.deepEqual(composed.map(({ arc: each }) => [each.layerOffset.offset, each.layerOffset.scale]), [[0, 1], [offset, scale]]);
        assert.deepEqual(composed.map(({ source }) => [source?.item.offset, source?.item.scale]), [[0, 1], [offset, scale]], "each arc's row names its own item");
        assert.deepEqual(uncomposedHere(arcs, stage.root, "/World/Pile", read), [], "neither is listed as composing nothing");

        const [gone, kept] = second ? [[offset, scale], [0, 1]] : [[0, 1], [offset, scale]];
        assert.equal(store.removeArc("/World/Pile", composed[second ? 1 : 0]!.source!.item), undefined);
        const after = { prepended: [kept], deleted: [gone] };
        assert.deepEqual(pileOffsets(stage, field), after, `${kind}: removing the one at ${gone} keeps the one at ${kept}`);
        const left = stage.prim("/World/Pile")!.compositionArcs().filter((each) => each.arc !== "local");
        assert.deepEqual(left.map((each) => [each.layerOffset.offset, each.layerOffset.scale]), [kept]);
        await h.settle(store.undo());
        assert.deepEqual(pileOffsets(stage, field), both, "undo: both back, in order, at their offsets");
        await h.settle(store.redo());
        assert.deepEqual(pileOffsets(stage, field), after, "redo");
      }
    }
  }
});

test("of two references to one asset, to its default prim and to another prim, each is read as its own item", async () => {
  const { h, store, stage } = await opened({ "assets/props.usda": PROPS });
  assert.equal(store.definePrim("/World", "Pile", "Xform"), undefined);
  await h.settle(store.addArc("/World/Pile", reference("./assets/props.usda")).done);
  await h.settle(store.addArc("/World/Pile", reference("./assets/props.usda", "/Props/Crate")).done);
  const read = store.specsOf;
  const arcs = stage.prim("/World/Pile")!.compositionArcs();
  // An item with no prim path composes any site, so the second arc must not be read as the first's item.
  const sources = arcs.flatMap((arc, index) => (arc.arc === "references" ? [[arc.site, findAuthoring(arcs, index, read)?.item.primPath]] : []));
  assert.deepEqual(sources, [["/Props", null], ["/Props/Crate", "/Props/Crate"]]);
  assert.deepEqual(uncomposedHere(arcs, stage.root, "/World/Pile", read), [], "the one to /Props/Crate composes, and is not listed as composing nothing");
  // Removing the one to /Props/Crate leaves the default prim's.
  const crate = findAuthoring(arcs, arcs.findIndex((arc) => arc.site === "/Props/Crate"), read)!;
  assert.equal(store.removeArc("/World/Pile", crate.item), undefined);
  assert.deepEqual(stage.prim("/World/Pile")!.compositionArcs().filter((arc) => arc.arc === "references").map((arc) => arc.site), ["/Props"]);
});

test("adding an arc the layer has already changes nothing, and is not noted as the step before it", async () => {
  const { h, store, stage, steps } = await opened();
  assert.equal(store.setValue("/World/Rock.radius", { type: "double", value: 3 }), undefined);
  const before = steps();
  const text = new TextDecoder().decode(stage.exportLayer(stage.root, "usda"));
  // As OpenUSD's AddReference of an item the list holds at that place: nothing to author.
  const adding = store.addArc("/World/Robot", reference("./assets/robot.usda"));
  // Known at once, for the dialog to say so and stay open.
  assert.deepEqual([adding.refusal, adding.unchanged], [undefined, true]);
  await h.settle(adding.done);
  assert.equal(steps(), before, "no step");
  assert.equal(new TextDecoder().decode(stage.exportLayer(stage.root, "usda")), text);
  assert.equal(store.compositionEdit, null, "the value edit before it is not offered as a composition edit of the robot");
});

test("the USDA an added inherit or internal reference is shown to author is what its layer then holds, through an arc and inside a variant", async () => {
  const { h, store, stage } = await opened();
  const inherit = (primPath: string): NewArc => ({ kind: "inherit", asset: null, primPath, position: "backOfPrepend", offset: 0, scale: 1 });
  // Through the robot's reference, a prim of the stage is named as the asset names it; a root prim's path as given.
  const through = stage.prim("/World/Robot")!.compositionArcs().find((arc) => arc.arc === "references")!.editTarget();
  store.setTarget(through);
  assert.equal(newArcUsda(inherit("/World/Robot/Head"), through), "prepend inherits = </Robot/Head>");
  assert.equal(newArcUsda(inherit("/Shape"), through), "prepend inherits = </Shape>");
  assert.equal(newArcUsda(reference(null, "/World/Robot/Head"), through), "prepend references = </Robot/Head>");
  await h.settle(store.addArc("/World/Robot/Body", inherit("/World/Robot/Head")).done);
  await h.settle(store.addArc("/World/Robot/Body", inherit("/Shape")).done);
  await h.settle(store.addArc("/World/Robot/Body", reference(null, "/World/Robot/Head")).done);
  const body = stage.layerSpec(through.layer, "/Robot/Body");
  assert.ok(body !== null && body.type === "prim");
  assert.deepEqual(body.inherits?.prepended, ["/Robot/Head", "/Shape"]);
  assert.deepEqual(body.references?.prepended.map((item) => item.primPath), ["/Robot/Head"]);

  // Inside a variant the layer's namespace is the stage's, and a selection is no part of the path.
  store.setTarget(undefined);
  store.editInsideVariant("/World/Robot", "finish", "painted");
  const inside = store.session!.target!;
  assert.equal(newArcUsda(inherit("/World/Robot/Head"), inside), "prepend inherits = </World/Robot/Head>");
  await h.settle(store.addArc("/World/Robot", inherit("/World/Robot/Head")).done);
  const painted = stage.layerSpec(stage.root, "/World/Robot{finish=painted}");
  assert.ok(painted !== null && painted.type === "variant");
  assert.deepEqual(painted.inherits?.prepended, ["/World/Robot/Head"]);
  // A layer of the stack authors it as given.
  assert.equal(newArcUsda(inherit("/World/Robot/Head"), stage.root), "prepend inherits = </World/Robot/Head>");
});

test("a variant set nested in a variant is found in that variant's branch, to remove it there", async () => {
  const { store, stage } = await opened();
  const read = store.specsOf;
  assert.equal(store.addVariant("/World/Rock", "look", "mossy", true), undefined);
  store.editInsideVariant("/World/Rock", "look", "mossy");
  assert.equal(store.addVariant("/World/Rock", "wear", "worn", true), undefined);
  const arcs = stage.prim("/World/Rock")!.compositionArcs();
  const held = variantSetSource(arcs, "wear", read);
  assert.ok(held !== null, "the layer that holds the nested set is found");
  assert.equal(held.layer, stage.root);
  assert.equal(held.specPath, "/World/Rock{look=mossy}");
  // The arc that reads it gives the target that edits there: where edits go now.
  const there = stage.prim("/World/Rock")!.compositionArcs()[held.arc]!.editTarget(held.layer);
  assert.equal(there.mapPath("/World/Rock"), held.specPath);
  assert.equal(mapping(store.session!.target!, "/World/Rock").specPath, held.specPath);
  assert.equal(store.removeVariantSet("/World/Rock", "wear"), undefined);
  assert.equal(variantSetSource(stage.prim("/World/Rock")!.compositionArcs(), "wear", read), null);
});

test("an arc a prim composes through an ancestor is read as authored on that ancestor", async () => {
  const { store, stage } = await opened();
  const read = store.specsOf;
  // The body is in the robot's asset because /World/Robot references it: no layer authors an arc on the body itself.
  const arcs = stage.prim("/World/Robot/Body")!.compositionArcs();
  const index = arcs.findIndex((arc) => arc.arc === "references");
  assert.equal(arcs[index]!.site, "/Robot/Body");
  const source = findAuthoring(arcs, index, read);
  assert.ok(source !== null, "the layer that authors it is read, on the ancestor");
  assert.equal(source.layer, stage.root);
  assert.equal(source.specPath, "/World/Robot");
  assert.equal(source.ancestor, "/World/Robot");
  assert.equal(source.item.asset, "./assets/robot.usda");
  // It is not this prim's to remove, where edits go or anywhere.
  assert.equal(authoredHere(source, stage.root, "/World/Robot/Body"), false);
  // On the prim that authors it, nothing is said of an ancestor.
  const robot = stage.prim("/World/Robot")!.compositionArcs();
  assert.equal(findAuthoring(robot, robot.findIndex((arc) => arc.arc === "references"), read)?.ancestor, null);
});

test("through a reference read at an offset, the arc removed is the one at the offset its layer authors", async () => {
  const { h, store, stage } = await opened({ "assets/props.usda": PROPS });
  assert.equal(store.definePrim("/World", "Pile", "Xform"), undefined);
  await h.settle(store.addArc("/World/Pile", { ...reference("./assets/props.usda"), offset: 10, scale: 2 }).done);
  const through = stage.prim("/World/Pile")!.compositionArcs().find((arc) => arc.arc === "references")!.editTarget();
  store.setTarget(through);
  assert.deepEqual(through.layerOffset, { offset: 10, scale: 2 });
  // In the asset, two references to the chrome finish beside it, at the asset's own offsets 0 and 24.
  await h.settle(store.addArc("/World/Pile/Crate", reference("./chrome.usda")).done);
  await h.settle(store.addArc("/World/Pile/Crate", { ...reference("./chrome.usda"), offset: 24 }).done);
  const authored = () => {
    const spec = stage.layerSpec(through.layer, "/Props/Crate");
    assert.ok(spec !== null && spec.type === "prim");
    return { prepended: spec.references?.prepended.map((item) => item.layerOffset.offset), deleted: spec.references?.deleted.map((item) => item.layerOffset.offset) };
  };
  assert.deepEqual(authored(), { prepended: [0, 24], deleted: [] }, "authored as given, not mapped through the target's offset");
  const arcs = stage.prim("/World/Pile/Crate")!.compositionArcs();
  const chrome = arcs.flatMap((arc, index) => (arc.layerStack.identifier === "assets/chrome.usda" ? [{ arc, source: findAuthoring(arcs, index, store.specsOf) }] : []));
  // To stage times: the asset's offset (10, scale 2) over each item's.
  assert.deepEqual(chrome.map(({ arc }) => arc.layerOffset), [{ offset: 10, scale: 2 }, { offset: 58, scale: 2 }]);
  assert.deepEqual(chrome.map(({ source }) => source?.item.offset), [0, 24]);
  assert.equal(store.removeArc("/World/Pile/Crate", chrome[1]!.source!.item), undefined);
  assert.deepEqual(authored(), { prepended: [0], deleted: [24] });
});
