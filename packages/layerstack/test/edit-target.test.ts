// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { CompositionArc, EditTarget, LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Layer, Layerstack, Stage, Value } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/**
 * A robot referenced from its own file with `(offset = 10; scale = 2)`, a
 * spare referencing it plainly, a local variant set, and a sublayer
 * authored with `(offset = 10)`.
 */
const files: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    subLayers = [
        @./shot.usda@ (offset = 10)
    ]
)
def Xform "World" {
    def Xform "Robot" (
        prepend references = @./robot.usda@ (offset = 10; scale = 2)
        variants = {
            string finish = "painted"
        }
        prepend variantSets = "finish"
    ) {
        variantSet "finish" = {
            "chrome" {
            }
            "painted" {
                color3f color = (0.8, 0.2, 0.1)
            }
        }
    }
    def Xform "Spare" (
        prepend references = @./robot.usda@
    ) {
    }
    def "Lamp" {
    }
}
`,
  "robot.usda": `#usda 1.0
(
    defaultPrim = "Robot"
)
def Xform "Robot" {
    double height = 1.2
    color3f color = (0.5, 0.5, 0.5)
    double spin.timeSamples = {
        0: 0,
    }
    rel aim = </Robot/Head>
    def Sphere "Head" {
    }
}
`,
  "shot.usda": `#usda 1.0
over "World" {
    over "Lamp" {
        double glow.timeSamples = {
            0: 1,
        }
    }
    over "Robot" {
        variantSet "finish" = {
            "painted" {
                double glow.timeSamples = {
                    0: 1,
                }
            }
        }
    }
}
`,
};

async function openScene(): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: text.encode(files["scene.usda"]) });
  await stage.resolve(mockSource({ "robot.usda": files["robot.usda"] as string, "shot.usda": files["shot.usda"] as string }));
  return stage;
}

function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

function layer(stage: Stage, identifier: string): Layer {
  const found = stage.layers().find((each) => each.identifier === identifier);
  assert.ok(found !== undefined, `the stage has ${identifier}`);
  return found;
}

function double(value: number): Value {
  return { kind: "scalar", type: "double", value };
}

/** The target of the opinion of `path` from `identifier`. */
function opinionTarget(stage: Stage, path: string, identifier: string): EditTarget {
  const opinion = stage.attribute(path)?.explain()?.opinions.find((each) => each.layer.identifier === identifier);
  assert.ok(opinion?.editTarget, `${path} has an opinion in ${identifier}`);
  return opinion.editTarget;
}

describe("edit targets", () => {
  it("name where each opinion lives, as one interned object per mapping", async () => {
    using stage = await openScene();
    const robot = layer(stage, "robot.usda");
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    assert.ok(height instanceof EditTarget);
    assert.equal(height.kind, "arc");
    assert.equal(height.layer, robot);
    assert.equal(height.prim, "/World/Robot");
    assert.equal(height.site, "/Robot");
    assert.equal(height.arc, "references");
    assert.equal(height.mapsOutsidePrim, false);
    assert.deepEqual(height.layerOffset, { offset: 10, scale: 2 });
    assert.equal(height.stage, stage);
    assert.equal(opinionTarget(stage, "/World/Robot.color", "robot.usda"), height, "one target per mapping");
    const reference = stage
      .prim("/World/Robot")
      ?.compositionArcs()
      .find((arc) => arc.arc === "references");
    assert.ok(reference instanceof CompositionArc);
    assert.equal(reference.editTarget(), height, "the arc's target is the opinion's");

    const color = stage.attribute("/World/Robot.color")?.explain()?.opinions[0]?.editTarget;
    assert.equal(color?.kind, "variant");
    assert.equal(color?.site, "/World/Robot{finish=painted}");
    assert.equal(stage.prim("/World/Robot")?.variantEditTarget("finish", "painted"), color);
    assert.equal(color?.composed(), true);
    assert.equal(stage.prim("/World/Robot")?.variantEditTarget("finish", "chrome").composed(), false, "not selected");

    const shot = stage.editTargetForLayer(layer(stage, "shot.usda"));
    assert.equal(shot.kind, "layer");
    assert.deepEqual(shot.layerOffset, { offset: 10, scale: 1 });
    assert.equal(shot.mapTime(15), 5);
    assert.equal(stage.hasLocalLayer(robot), false);
    assert.equal(stage.hasLocalLayer(stage.root), true);
    assert.equal(failure(() => stage.editTargetForLayer(robot)).code, "layer.outside_stack");
  });

  it("map paths and times into the target layer", async () => {
    using stage = await openScene();
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    assert.equal(height.mapPath("/World/Robot.height"), "/Robot.height");
    assert.equal(height.mapPath("/World/Robot/Head"), "/Robot/Head");
    assert.equal(height.mapPath("/World/Lamp"), null, "outside the reference");
    assert.equal(height.mapTime(14), 2, "(14 - 10) / 2");
    assert.equal(stage.editTargetForLayer(stage.root).mapPath("/World/Lamp"), "/World/Lamp");
  });

  it("write through a reference: the asset changes for every prim that references it", async () => {
    using stage = await openScene();
    const robot = layer(stage, "robot.usda");
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    const receipt = stage.apply(
      [
        { type: "setValue", path: "/World/Robot.height", value: { type: "double", value: 1.5 } },
        { type: "setValue", path: "/World/Robot.spin", time: 14, value: { type: "double", value: 70 } },
        { type: "setTargets", path: "/World/Robot.aim", targets: ["/World/Robot/Head", "/World/Robot.height"] },
      ],
      height,
    );
    assert.equal(receipt.target, height);
    assert.equal(receipt.target.layer, robot);
    assert.deepEqual(receipt.layers, [robot]);
    assert.equal(stage.isDirty(robot), true);
    assert.equal(stage.isDirty(stage.root), false);
    assert.deepEqual(stage.attribute("/World/Robot.height")?.get(), double(1.5));
    assert.deepEqual(stage.attribute("/World/Spare.height")?.get(), double(1.5));
    assert.deepEqual(stage.attribute("/World/Robot.spin")?.timeSamples(), [10, 14], "stage time 14 read back at 14");
    assert.deepEqual(stage.attribute("/World/Spare.spin")?.timeSamples(), [0, 2], "written at layer time 2");
    assert.deepEqual(stage.relationship("/World/Spare.aim")?.targets(), ["/World/Spare/Head", "/World/Spare.height"]);
    receipt.undo().release();
    assert.deepEqual(stage.attribute("/World/Spare.height")?.get(), double(1.2));
  });

  it("reject what the target does not map, naming the edit", async () => {
    using stage = await openScene();
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    const error = failure(() =>
      stage.apply(
        [
          { type: "setValue", path: "/World/Robot.height", value: { type: "double", value: 1 } },
          { type: "setValue", path: "/World/Lamp.glow", value: { type: "double", value: 1 } },
        ],
        height,
      ),
    );
    assert.equal(error.code, "edit.rejected.unmappable");
    assert.equal(error.edit, 1);
    assert.equal(error.path, "/World/Lamp.glow");
    const targets = failure(() =>
      stage.apply([{ type: "setTargets", path: "/World/Robot.aim", targets: ["/World/Lamp"] }], height),
    );
    assert.equal(targets.code, "edit.rejected.unmappable_target");
    assert.equal(stage.isDirty(layer(stage, "robot.usda")), false, "nothing applied");
  });

  it("write inside a variant, selected or not", async () => {
    using stage = await openScene();
    const chrome = stage.prim("/World/Robot")?.variantEditTarget("finish", "chrome");
    assert.ok(chrome);
    stage.apply([{ type: "createAttribute", path: "/World/Robot.gloss", typeName: "double", value: { type: "double", value: 0.9 } }], chrome).release();
    assert.equal(stage.attribute("/World/Robot.gloss"), null, "the chrome variant is not selected");
    stage.setVariantSelection("/World/Robot", "finish", "chrome").release();
    assert.equal(chrome.composed(), true);
    assert.deepEqual(stage.attribute("/World/Robot.gloss")?.get(), double(0.9));
    assert.equal(failure(() => stage.prim("/World/Robot")?.variantEditTarget("finish", "{bad")).code, "path.invalid");
  });

  it("write a variant target's times unmapped by the sublayer's offset, as OpenUSD does", async () => {
    using stage = await openScene();
    const shot = layer(stage, "shot.usda");
    const robot = stage.prim("/World/Robot");
    assert.ok(robot);
    const direct = robot.variantEditTarget("finish", "painted", shot);
    assert.equal(direct.kind, "variant");
    assert.deepEqual(direct.layerOffset, { offset: 0, scale: 1 }, "ForLocalDirectVariant has no offset");
    assert.equal(direct.mapTime(15), 15);
    const glow = stage.attribute("/World/Robot.glow");
    const receipt = stage.apply([{ type: "setValue", path: "/World/Robot.glow", time: 15, value: { type: "double", value: 2 } }], direct);
    assert.deepEqual(glow?.timeSamples(), [10, 25], "written at the sublayer's time 15, read at 25");
    receipt.undo().release();

    // The selected variant's own arc in the sublayer maps times through its offset.
    const opinion = glow?.explain(0)?.opinions[0]?.editTarget;
    assert.ok(opinion);
    assert.equal(opinion.kind, "arc");
    assert.equal(opinion.arc, "variants");
    assert.deepEqual(opinion.layerOffset, { offset: 10, scale: 1 });
    const variantArc = robot.compositionArcs().find((arc) => arc.arc === "variants");
    assert.equal(variantArc?.editTarget(shot), opinion);
    assert.equal(variantArc?.editTarget(stage.root), robot.variantEditTarget("finish", "painted"), "in the root they agree");
    stage.apply([{ type: "setValue", path: "/World/Robot.glow", time: 15, value: { type: "double", value: 2 } }], opinion).release();
    assert.deepEqual(glow?.timeSamples(), [10, 15], "written at the sublayer's time 5");
  });

  it("go stale when their arc or prim goes, and undo still works", async () => {
    using stage = await openScene();
    const spare = opinionTarget(stage, "/World/Spare.height", "robot.usda");
    const edit = stage.apply([{ type: "setValue", path: "/World/Spare.height", value: { type: "double", value: 2 } }], spare);
    const removal = stage.apply([{ type: "removeReference", path: "/World/Spare", asset: "./robot.usda" }]);
    const stale = failure(() => spare.mapPath("/World/Spare"));
    assert.equal(stale.code, "target.stale");
    assert.equal(stale.path, "/World/Spare");
    assert.equal(spare.composed(), false);
    assert.equal(
      failure(() => stage.apply([{ type: "setValue", path: "/World/Spare.height", value: { type: "double", value: 3 } }], spare)).code,
      "target.stale",
    );
    edit.undo().release();
    removal.undo().release();
    assert.equal(spare.composed(), true, "the reference is back");
    const robot = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    stage.move("/World/Robot", "/World/Rover").release();
    assert.equal(failure(() => robot.mapPath("/World/Robot")).code, "target.stale", "renamed");
  });

  it("refuse moves and deletes through arcs and variants, before calling the module", async () => {
    using stage = await openScene();
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    const move = failure(() => stage.move("/World/Robot", "/World/Rover", height));
    assert.equal(move.code, "namespace.unmappable");
    assert.equal(move.path, "/World/Robot");
    const variant = stage.prim("/World/Robot")?.variantEditTarget("finish", "painted");
    assert.equal(failure(() => stage.delete("/World/Robot", variant)).code, "namespace.unmappable");
    const receipt = stage.move("/World/Lamp", "/World/Light", stage.editTargetForLayer(stage.root));
    assert.deepEqual(new Set(receipt.layers), new Set([stage.root, layer(stage, "shot.usda")]));
  });

  it("check affinity, arc layers and relocate sources", async () => {
    using stage = await openScene();
    using other = await openScene();
    const height = opinionTarget(stage, "/World/Robot.height", "robot.usda");
    const error = failure(() => other.apply([{ type: "setActive", path: "/World/Robot", active: false }], height));
    assert.equal(error.code, "target.affinity");
    const arcs = stage.prim("/World/Robot")?.compositionArcs() ?? [];
    assert.deepEqual(
      arcs.map((arc) => `${arc.arc} ${arc.site} ${arc.layerStack.identifier} ${arc.parent}`),
      ["local /World/Robot scene.usda null", "variants /World/Robot{finish=painted} scene.usda 0", "references /Robot robot.usda 0"],
    );
    assert.deepEqual(
      arcs[0]?.layers.map((each) => each.identifier),
      ["scene.usda", "shot.usda"],
    );
    assert.equal(arcs[0]?.editTarget(layer(stage, "shot.usda")), stage.editTargetForLayer(layer(stage, "shot.usda")));
    assert.equal(failure(() => arcs[2]?.editTarget(stage.root)).code, "target.not_in_arc");

    const relocated = layerstack.open({
      identifier: "relocated.usda",
      bytes: text.encode(`#usda 1.0
(
    relocates = {
        </Model/Old>: </Model/New>
    }
)
def "Model" (
    prepend references = </Asset>
) {
}
def "Asset" {
    def "Old" {
        double size = 1
    }
}
`),
    });
    try {
      const arc = relocated
        .prim("/Model/New")
        ?.compositionArcs()
        .find((each) => each.arc === "relocates");
      assert.ok(arc, "the relocated prim has a relocates arc");
      assert.equal(failure(() => arc.editTarget()).code, "target.relocate_source");
    } finally {
      relocated.free();
    }
  });

  it("refuse a package member through an arc with a way on", async () => {
    const asset = layerstack.open({ identifier: "robot.usda", bytes: text.encode(files["robot.usda"]) });
    const packaged = asset.exportPackage().bytes;
    asset.free();
    assert.ok(packaged);
    using stage = layerstack.open({
      identifier: "scene.usda",
      bytes: text.encode(`#usda 1.0\ndef "Packed" (\n    prepend references = @./robot.usdz@\n) {\n}\n`),
    });
    await stage.resolve(mockSource({ "robot.usdz": packaged }));
    const member = opinionTarget(stage, "/Packed.height", "robot.usdz");
    assert.equal(member.layer.readOnly, true, "a caller can check before offering the edit");
    const error = failure(() => stage.apply([{ type: "setValue", path: "/Packed.height", value: { type: "double", value: 2 } }], member));
    assert.equal(error.code, "layer.read_only");
    assert.match(error.message, /editTargetForLayer/);
  });
});
