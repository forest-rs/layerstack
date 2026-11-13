// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { AttributeSpec, Layer, LayerSpec, Layerstack, ListOp, PrimSpec, Stage } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/**
 * A robot referenced from its own file and from another file with an
 * offset, removed from a third; a variant set whose branch holds a prim
 * and an arc; list ops with items in every list; a sublayer with an
 * offset; layer metadata.
 */
const files: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    defaultPrim = "World"
    doc = "A scene"
    upAxis = "Z"
    subLayers = [
        @./shot.usda@ (offset = 10)
    ]
)
def Xform "World" (
    kind = "assembly"
) {
    def Xform "Robot" (
        active = true
        doc = "A robot"
        prepend apiSchemas = ["CollectionAPI:parts"]
        delete references = @./old.usda@
        prepend references = @./robot.usda@ (offset = 5; scale = 2)
        append references = </World/Spare>
        add inherits = </World/Base>
        reorder inherits = [</World/Base>, </World/Other>]
        specializes = None
        variants = {
            string finish = "painted"
        }
        prepend variantSets = "finish"
    ) {
        double speed.timeSamples = {
            4: 1,
            1: 2,
        }
        uniform token mode = None
        custom rel parts = [</World/Spare>]
        color3f out.connect = </World/Spare.color>
        variantSet "finish" = {
            "chrome" {
            }
            "painted" (
                prepend payload = @./paint.usda@</Paint>
            ) {
                color3f color = (0.8, 0.2, 0.1)
                def "Decal" {
                    double size = 1
                }
            }
        }
    }
    def Xform "Spare" {
        color3f color = (1, 1, 1)
    }
}
`,
  "robot.usda": `#usda 1.0
(
    defaultPrim = "Robot"
)
def Xform "Robot" {
    double height = 1.2
    def Sphere "Head" {
    }
}
`,
  "shot.usda": `#usda 1.0
over "World" {
    over "Robot" (
        instanceable = true
    ) {
    }
}
`,
  "paint.usda": `#usda 1.0
def "Paint" {
}
`,
};

const empty = { explicit: null, prepended: [], appended: [], deleted: [], added: [], ordered: [] };

function listOp<T>(lists: Partial<ListOp<T>>): ListOp<T> {
  return { ...empty, ...lists };
}

async function openScene(options: { editLayer?: "root" | "session" } = {}): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: text.encode(files["scene.usda"]) }, options);
  const { "scene.usda": _root, ...rest } = files;
  await stage.resolve(mockSource({ ...rest, "old.usda": null }));
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

function prim(spec: LayerSpec | null): PrimSpec {
  assert.ok(spec !== null && (spec.type === "prim" || spec.type === "variant"), `a prim or variant spec, not ${JSON.stringify(spec)}`);
  return spec;
}

function attribute(spec: LayerSpec | null): AttributeSpec {
  assert.ok(spec !== null && spec.type === "attribute", `an attribute spec, not ${JSON.stringify(spec)}`);
  return spec;
}

describe("layer specs", () => {
  it("read the pseudo-root's metadata, sublayers and root prims", async () => {
    using stage = await openScene();
    assert.deepEqual(stage.layerSpec(stage.root, "/"), {
      type: "pseudoRoot",
      path: "/",
      fields: ["defaultPrim", "documentation", "subLayerOffsets", "subLayers", "upAxis"],
      defaultPrim: "World",
      documentation: "A scene",
      sublayers: [{ asset: "./shot.usda", layerOffset: { offset: 10, scale: 1 } }],
      primChildren: ["World"],
    });
    const shot = stage.layerSpec(layer(stage, "shot.usda"), "/");
    assert.deepEqual(shot.fields, [], "a layer without metadata");
    assert.deepEqual(shot.primChildren, ["World"]);
  });

  it("read a prim spec's fields, with each list of its list ops apart", async () => {
    using stage = await openScene();
    const robot = prim(stage.layerSpec(stage.root, "/World/Robot"));
    assert.deepEqual(robot, {
      type: "prim",
      path: "/World/Robot",
      fields: [
        "active",
        "apiSchemas",
        "documentation",
        "inheritPaths",
        "references",
        "specializes",
        "specifier",
        "typeName",
        "variantSelection",
        "variantSetNames",
      ],
      specifier: "def",
      typeName: "Xform",
      kind: null,
      active: true,
      instanceable: null,
      documentation: "A robot",
      references: listOp({
        prepended: [{ asset: "./robot.usda", primPath: null, layerOffset: { offset: 5, scale: 2 } }],
        appended: [{ asset: null, primPath: "/World/Spare", layerOffset: { offset: 0, scale: 1 } }],
        deleted: [{ asset: "./old.usda", primPath: null, layerOffset: { offset: 0, scale: 1 } }],
      }),
      payloads: null,
      inherits: listOp({ added: ["/World/Base"], ordered: ["/World/Base", "/World/Other"] }),
      specializes: listOp({ explicit: [] }),
      apiSchemas: listOp({ prepended: ["CollectionAPI:parts"] }),
      variantSetNames: listOp({ prepended: ["finish"] }),
      variantSelections: new Map([["finish", "painted"]]),
      variantSets: [{ name: "finish", variants: ["chrome", "painted"] }],
      primChildren: [],
      properties: ["speed", "mode", "parts", "out"],
    });
    assert.equal(prim(stage.layerSpec(stage.root, "/World")).kind, "assembly");
    const over = prim(stage.layerSpec(layer(stage, "shot.usda"), "/World/Robot"));
    assert.equal(over.specifier, "over");
    assert.equal(over.instanceable, true, "the sublayer authors instanceable, the root does not");
    assert.deepEqual(over.fields, ["instanceable", "specifier"]);
  });

  it("read property specs: presence and blocks of values, sample times, targets and connections", async () => {
    using stage = await openScene();
    const speed = attribute(stage.layerSpec(stage.root, "/World/Robot.speed"));
    assert.deepEqual(speed, {
      type: "attribute",
      path: "/World/Robot.speed",
      fields: ["timeSamples", "typeName"],
      typeName: "double",
      custom: false,
      variability: "varying",
      default: null,
      timeSamples: [1, 4],
      connections: null,
    });
    const mode = attribute(stage.layerSpec(stage.root, "/World/Robot.mode"));
    assert.equal(mode.default, "block");
    assert.equal(mode.variability, "uniform");
    assert.deepEqual(mode.fields, ["default", "typeName", "variability"]);
    assert.deepEqual(attribute(stage.layerSpec(stage.root, "/World/Robot.out")).connections, listOp({ explicit: ["/World/Spare.color"] }));
    assert.deepEqual(stage.layerSpec(stage.root, "/World/Robot.parts"), {
      type: "relationship",
      path: "/World/Robot.parts",
      fields: ["custom", "targetPaths"],
      custom: true,
      targets: listOp({ explicit: ["/World/Spare"] }),
    });
  });

  it("reach specs inside a variant by its selection in the path", async () => {
    using stage = await openScene();
    const painted = prim(stage.layerSpec(stage.root, "/World/Robot{finish=painted}"));
    assert.equal(painted.type, "variant");
    assert.equal(painted.specifier, null);
    assert.deepEqual(painted.fields, ["payload"]);
    assert.deepEqual(painted.payloads, listOp({ prepended: [{ asset: "./paint.usda", primPath: "/Paint", layerOffset: { offset: 0, scale: 1 } }] }));
    assert.deepEqual(painted.primChildren, ["Decal"]);
    assert.deepEqual(painted.properties, ["color"]);
    assert.equal(prim(stage.layerSpec(stage.root, "/World/Robot{finish=painted}Decal")).specifier, "def");
    assert.equal(attribute(stage.layerSpec(stage.root, "/World/Robot{finish=painted}.color")).typeName, "color3f");
    assert.equal(stage.layerSpec(stage.root, "/World/Robot{finish=chrome}Decal"), null);
    assert.equal(stage.layerSpec(stage.root, "/World/Robot/Decal"), null, "the variant's prim is not outside it");

    // A variant edit target maps a stage path to that spelling.
    const target = stage.prim("/World/Robot")!.variantEditTarget("finish", "painted");
    const mapped = target.mapPath("/World/Robot/Decal.size");
    assert.equal(mapped, "/World/Robot{finish=painted}Decal.size");
    assert.equal(attribute(stage.layerSpec(target.layer, mapped!)).default, "value");
  });

  it("read nothing where the layer authors nothing", async () => {
    using stage = await openScene();
    assert.equal(stage.layerSpec(stage.root, "/World/Lamp"), null);
    assert.equal(stage.layerSpec(stage.root, "/World/Robot.height"), null, "the referenced layer authors it");
    assert.equal(stage.layerSpec(layer(stage, "shot.usda"), "/World/Spare"), null);
  });

  it("read a referenced layer at the path an arc's edit target maps a stage path to", async () => {
    using stage = await openScene();
    const arc = stage.prim("/World/Robot")!.compositionArcs().find((each) => each.arc === "references");
    assert.ok(arc !== undefined);
    const target = arc.editTarget();
    const mapped = target.mapPath("/World/Robot.height");
    assert.equal(mapped, "/Robot.height");
    const height = attribute(stage.layerSpec(target.layer, mapped!));
    assert.equal(height.default, "value");
    assert.equal(stage.layerSpec(target.layer, "/World/Robot.height"), null, "a stage path is not the layer's");
    assert.deepEqual(prim(stage.layerSpec(target.layer, target.mapPath("/World/Robot")!)).primChildren, ["Head"]);
  });

  it("read the session layer, where edits go", async () => {
    using stage = await openScene({ editLayer: "session" });
    const session = stage.editLayer!;
    assert.notEqual(session, stage.root);
    assert.equal(stage.layerSpec(session, "/World/Robot"), null);
    stage.apply([{ type: "setActive", path: "/World/Robot", active: false }]);
    const robot = prim(stage.layerSpec(session, "/World/Robot"));
    assert.equal(robot.specifier, "over");
    assert.equal(robot.active, false);
    assert.equal(prim(stage.layerSpec(stage.root, "/World/Robot")).active, true, "the root keeps its own");
    assert.deepEqual(stage.layerSpec(session, "/").primChildren, ["World"]);
  });

  it("read a layer from a USDC file as from its USDA text", async () => {
    using stage = await openScene();
    const bytes = stage.exportLayer(stage.root, "usdc");
    using binary = layerstack.open({ identifier: "scene.usdc", bytes });
    for (const path of [
      "/",
      "/World",
      "/World/Robot",
      "/World/Robot{finish=painted}",
      "/World/Robot{finish=painted}Decal",
      "/World/Robot.speed",
      "/World/Robot.mode",
      "/World/Robot.parts",
      "/World/Robot.out",
    ]) {
      assert.deepEqual(binary.layerSpec(binary.root, path), stage.layerSpec(stage.root, path), path);
    }
  });

  it("follow an edit and its undo", async () => {
    using stage = await openScene();
    const before = stage.layerSpec(stage.root, "/World/Spare");
    const receipt = stage.apply([
      { type: "addReference", path: "/World/Spare", asset: "./robot.usda", position: "frontOfAppend" },
      { type: "setKind", path: "/World/Spare", kind: "component" },
    ]);
    const after = prim(stage.layerSpec(stage.root, "/World/Spare"));
    assert.deepEqual(after.references, listOp({ appended: [{ asset: "./robot.usda", primPath: null, layerOffset: { offset: 0, scale: 1 } }] }));
    assert.equal(after.kind, "component");
    assert.deepEqual(after.fields, ["kind", "references", "specifier", "typeName"]);
    receipt.undo();
    assert.deepEqual(stage.layerSpec(stage.root, "/World/Spare"), before);
  });

  it("refuse another stage's layer, and paths that are not spec paths", async () => {
    using stage = await openScene();
    using other = await openScene();
    const refused = failure(() => stage.layerSpec(other.root, "/"));
    assert.equal(refused.code, "layer.affinity");
    assert.equal(refused.operation, "layerSpec");
    for (const path of ["World", "/World/", "/World{finish}", "/.size", ""]) {
      const invalid = failure(() => stage.layerSpec(stage.root, path));
      assert.equal(invalid.code, "path.invalid", path);
      assert.equal(invalid.path, path);
    }
    assert.equal(failure(() => stage.layerSpec(stage.root, 1 as unknown as string)).code, "argument.invalid");
  });
});
