// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring prims where the stage is more than one layer: a root layer
// over a weaker sublayer, a referenced asset, a variant, inherited
// material bindings and keyed visibility. What an edit authors is read
// from the layer it went to (`Stage.layerSpec`), and what the stage then
// composes from the stage.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Layer, Stage } from "@forest-rs/layerstack";
import { copyPath, copyRefusal, copySites, nameOf, opinionSites } from "../src/model/primAuthoring.ts";
import { primActions } from "../src/model/primActions.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

const ROOT = `#usda 1.0
(
    defaultPrim = "World"
    subLayers = [
        @./weak.usda@
    ]
)

def Xform "World" (
    prepend apiSchemas = ["MaterialBindingAPI"]
)
{
    rel material:binding = </World/Stone>

    def Sphere "Ball"
    {
    }

    def Sphere "Pebble"
    {
    }

    def Material "Stone"
    {
    }

    def Material "Moss"
    {
    }

    def Xform "Tower" (
        references = @./asset.usda@</Asset>
    )
    {
        def Sphere "Flag"
        {
        }
    }

    def Xform "Blink"
    {
        token visibility.timeSamples = {
            10: "invisible",
        }
    }

    def Xform "Shed" (
        variants = {
            string look = "red"
        }
        prepend variantSets = "look"
    )
    {
        variantSet "look" = {
            "red" {
                def Sphere "Door"
                {
                }

            }
        }
    }
}
`;

const WEAK = `#usda 1.0

def Xform "World"
{
    def Scope "Group" (
        doc = "Only the sublayer says so."
    )
    {
    }

    def "Keep" (
        references = @./asset.usda@</Asset>
    )
    {
    }

    def Sphere "Cone"
    {
    }

    def Sphere "Pebble"
    {
        double radius = 3

        def Sphere "Chip"
        {
        }
    }

    def Xform "Hall"
    {
        token visibility = "invisible"
    }
}
`;

const ASSET = `#usda 1.0

def Xform "Asset"
{
    double size = 2

    def Sphere "Base"
    {
    }

    def Sphere "Roof"
    {
    }
}
`;

/** The layered scene, open. */
async function opened() {
  const h = await harness();
  await h.open(sampleSource("root.usda", { "root.usda": ROOT, "weak.usda": WEAK, "asset.usda": ASSET }));
  const stage = h.store.session!.stage;
  const weak = stage.layers().find((layer) => layer.identifier === "weak.usda")!;
  return { h, store: h.store, stage, weak, steps: () => h.store.session!.history.undoSteps.length };
}

/** The names of the children of the prim at `path`, in order. */
const names = (stage: Stage, path: string) => stage.children(path).map(nameOf);

/** The layers `sites` name. */
const layersOf = (sites: readonly { readonly layer: Layer }[]) => sites.map((site) => site.layer.identifier);

test("where a prim's opinions are: the layers that hold a spec of it, not the root of a stack that holds none", async () => {
  const { store, stage, weak } = await opened();
  // Only the sublayer defines the group, with no attribute authored: the root layer holds no spec of it.
  assert.equal(stage.layerSpec(stage.root, "/World/Group"), null);
  assert.deepEqual(layersOf(opinionSites(stage, "/World/Group")), ["weak.usda"]);
  const refusal = store.duplicate("/World/Group");
  assert.match(refusal!, /root\.usda has no spec of \/World\/Group to copy: its opinions are in weak\.usda\./);
  // The sublayer authors the reference; the asset's attributes are not all there is of the prim.
  const sites = opinionSites(stage, "/World/Keep");
  assert.deepEqual(layersOf(sites), ["weak.usda", "asset.usda"]);
  assert.deepEqual(
    sites.map((site) => site.arc),
    [null, "references"],
  );
  // The copy is made where the first site's target writes.
  const there = sites[0]!.editTarget!;
  store.setTarget(there.kind === "layer" ? there.layer : there);
  assert.equal(store.duplicate("/World/Keep"), undefined);
  assert.notEqual(stage.layerSpec(weak, "/World/Keep1"), null);
  assert.deepEqual(names(stage, "/World/Keep1"), ["Base", "Roof"]);
});

test("a reorder that the stage would not compose is refused with nothing authored; one that it composes is a step", async () => {
  const { h, store, stage, weak, steps } = await opened();
  const before = names(stage, "/World");
  assert.deepEqual(before, ["Group", "Keep", "Cone", "Pebble", "Hall", "Ball", "Stone", "Moss", "Tower", "Blink", "Shed"]);
  // In the root layer, the strongest, every child is known: Ball moves above Hall, which only the sublayer defines.
  assert.equal(store.reorderChild("/World/Ball", -1), undefined);
  const moved = names(stage, "/World");
  assert.deepEqual(moved.slice(3, 6), ["Pebble", "Ball", "Hall"]);
  await h.settle(store.undo());
  assert.deepEqual(names(stage, "/World"), before);

  // In the sublayer, Ball is not known: its reorder cannot place it, and the composed order would stay as it was.
  store.setTarget(weak);
  const refusal = store.reorderChild("/World/Ball", -1);
  assert.match(refusal!, /The children of \/World do not take that order from weak\.usda/);
  assert.equal(store.notices.at(-1)?.message, refusal);
  assert.deepEqual(names(stage, "/World"), before);
  const spec = stage.layerSpec(weak, "/World");
  assert.ok(spec?.type === "prim");
  assert.ok(!spec.fields.includes("primOrder"), "nothing was authored");
  assert.equal(steps(), 0, "and nothing is there to undo");
  assert.equal(store.isEdited(weak), false);
  // The refusal took nothing from what can be redone.
  await h.settle(store.redo());
  assert.deepEqual(names(stage, "/World"), moved);
  await h.settle(store.undo());
  // Among the sublayer's own children it orders, and shows.
  assert.equal(store.reorderChild("/World/Cone", -1), undefined);
  assert.deepEqual(names(stage, "/World").slice(0, 3), ["Group", "Cone", "Keep"]);
  await h.settle(store.undo());
  // Nor from a step that wrote the same field of the same spec.
  assert.ok(store.reorderChild("/World/Ball", -1) !== undefined);
  await h.settle(store.redo());
  assert.deepEqual(names(stage, "/World").slice(0, 3), ["Group", "Cone", "Keep"]);
  await h.settle(store.undo());
  assert.deepEqual(store.notices.map((notice) => notice.code), [null, null], "the two refusals, and no failed redo");
  store.setTarget(undefined);

  // Through the reference, the asset knows its own children, not the one the root layer adds beneath the prim.
  const reference = stage.prim("/World/Tower")!.compositionArcs().find((arc) => arc.arc === "references")!;
  store.setTarget(reference.editTarget());
  assert.deepEqual(names(stage, "/World/Tower"), ["Base", "Roof", "Flag"]);
  assert.match(store.reorderChild("/World/Tower/Flag", -1)!, /do not take that order from asset\.usda/);
  assert.equal(store.reorderChild("/World/Tower/Roof", -1), undefined);
  assert.deepEqual(names(stage, "/World/Tower"), ["Roof", "Base", "Flag"]);
  // A drop between siblings goes the same way.
  assert.match(store.reorderChildren("/World/Tower", ["Flag", "Roof", "Base"])!, /do not take that order/);
});

test("the eye on a prim whose visibility has one time sample authors the sample, not a default the sample hides", async () => {
  const { h, store, stage } = await opened();
  store.seek(10);
  assert.deepEqual(stage.visibilities(["/World/Blink"], { time: 10 }), ["invisible"]);
  assert.equal(store.setVisibility("/World/Blink", true), undefined);
  assert.deepEqual(stage.visibilities(["/World/Blink"], { time: 10 }), ["inherited"], "it is visible at the time it was made visible at");
  const spec = stage.layerSpec(stage.root, "/World/Blink.visibility");
  assert.ok(spec?.type === "attribute");
  assert.equal(spec.default, null, "no default was authored beside the sample");
  assert.deepEqual(spec.timeSamples, [10]);
  await h.settle(store.undo());
  assert.deepEqual(stage.visibilities(["/World/Blink"], { time: 10 }), ["invisible"]);
});

test("the eye says so when a stronger layer's visibility still decides", async () => {
  const { store, stage, weak } = await opened();
  // The root layer says the hall is visible; edits then go to the weaker sublayer.
  store.setVisibility("/World/Hall", true);
  assert.deepEqual(stage.visibilities(["/World/Hall"]), ["inherited"]);
  assert.equal(store.notices.length, 0);
  store.setTarget(weak);
  assert.equal(store.setVisibility("/World/Hall", false), undefined);
  assert.deepEqual(stage.visibilities(["/World/Hall"]), ["inherited"], "the press changed nothing that shows");
  assert.equal(store.notices.at(-1)?.message, "/World/Hall is still visible: its visibility is authored in weak.usda, but the opinion of root.usda at /World/Hall.visibility is stronger.");
  // One that shows says nothing.
  store.setTarget(undefined);
  const said = store.notices.length;
  store.setVisibility("/World/Hall", false);
  assert.deepEqual(stage.visibilities(["/World/Hall"]), ["invisible"]);
  assert.equal(store.notices.length, said);
});

test("a duplicate inside a variant is refused before it is tried, as itself and not as a missing spec", async () => {
  const { store, stage, steps } = await opened();
  store.editInsideVariant("/World/Shed", "look", "red");
  const target = store.session!.target;
  assert.match(copyRefusal(target, copyPath(stage, "/World/Shed/Door"))!, /a copy is not made inside a variant/);
  const refusal = store.duplicate("/World/Shed/Door");
  assert.match(refusal!, /a copy is not made inside a variant/);
  // Both menus list it disabled with that reason: the one list they show agrees with the store.
  const listed = primActions(stage, "/World/Shed/Door", target).find((action) => action.id === "duplicate")!;
  assert.deepEqual([listed.enabled, listed.refusal], [false, refusal]);
  // Not the code the "Nothing to copy" dialog opens for: the variant holds the spec.
  assert.notEqual(store.notices.at(-1)?.code, "edit.rejected.no_such_spec");
  assert.equal(steps(), 0);
});

test("a duplicate is offered at a site only where its copy can be written", async () => {
  const { store, stage } = await opened();
  // The reference is on the prim itself: its target maps the prim and beneath, not a sibling beside it.
  assert.deepEqual(
    copySites(stage, "/World/Keep").map((site) => [site.layer.identifier, site.copies]),
    [
      ["weak.usda", true],
      ["asset.usda", false],
    ],
  );
  // Beneath the referenced prim, the asset is where the copy is made, through the reference above it, which maps the copy too.
  const beneath = copySites(stage, "/World/Tower/Base");
  assert.deepEqual(
    beneath.map((site) => [site.layer.identifier, site.copies]),
    [["asset.usda", true]],
  );
  store.setTarget(beneath[0]!.editTarget);
  assert.equal(store.duplicate("/World/Tower/Base"), undefined);
  assert.notEqual(stage.layerSpec(beneath[0]!.layer, "/Asset/Base1"), null);
});
