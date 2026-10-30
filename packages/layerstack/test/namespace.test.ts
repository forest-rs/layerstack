// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Changes, Layerstack, Stage } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/**
 * A root over a sublayer: `/World/Rock` has opinions in both, a variant
 * set whose variants author inside it, and a relationship naming it; an
 * instance of a referenced part sits beside it.
 */
const files: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    subLayers = [@./sub.usda@]
)
def Xform "World" {
    rel lookAt = </World/Rock>

    def Sphere "Rock" (
        variants = {
            string size = "small"
        }
        prepend variantSets = "size"
    ) {
        variantSet "size" = {
            "large" {
                double radius = 4
            }
            "small" {
                double radius = 1
                def "Moss" {}
            }
        }
    }

    def "Part" (
        instanceable = true
        prepend references = @./part.usda@
    ) {
    }
}
`,
  "sub.usda": `#usda 1.0
over "World" {
    over "Rock" {
        color3f color = (0.5, 0.5, 0.5)
    }
}
`,
  "part.usda": `#usda 1.0
(
    defaultPrim = "Part"
)
def "Part" {
    def "Bolt" {}
}
`,
};

async function openScene(): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: text.encode(files["scene.usda"] as string) });
  await stage.resolve(
    mockSource({ "sub.usda": files["sub.usda"] as string, "part.usda": files["part.usda"] as string }),
  );
  return stage;
}

/** Runs `f` and returns the `LayerstackError` it throws. */
function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

function layer(stage: Stage, identifier: string) {
  const found = stage.layers().find((each) => each.identifier === identifier);
  assert.ok(found !== undefined, `the stage has ${identifier}`);
  return found;
}

describe("namespace edits", () => {
  it("move renames a prim with its variants, its opinions in every layer, and the paths naming it", async () => {
    using stage = await openScene();
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const receipt = stage.move("/World/Rock", "/World/Stone");
    assert.equal(receipt.target.layer, stage.root);
    assert.equal(receipt.target.kind, "layer");
    assert.deepEqual(
      new Set(receipt.layers),
      new Set([stage.root, layer(stage, "sub.usda")]),
      "one move wrote both layers of the stack",
    );
    assert.equal(stage.prim("/World/Rock"), null);
    assert.equal(stage.prim("/World/Stone")?.typeName, "Sphere");
    assert.deepEqual(stage.prim("/World/Stone")?.variantSets(), [
      { name: "size", selection: "small", variants: ["large", "small"] },
    ]);
    assert.deepEqual(stage.children("/World/Stone"), ["/World/Stone/Moss"], "the selected variant's child moved");
    assert.deepEqual(stage.attribute("/World/Stone.radius")?.get(), { kind: "scalar", type: "double", value: 1 });
    assert.deepEqual(
      stage.attribute("/World/Stone.color")?.explain()?.opinions.map((opinion) => `${opinion.layer.identifier} ${opinion.specPath}`),
      ["sub.usda /World/Stone.color"],
      "the sublayer's over moved too",
    );
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/World/Stone"]);
    assert.equal(stage.isDirty(stage.root), true);
    assert.equal(stage.isDirty(layer(stage, "sub.usda")), true);
    assert.ok(heard.length > 0, "the listeners heard the move");

    // The other variant moved with the set: selecting it reads its radius at the new path.
    stage.setVariantSelection("/World/Stone", "size", "large");
    assert.deepEqual(stage.attribute("/World/Stone.radius")?.get(), { kind: "scalar", type: "double", value: 4 });
  });

  it("the receipt of a move undoes it and redoes it", async () => {
    using stage = await openScene();
    const receipt = stage.move("/World/Rock", "/World/Stone");
    const redo = receipt.undo();
    assert.equal(stage.prim("/World/Stone"), null);
    assert.deepEqual(stage.children("/World/Rock"), ["/World/Rock/Moss"]);
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/World/Rock"]);
    redo.undo();
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/World/Stone"]);
  });

  it("move reparents a prim and renames a property", async () => {
    using stage = await openScene();
    stage.apply([{ type: "definePrim", path: "/World/Pile", typeName: "Xform" }]);
    stage.move("/World/Rock", "/World/Pile/Rock");
    assert.deepEqual(stage.children("/World/Pile"), ["/World/Pile/Rock"]);
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/World/Pile/Rock"]);
    stage.move("/World/Pile/Rock.color", "/World/Pile/Rock.tint");
    assert.equal(stage.attribute("/World/Pile/Rock.color"), null);
    assert.equal(stage.attribute("/World/Pile/Rock.tint")?.typeName, "color3f");
  });

  it("delete removes a prim from every layer and the paths naming it, and undoes", async () => {
    using stage = await openScene();
    const receipt = stage.delete("/World/Rock");
    assert.equal(stage.prim("/World/Rock"), null);
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), []);
    assert.equal(stage.isDirty(layer(stage, "sub.usda")), true);
    receipt.undo();
    assert.equal(stage.prim("/World/Rock")?.typeName, "Sphere");
    assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/World/Rock"]);
  });

  it("a move that cannot be made throws a namespace code and changes nothing", async () => {
    using stage = await openScene();
    const occupied = failure(() => stage.move("/World/Rock", "/World/Part"));
    assert.equal(occupied.code, "namespace.collision");
    assert.equal(occupied.operation, "move");
    assert.equal(occupied.path, "/World/Rock");
    assert.equal(failure(() => stage.move("/World/Rock", "/Nowhere/Rock")).code, "namespace.missing_parent");
    assert.equal(failure(() => stage.move("/World/Gone", "/World/Here")).code, "namespace.not_found");
    assert.equal(failure(() => stage.move("/World", "/World/Rock/World")).code, "namespace.cycle");
    assert.equal(failure(() => stage.delete("/World/Part/Bolt")).code, "namespace.instance_proxy");
    assert.equal(failure(() => stage.move("/World/Rock", "World")).code, "path.invalid");
    assert.equal(failure(() => stage.move("/World/Rock", 3 as unknown as string)).code, "argument.invalid");
    assert.equal(stage.isDirty(stage.root), false, "nothing changed");
    assert.equal(stage.prim("/World/Rock")?.typeName, "Sphere");
  });

  it("move targets the edit layer's stack and refuses another stage's layer", async () => {
    using stage = await openScene();
    using other = await openScene();
    assert.equal(failure(() => stage.move("/World/Rock", "/World/Stone", other.root)).code, "layer.affinity");
    const outside = layer(stage, "part.usda");
    assert.equal(failure(() => stage.move("/World/Rock", "/World/Stone", outside)).code, "layer.outside_stack");
  });
});
