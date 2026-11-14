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
import { nameOf, opinionSites } from "../src/model/primAuthoring.ts";
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
