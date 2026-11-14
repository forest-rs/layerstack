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
import { initialValue, movedTarget, propertyActions, propertyNameProblem, targetOpinion, valueTypeGroups, withoutTarget, withTarget } from "../src/model/propertyOps.ts";
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
  for (const { types } of groups) {
    for (const scalar of types) {
      for (const typeName of [scalar.name, scalar.arrayType!]) {
        const name = typeName.replace("[]", "Array");
        const value = initialValue(typeName);
        assert.notEqual(value, null, typeName);
        stage.apply([{ type: "createAttribute", path: `/World/Top.types:${name}`, typeName, value: value! }]);
        assert.equal(stage.attribute(`/World/Top.types:${name}`)!.typeName, typeName);
      }
    }
  }
  const identity = stage.attribute("/World/Top.types:matrix4d")!.get();
  assert.deepEqual(identity?.kind === "tuple" ? identity.value : null, [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  // A quaternion's real part first, as USDA writes it: the identity rotation.
  const rotation = stage.attribute("/World/Top.types:quatd")!.get();
  assert.deepEqual(rotation, { kind: "tuple", type: "quatd", value: [1, 0, 0, 0] });
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
