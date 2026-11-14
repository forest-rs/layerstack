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
  holdsField,
  hiddenBy,
  instanceNameProblem,
  kindChoices,
  moveProblem,
  noSpecToCopy,
  opinionSites,
  primNameProblem,
  rowVisibilities,
  schemaRemoval,
  schemaSites,
  siblingOrder,
  specStagePath,
  treeDrop,
  typeForFilter,
  typeGroups,
  typeMatches,
} from "../src/model/primAuthoring.ts";
import { primActions, PROXY_REFUSAL, refusals } from "../src/model/primActions.ts";
import { targetSite } from "../src/model/propertyOps.ts";
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
  assert.equal(kindChoices("prop").at(-1)?.kind, "prop");
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

test("the type chosen follows the filter to a type the list shows", () => {
  const groups: [string, { name: string }[]][] = [
    ["Transforms and groups", [{ name: "Xform" }, { name: "Scope" }]],
    ["Geometry", [{ name: "Cone" }, { name: "Sphere" }]],
    ["Lights", [{ name: "SphereLight" }]],
  ];
  // Still listed: kept, whatever the case typed.
  assert.equal(typeForFilter(groups, "", "Xform"), "Xform");
  assert.equal(typeForFilter(groups, "xf", "Xform"), "Xform");
  // No longer listed: the first that is, in the list's order.
  assert.equal(typeForFilter(groups, "Cone", "Xform"), "Cone");
  assert.equal(typeForFilter(groups, "sphere", "Xform"), "Sphere");
  // "No type" is always listed, and a filter that matches nothing changes nothing.
  assert.equal(typeForFilter(groups, "Cone", null), null);
  assert.equal(typeForFilter(groups, "zzz", "Xform"), "Xform");
  assert.equal(typeMatches("SphereLight", "light"), true);
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
  // A namespaced instance name is one OpenUSD takes (`CollectionAPI:render:parts`); an empty component is not.
  assert.equal(instanceNameProblem(collection, "render:parts", []), null);
  assert.match(instanceNameProblem(collection, "render:", [])!, /joined by colons/);
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
  // A suffix no number counts on from: the name after it, not the same one for ever.
  assert.equal(freeSiblingName("Rock9007199254740992", new Set(["Rock9007199254740992"])), "Rock9007199254740993");
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

test("a prim under a group that is not imageable is hidden by the ancestor above the group, not by itself", () => {
  // A typeless group has no computed visibility, and its children inherit through it (`UsdGeomImageable::ComputeVisibility`).
  const paths = ["/World", "/World/Hid", "/World/Hid/Group", "/World/Hid/Group/Deep", "/World/Hid/Group/Deep/Leaf"];
  const rows = rowVisibilities(paths, ["inherited", "invisible", null, "invisible", "invisible"]);
  assert.equal(rows.get("/World/Hid/Group/Deep"), "hidden", "its eye selects what hides it, and does not offer to make it visible");
  assert.equal(rows.get("/World/Hid/Group/Deep/Leaf"), "hidden");
  assert.equal(hiddenBy("/World/Hid/Group/Deep", rows), "/World/Hid");
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

test("Clear is offered for a prim field only when the spec edits go to holds it", async () => {
  const { store, stage } = await opened();
  const library = store.session!.layerNamed("library.usda");
  // The sublayer deactivates the lamp, kinds it and documents it; the root, where edits go, holds none of these.
  store.setTarget(library);
  store.apply("deactivate", [{ type: "setActive", path: "/World/Lamp", active: false }]);
  assert.equal(store.setKind("/World/Lamp", "component"), undefined);
  assert.equal(store.setDocumentation("/World/Lamp", "A lamp"), undefined);
  for (const field of ["active", "kind", "documentation", "typeName"] as const) assert.equal(holdsField(store.targetSpec("/World/Lamp"), field), true, field);
  store.setTarget(undefined);
  assert.equal(stage.prim("/World/Lamp")!.active, false, "composed: what the sublayer says");
  const own = store.targetSpec("/World/Lamp");
  for (const field of ["active", "instanceable", "kind", "documentation", "typeName"] as const) assert.equal(holdsField(own, field), false, field);
  // The refusal stays as a backstop.
  assert.match(store.clearPrimOpinion("/World/Lamp", "active")!, /has no opinion of active/);
  // The root's own opinion is offered, and a cleared one is not again.
  store.apply("deactivate", [{ type: "setActive", path: "/World/Lamp", active: true }]);
  assert.equal(holdsField(store.targetSpec("/World/Lamp"), "active"), true);
  assert.equal(store.clearPrimOpinion("/World/Lamp", "active"), undefined);
  assert.equal(holdsField(store.targetSpec("/World/Lamp"), "active"), false);
  // Inside a variant, the variant's spec is where edits go.
  store.editInsideVariant("/World/Robot", "finish", "painted");
  assert.equal(store.targetSpec("/World/Robot")?.path, "/World/Robot{finish=painted}");
  assert.equal(holdsField(store.targetSpec("/World/Robot"), "kind"), false);
});

test("an applied schema is removed where edits go apply it, deleted there when only a weaker layer does, and named when a stronger one does", async () => {
  const { store, stage } = await opened();
  const library = store.session!.layerNamed("library.usda");
  const removals = (path: string) => {
    const prim = stage.prim(path)!;
    const session = store.session!;
    const { sites, target } = schemaSites(prim.compositionArcs(), targetSite(stage, session.target, path), (layer, at) => store.authoredSpec(layer, at)?.spec ?? null);
    return Object.fromEntries(
      appliedSchemaRows(prim.appliedSchemas(), authoredApiSchemas(prim)).map((row) => {
        const removal = schemaRemoval(row, sites, target);
        return [row.applied, "layer" in removal ? `${removal.kind} ${removal.layer?.identifier}` : removal.kind];
      }),
    );
  };
  // The sublayer applies ShadowAPI to its lamp; the root applies ShapingAPI.
  store.setTarget(library);
  assert.equal(store.applySchema("/World/Lamp", "ShadowAPI"), undefined);
  store.setTarget(undefined);
  assert.equal(store.applySchema("/World/Lamp", "ShapingAPI"), undefined);
  const lamp = removals("/World/Lamp");
  assert.equal(lamp.ShapingAPI, "remove");
  assert.equal(lamp.ShadowAPI, "delete library.usda", "only the weaker sublayer applies it: a delete here");
  assert.equal(lamp.LightAPI, "builtIn", "its type applies it");
  // With edits in the sublayer, the root's application is stronger: no delete there takes it.
  store.setTarget(library);
  assert.equal(removals("/World/Lamp").ShapingAPI, "stronger scene.usda");
  assert.equal(removals("/World/Lamp").ShadowAPI, "remove");
  store.setTarget(undefined);
  // Deleting it in the root removes what the sublayer applies, which keeps its own.
  assert.equal(store.removeSchema("/World/Lamp", "ShadowAPI"), undefined);
  assert.equal(stage.prim("/World/Lamp")!.hasAPI("ShadowAPI"), false);
  const sub = stage.layerSpec(library, "/World/Lamp");
  assert.ok(sub !== null && sub.type === "prim");
  assert.deepEqual(sub.apiSchemas?.prepended, ["ShadowAPI"]);
});

test("a schema a variant not selected applies is removed there when edits go inside it, though the prim's own application is what composes", async () => {
  const { store, stage } = await opened();
  const removal = () => {
    const prim = stage.prim("/World/Robot")!;
    const read = schemaSites(prim.compositionArcs(), targetSite(stage, store.session!.target, "/World/Robot"), (layer, at) => store.authoredSpec(layer, at)?.spec ?? null);
    const row = appliedSchemaRows(prim.appliedSchemas(), authoredApiSchemas(prim)).find((each) => each.applied === "MotionAPI")!;
    return { read, removal: schemaRemoval(row, read.sites, read.target, read.own) };
  };
  const chrome = () => stage.layerSpec(stage.root, "/World/Robot{finish=chrome}");
  // The root applies it on the robot, and so does its chrome variant, which is not selected.
  assert.equal(store.applySchema("/World/Robot", "MotionAPI"), undefined);
  store.editInsideVariant("/World/Robot", "finish", "chrome");
  assert.equal(removal().read.target, -1, "the prim composes nothing of chrome");
  assert.deepEqual(removal().removal, { kind: "stronger", layer: stage.root }, "chrome applies none yet: the root's is not its to take");
  assert.equal(store.applySchema("/World/Robot", "MotionAPI"), undefined);
  const before = chrome();
  assert.ok(before !== null && before.type === "variant");
  assert.deepEqual(before.apiSchemas?.prepended, ["MotionAPI"]);
  assert.deepEqual(removal().removal, { kind: "remove" }, "its own application is its to remove");
  assert.equal(store.removeSchema("/World/Robot", "MotionAPI"), undefined);
  const after = chrome();
  assert.ok(after !== null && after.type === "variant");
  assert.deepEqual(after.apiSchemas?.prepended, []);
  assert.equal(stage.prim("/World/Robot")!.hasAPI("MotionAPI"), true, "the root's application stays");
  assert.equal(removal().removal.kind, "stronger");
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

  assert.equal(store.applySchema("/World/Rock", "CollectionAPI", "render:parts"), undefined);
  assert.ok(stage.prim("/World/Rock")!.hasAPI("CollectionAPI", "render:parts"));
  assert.ok(stage.prim("/World/Rock")!.attributeNames().includes("collection:render:parts:expansionRule"));
  await h.settle(store.undo());

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

test("two duplicates asked for while an action fetches make two copies, each of the prim as it was when its turn came", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const stage = store.session!.stage;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  store.duplicate("/World/Rock");
  store.setValue("/World/Rock.radius", { type: "double", value: 9 });
  store.duplicate("/World/Rock");
  assert.equal(store.queued, 3);
  release();
  await h.settle(variant);
  await h.until(() => store.running === null && store.queued === 0, "the edits behind the variant");
  assert.deepEqual(store.notices, []);
  // The second copy took the next name: it did not replace the first, made before the radius changed.
  const radius = (path: string) => (stage.layerSpec(stage.root, `${path}.radius`) === null ? null : stage.attribute(`${path}.radius`)!.get());
  assert.equal(radius("/World/Rock1"), null);
  assert.deepEqual(radius("/World/Rock2"), { kind: "scalar", type: "double", value: 9 });
  assert.equal(store.selection, "/World/Rock2");
  await h.settle(store.undo());
  assert.equal(stage.prim("/World/Rock2"), null);
  assert.notEqual(stage.prim("/World/Rock1"), null);
});

// Owned by the review of property operations (`review-property-ops`), which fixes it once in the store's shared
// edit path: an edit queued behind a fetch goes where edits go when it runs, not where they went when it was asked for.
test("a kind set while an action fetches is authored where edits went when it was asked for", { todo: "fixed with the store's queued edits, in the property operations review" }, async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const stage = store.session!.stage;
  const library = stage.layers().find((layer) => layer.identifier === "library.usda")!;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  store.setKind("/World/Top", "component");
  store.setTarget(library);
  release();
  await h.settle(variant);
  await h.until(() => store.running === null && store.queued === 0, "the edit behind the variant");
  const kindIn = (layer: typeof library) => {
    const spec = stage.layerSpec(layer, "/World/Top");
    return spec?.type === "prim" ? spec.kind : null;
  };
  assert.equal(kindIn(stage.root), "component");
  assert.equal(kindIn(library), null);
});

test("moving a prim under another, reordering siblings, each undone; unrepaired paths are a notice that points at Health", async () => {
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
  assert.equal(notice.message, "1 path still names what was moved, in specs no composed prim uses: see Health.");
  assert.equal(notice.tab, "health", "a click brings the Health tab forward");
  // Health lists them, once.
  assert.deepEqual(
    store.health!.namespaceEdit!.unrepaired.map((each) => [each.layer.identifier, specStagePath(each.spec), each.field, each.path]),
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

test("through an arc, a move is refused before it is tried, and so is a duplicate the target does not map", async () => {
  const { store, stage, steps } = await opened();
  const reference = stage.prim("/World/Robot")!.compositionArcs().find((arc) => arc.arc === "references")!;
  store.setTarget(reference.editTarget());
  assert.match(store.moveTo("/World/Robot/Body", "/World/Robot/Looks")!, /Renaming and deleting edit the opinions of the stage's own layer stack/);
  assert.match(store.duplicate("/World/Rock")!, /does not map \/World\/Rock1/);
  // Inside the referenced prim, the copy is made in its asset.
  assert.equal(store.duplicate("/World/Robot/Body"), undefined);
  assert.notEqual(stage.prim("/World/Robot/Body1"), null);
  assert.equal(steps(), 1);
});

test("a prim's actions are one list, one wording, for its menu and its path's context menu, a refused one disabled with why", async () => {
  const { store, stage } = await opened();
  const at = (path: string) => primActions(stage, path, store.session!.target);
  const titles = (path: string) => at(path).map((action) => [action.title, action.enabled]);
  // Where edits go is the root: everything on the rock, and Move up only where it is not first.
  assert.deepEqual(titles("/World/Rock").map(([title]) => title), [
    "Add a child prim…",
    "Duplicate",
    "Move to…",
    "Move up",
    "Move down",
    "Bind a material…",
    "Rename…",
    "Delete",
  ]);
  assert.deepEqual(refusals(at("/World/Rock")), []);
  const first = stage.children("/World")[0]!;
  assert.equal(at(first).find((action) => action.id === "moveUp")?.enabled, false, "the first sibling moves up no further");
  assert.deepEqual(refusals(at(first)), [], "which needs no sentence");
  // Inside a variant: rename, delete and move are listed disabled, with why; the duplicate's copy is mapped there.
  store.editInsideVariant("/World/Robot", "finish", "painted");
  const inside = at("/World/Robot");
  assert.deepEqual(
    inside.filter((action) => !action.enabled).map((action) => action.id),
    ["moveTo", "rename", "delete"],
  );
  assert.equal(inside.find((action) => action.id === "delete")?.title, "Delete", "the same wording, disabled");
  assert.equal(refusals(inside).length, 1);
  assert.match(refusals(inside)[0]!, /Renaming and deleting edit the opinions of the stage's own layer stack/);
  // Through the robot's reference, a duplicate of the rock is not one the target maps.
  store.setTarget(stage.prim("/World/Robot")!.compositionArcs().find((arc) => arc.arc === "references")!.editTarget());
  const rock = at("/World/Rock");
  assert.equal(rock.find((action) => action.id === "duplicate")?.enabled, false);
  assert.ok(refusals(rock).some((why) => /does not map \/World\/Rock1/.test(why)));
  // Nor is its parent, where its place among its siblings is authored: the store refuses the move, so it is not offered.
  assert.match(store.reorderChild("/World/Rock", 1)!, /does not map \/World/);
  for (const id of ["moveUp", "moveDown"] as const) assert.equal(rock.find((action) => action.id === id)?.enabled, false, id);
  assert.ok(refusals(rock).some((why) => /\/World is not written through .*its children's order is authored on it\.$/.test(why)));
  // Beneath the robot the target maps the parent: its children move.
  const body = at("/World/Robot/Body");
  assert.equal(body.find((action) => action.id === "moveDown")?.enabled, true);
  assert.equal(store.reorderChild("/World/Robot/Body", 1), undefined);
  // An instance proxy: listed, and read-only.
  const proxy = stage.children("/World/Beacons/North")[0]!;
  assert.ok(at(proxy).every((action) => !action.enabled));
  assert.deepEqual(refusals(at(proxy)), [PROXY_REFUSAL]);
  // The pseudo-root takes a child alone.
  assert.deepEqual(at("/").map((action) => action.id), ["add"]);
  assert.deepEqual(at("/Nowhere"), []);
});
