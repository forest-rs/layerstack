// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Presentations: which registration shows an object, by its presentation
// types (a prim's schema type and ancestors, then its applied schemas; a
// value's type, role and shape), and the colour conversions a swatch
// paints with.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { loadLayerstack } from "@forest-rs/layerstack";
import { hexToLinear, linearToHex, linearToSrgb, srgbToLinear, swatchCss, swatches } from "../src/presentations/color.ts";
import { CommandRegistry, type PresentationContext, PresentationRegistry, primTypes, typesOf, valueRole, valueTypes } from "../src/presentations/registry.ts";

const LAYER = `#usda 1.0
def Xform "World"
{
    def Cube "Box" (
        prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:parts"]
    )
    {
    }
    def Mesh "Ground"
    {
    }
    def "Bare"
    {
    }
    def SphereLight "Lamp"
    {
    }
}
`;

const layerstack = await loadLayerstack();
const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(LAYER) });
// The registry hands the context through; nothing here reads the store.
const context = { store: null, stage, time: undefined } as unknown as PresentationContext;
const prim = (path: string) => ({ kind: "prim", prim: stage.prim(path)! }) as const;
/** A view as the registry holds it: a component only by type. */
const View = () => null;

describe("presentation types", () => {
  it("are a prim's type and its ancestors, its applied schemas, then Prim", () => {
    const types = primTypes(stage.prim("/World/Box")!);
    assert.deepEqual(types.slice(0, 3), ["Cube", "Gprim", "Boundable"]);
    assert.ok(types.indexOf("Xformable") > types.indexOf("Boundable"));
    const binding = types.indexOf("MaterialBindingAPI");
    assert.ok(binding > types.indexOf("Typed"), "applied schemas after the typed ones");
    assert.deepEqual(types.slice(binding, binding + 3), ["MaterialBindingAPI", "CollectionAPI:parts", "CollectionAPI"]);
    assert.equal(types.at(-1), "Prim");
    assert.deepEqual(primTypes(stage.prim("/World/Bare")!), ["Prim"]);
    // A type's built-in applied schemas are the prim's too.
    const lamp = primTypes(stage.prim("/World/Lamp")!);
    assert.ok(lamp.includes("LightAPI") && lamp.includes("CollectionAPI:lightLink"), lamp.join(" "));
  });

  it("are a value's type, role, kind and shape", () => {
    assert.deepEqual(valueTypes("color3f"), ["color3f", "color", "tuple", "value"]);
    assert.deepEqual(valueTypes("color3f[]"), ["color3f[]", "color[]", "array", "value"]);
    assert.deepEqual(valueTypes("color4h"), ["color4h", "color", "tuple", "value"]);
    assert.deepEqual(valueTypes("float"), ["float", "number", "value"]);
    assert.deepEqual(valueTypes("token", { allowedTokens: true }), ["allowedToken", "token", "value"]);
    assert.deepEqual(valueTypes("token"), ["token", "value"]);
    assert.deepEqual(valueTypes("asset"), ["asset", "value"]);
    assert.deepEqual(valueTypes("point3f[]"), ["point3f[]", "point[]", "array", "value"]);
    assert.equal(valueRole("double3"), null);
    assert.equal(valueRole("normal3f[]"), "normal");
  });
});

describe("the presentation registry", () => {
  it("shows a one-of view by the most specific type that has it", () => {
    const registry = new PresentationRegistry();
    registry.register({ id: "any", kind: "prim", type: "Prim", inline: View });
    registry.register({ id: "gprim", kind: "prim", type: "Gprim", inline: View });
    registry.register({ id: "binding", kind: "prim", type: "MaterialBindingAPI", inline: View });
    registry.register({ id: "cube-panel", kind: "prim", type: "Cube", panel: View });
    assert.equal(registry.find(prim("/World/Box"), "inline", context)?.id, "gprim", "Gprim is more specific than an applied schema");
    assert.equal(registry.find(prim("/World/Bare"), "inline", context)?.id, "any");
    assert.equal(registry.find(prim("/World/Box"), "tooltip", context), null, "nothing has the view");
  });

  it("breaks a tie by priority, then by the later registration, and skips what does not apply", () => {
    const registry = new PresentationRegistry();
    registry.register({ id: "low", kind: "prim", type: "Mesh", priority: -1, inline: View });
    registry.register({ id: "high", kind: "prim", type: "Mesh", priority: 5, inline: View });
    assert.equal(registry.find(prim("/World/Ground"), "inline", context)?.id, "high");
    registry.register({ id: "later", kind: "prim", type: "Mesh", priority: 5, inline: View });
    assert.equal(registry.find(prim("/World/Ground"), "inline", context)?.id, "later");
    registry.register({ id: "never", kind: "prim", type: "Mesh", priority: 9, applies: () => false, inline: View });
    assert.equal(registry.find(prim("/World/Ground"), "inline", context)?.id, "later");
    // A registration of the same id replaces the old; its remover removes only it.
    const remove = registry.register({ id: "later", kind: "prim", type: "Gprim", inline: View });
    assert.equal(registry.find(prim("/World/Ground"), "inline", context)?.id, "high");
    remove();
    assert.equal(
      registry.all().some((entry) => entry.id === "later"),
      false,
    );
  });

  it("lists every panel that applies, by priority, then specificity", () => {
    const registry = new PresentationRegistry();
    registry.register({ id: "properties", kind: "prim", type: "Prim", panel: View });
    registry.register({ id: "state", kind: "prim", type: "Prim", priority: 100, panel: View });
    registry.register({ id: "binding", kind: "prim", type: "MaterialBindingAPI", priority: 50, panel: View });
    registry.register({ id: "cube", kind: "prim", type: "Cube", priority: 50, panel: View });
    registry.register({ id: "parts", kind: "prim", type: "CollectionAPI:parts", priority: 50, panel: View });
    registry.register({ id: "mesh", kind: "prim", type: "Mesh", priority: 50, panel: View });
    registry.register({ id: "inline-only", kind: "prim", type: "Prim", inline: View });
    registry.register({ id: "value-panel", kind: "value", type: "value", panel: View });
    assert.deepEqual(
      registry.panels(prim("/World/Box"), context).map((entry) => entry.id),
      ["state", "cube", "binding", "parts", "properties"],
    );
    assert.deepEqual(
      registry.panels(prim("/World/Ground"), context).map((entry) => entry.id),
      ["state", "mesh", "properties"],
    );
  });

  it("dispatches values by type, role and shape", () => {
    const registry = new PresentationRegistry();
    registry.register({ id: "text", kind: "value", type: "value", inline: View });
    registry.register({ id: "swatch", kind: "value", type: "color", inline: View, accept: View });
    registry.register({ id: "strip", kind: "value", type: "color[]", inline: View });
    registry.register({ id: "fields", kind: "value", type: "tuple", accept: View });
    const value = (typeName: string) => ({ kind: "value", attribute: null, typeName, shown: { value: null, text: "" }, at: null }) as const;
    assert.equal(registry.find(value("color3f"), "inline", context)?.id, "swatch");
    assert.equal(registry.find(value("color3f"), "accept", context)?.id, "swatch");
    assert.equal(registry.find(value("float3"), "accept", context)?.id, "fields");
    assert.equal(registry.find(value("color3f[]"), "inline", context)?.id, "strip");
    assert.equal(registry.find(value("float3[]"), "inline", context)?.id, "text");
    assert.deepEqual(typesOf({ kind: "primPath", path: "/World" }), ["primPath", "path"]);
  });
});

describe("the command registry", () => {
  it("offers the commands of an object's types that apply, the more specific first", () => {
    const registry = new CommandRegistry();
    const ran: string[] = [];
    registry.register({ id: "copy", kind: "primPath", type: "path", title: "Copy", run: () => ran.push("copy") });
    registry.register({ id: "select", kind: "primPath", type: "primPath", title: "Select", run: (object) => ran.push(`select ${object.path}`) });
    registry.register({ id: "frame", kind: "primPath", type: "primPath", title: "Frame", applies: (object, { stage }) => stage.prim(object.path) !== null, run: () => ran.push("frame") });
    registry.register({ id: "explain", kind: "propertyPath", type: "propertyPath", title: "Explain", run: () => ran.push("explain") });
    const offered = registry.forObject({ kind: "primPath", path: "/World/Box" }, context);
    assert.deepEqual(
      offered.map((command) => command.id),
      ["select", "frame", "copy"],
    );
    offered[0]!.run({ kind: "primPath", path: "/World/Box" }, context);
    assert.deepEqual(ran, ["select /World/Box"]);
    assert.deepEqual(
      registry.forObject({ kind: "primPath", path: "/Nowhere" }, context).map((command) => command.id),
      ["select", "copy"],
    );
  });
});

describe("instances", () => {
  it("are presented as `instance`, with their own commands", () => {
    const instance = { kind: "instance", instancer: "/World/Field", index: 2, id: 30n, prototype: "/World/Field/Rock" } as const;
    assert.deepEqual(typesOf(instance), ["instance"]);
    const registry = new CommandRegistry();
    const ran: string[] = [];
    registry.register({ id: "path.select", kind: "primPath", type: "primPath", title: "Select", run: () => ran.push("path") });
    registry.register({ id: "select", kind: "instance", type: "instance", title: "Select", run: (object) => ran.push(`select ${object.instancer} ${object.index}`) });
    registry.register({ id: "prototype", kind: "instance", type: "instance", title: "Select its prototype", applies: (object) => object.prototype !== null, run: () => ran.push("prototype") });
    assert.deepEqual(
      registry.forObject(instance, context).map((command) => command.id),
      ["select", "prototype"],
    );
    registry.forObject(instance, context)[0]!.run(instance, context);
    assert.deepEqual(ran, ["select /World/Field 2"]);
    // An instance that names no prototype is not offered its prototype.
    assert.deepEqual(
      registry.forObject({ ...instance, prototype: null }, context).map((command) => command.id),
      ["select"],
    );
  });
});

describe("swatch colours", () => {
  it("paint linear values through the sRGB transfer function", () => {
    assert.equal(linearToSrgb(0), 0);
    assert.ok(Math.abs(linearToSrgb(1) - 1) < 1e-12);
    assert.ok(Math.abs(linearToSrgb(0.0031308) - 0.0404499) < 1e-6, "the linear segment's end");
    assert.ok(Math.abs(linearToSrgb(0.18) - 0.461356) < 1e-5, "18% grey is sRGB 0.46");
    assert.equal(linearToSrgb(2), linearToSrgb(1), "clamped for the swatch");
    assert.equal(linearToSrgb(-1), 0);
    for (const value of [0, 0.001, 0.02, 0.18, 0.5, 0.9, 1]) assert.ok(Math.abs(srgbToLinear(linearToSrgb(value)) - value) < 1e-9, `${value}`);
    assert.equal(linearToHex([0.18, 0.18, 0.18]), "#767676");
    assert.equal(linearToHex([1, 0, 0]), "#ff0000");
    const back = hexToLinear("#767676")!;
    assert.ok(Math.abs(back[0] - 0.18) < 0.003, `${back[0]}`);
    assert.equal(hexToLinear("red"), null);
    assert.equal(swatchCss([1, 1, 1]), "rgb(255 255 255)");
    assert.equal(swatchCss([0, 0, 0], 0.25), "rgb(0 0 0 / 0.25)");
  });

  it("read an array's first elements, with opacity element by element or one for all", () => {
    const values = [1, 0, 0, 0, 1, 0, 0, 0, 1];
    assert.deepEqual(
      swatches(values, 3, 2, [0.5, 0.25]).map((swatch) => swatch.alpha),
      [0.5, 0.25],
    );
    assert.deepEqual(
      swatches(values, 3, 8, [0.5]).map((swatch) => swatch.alpha),
      [0.5, 0.5, 0.5],
    );
    assert.deepEqual(swatches([0.1, 0.2, 0.3, 0.4], 4, 8)[0], { rgb: [0.1, 0.2, 0.3], alpha: 0.4 });
  });
});

describe("skeleton joints", () => {
  it("nest by their paths", async () => {
    const { jointTree } = await import("../src/model/joints.ts");
    assert.deepEqual(jointTree(["Hips", "Hips/Torso", "Hips/Torso/Chest", "Hips/LLeg"]), [
      { path: "Hips", name: "Hips", depth: 0 },
      { path: "Hips/Torso", name: "Torso", depth: 1 },
      { path: "Hips/Torso/Chest", name: "Chest", depth: 2 },
      { path: "Hips/LLeg", name: "LLeg", depth: 1 },
    ]);
  });
});

describe("prim revisions", () => {
  it("change only when a recomposition reaches the prim", async () => {
    const { primRevision, reaches } = await import("../src/model/revisions.ts");
    const none = { created: [], removed: [], resynced: [], changedInfoOnly: [], properties: [], resyncedProperties: [], mutedLayers: [], unmutedLayers: [], mutedIdentifiers: [], unmutedIdentifiers: [] } as { readonly [key: string]: readonly string[] };
    const changeLog: { cause: string; changes: typeof none }[] = [];
    const session = { changeLog } as unknown as Parameters<typeof primRevision>[0];
    assert.equal(primRevision(session, "/World/Rock"), 0);
    assert.equal(primRevision(session, "/World/Tree"), 0);
    changeLog.push({ cause: "set", changes: { ...none, changedInfoOnly: ["/World/Rock"] } });
    assert.equal(primRevision(session, "/World/Rock"), 1);
    assert.equal(primRevision(session, "/World/Tree"), 0, "an edit elsewhere");
    changeLog.push({ cause: "subset", changes: { ...none, resyncedProperties: ["/World/Rock/Part.indices"] } });
    assert.equal(primRevision(session, "/World/Rock"), 2, "a change beneath it");
    changeLog.push({ cause: "resync", changes: { ...none, resynced: ["/World"] } });
    assert.equal(primRevision(session, "/World/Tree"), 1, "a resync above it");
    assert.equal(primRevision(session, "/World/Rock"), 3);
    assert.ok(!reaches("/World/Rock", ["/World/Rocks"], true), "a sibling with a longer name");
    assert.ok(reaches("/World/Rock", ["/World/Rock.points"], true));
    assert.ok(!reaches("/World/Rock", ["/World/Rock/Part"], false));
  });
});
