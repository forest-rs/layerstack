// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The pure pieces of authoring composition: what a layer's USDA authors
// about arcs and variant sets, which layer authors each arc the stage
// composes (and so whether it can be removed where edits go), the USDA an
// added arc authors, asset paths relative to a layer, where an edit
// target writes a prim, and the names USD takes. Read over the sample
// scene opened in Node, as the inspector reads it.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { type Layer, loadLayerstack, type Stage } from "@forest-rs/layerstack";
import {
  addArcEdit,
  anchorAsset,
  arcItemText,
  authoredHere,
  deletedHere,
  describeTarget,
  findAuthoring,
  layerOffsetProblem,
  mapping,
  namespaceRefusal,
  newArcLabel,
  newArcUsda,
  primPathProblem,
  relativeAsset,
  removeArcEdit,
  stagePathOf,
  uncomposedHere,
  variantNameProblem,
  variantOfSite,
  variantSetNameProblem,
  variantSetSource,
} from "../src/model/composition.ts";
import { type LayerSpecs, readLayerSpecs } from "../src/model/layerSpecs.ts";

const SCENE = new URL("../public/scene/", import.meta.url);

/** The sample scene, opened and resolved. */
async function sample(): Promise<Stage> {
  const layerstack = await loadLayerstack();
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new Uint8Array(await readFile(new URL("scene.usda", SCENE))) });
  await stage.resolve({
    fetch: async (identifier: string) => {
      try {
        return new Uint8Array(await readFile(new URL(identifier, SCENE)));
      } catch {
        return null;
      }
    },
  });
  return stage;
}

/** Each layer's specs as the inspector reads them: its exported text. */
function specsOf(stage: Stage): (layer: Layer) => LayerSpecs {
  return (layer) => readLayerSpecs(new TextDecoder().decode(stage.exportLayer(layer, "usda")));
}

test("a layer's specs: arcs as list ops, variant sets and their variants, selections, default and root prims", () => {
  const specs = readLayerSpecs(`#usda 1.0
(
    defaultPrim = "World"
    doc = """A layer
with (parentheses) and "quotes" in its doc."""
    subLayers = [
        @./library.usda@ (offset = 2)
    ]
)

def Xform "World" (
    kind = "assembly"
)
{
    def Sphere "Rock" (
        inherits = </Shape>
        delete references = @./x.usda@
        customData = {
            string note = "def Xform \\"Fake\\" ( references = @nope.usda@ )"
        }
    )
    {
        double3 xformOp:translate = (-4, 2, 0)
        double radius.timeSamples = {
            0: 1,
            12: 2,
        }
        color3f[] primvars:displayColor = [(0.3, 0.3, 0.32)] (
            interpolation = "constant"
        )
    }

    def Xform "Robot" (
        prepend references = [
            @./assets/robot.usda@,
            @./assets/beacon.usda@</Beacon> (offset = 10; scale = 2)
        ]
        append payload = </World/Rock>
        prepend specializes = </Shape>
        variants = {
            string finish = "painted"
        }
        prepend variantSets = ["finish", "look"]
    )
    {
        variantSet "finish" = {
            "chrome" (
                prepend references = @./assets/chrome.usda@
            ) {
                rel mirrors = </World/Robot>

                def "Trim" (
                    inherits = </Shape>
                )
                {
                }
            }
            "painted" {
                color3f color = (0.8, 0.2, 0.1)
            }
        }
    }
}

class "Shape"
{
    double radius = 2
}
`);
  assert.equal(specs.defaultPrim, "World");
  assert.deepEqual(specs.roots, [
    { name: "World", specifier: "def", typeName: "Xform" },
    { name: "Shape", specifier: "class", typeName: null },
  ]);
  const rock = specs.specs.get("/World/Rock")!;
  assert.deepEqual(
    rock.arcs.map((arc) => [arc.field, arc.op, arc.asset, arc.primPath]),
    [
      ["inherits", "explicit", null, "/Shape"],
      ["references", "delete", "./x.usda", null],
    ],
    "a dictionary's strings are not read as specs",
  );
  assert.equal(specs.specs.has("/World/Fake"), false);
  const robot = specs.specs.get("/World/Robot")!;
  assert.deepEqual(
    robot.arcs.map((arc) => [arc.field, arc.op, arc.asset, arc.primPath, arc.offset, arc.scale]),
    [
      ["references", "prepend", "./assets/robot.usda", null, 0, 1],
      ["references", "prepend", "./assets/beacon.usda", "/Beacon", 10, 2],
      ["payload", "append", null, "/World/Rock", 0, 1],
      ["specializes", "prepend", null, "/Shape", 0, 1],
    ],
  );
  assert.deepEqual([...robot.selections], [["finish", "painted"]]);
  assert.deepEqual(robot.variantSetNames, [{ op: "prepend", names: ["finish", "look"] }]);
  assert.deepEqual([...robot.variantSets], [["finish", ["chrome", "painted"]]]);
  const chrome = specs.specs.get("/World/Robot{finish=chrome}")!;
  assert.equal(chrome.specifier, null);
  assert.deepEqual(chrome.arcs.map((arc) => arc.asset), ["./assets/chrome.usda"]);
  assert.deepEqual(specs.specs.get("/World/Robot{finish=chrome}Trim")!.arcs.map((arc) => arc.primPath), ["/Shape"]);
  assert.ok(specs.specs.has("/World/Robot{finish=painted}"));
  assert.ok(specs.specs.has("/Shape"));
  // Lines are where the text has them, for the Text tab to show.
  assert.equal(robot.arcs[1]!.line, 35);
});

test("which layer authors each composed arc, and whether it can be removed where edits go", async () => {
  const stage = await sample();
  const read = specsOf(stage);
  const robot = stage.prim("/World/Robot")!.compositionArcs();
  const reference = robot.findIndex((arc) => arc.arc === "references");
  const source = findAuthoring(robot, reference, read)!;
  assert.equal(source.layer, stage.root);
  assert.equal(source.specPath, "/World/Robot");
  assert.equal(source.item.asset, "./assets/robot.usda");
  assert.equal(authoredHere(source, stage.root, "/World/Robot"), true, "the root authors it: removable with edits there");
  const library = stage.layers().find((layer) => layer.identifier === "library.usda")!;
  assert.equal(authoredHere(source, library, "/World/Robot"), false, "not with edits in the sublayer");
  const variant = stage.prim("/World/Robot")!.variantEditTarget("finish", "painted");
  assert.equal(authoredHere(source, variant, "/World/Robot"), false, "not with edits inside a variant");
  assert.equal(findAuthoring(robot, 0, read), null, "the prim's own opinions are no arc to author");
  assert.equal(findAuthoring(robot, robot.findIndex((arc) => arc.arc === "variants"), read), null);

  // The rock's inherit, authored in the root; the class it names is the sublayer's.
  const rock = stage.prim("/World/Rock")!.compositionArcs();
  const inherit = findAuthoring(rock, rock.findIndex((arc) => arc.arc === "inherits"), read)!;
  assert.deepEqual([inherit.layer.identifier, inherit.item.op, inherit.item.primPath], ["scene.usda", "explicit", "/Shape"]);

  // Selecting chrome composes the reference its variant authors: authored inside the variant.
  stage.setVariantSelection("/World/Robot", "finish", "chrome");
  await stage.resolve({ fetch: async (identifier: string) => new Uint8Array(await readFile(new URL(identifier, SCENE))) });
  const chromed = stage.prim("/World/Robot")!.compositionArcs();
  const chrome = findAuthoring(chromed, chromed.findIndex((arc) => arc.layerStack.identifier === "assets/chrome.usda"), read)!;
  assert.equal(chrome.specPath, "/World/Robot{finish=chrome}");
  assert.equal(authoredHere(chrome, stage.root, "/World/Robot"), false, "the root layer's prim spec does not author it");
  assert.equal(authoredHere(chrome, stage.prim("/World/Robot")!.variantEditTarget("finish", "chrome"), "/World/Robot"), true, "inside the variant does");
  // Removing it there deletes it in the variant.
  stage.apply([removeArcEdit("/World/Robot", chrome.item, null)], stage.prim("/World/Robot")!.variantEditTarget("finish", "chrome"));
  const after = stage.prim("/World/Robot")!.compositionArcs();
  assert.equal(after.some((arc) => arc.layerStack.identifier === "assets/chrome.usda"), false);
  assert.deepEqual(
    deletedHere(stage.prim("/World/Robot")!.variantEditTarget("finish", "chrome"), "/World/Robot", read).map((arc) => arc.asset),
    ["./assets/chrome.usda"],
  );
  stage.free();
});

test("an arc authored where edits go that composes nothing is listed: a missing asset", async () => {
  const stage = await sample();
  const read = specsOf(stage);
  const missing = { kind: "reference", asset: "./missing.usda", primPath: null, position: "backOfPrepend", offset: 0, scale: 1 } as const;
  stage.apply([addArcEdit("/World/Rock", missing)]);
  await stage.resolve({ fetch: async () => null });
  const arcs = stage.prim("/World/Rock")!.compositionArcs();
  assert.deepEqual(
    uncomposedHere(arcs, stage.root, "/World/Rock", read).map((arc) => arc.asset),
    ["./missing.usda"],
  );
  assert.equal(stage.status().compositionErrors.some((issue) => issue.prim === "/World/Rock" && issue.message.includes("@./missing.usda@")), true);
  stage.free();
});

test("the edits and USDA of an added arc, and its removal", () => {
  const reference = { kind: "reference", asset: "./assets/chrome.usda", primPath: "/Chrome", position: "backOfPrepend", offset: 10, scale: 2 } as const;
  assert.deepEqual(addArcEdit("/World/New", reference), {
    type: "addReference",
    path: "/World/New",
    asset: "./assets/chrome.usda",
    primPath: "/Chrome",
    position: "backOfPrepend",
    layerOffset: { offset: 10, scale: 2 },
  });
  assert.equal(newArcUsda(reference), "prepend references = @./assets/chrome.usda@</Chrome> (offset = 10; scale = 2)");
  assert.equal(newArcLabel("/World/New", reference), "add reference @./assets/chrome.usda@</Chrome> to /World/New");
  const internal = { kind: "payload", asset: null, primPath: "/World/Rock", position: "backOfAppend", offset: 0, scale: 1 } as const;
  assert.deepEqual(addArcEdit("/World/New", internal), { type: "addPayload", path: "/World/New", primPath: "/World/Rock", position: "backOfAppend" });
  assert.equal(newArcUsda(internal), "append payload = </World/Rock>");
  const inherit = { kind: "inherit", asset: null, primPath: "/Shape", position: "backOfPrepend", offset: 0, scale: 1 } as const;
  assert.deepEqual(addArcEdit("/World/New", inherit), { type: "addInherit", path: "/World/New", primPath: "/Shape", position: "backOfPrepend" });
  assert.equal(newArcUsda({ ...inherit, kind: "specialize" }), "prepend specializes = </Shape>");
  assert.throws(() => addArcEdit("/World/New", { ...inherit, primPath: null }));
  assert.equal(arcItemText("./a.usda", null, 0, 0.5), "@./a.usda@ (scale = 0.5)");

  const authored = { field: "references", op: "prepend", asset: null, primPath: "/Robot/Head", offset: 0, scale: 1, line: 0 } as const;
  // An internal path in a referenced layer is given as the stage path that writes there.
  assert.deepEqual(removeArcEdit("/World/Robot/Body", authored, { prim: "/World/Robot", site: "/Robot" }), {
    type: "removeReference",
    path: "/World/Robot/Body",
    primPath: "/World/Robot/Head",
  });
  assert.equal(stagePathOf({ prim: "/World/Robot", site: "/Robot" }, "/Shape"), "/Shape", "a root path outside the site is authored as given");
  assert.deepEqual(removeArcEdit("/World/Rock", { ...authored, field: "inherits", primPath: "/Shape" }, null), { type: "removeInherit", path: "/World/Rock", primPath: "/Shape" });
  // A reference or payload is named with the offset its layer authors: it is part of which item it is.
  assert.deepEqual(removeArcEdit("/World/Robot", { ...authored, field: "payload", asset: "./robot.usda", primPath: null, offset: 24, scale: 0.5 }, null), {
    type: "removePayload",
    path: "/World/Robot",
    asset: "./robot.usda",
    layerOffset: { offset: 24, scale: 0.5 },
  });
});

test("asset paths relative to the layer that authors them", () => {
  assert.equal(relativeAsset("scene.usda", "assets/chrome.usda"), "./assets/chrome.usda");
  assert.equal(relativeAsset("assets/robot.usda", "library.usda"), "../library.usda");
  assert.equal(relativeAsset("assets/robot.usda", "assets/chrome.usda"), "./chrome.usda");
  assert.equal(relativeAsset("set/a/b.usda", "set/c/d.usda"), "../c/d.usda");
  assert.equal(relativeAsset(null, "assets/chrome.usda"), "assets/chrome.usda");
  assert.equal(relativeAsset("scene.usda", "https://example.com/a.usda"), "https://example.com/a.usda");
  assert.equal(anchorAsset("scene.usda", "./assets/chrome.usda"), "assets/chrome.usda");
  assert.equal(anchorAsset("assets/robot.usda", "../library.usda"), "library.usda");
  assert.equal(anchorAsset("assets/robot.usda", "chrome.usda"), "assets/chrome.usda", "a search path, beside its layer");
  assert.equal(anchorAsset("scene.usda", "/abs/a.usda"), "/abs/a.usda");
});

test("where a target writes the selected prim, in words, and the namespace actions it cannot take", async () => {
  const stage = await sample();
  const robot = stage.prim("/World/Robot")!;
  const reference = robot.compositionArcs().find((arc) => arc.arc === "references")!.editTarget();
  assert.equal(describeTarget(stage.root), "scene.usda");
  assert.equal(describeTarget(reference), "assets/robot.usda at /Robot, through /World/Robot's reference");
  const variant = robot.variantEditTarget("finish", "chrome");
  assert.equal(describeTarget(variant), "scene.usda, inside finish=chrome of /World/Robot");
  assert.deepEqual(variantOfSite(variant.site), { set: "finish", variant: "chrome" });
  assert.equal(mapping(reference, "/World/Robot").text, "/World/Robot → /Robot in assets/robot.usda");
  assert.equal(mapping(reference, "/World/Robot/Head").specPath, "/Robot/Head");
  assert.equal(
    mapping(reference, "/World/Rock").text,
    "/World/Rock is not written through assets/robot.usda at /Robot, through /World/Robot's reference: across a reference, it maps only /World/Robot and what is beneath it",
  );
  assert.equal(mapping(variant, "/World/Robot").text, "/World/Robot → /World/Robot{finish=chrome} in scene.usda");
  assert.equal(mapping(stage.root, "/World/Rock").text, "/World/Rock in scene.usda");

  assert.equal(namespaceRefusal(undefined, "delete", "/World/Robot"), null);
  assert.equal(namespaceRefusal(stage.root, "rename", "/World/Robot"), null);
  assert.match(namespaceRefusal(variant, "delete", "/World/Robot")!, /stage's own layer stack/);
  assert.match(namespaceRefusal(reference, "rename", "/World/Robot/Head")!, /assets\/robot\.usda at \/Robot/);
  assert.equal(namespaceRefusal(reference, "define", "/World/Robot/Arm"), null);
  assert.match(namespaceRefusal(reference, "define", "/World/Other")!, /does not map \/World\/Other/);
  // A target whose arc is gone maps nothing, and says so rather than throw.
  stage.apply([{ type: "removeReference", path: "/World/Robot", asset: "./assets/robot.usda" }]);
  assert.equal(mapping(reference, "/World/Robot").specPath, null);
  assert.match(mapping(reference, "/World/Robot").text, /no longer composes anything/);
  assert.match(namespaceRefusal(reference, "define", "/World/Robot/Arm")!, /does not map/);
  stage.free();
});

test("the layer that holds a variant set's spec", async () => {
  const stage = await sample();
  const read = specsOf(stage);
  const arcs = stage.prim("/World/Robot")!.compositionArcs();
  const held = variantSetSource(arcs, "finish", read)!;
  assert.deepEqual([held.layer.identifier, held.specPath, arcs[held.arc]!.arc], ["scene.usda", "/World/Robot", "local"]);
  assert.equal(variantSetSource(arcs, "look", read), null);
  stage.free();
});

test("names USD takes, and layer offsets", () => {
  assert.equal(variantSetNameProblem("finish"), null);
  assert.match(variantSetNameProblem("9lives")!, /identifier/);
  assert.match(variantSetNameProblem("finish", ["finish"])!, /already/);
  assert.equal(variantNameProblem(".a-b|c"), null);
  assert.equal(variantNameProblem("9"), null);
  assert.match(variantNameProblem("a b")!, /letters, digits/);
  assert.match(variantNameProblem("chrome", ["chrome"])!, /already/);
  assert.equal(primPathProblem("/Robot/Head"), null);
  assert.match(primPathProblem("Robot")!, /starts at the root/);
  assert.match(primPathProblem("/Robot.height")!, /no property/);
  assert.match(primPathProblem("")!, /Type a prim path/);
  assert.equal(layerOffsetProblem(10, 2), null);
  assert.match(layerOffsetProblem(0, 0)!, /positive/);
});
