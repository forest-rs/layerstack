// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring prims, end to end over the sample scene: binding and
// unbinding a material, kind, type, specifier, clearing a layer's
// opinion of active and instanceable, applying and removing API schemas,
// duplicating, reparenting and reordering, visibility and documentation,
// each one undoable step whose refusal is returned and kept as a notice;
// and the pure pieces the inspector and the tree decide with.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { SchemaInfo } from "@forest-rs/layerstack";
import {
  applicableSchemas,
  appliedSchemaRows,
  authoredApiSchemas,
  freeSiblingName,
  hiddenBy,
  instanceNameProblem,
  kindChoices,
  moveProblem,
  noSpecToCopy,
  opinionSites,
  primNameProblem,
  rowVisibilities,
  siblingOrder,
  specStagePath,
  treeDrop,
  typeGroups,
} from "../src/model/primAuthoring.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

/** The sample scene, open, with nothing reported. */
async function opened() {
  const h = await harness();
  await h.open(sampleSource());
  const stage = h.store.session!.stage;
  return { h, store: h.store, stage, steps: () => h.store.session!.history.undoSteps.length };
}

const schema = (name: string, kind: SchemaInfo["kind"], extra: Partial<SchemaInfo> = {}): SchemaInfo => ({
  name,
  kind,
  bases: [],
  canOnlyApplyTo: [],
  allowedInstanceNames: [],
  ...extra,
});

test("kinds: the model hierarchy's, and a prim's own kind when it is another", () => {
  assert.deepEqual(
    kindChoices(null).map((each) => each.kind),
    ["component", "assembly", "group", "subcomponent", "model"],
  );
  assert.equal(kindChoices("component").length, 5);
  assert.equal(kindChoices("chair").at(-1)?.kind, "chair");
});

test("types are grouped by what they are", () => {
  const groups = typeGroups([
    schema("Xform", "concreteTyped", { bases: ["Xformable", "Imageable", "Typed"] }),
    schema("Sphere", "concreteTyped", { bases: ["Gprim", "Boundable", "Xformable"] }),
    schema("SphereLight", "concreteTyped", { bases: ["BoundableLightBase"] }),
    schema("Material", "concreteTyped", { bases: ["NodeGraph"] }),
    schema("Gprim", "abstractTyped"),
    schema("ShapingAPI", "singleApplyApi"),
  ]);
  assert.deepEqual(
    groups.map(([group, schemas]) => [group, schemas.map((each) => each.name)]),
    [
      ["Transforms and groups", ["Xform"]],
      ["Geometry", ["Sphere"]],
      ["Lights", ["SphereLight"]],
      ["Shading", ["Material"]],
    ],
  );
});

test("the API schemas that can apply: what they apply to, and single-apply ones not had", () => {
  const schemas = [
    schema("ShapingAPI", "singleApplyApi"),
    schema("MaterialBindingAPI", "singleApplyApi"),
    schema("CollectionAPI", "multipleApplyApi"),
    schema("MaterialXConfigAPI", "singleApplyApi", { canOnlyApplyTo: ["Material"] }),
    schema("Sphere", "concreteTyped"),
  ];
  const sphere = { isA: (name: string) => name === "Sphere", hasAPI: () => false };
  const offered = applicableSchemas(schemas, sphere, ["MaterialBindingAPI", "CollectionAPI:parts"]).flatMap(([, list]) => list.map((each) => each.name));
  assert.deepEqual(offered.sort(), ["CollectionAPI", "ShapingAPI"]);
  const material = { isA: (name: string) => name === "Material", hasAPI: () => false };
  assert.ok(applicableSchemas(schemas, material, []).some(([, list]) => list.some((each) => each.name === "MaterialXConfigAPI")));
});

test("an instance name is an identifier, allowed, and not applied already", () => {
  const collection = schema("CollectionAPI", "multipleApplyApi");
  assert.match(instanceNameProblem(collection, "", [])!, /name this one/);
  assert.match(instanceNameProblem(collection, "2parts", [])!, /letter or _/);
  assert.match(instanceNameProblem(collection, "parts", ["CollectionAPI:parts"])!, /already/);
  assert.equal(instanceNameProblem(collection, "parts", ["CollectionAPI:other"]), null);
  const limited = schema("PhysicsDriveAPI", "multipleApplyApi", { allowedInstanceNames: ["linear", "angular"] });
  assert.match(instanceNameProblem(limited, "spin", [])!, /only the instance names linear, angular/);
  assert.equal(instanceNameProblem(limited, "linear", []), null);
});

test("applied schemas: split into schema and instance, authored ones marked", () => {
  assert.deepEqual(appliedSchemaRows(["LightAPI", "CollectionAPI:lightLink", "ShapingAPI"], ["ShapingAPI"]), [
    { applied: "LightAPI", schema: "LightAPI", instance: null, authored: false },
    { applied: "CollectionAPI:lightLink", schema: "CollectionAPI", instance: "lightLink", authored: false },
    { applied: "ShapingAPI", schema: "ShapingAPI", instance: null, authored: true },
  ]);
});

test("names, moves and orders", () => {
  assert.equal(freeSiblingName("Rock", new Set(["Rock"])), "Rock1");
  assert.equal(freeSiblingName("Rock", new Set(["Rock", "Rock1"])), "Rock2");
  assert.equal(freeSiblingName("Rock7", new Set(["Rock7"])), "Rock8");
  assert.equal(primNameProblem("Rock", "/World", new Set()), null);
  assert.match(primNameProblem("Rock", "/World", new Set(["Rock"]))!, /already has a child Rock/);
  assert.match(primNameProblem("a b", "/World", new Set())!, /letter or _/);
  assert.equal(moveProblem("/World/Rock", "/World/Beacons", new Set(["North"])), null);
  assert.match(moveProblem("/World/Rock", "/World", new Set(["Rock"]))!, /child of \/World already/);
  assert.match(moveProblem("/World", "/World/Rock", new Set())!, /beneath itself/);
  assert.match(moveProblem("/World/Rock", "/Other", new Set(["Rock"]))!, /already has a child named Rock/);
  assert.deepEqual(siblingOrder(["A", "B", "C"], "B", -1), ["B", "A", "C"]);
  assert.deepEqual(siblingOrder(["A", "B", "C"], "B", 1), ["A", "C", "B"]);
  assert.equal(siblingOrder(["A", "B", "C"], "A", -1), null);
  assert.equal(siblingOrder(["A", "B", "C"], "C", 1), null);
  assert.equal(specStagePath("/World/Robot{finish=chrome}.mirrors"), "/World/Robot.mirrors");
});

test("a drop on a row moves beneath it; between its own siblings, reorders", () => {
  const siblings = ["North", "South", "Rock"];
  assert.deepEqual(treeDrop("/World/Rock", "/World/Beacons", "on", []), { kind: "move", parent: "/World/Beacons" });
  assert.equal(treeDrop("/World/Rock", "/World", "on", []), null, "its own parent");
  assert.equal(treeDrop("/World", "/World/Rock", "on", []), null, "its own subtree");
  assert.equal(treeDrop("/World/Rock", "/World/Rock", "before", siblings), null, "itself");
  assert.deepEqual(treeDrop("/World/B/Rock", "/World/B/North", "before", siblings), { kind: "reorder", parent: "/World/B", order: ["Rock", "North", "South"] });
  assert.deepEqual(treeDrop("/World/B/North", "/World/B/Rock", "after", siblings), { kind: "reorder", parent: "/World/B", order: ["South", "Rock", "North"] });
  assert.equal(treeDrop("/World/B/North", "/World/B/South", "before", siblings), null, "where it is already");
  assert.equal(treeDrop("/World/Rock", "/World/B/North", "before", siblings), null, "another parent's children");
});

test("rows' visibility: hidden by itself, or by an ancestor", () => {
  const paths = ["/World", "/World/A", "/World/A/B", "/World/C", "/Looks"];
  const rows = rowVisibilities(paths, ["inherited", "invisible", "invisible", "inherited", null]);
  assert.deepEqual([...rows], [
    ["/World", "visible"],
    ["/World/A", "invisible"],
    ["/World/A/B", "hidden"],
    ["/World/C", "visible"],
  ]);
  assert.equal(hiddenBy("/World/A/B", rows), "/World/A");
  assert.equal(hiddenBy("/World/C", rows), null);
});

test("binding a material and unbinding it, each undone; a missing material refused", async () => {
  const { h, store, stage, steps } = await opened();
  assert.equal(store.bindMaterial("/World/Rock", "/World/Looks/Checker"), undefined);
  assert.equal(stage.prim("/World/Rock")!.boundMaterial()?.material, "/World/Looks/Checker");
  assert.ok(stage.prim("/World/Rock")!.hasAPI("MaterialBindingAPI"));
  assert.equal(store.unbindMaterial("/World/Rock"), undefined);
  assert.equal(stage.prim("/World/Rock")!.boundMaterial(), null);
  assert.equal(steps(), 2);
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.boundMaterial()?.material, "/World/Looks/Checker");
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.boundMaterial(), null);
  assert.equal(stage.prim("/World/Rock")!.hasAPI("MaterialBindingAPI"), false);

  const refusal = store.bindMaterial("/World/Rock", "/World/Looks/Nothing");
  assert.ok(refusal !== undefined);
  assert.equal(store.notices.at(-1)?.message, refusal);
  assert.equal(steps(), 0);
});

test("kind, type and specifier, each undone; clearing what the layer does not author is refused", async () => {
  const { h, store, stage, steps } = await opened();
  store.setKind("/World/Rock", "component");
  assert.deepEqual(stage.prim("/World/Rock")!.metadata("kind"), { kind: "scalar", type: "token", value: "component" });
  store.setKind("/World/Rock", null);
  assert.equal(stage.prim("/World/Rock")!.metadata("kind"), null);
  const refusal = store.setKind("/World/Rock", null);
  assert.match(refusal!, /scene\.usda authors no kind of \/World\/Rock/);

  store.setTypeName("/World/Rock", "Cube");
  assert.equal(stage.prim("/World/Rock")!.typeName, "Cube");
  // The specifier shown is composed: the rock's root spec as an over, the
  // class it inherits gives `class`; the top, defined only in the root, is
  // then an over alone.
  store.setSpecifier("/World/Rock", "over");
  assert.equal(stage.prim("/World/Rock")!.specifier, "class");
  store.setSpecifier("/World/Top", "over");
  assert.equal(stage.prim("/World/Top")!.specifier, "over");
  assert.equal(steps(), 5);
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Top")!.specifier, "def");
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.specifier, "def");
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.typeName, "Sphere");
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock")!.metadata("kind"), null);
});

test("a child added as an over or a class", async () => {
  const { store, stage } = await opened();
  store.definePrim("/World", "Base", "Xform", "class");
  assert.equal(stage.prim("/World/Base")?.specifier, "class");
  store.definePrim("/World", "Later", null, "over");
  assert.equal(stage.prim("/World/Later")?.specifier, "over");
  assert.equal(store.selection, "/World/Later");
});

test("clearing the edit layer's opinion of active and instanceable", async () => {
  const { h, store, stage, steps } = await opened();
  store.apply("deactivate", [{ type: "setActive", path: "/World/Rock", active: false }]);
  assert.equal(stage.prim("/World/Rock")!.active, false);
  assert.equal(store.clearPrimOpinion("/World/Rock", "active"), undefined);
  assert.equal(stage.prim("/World/Rock")!.active, true);
  // The beacons' instanceable is authored in the root layer.
  assert.equal(store.clearPrimOpinion("/World/Beacons/North", "instanceable"), undefined);
  assert.equal(stage.prim("/World/Beacons/North")!.instance, false);
  const refusal = store.clearPrimOpinion("/World/Rock", "instanceable");
  assert.match(refusal!, /has no opinion of instanceable on \/World\/Rock/);
  assert.equal(steps(), 3);
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Beacons/North")!.instance, true);
});

test("applying an API schema shows what it brought; removing it; a multiple-apply schema without a name refused", async () => {
  const { h, store, stage, steps } = await opened();
  assert.equal(store.applySchema("/World/Rock", "ShapingAPI"), undefined);
  assert.ok(stage.prim("/World/Rock")!.hasAPI("ShapingAPI"));
  assert.equal(store.revealed?.prim, "/World/Rock");
  assert.ok(store.revealed!.names.some((name) => name.startsWith("inputs:shaping:")));
  assert.deepEqual(authoredApiSchemas(stage.prim("/World/Rock")!), ["ShapingAPI"]);

  assert.equal(store.applySchema("/World/Rock", "CollectionAPI", "parts"), undefined);
  assert.ok(stage.prim("/World/Rock")!.hasAPI("CollectionAPI", "parts"));
  assert.equal(store.removeSchema("/World/Rock", "CollectionAPI", "parts"), undefined);
  assert.equal(stage.prim("/World/Rock")!.hasAPI("CollectionAPI", "parts"), false);

  const refusal = store.applySchema("/World/Rock", "CollectionAPI");
  assert.match(refusal!, /instance name/);
  assert.equal(store.notices.at(-1)?.code, "schema.cannot_apply");
  assert.equal(steps(), 3);
  await h.settle(store.undo());
  assert.ok(stage.prim("/World/Rock")!.hasAPI("CollectionAPI", "parts"));
});

test("duplicating a prim beside itself, undone; one whose opinions are in another layer says where", async () => {
  const { h, store, stage, steps } = await opened();
  assert.equal(store.duplicate("/World/Rock"), undefined);
  assert.equal(store.selection, "/World/Rock1");
  assert.equal(stage.prim("/World/Rock1")?.typeName, "Sphere");
  store.duplicate("/World/Rock");
  assert.notEqual(stage.prim("/World/Rock2"), null);
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock1"), null);

  // The lamp is the sublayer's: the root layer holds no spec of it.
  const refusal = store.duplicate("/World/Lamp");
  assert.match(refusal!, /scene\.usda has no spec of \/World\/Lamp to copy: its opinions are in library\.usda/);
  assert.equal(store.notices.at(-1)?.code, "edit.rejected.no_such_spec");
  assert.equal(steps(), 0);
  const sites = opinionSites(stage, "/World/Lamp");
  assert.deepEqual(
    sites.map((site) => site.layer.identifier),
    ["library.usda"],
  );
  // Made where edits go, it copies.
  store.setTarget(sites[0]!.editTarget!.layer);
  assert.equal(store.duplicate("/World/Lamp"), undefined);
  assert.notEqual(stage.prim("/World/Lamp1"), null);
  assert.match(noSpecToCopy("/X", "a.usda", []), /^a\.usda has no spec of \/X to copy\. /);
});

test("moving a prim under another, reordering siblings, each undone; unrepaired paths are a live notice", async () => {
  const { h, store, stage, steps } = await opened();
  assert.equal(store.moveTo("/World/Rock", "/World/Beacons"), undefined);
  assert.equal(store.selection, null);
  assert.notEqual(stage.prim("/World/Beacons/Rock"), null);
  const refusal = store.moveTo("/World/Beacons/Rock", "/World/Beacons");
  assert.match(refusal!, /child of \/World\/Beacons already/);

  const before = stage.children("/World/Beacons");
  assert.deepEqual(before, ["/World/Beacons/North", "/World/Beacons/South", "/World/Beacons/Rock"]);
  store.reorderChild("/World/Beacons/Rock", -1);
  assert.deepEqual(stage.children("/World/Beacons"), ["/World/Beacons/North", "/World/Beacons/Rock", "/World/Beacons/South"]);
  assert.equal(steps(), 2);
  await h.settle(store.undo());
  assert.deepEqual(stage.children("/World/Beacons"), before);
  await h.settle(store.undo());
  assert.notEqual(stage.prim("/World/Rock"), null);

  // The chrome variant's relationship names the robot, and no prim selects it: the move leaves it.
  store.moveTo("/World/Robot", "/World/Beacons");
  const notice = store.notices.at(-1)!;
  assert.match(notice.message, /1 path still names what was moved/);
  assert.deepEqual(
    notice.unrepaired.map((each) => [each.layer.identifier, specStagePath(each.spec), each.field, each.path]),
    [["scene.usda", "/World/Beacons/Robot.mirrors", "targets", "/World/Robot"]],
  );
});

test("visibility and documentation, each undone", async () => {
  const { h, store, stage } = await opened();
  store.setVisibility("/World/Rock", false);
  assert.deepEqual(stage.visibilities(["/World/Rock"]), ["invisible"]);
  store.setVisibility("/World/Rock", true);
  assert.deepEqual(stage.visibilities(["/World/Rock"]), ["inherited"]);
  store.setDocumentation("/World/Rock", "A rock the class shapes.");
  assert.deepEqual(stage.prim("/World/Rock")!.metadata("documentation"), { kind: "scalar", type: "string", value: "A rock the class shapes." });
  store.setDocumentation("/World/Rock", "");
  assert.equal(stage.prim("/World/Rock")!.metadata("documentation"), null);
  assert.match(store.setDocumentation("/World/Rock", "")!, /authors no documentation/);
  await h.settle(store.undo());
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.deepEqual(stage.visibilities(["/World/Rock"]), ["invisible"]);
});
