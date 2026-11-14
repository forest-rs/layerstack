// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring properties, end to end over the sample scene: clearing,
// blocking and removing an attribute where edits go, keying it at a frame
// and deleting the key, creating attributes and relationships and editing
// a relationship's targets, each one undoable step whose refusal is
// returned; whether an edit shows or a stronger opinion still wins; and
// the pure pieces the inspector decides with (names, value types, target
// lists, finding prims).
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Stage } from "@forest-rs/layerstack";
import { searchPrims } from "../src/model/primSearch.ts";
import { initialValue, movedTarget, propertyActions, propertyNameProblem, targetOpinion, usdaValueText, valueTypeGroups, withoutTarget, withTarget } from "../src/model/propertyOps.ts";
import { formatValue } from "../src/model/values.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

/** The scalar value of the attribute at `path`; `null` when it has none. */
function valueOf(stage: Stage, path: string, time?: number): unknown {
  const value = stage.attribute(path)?.get(time) ?? null;
  return value?.kind === "scalar" ? value.value : null;
}

/** The sample scene, open, with nothing reported. */
async function opened() {
  const h = await harness();
  await h.open(sampleSource());
  const stage = h.store.session!.stage;
  return { h, store: h.store, stage, history: () => h.store.session!.history.undoSteps.length };
}

/**
 * A root over a sublayer and a referenced asset, each read at a layer
 * offset: opinions of one attribute in two layers, time samples whose
 * layer times are not the stage's, and a relationship only the asset
 * authors.
 */
const STACKED: Readonly<Record<string, string>> = {
  "stacked.usda": `#usda 1.0
(
    defaultPrim = "World"
    endTimeCode = 48
    startTimeCode = 0
    subLayers = [
        @./under.usda@ (offset = 10; scale = 2)
    ]
)

def Xform "World"
{
    def Sphere "Ball"
    {
    }

    def "Part" (
        references = @./part.usda@ (offset = 10; scale = 2)
    )
    {
    }

    def Scope "Notes"
    {
        custom double level = 1
        custom double pulse.timeSamples = {
            0: 1,
            10: 2,
        }
    }
}
`,
  "under.usda": `#usda 1.0

over "World"
{
    over "Ball"
    {
        double radius.timeSamples = {
            0: 1,
            10: 2,
        }
    }

    over "Notes"
    {
        custom double level = 2
        double level.timeSamples = {
            1: 5,
            3: 6,
        }
    }

    def Scope "Switch" (
        variants = {
            string mode = "on"
        }
        prepend variantSets = "mode"
    )
    {
        variantSet "mode" = {
            "on" {
                double level.timeSamples = {
                    0: 1,
                    10: 2,
                    20: 3,
                }

            }
        }
    }
}
`,
  "part.usda": `#usda 1.0
(
    defaultPrim = "Part"
)

def Xform "Part"
{
    double height = 1
    double height.timeSamples = {
        0: 1,
        4: 2,
    }
    rel lookAt = </Part/Eye>

    def Scope "Eye"
    {
    }
}
`,
};

/** {@link STACKED}, open, with nothing reported. */
async function stacked() {
  const h = await harness();
  await h.open(sampleSource("stacked.usda", STACKED));
  const session = h.store.session!;
  return { h, store: h.store, session, stage: session.stage, root: session.layerNamed("stacked.usda"), under: session.layerNamed("under.usda") };
}

test("removing a relationship is offered where the target's layer holds its spec, not where another layer authors it", async () => {
  const { store, stage, session, root } = await stacked();
  const path = "/World/Part.lookAt";
  assert.deepEqual(stage.relationship(path)!.targets(), ["/World/Part/Eye"], "from the referenced asset");
  assert.equal(stage.layerSpec(root, path), null);
  // The root has no spec of it: removing it there would be refused, so it is not offered.
  assert.equal(propertyActions(stage, path, session.target, 0).remove, false);
  assert.equal(typeof store.removeProperty(path), "string");
  // Through the reference, where the asset's spec is, it is.
  const part = stage.attribute("/World/Part.height")!.explain()!.opinions[0]!.editTarget!;
  store.setTarget(part);
  assert.equal(propertyActions(stage, path, session.target, 0).remove, true);
  assert.equal(store.removeProperty(path), undefined);
  assert.equal(stage.layerSpec(part.layer, "/Part.lookAt"), null);
  // One the root authors with no targets, and not custom, is still its spec to remove.
  store.setTarget(undefined);
  stage.apply([{ type: "setTargets", path: "/World/Ball.proxyPrim", targets: [] }]);
  assert.equal(stage.relationship("/World/Ball.proxyPrim")!.custom, false);
  assert.equal(propertyActions(stage, "/World/Ball.proxyPrim", session.target, 0).remove, true);
});

test("what a weaker layer holds under a stronger opinion is still its own to clear, key and remove", async () => {
  const { h, store, stage, session, under } = await stacked();
  const path = "/World/Notes.level";
  const spec = () => stage.layerSpec(under, path);
  const held = () => {
    const read = spec();
    return read?.type === "attribute" ? [read.default, read.timeSamples] : null;
  };
  store.setTarget(under);
  assert.equal(valueOf(stage, path, 12), 1, "the root's default wins at every time");
  assert.deepEqual(held(), ["value", [1, 3]]);
  // The sublayer is read at (offset = 10; scale = 2): its sample at 1 is at stage time 12.
  assert.deepEqual(targetOpinion(stage, stage.attribute(path)!, under).samples, [1, 3], "its own samples, in its own time");
  assert.deepEqual(propertyActions(stage, path, session.target, 12), { clear: true, block: true, remove: true, key: false, deleteKey: true, lastKey: false });
  assert.deepEqual(propertyActions(stage, path, session.target, 13), { clear: true, block: true, remove: true, key: true, deleteKey: false, lastKey: false });
  assert.equal(store.deleteKey(path, 12), undefined);
  assert.deepEqual(held(), ["value", [3]]);
  // Clear takes the default and the samples, as UsdAttribute::Clear does: nothing of its value is left behind, hidden.
  assert.equal(store.clearValue(path), undefined);
  assert.deepEqual(held(), [null, null]);
  assert.equal(session.history.nextUndo?.label, `clear ${path}`);
  // Its spec is still there, and still the layer's to remove.
  assert.deepEqual(propertyActions(stage, path, session.target, 12), { clear: false, block: true, remove: true, key: true, deleteKey: false, lastKey: false });
  assert.equal(store.clearValue(path), "under.usda has no value of /World/Notes.level to clear");
  assert.equal(store.removeProperty(path), undefined);
  assert.equal(spec(), null);
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.deepEqual(held(), ["value", [3]], "one step back: the default and the sample together");
  await h.settle(store.undo());
  assert.deepEqual(held(), ["value", [1, 3]]);
});

test("clearing through a variant in a sublayer read at an offset clears every sample the variant holds", async () => {
  const { h, store, stage, session, under } = await stacked();
  const path = "/World/Switch.level";
  // Composed through the sublayer's (offset = 10; scale = 2).
  assert.deepEqual(stage.attribute(path)!.timeSamples(), [10, 30, 50]);
  const inside = stage.prim("/World/Switch")!.variantEditTarget("mode", "on", under);
  // A variant's own target maps time as it is (UsdEditTarget::ForLocalDirectVariant): the stage's times are not its layer's.
  assert.equal(inside.mapTime(30), 30);
  store.setTarget(inside);
  const samples = () => {
    const spec = stage.layerSpec(under, inside.mapPath(path)!);
    return spec?.type === "attribute" ? spec.timeSamples : null;
  };
  assert.deepEqual(samples(), [0, 10, 20]);
  // A key is offered for deleting where the target would delete one.
  assert.equal(propertyActions(stage, path, session.target, 20).deleteKey, true);
  assert.equal(propertyActions(stage, path, session.target, 30).deleteKey, false);
  assert.equal(store.clearValue(path), undefined);
  assert.equal(samples(), null, "none left behind");
  assert.deepEqual(stage.attribute(path)?.timeSamples() ?? [], []);
  await h.settle(store.undo());
  assert.deepEqual(samples(), [0, 10, 20]);
  assert.deepEqual(store.notices, []);
});

test("clearing a value falls back to the schema's, is one undoable step, and clearing nothing is refused", async () => {
  const { h, store, stage, history } = await opened();
  assert.deepEqual(targetOpinion(stage, stage.attribute("/World/Top.size")!, undefined).default, "value");
  assert.equal(store.clearValue("/World/Top.size"), undefined);
  assert.equal(valueOf(stage, "/World/Top.size"), 2, "a Cube's fallback size");
  assert.equal(store.session!.history.nextUndo?.label, "clear /World/Top.size");
  const steps = history();
  const refusal = store.clearValue("/World/Top.size");
  assert.equal(refusal, "scene.usda has no value of /World/Top.size to clear");
  assert.equal(store.notices.at(-1)!.message, refusal);
  assert.equal(history(), steps, "nothing is recorded");
  await h.settle(store.undo());
  assert.equal(valueOf(stage, "/World/Top.size"), 0.6);
});

test("clearing an animated value clears its default and the samples the target authors", async () => {
  const { h, store, stage } = await opened();
  const path = "/World/Top.xformOp:rotateY";
  assert.deepEqual(targetOpinion(stage, stage.attribute(path)!, undefined).samples, [0, 48]);
  assert.equal(store.clearValue(path), undefined);
  assert.deepEqual(stage.attribute(path)!.timeSamples(), []);
  assert.equal(valueOf(stage, path), null);
  await h.settle(store.undo());
  assert.deepEqual(stage.attribute(path)!.timeSamples(), [0, 48]);
  assert.equal(valueOf(stage, path, 24), 180);
  assert.deepEqual(store.notices, []);
});

test("blocking hides weaker opinions, and undoing brings them back", async () => {
  const { h, store, stage } = await opened();
  // The rock's radius comes from the class it inherits, in the sublayer.
  const path = "/World/Rock.radius";
  const before = valueOf(stage, path);
  assert.notEqual(before, 1);
  assert.equal(store.blockValue(path), undefined);
  assert.equal(valueOf(stage, path), 1, "a Sphere's fallback radius");
  assert.equal(targetOpinion(stage, stage.attribute(path)!, undefined).default, "block");
  assert.equal(store.session!.history.nextUndo?.label, "block /World/Rock.radius");
  await h.settle(store.undo());
  assert.equal(valueOf(stage, path), before);
  assert.deepEqual(store.notices, []);
});

test("removing a property removes its spec where edits go, and removing what is not there is refused", async () => {
  const { h, store, stage } = await opened();
  assert.equal(store.removeProperty("/World/Top.size"), undefined);
  assert.equal(stage.attribute("/World/Top.size")!.authored, false);
  assert.equal(valueOf(stage, "/World/Top.size"), 2);
  assert.equal(typeof store.removeProperty("/World/Top.size"), "string");
  await h.settle(store.undo());
  assert.equal(valueOf(stage, "/World/Top.size"), 0.6);
  // A relationship too.
  assert.equal(store.removeProperty("/World/Spot.collection:lightLink:includes"), undefined);
  assert.deepEqual(stage.relationship("/World/Spot.collection:lightLink:includes")?.targets() ?? [], []);
  await h.settle(store.undo());
  assert.deepEqual(stage.relationship("/World/Spot.collection:lightLink:includes")!.targets(), ["/World/Robot"]);
});

test("keying at a frame authors the value read there as a sample, and deleting the key takes it out", async () => {
  const { h, store, stage } = await opened();
  const path = "/World/Top.size";
  assert.equal(store.key(path, 10), undefined);
  assert.deepEqual(stage.attribute(path)!.timeSamples(), [10]);
  assert.equal(valueOf(stage, path, 10), 0.6);
  assert.deepEqual(targetOpinion(stage, stage.attribute(path)!, undefined).samples, [10]);
  assert.equal(store.session!.history.nextUndo?.label, "key /World/Top.size at 10");

  assert.equal(store.deleteKey(path, 10), undefined);
  assert.deepEqual(stage.attribute(path)!.timeSamples(), []);
  assert.equal(store.deleteKey(path, 10), "scene.usda has no key of /World/Top.size at 10");
  await h.settle(store.undo());
  assert.deepEqual(stage.attribute(path)!.timeSamples(), [10]);
  await h.settle(store.undo());
  assert.deepEqual(stage.attribute(path)!.timeSamples(), []);
  // An array keys too, read from the module's memory and released.
  assert.equal(store.key("/World/Top.primvars:displayColor", 5), undefined);
  assert.deepEqual(stage.attribute("/World/Top.primvars:displayColor")!.timeSamples(), [5]);
});

test("creating an attribute authors its spec and starting value, marks it, and refuses a bad name or type", async () => {
  const { h, store, stage } = await opened();
  store.select("/World/Top");
  assert.equal(store.createAttribute("/World/Top.notes:label", "string", "uniform", initialValue("string")!), undefined);
  const created = stage.attribute("/World/Top.notes:label")!;
  assert.deepEqual([created.typeName, created.variability, created.custom, created.authored], ["string", "uniform", true, true]);
  assert.equal(valueOf(stage, "/World/Top.notes:label"), "");
  assert.equal(store.property, "/World/Top.notes:label");
  assert.equal(typeof store.createAttribute("/World/Top.two words", "string", "varying"), "string");
  assert.equal(typeof store.createAttribute("/World/Top.tint", "colour3f", "varying"), "string");
  assert.equal(store.notices.length, 2);
  await h.settle(store.undo());
  assert.equal(stage.attribute("/World/Top.notes:label"), null);
});

test("a relationship's targets are created, reordered and undone as edits where edits go", async () => {
  const { h, store, stage } = await opened();
  const path = "/World/Top.lookAt";
  assert.equal(store.setTargets(path, ["/World/Rock", "/World/Robot"]), undefined);
  const relationship = stage.relationship(path)!;
  assert.equal(relationship.custom, true);
  assert.deepEqual(relationship.targets(), ["/World/Rock", "/World/Robot"]);
  assert.equal(store.property, path);
  store.setTargets(path, movedTarget(relationship.targets(), 1, -1));
  assert.deepEqual(stage.relationship(path)!.targets(), ["/World/Robot", "/World/Rock"]);
  store.setTargets(path, withTarget(withoutTarget(stage.relationship(path)!.targets(), "/World/Rock"), "/World/Top.size"));
  assert.deepEqual(stage.relationship(path)!.targets(), ["/World/Robot", "/World/Top.size"]);
  assert.equal(typeof store.setTargets(path, ["not a path"]), "string");
  await h.settle(store.undo());
  assert.deepEqual(stage.relationship(path)!.targets(), ["/World/Robot", "/World/Rock"]);
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.equal(stage.relationship(path), null);
});

test("an edit a stronger opinion hides says which, and editing there shows", async () => {
  const { h, store, stage } = await opened();
  const library = store.session!.layerNamed("library.usda");
  store.setTarget(library);
  store.setValue("/World/Top.size", { type: "double", value: 5 });
  assert.deepEqual(store.notices, []);
  assert.equal(valueOf(stage, "/World/Top.size"), 0.6, "the root layer's opinion still wins");
  const shadow = store.shadow!;
  assert.equal(shadow.path, "/World/Top.size");
  assert.equal(shadow.layer.identifier, "scene.usda");
  assert.equal(shadow.specPath, "/World/Top.size");
  assert.equal(shadow.composed, true);
  assert.notEqual(shadow.editTarget, null);
  // "Edit there": the winning opinion's target, where the next edit shows.
  store.setTarget(shadow.editTarget!);
  assert.equal(store.shadow, null);
  store.setValue("/World/Top.size", { type: "double", value: 5 });
  assert.equal(valueOf(stage, "/World/Top.size"), 5);
  assert.equal(store.shadow, null);
  // An undo forgets it too.
  store.setTarget(library);
  store.setValue("/World/Top.size", { type: "double", value: 7 });
  assert.notEqual(store.shadow, null);
  await h.settle(store.undo());
  assert.equal(store.shadow, null);
});

test("the note of an edit that does not show goes with the edit: cleared, removed, or another scene opened", async () => {
  const { h, store, stage } = await opened();
  const library = store.session!.layerNamed("library.usda");
  store.setTarget(library);
  const hidden = () => {
    store.setValue("/World/Top.size", { type: "double", value: 5 });
    assert.equal(store.shadow?.path, "/World/Top.size");
  };
  // Clearing the opinion the note is about leaves nothing in the layer for it to speak of.
  hidden();
  assert.equal(store.clearValue("/World/Top.size"), undefined);
  assert.equal(store.shadow, null);
  hidden();
  assert.equal(store.removeProperty("/World/Top.size"), undefined);
  assert.equal(store.shadow, null);
  // Any other edit is the last edit now.
  hidden();
  assert.equal(store.definePrim("/World", "Other", "Scope"), undefined);
  assert.equal(store.shadow, null);
  // Selecting a variant recomposes what the note was read from.
  hidden();
  await h.settle(store.selectVariant("/World/Robot", "finish", "chrome"));
  assert.equal(store.shadow, null);
  // Another scene: the note's layer and its "Edit there" target are the freed stage's.
  hidden();
  await h.open(sampleSource());
  assert.equal(store.shadow, null);
  assert.notEqual(store.session!.stage, stage);
  assert.deepEqual(store.notices, []);
});

test("a block under a stronger layer's samples is said not to show, though it wins at the default time", async () => {
  const { store, stage, under } = await stacked();
  const path = "/World/Notes.pulse";
  store.setTarget(under);
  store.setTime(5);
  assert.equal(store.blockValue(path), undefined);
  const spec = stage.layerSpec(under, path);
  assert.equal(spec?.type === "attribute" ? spec.default : null, "block", "the block is authored");
  assert.equal(valueOf(stage, path, 5), 1.5, "and the root's samples are still read");
  const shadow = store.shadow!;
  assert.notEqual(shadow, null);
  assert.deepEqual([shadow.path, shadow.time, shadow.layer.identifier, shadow.specPath], [path, null, "stacked.usda", path]);
  // "Edit there", and the block shows.
  store.setTarget(shadow.editTarget!);
  assert.equal(store.blockValue(path), undefined);
  assert.equal(valueOf(stage, path, 5), null);
  assert.equal(store.shadow, null);
});

test("an edit through a variant not selected is said to go where the stage does not compose", async () => {
  const { store, stage } = await opened();
  const chrome = stage.prim("/World/Robot")!.variantEditTarget("finish", "chrome");
  store.setTarget(chrome);
  store.setValue("/World/Robot.color", { type: "color3f", value: [0, 0, 1] });
  assert.deepEqual(store.notices, []);
  const shadow = store.shadow!;
  assert.equal(shadow.composed, false);
  assert.equal(shadow.specPath, "/World/Robot{finish=painted}.color");
});

test("every value type offered can be created with its starting value, scalar and array", async () => {
  const { store, stage } = await opened();
  const groups = valueTypeGroups(store.layerstack.valueTypes());
  const offered = groups.flatMap(({ types }) => types.map((type) => type.name));
  assert.ok(offered.includes("color3f") && offered.includes("pathExpression") && offered.includes("frame4d"));
  assert.ok(!offered.some((name) => name.endsWith("[]") || name === "opaque" || name === "group" || name === "Transform"), "scalars, holding a value, not legacy");
  assert.deepEqual(groups.find(({ group }) => group === "Colors")?.types.map((type) => type.name), ["color3h", "color3f", "color3d", "color4h", "color4f", "color4d"]);
  const previews = new Map<string, string>();
  for (const { types } of groups) {
    for (const scalar of types) {
      for (const typeName of [scalar.name, scalar.arrayType!]) {
        const name = typeName.replace("[]", "Array");
        const value = initialValue(typeName);
        assert.notEqual(value, null, typeName);
        stage.apply([{ type: "createAttribute", path: `/World/Top.types:${name}`, typeName, value: value! }]);
        assert.equal(stage.attribute(`/World/Top.types:${name}`)!.typeName, typeName);
        previews.set(`custom ${typeName} types:${name}`, usdaValueText(value));
      }
    }
  }
  // What the dialog says will be authored is what the layer's USDA then reads, for every type.
  const authored = new Map(
    new TextDecoder()
      .decode(stage.exportLayer(stage.root, "usda"))
      .split("\n")
      .filter((line) => line.includes(" types:"))
      .map((line) => line.trim().split(" = ") as [string, string]),
  );
  assert.equal(authored.size, previews.size);
  for (const [declaration, preview] of previews) assert.equal(preview, authored.get(declaration), declaration);
  const identity = stage.attribute("/World/Top.types:matrix4d")!.get();
  assert.deepEqual(identity?.kind === "tuple" ? identity.value : null, [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  // A quaternion starts as the identity rotation: real part 1, which USDA writes first and the bindings read last.
  for (const quaternion of ["quath", "quatf", "quatd"]) {
    assert.equal(authored.get(`custom ${quaternion} types:${quaternion}`), "(1, 0, 0, 0)");
    const rotation = stage.attribute(`/World/Top.types:${quaternion}`)!.get();
    assert.deepEqual(rotation, { kind: "tuple", type: quaternion, value: [0, 0, 0, 1] });
    // And the inspector shows it as USDA writes it.
    assert.equal(formatValue(rotation), "(1, 0, 0, 0)");
  }
});

test("what is offered on a property is what would change something where edits go", async () => {
  const { store, stage } = await opened();
  const at = (path: string, time: number | undefined = 10) => propertyActions(stage, path, store.session!.target, time);
  // Authored in the edit layer: everything but deleting a key that is not there.
  assert.deepEqual(at("/World/Top.size"), { clear: true, block: true, remove: true, key: true, deleteKey: false, lastKey: false });
  // No range: nothing is keyed.
  assert.equal(propertyActions(stage, "/World/Top.size", undefined, undefined).key, false);
  // Animated in the edit layer, at a sample's time: its key is deleted, not added.
  assert.deepEqual(at("/World/Top.xformOp:rotateY", 48), { clear: true, block: true, remove: true, key: false, deleteKey: true, lastKey: false });
  // Only its schema's fallback: nothing to clear, block or remove, but a value to key.
  assert.deepEqual(at("/World/Top.purpose"), { clear: false, block: false, remove: false, key: false, deleteKey: false, lastKey: false });
  assert.deepEqual(at("/World/Top.visibility"), { clear: false, block: false, remove: false, key: true, deleteKey: false, lastKey: false });
  // A uniform one is not keyed.
  assert.equal(at("/World/Top.doubleSided").key, false);
  // From the sublayer, through an inherit: blocked or keyed here, not cleared or removed here.
  assert.deepEqual(at("/World/Rock.radius"), { clear: false, block: true, remove: false, key: true, deleteKey: false, lastKey: false });
  // With the sublayer as the target, the class's opinion is still not its own spec: the rock's is /World/Rock.radius there.
  store.setTarget(store.session!.layerNamed("library.usda"));
  assert.equal(at("/World/Rock.radius").clear, false);
  store.setTarget(undefined);
  // Blocked here already: not blocked again.
  store.blockValue("/World/Rock.radius");
  assert.deepEqual(at("/World/Rock.radius"), { clear: true, block: false, remove: true, key: true, deleteKey: false, lastKey: false });
  // Relationships: a custom one or one with targets can be removed; a schema's empty one cannot.
  assert.deepEqual(at("/World/Spot.collection:lightLink:includes"), { clear: false, block: false, remove: true, key: false, deleteKey: false, lastKey: false });
  assert.equal(at("/World/Top.proxyPrim").remove, false);
  // An instance proxy's properties are its prototype's.
  const proxy = stage.children("/World/Beacons/North")[0]!;
  assert.equal(stage.prim(proxy)!.instanceProxy, true);
  assert.deepEqual(Object.values(at(`${proxy}.visibility`)).some(Boolean), false);
  // Not a property path.
  assert.deepEqual(Object.values(at("/World/Top")).some(Boolean), false);
});

test("Remove is offered exactly when the layer edits go to holds the property's spec, the empty one a deleted last key leaves included", async () => {
  const { store, stage } = await opened();
  const at = (path: string, time: number | undefined = 10) => propertyActions(stage, path, store.session!.target, time);
  // The lamp's aim is the sublayer's: with edits in the root, the root has no spec of it to remove.
  assert.equal(stage.relationship("/World/Lamp.aim")!.targets().length, 1);
  assert.equal(at("/World/Lamp.aim").remove, false);
  store.setTarget(store.session!.layerNamed("library.usda"));
  assert.equal(at("/World/Lamp.aim").remove, true, "the sublayer holds it");
  store.setTarget(undefined);

  // Keyed in the root, where it had no spec: that key is all the spec holds.
  assert.equal(store.key("/World/Top.visibility", 10), undefined);
  assert.deepEqual(at("/World/Top.visibility"), { clear: true, block: true, remove: true, key: false, deleteKey: true, lastKey: true });
  // A key of an attribute the root also gives a default is not its last opinion there.
  assert.equal(store.key("/World/Top.size", 10), undefined);
  assert.equal(at("/World/Top.size").lastKey, false);
  // Deleting the last key leaves the spec, empty: nothing to clear, and Remove takes it.
  assert.equal(store.deleteKey("/World/Top.visibility", 10), undefined);
  const spec = stage.layerSpec(stage.root, "/World/Top.visibility");
  assert.ok(spec !== null && spec.type === "attribute");
  assert.deepEqual([spec.default, spec.timeSamples], [null, null], "no default, no samples");
  assert.deepEqual(at("/World/Top.visibility"), { clear: false, block: false, remove: true, key: true, deleteKey: false, lastKey: false });
  assert.equal(store.removeProperty("/World/Top.visibility"), undefined);
  assert.equal(stage.layerSpec(stage.root, "/World/Top.visibility"), null);
  assert.equal(at("/World/Top.visibility").remove, false);
});

test("Remove is offered for the spec the layer edits go to holds, though the stage composes a stronger layer's property of the other kind", async () => {
  const h = await harness();
  await h.open(
    sampleSource("kinds.usda", {
      "kinds.usda": `#usda 1.0
(
    subLayers = [
        @./kinds_weak.usda@
    ]
)

def Xform "P"
{
    custom rel foo = </T>
    custom double bar = 1
}

def Xform "T"
{
}
`,
      "kinds_weak.usda": `#usda 1.0

over "P"
{
    custom double foo = 1
    custom rel bar = </T>
}
`,
    }),
  );
  const store = h.store;
  const stage = store.session!.stage;
  const weak = store.session!.layerNamed("kinds_weak.usda");
  store.setTarget(weak);
  // The stage composes the root's relationship; the sublayer, where edits go, holds an attribute of that name.
  assert.ok(stage.relationship("/P.foo") !== null && stage.attribute("/P.foo") === null);
  assert.equal(stage.layerSpec(weak, "/P.foo")?.type, "attribute");
  assert.equal(propertyActions(stage, "/P.foo", weak, undefined).remove, true);
  assert.equal(store.removeProperty("/P.foo"), undefined);
  assert.equal(stage.layerSpec(weak, "/P.foo"), null);
  assert.equal(stage.layerSpec(stage.root, "/P.foo")?.type, "relationship", "the root's stays");
  assert.equal(propertyActions(stage, "/P.foo", weak, undefined).remove, false);
  // And the reverse.
  assert.ok(stage.attribute("/P.bar") !== null);
  assert.equal(stage.layerSpec(weak, "/P.bar")?.type, "relationship");
  assert.equal(propertyActions(stage, "/P.bar", weak, undefined).remove, true);
  assert.equal(store.removeProperty("/P.bar"), undefined);
  assert.equal(stage.layerSpec(weak, "/P.bar"), null);
  assert.equal(stage.layerSpec(stage.root, "/P.bar")?.type, "attribute");
});

test("a property name is identifiers joined by colons, and not one the prim has", () => {
  const taken = new Set(["size", "primvars:displayColor"]);
  assert.equal(propertyNameProblem("tint", taken), null);
  assert.equal(propertyNameProblem("primvars:tint", taken), null);
  assert.equal(propertyNameProblem("_a:b2:c", taken), null);
  assert.notEqual(propertyNameProblem("", taken), null);
  assert.notEqual(propertyNameProblem("2d", taken), null);
  assert.notEqual(propertyNameProblem("two words", taken), null);
  assert.notEqual(propertyNameProblem("primvars:", taken), null);
  assert.notEqual(propertyNameProblem("a::b", taken), null);
  assert.equal(propertyNameProblem("size", taken), "The prim already has a property size.");
});

test("a target list is edited one target at a time", () => {
  assert.deepEqual(withTarget(["/A"], "/B"), ["/A", "/B"]);
  assert.deepEqual(withTarget(["/A"], "/A"), ["/A"]);
  assert.deepEqual(withoutTarget(["/A", "/B"], "/A"), ["/B"]);
  assert.deepEqual(movedTarget(["/A", "/B", "/C"], 2, -1), ["/A", "/C", "/B"]);
  assert.deepEqual(movedTarget(["/A", "/B"], 0, -1), ["/A", "/B"]);
  assert.deepEqual(movedTarget(["/A", "/B"], 1, 1), ["/A", "/B"]);
});

test("prims are found by name and by schema", async () => {
  const { stage } = await opened();
  assert.deepEqual(searchPrims(stage, { text: "", limit: 10, type: "Material" }), ["/World/Robot/Looks/Paint", "/World/Looks/Checker", "/World/Looks/Tiles"]);
  assert.deepEqual(searchPrims(stage, { text: "che", limit: 10, type: "Material" }), ["/World/Looks/Checker"]);
  assert.ok(searchPrims(stage, { text: "stone", limit: 10 }).includes("/World/Stones/Stone"));
  assert.equal(searchPrims(stage, { text: "", limit: 3 }).length, 3);
  assert.ok(searchPrims(stage, { text: "", limit: 50, type: "MaterialBindingAPI" }).includes("/World/Sign"));
});
