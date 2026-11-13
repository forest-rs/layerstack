// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Editing a shading network, as the shading view does it: which ports a
// definition adds, what may connect to what and why not, the edits each
// gesture and menu item authors, the drawing kept still across an edit,
// the drag's state machine, the keyboard's focus, and the problems the
// connections have.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { loadLayerstack, type Stage } from "@forest-rs/layerstack";
import { cycleEdges, destinationsFor, findPort, judge, readIssues, sourcesFor, typeFamily, valueShape } from "../src/model/connections.ts";
import { dropTargetAt, type Gesture, type GestureInput, graphKey, initialFocus, judgeDrop, moveFocus, stepGesture } from "../src/model/graphGesture.ts";
import { connectedOnly, layoutNetwork, readNetwork, type ShadingNetwork } from "../src/model/network.ts";
import {
  addShaderEdits,
  authoredPlace,
  backgroundMenu,
  connectEdits,
  disconnectEdits,
  dropMenu,
  edgeMenu,
  type MenuEntry,
  newShaderPath,
  nodeMenu,
  portMenu,
  uniqueName,
} from "../src/model/networkEdits.ts";
import { keepPlaces } from "../src/model/networkPlaces.ts";
import { SHADER_DEFINITIONS, shaderDefinition } from "../src/model/shaderDefinitions.ts";
import { groupInputs } from "../src/model/shadingInputs.ts";
import { typesOf } from "../src/presentations/registry.ts";

const layerstack = await loadLayerstack();
const SAMPLE = readFileSync(new URL("../public/scene/scene.usda", import.meta.url));
const CHECKER = "/World/Looks/Checker";

/** The sample scene's root layer, alone: its materials are all in it. */
function sample(): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: SAMPLE });
}

/** The port at `path` in `network`, which must be there. */
function port(network: ShadingNetwork, path: string) {
  const found = findPort(network, path);
  assert.ok(found !== null, `${path} is in the network`);
  return found;
}

test("a known shader has its definition's ports, unauthored ones marked, in the definition's order", () => {
  const stage = sample();
  const network = readNetwork(stage, CHECKER);
  const surface = network.nodes.find((node) => node.key === `${CHECKER}/Surface`)!;
  const definition = shaderDefinition("UsdPreviewSurface")!;
  assert.deepEqual(
    surface.ports.map((each) => each.name),
    [...definition.inputs.map((each) => each.name), ...definition.outputs.map((each) => each.name)],
  );
  assert.deepEqual(
    surface.ports.filter((each) => each.authored).map((each) => each.name),
    ["diffuseColor", "roughness", "surface"],
  );
  assert.equal(port(network, `${CHECKER}/Surface.inputs:metallic`).port.typeName, "float");
  // Folded, the definition's ports go with the other unconnected ones; unfolded, a node keeps them.
  const folded = connectedOnly(network);
  assert.deepEqual(folded.network.nodes.find((node) => node.key === `${CHECKER}/Surface`)!.ports.map((each) => each.name), ["diffuseColor", "surface"]);
  const unfolded = connectedOnly(network, new Set([`${CHECKER}/Surface`]));
  assert.equal(unfolded.network.nodes.find((node) => node.key === `${CHECKER}/Surface`)!.ports.length, surface.ports.length);
  assert.equal(folded.hidden - unfolded.hidden, surface.ports.length - 2);
  // Every definition's fallback has its input's type.
  for (const each of SHADER_DEFINITIONS) for (const input of each.inputs) assert.equal(input.fallback.type, input.typeName, `${each.id}.${input.name}`);
});

test("the inspector lists a known shader's inputs in its definition's order, unauthored ones with their definition", () => {
  const stage = sample();
  const texture = `${CHECKER}/Texture`;
  const groups = groupInputs(stage, texture, stage.shader(texture)!.inputs(), shaderDefinition("UsdUVTexture"));
  assert.equal(groups.length, 1);
  const inputs = groups[0]!.inputs;
  assert.deepEqual(
    inputs.map((input) => [input.name, input.port !== null]),
    [["file", true], ["st", true], ["wrapS", true], ["wrapT", true], ["fallback", false], ["scale", true], ["bias", false], ["sourceColorSpace", true]],
  );
  const bias = inputs.find((input) => input.name === "bias")!;
  assert.equal(bias.path, `${texture}.inputs:bias`);
  assert.deepEqual(bias.definition?.fallback, { kind: "tuple", type: "float4", value: [0, 0, 0, 0] });
  // A token the definition restricts is presented as a menu of them, with or without an attribute.
  const wrap = inputs.find((input) => input.name === "wrapS")!;
  assert.deepEqual(typesOf({ kind: "value", attribute: null, typeName: "token", shown: { value: null, text: "" }, at: null, allowedTokens: wrap.definition!.allowedTokens }).slice(0, 2), [
    "allowedToken",
    "token",
  ]);
  assert.equal(typesOf({ kind: "value", attribute: null, typeName: "token", shown: { value: null, text: "" }, at: null })[0], "token");
});

test("what may connect: types by shape, refusals for a port's self, what it reads already and cycles", () => {
  const network = readNetwork(sample(), CHECKER);
  const at = (path: string) => port(network, `${CHECKER}/${path}`);
  assert.equal(valueShape("color3f"), "float3");
  assert.equal(valueShape("texCoord2d"), "float2");
  assert.equal(valueShape("double"), "float");
  assert.equal(valueShape("token"), "token");
  // What the anchors' shapes say, besides their colour.
  assert.deepEqual(
    ["color3f", "float", "int", "float2", "normal3f", "matrix4d", "asset", "string", "bool", "token", null].map(typeFamily),
    ["color", "number", "number", "vector", "vector", "vector", "text", "text", "bool", "token", "other"],
  );
  // A texture's channel into a float input: a match; its rgb into a float: USD takes it, with a warning.
  assert.deepEqual(judge(network, at("Texture.outputs:r"), at("Surface.inputs:metallic")), { kind: "match" });
  assert.equal(judge(network, at("Texture.outputs:rgb"), at("Surface.inputs:metallic")).kind, "warn");
  assert.deepEqual(judge(network, at("Texture.outputs:rgb"), at("Surface.inputs:diffuseColor")), {
    kind: "refused",
    reason: "Surface.diffuseColor already reads Texture.rgb",
  });
  assert.deepEqual(judge(network, at("Texture.outputs:rgb"), at("Texture.inputs:bias")), { kind: "refused", reason: "Texture would read its own output: a cycle" });
  // The surface's output into the texture, which feeds the surface: a cycle, named.
  assert.deepEqual(judge(network, at("Surface.outputs:surface"), at("Texture.inputs:file")), {
    kind: "refused",
    reason: "it would make a cycle: Texture → Surface → Texture",
  });
  // An input is not a source; the material's own interface reads nothing inside it.
  assert.equal(judge(network, at("Surface.inputs:metallic"), at("Texture.inputs:file")).kind, "refused");
  assert.equal(judge(network, at("Reader.outputs:result"), port(network, `${CHECKER}.inputs:coordinates`)).kind, "refused");
  // A material output reads a node directly inside it.
  assert.deepEqual(judge(network, at("Surface.outputs:displacement"), port(network, `${CHECKER}.outputs:surface`)), { kind: "match" });

  // Candidates come best first, refusals last.
  const sources = sourcesFor(network, at("Surface.inputs:metallic"));
  const kinds = sources.map((candidate) => candidate.verdict.kind);
  assert.deepEqual([...kinds].sort((a, b) => ["match", "warn", "refused"].indexOf(a) - ["match", "warn", "refused"].indexOf(b)), kinds);
  assert.ok(sources.some((candidate) => candidate.ref.port.path === `${CHECKER}/Texture.outputs:a` && candidate.verdict.kind === "match"));
  assert.ok(destinationsFor(network, at("Reader.outputs:result")).every((candidate) => candidate.ref.port.kind === "input" || candidate.ref.node.role === "outputs"));
});

test("connecting creates the ports only the definition has, with the definition's types", () => {
  const stage = sample();
  // An unauthored output into an unauthored input: the output created first (with its own type, not the
  // input's), then the input by the connection.
  assert.deepEqual(connectEdits(stage, `${CHECKER}/Surface.inputs:opacity`, `${CHECKER}/Texture.outputs:rgb`), [
    { type: "connectToSource", path: `${CHECKER}/Surface.inputs:opacity`, source: `${CHECKER}/Texture.outputs:rgb`, typeName: "float" },
  ]);
  assert.deepEqual(connectEdits(stage, `${CHECKER}/Surface.inputs:opacity`, `${CHECKER}/Texture.outputs:b`), [
    { type: "createAttribute", path: `${CHECKER}/Texture.outputs:b`, typeName: "float", custom: false },
    { type: "connectToSource", path: `${CHECKER}/Surface.inputs:opacity`, source: `${CHECKER}/Texture.outputs:b`, typeName: "float" },
  ]);
  // Both authored: the connection alone.
  assert.deepEqual(connectEdits(stage, `${CHECKER}/Surface.inputs:roughness`, `${CHECKER}/Reader.outputs:result`), [
    { type: "connectToSource", path: `${CHECKER}/Surface.inputs:roughness`, source: `${CHECKER}/Reader.outputs:result` },
  ]);
  // The output created and connected to in one apply.
  stage.apply(connectEdits(stage, `${CHECKER}/Surface.inputs:opacity`, `${CHECKER}/Texture.outputs:a`));
  assert.equal(stage.port(`${CHECKER}/Texture.outputs:a`)?.typeName, "float");
  assert.deepEqual(stage.port(`${CHECKER}/Surface.inputs:opacity`)?.connections, [`${CHECKER}/Texture.outputs:a`]);
  stage.apply(disconnectEdits(`${CHECKER}/Surface.inputs:opacity`, `${CHECKER}/Texture.outputs:a`));
  assert.deepEqual(stage.port(`${CHECKER}/Surface.inputs:opacity`)?.connections, []);
  assert.deepEqual(disconnectEdits(`${CHECKER}/Surface.inputs:opacity`, null), [{ type: "disconnectSource", path: `${CHECKER}/Surface.inputs:opacity` }]);
});

test("a new shader is named after its definition, unique among its siblings, and placed when the network places its nodes", () => {
  const stage = sample();
  assert.equal(uniqueName("UVTexture", new Set(["UVTexture", "UVTexture1"])), "UVTexture2");
  const texture = shaderDefinition("UsdUVTexture")!;
  const path = newShaderPath(stage, CHECKER, texture);
  assert.equal(path, `${CHECKER}/UVTexture`);
  stage.apply(addShaderEdits(path, texture.id, null));
  assert.equal(stage.shader(path)?.id, "UsdUVTexture");
  assert.equal(newShaderPath(stage, CHECKER, texture), `${CHECKER}/UVTexture1`);
  const placed = addShaderEdits(`${CHECKER}/Reader2`, "UsdPrimvarReader_float2", [3, -1]);
  assert.deepEqual(placed.at(-1), {
    type: "createAttribute",
    path: `${CHECKER}/Reader2.ui:nodegraph:node:pos`,
    typeName: "float2",
    variability: "uniform",
    custom: false,
    value: { type: "float2", value: [3, -1] },
  });
});

/** A network whose shaders are placed by hand: `A` at x 0, `B` at x 10. */
const PLACED = `#usda 1.0
def Material "M"
{
    token outputs:surface.connect = </M/B.outputs:surface>
    def Shader "A"
    {
        uniform token info:id = "UsdUVTexture"
        uniform float2 ui:nodegraph:node:pos = (0, 0)
        float3 outputs:rgb
    }
    def Shader "B"
    {
        uniform token info:id = "UsdPreviewSurface"
        uniform float2 ui:nodegraph:node:pos = (10, 4)
        color3f inputs:diffuseColor.connect = </M/A.outputs:rgb>
        token outputs:surface
    }
}
`;

test("a place in a drawing of authored positions maps back to authored units", () => {
  const stage = layerstack.open({ identifier: "placed.usda", bytes: new TextEncoder().encode(PLACED) });
  const network = readNetwork(stage, "/M");
  const layout = layoutNetwork(connectedOnly(network).network, true);
  const centre = (key: string) => {
    const placed = layout.nodes.find((each) => each.node.key === key)!;
    return { x: placed.x + placed.width / 2, y: placed.y + placed.height / 2 };
  };
  const a = authoredPlace(network, layout, centre("/M/A"))!;
  const b = authoredPlace(network, layout, centre("/M/B"))!;
  assert.ok(Math.abs(a[0]) < 1e-6 && Math.abs(b[0] - 10) < 1e-6, "x fitted to the two nodes");
  const middle = authoredPlace(network, layout, { x: (centre("/M/A").x + centre("/M/B").x) / 2, y: centre("/M/A").y })!;
  assert.ok(middle[0] > 0 && middle[0] < 10, "between them, between them");
  assert.equal(authoredPlace(readNetwork(sample(), CHECKER), layoutNetwork(readNetwork(sample(), CHECKER)), { x: 0, y: 0 }), null);
});

test("after an edit, the nodes stay where they were drawn and a new one goes where it was asked for", () => {
  const stage = sample();
  const before = layoutNetwork(connectedOnly(readNetwork(stage, CHECKER)).network);
  // Connecting the reader straight into the surface's roughness would move columns in a fresh layout.
  stage.apply(connectEdits(stage, `${CHECKER}/Surface.inputs:roughness`, `${CHECKER}/Reader.outputs:result`));
  const path = `${CHECKER}/Extra`;
  stage.apply(addShaderEdits(path, "UsdTransform2d", null));
  const after = layoutNetwork(connectedOnly(readNetwork(stage, CHECKER), new Set([path])).network);
  const kept = keepPlaces(before, after, new Map([[path, { x: 40, y: 30 }]]));
  for (const old of before.nodes) {
    const now = kept.nodes.find((placed) => placed.node.key === old.node.key)!;
    if (old.node.key === `${CHECKER}/Surface`) {
      // It grew a row (roughness is connected now): it stays put, and what it overlaps moves down.
      assert.equal(now.x, old.x);
      assert.equal(now.y, old.y);
      continue;
    }
    assert.equal(now.x, old.x, `${old.node.key} keeps its x`);
  }
  const extra = kept.nodes.find((placed) => placed.node.key === path)!;
  assert.equal(extra.x, 40);
  assert.ok(extra.y >= 30, "at or below where it was asked for, past what it overlaps");
  for (const a of kept.nodes) {
    for (const b of kept.nodes) {
      if (a === b) continue;
      const apart = a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y;
      assert.ok(apart, `${a.node.key} and ${b.node.key} do not overlap`);
    }
  }
  assert.equal(kept.edges.length, after.edges.length);
  // Nothing before: the fresh layout.
  assert.deepEqual(keepPlaces(null, after).nodes.map((placed) => [placed.x, placed.y]), after.nodes.map((placed) => [placed.x, placed.y]));
});

test("a drag connects on a release over a port that can read it, and does nothing, a click, or asks otherwise", () => {
  const network = readNetwork(sample(), CHECKER);
  const judgeIt = (source: string, target: Parameters<typeof judgeDrop>[2]) => judgeDrop(network, source, target);
  const run = (inputs: readonly GestureInput[]) => {
    let state: Gesture = { kind: "idle" };
    const outcomes = [];
    for (const input of inputs) {
      const step = stepGesture(state, input, judgeIt);
      state = step.state;
      outcomes.push(step.outcome.kind);
      if (step.outcome.kind !== "none") return { state, outcome: step.outcome, outcomes };
    }
    return { state, outcome: null, outcomes };
  };
  const source = `${CHECKER}/Texture.outputs:a`;
  const metallic = { kind: "port", port: `${CHECKER}/Surface.inputs:metallic` } as const;
  // Pressed and released in place: a click on the row.
  assert.deepEqual(run([{ kind: "press", source, at: { x: 0, y: 0 } }, { kind: "move", at: { x: 1, y: 1 }, over: metallic }, { kind: "release" }]).outcome, { kind: "click", port: source });
  // Dragged onto a float input: a connection.
  const dragged = run([{ kind: "press", source, at: { x: 0, y: 0 } }, { kind: "move", at: { x: 40, y: 0 }, over: metallic }, { kind: "release" }]);
  assert.deepEqual(dragged.outcome, { kind: "connect", source, destination: metallic.port });
  // Over a refused port: dimmed while over it, and the release says why.
  const own = { kind: "port", port: `${CHECKER}/Texture.inputs:bias` } as const;
  const over = stepGesture({ kind: "pressed", source, start: { x: 0, y: 0 } }, { kind: "move", at: { x: 50, y: 0 }, over: own }, judgeIt).state;
  assert.equal(over.kind === "dragging" && over.verdict?.kind, "refused");
  assert.deepEqual(stepGesture(over, { kind: "release" }, judgeIt).outcome, { kind: "refused", reason: "Texture would read its own output: a cycle" });
  // Over a node: which input is asked on the drop.
  assert.deepEqual(run([{ kind: "press", source, at: { x: 0, y: 0 } }, { kind: "move", at: { x: 40, y: 0 }, over: { kind: "node", node: `${CHECKER}/Surface` } }, { kind: "release" }]).outcome, {
    kind: "choose",
    source,
    node: `${CHECKER}/Surface`,
  });
  // Over nothing, or cancelled: nothing.
  assert.equal(run([{ kind: "press", source, at: { x: 0, y: 0 } }, { kind: "move", at: { x: 40, y: 0 }, over: null }, { kind: "release" }]).outcomes.at(-1), "none");
  assert.deepEqual(stepGesture(dragged.state, { kind: "cancel" }, judgeIt).state, { kind: "idle" });
  // What the pointer is over: a row that reads, else its node.
  assert.deepEqual(dropTargetAt(network, { port: metallic.port, node: `${CHECKER}/Surface` }), metallic);
  assert.deepEqual(dropTargetAt(network, { port: `${CHECKER}/Surface.outputs:surface`, node: `${CHECKER}/Surface` }), { kind: "node", node: `${CHECKER}/Surface` });
  assert.equal(dropTargetAt(network, { port: null, node: null }), null);
  // A node none of whose inputs can read the source refuses the drop, saying why.
  assert.equal(judgeDrop(network, source, { kind: "node", node: `${CHECKER}/Texture` })?.kind, "refused");
});

test("the arrow keys move through a node's rows, across connections and between nodes", () => {
  const layout = layoutNetwork(connectedOnly(readNetwork(sample(), CHECKER)).network);
  const surface = `${CHECKER}/Surface`;
  let focus = initialFocus(layout, surface);
  assert.deepEqual(focus, { node: surface, port: null });
  focus = moveFocus(layout, focus, "ArrowDown");
  assert.deepEqual(focus, { node: surface, port: `${surface}.inputs:diffuseColor` });
  // Left follows the connection to what the row reads.
  assert.deepEqual(moveFocus(layout, focus, "ArrowLeft"), { node: `${CHECKER}/Texture`, port: `${CHECKER}/Texture.outputs:rgb` });
  focus = moveFocus(layout, focus, "ArrowDown");
  assert.deepEqual(focus, { node: surface, port: `${surface}.outputs:surface` });
  // Right from the surface's output follows it to the material's.
  assert.deepEqual(moveFocus(layout, focus, "ArrowRight"), { node: `${CHECKER}#outputs`, port: `${CHECKER}.outputs:surface` });
  assert.deepEqual(moveFocus(layout, { node: surface, port: `${surface}.inputs:diffuseColor` }, "ArrowUp"), { node: surface, port: null });
  assert.deepEqual(moveFocus(layout, { node: surface, port: null }, "End"), { node: surface, port: `${surface}.outputs:surface` });
  // From a header, Left goes to the nearest node in the column before.
  const left = moveFocus(layout, { node: surface, port: null }, "ArrowLeft");
  assert.equal(left.port, null);
  assert.ok(layout.nodes.find((placed) => placed.node.key === left.node)!.x < layout.nodes.find((placed) => placed.node.key === surface)!.x);

  const press = (key: string, more: Partial<{ metaKey: boolean; shiftKey: boolean }> = {}) => graphKey({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...more });
  assert.deepEqual(press("ArrowLeft"), { kind: "move", key: "ArrowLeft" });
  assert.deepEqual(press("Delete"), { kind: "delete" });
  assert.deepEqual(press("Backspace", { metaKey: true }), { kind: "delete" });
  assert.equal(press("Backspace"), null);
  assert.deepEqual(press("F10", { shiftKey: true }), { kind: "menu" });
  assert.deepEqual(press("Enter"), { kind: "activate" });
  assert.equal(press("z", { metaKey: true }), null, "undo stays the editor's");
});

/** A material with a connection to a deleted node, a cycle through two shaders, and one through its interface. */
const BROKEN = `#usda 1.0
def Material "M"
{
    float inputs:a.connect = </M.inputs:b>
    float inputs:b.connect = </M.inputs:a>
    token outputs:surface.connect = </M/Surface.outputs:surface>
    def Shader "Surface"
    {
        uniform token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor.connect = </M/Gone.outputs:rgb>
        float inputs:roughness.connect = </M/Loop.outputs:r>
        float inputs:opacity.connect = </M.inputs:a>
        token outputs:surface
        float outputs:out
    }
    def Shader "Loop"
    {
        uniform token info:id = "UsdUVTexture"
        float2 inputs:st.connect = </M/Surface.outputs:out>
        float outputs:r
    }
}
`;

test("connections that name nothing, and cycles, are found with their reasons", () => {
  const stage = layerstack.open({ identifier: "broken.usda", bytes: new TextEncoder().encode(BROKEN) });
  const network = readNetwork(stage, "/M");
  const cycles = cycleEdges(network);
  assert.deepEqual([...cycles.keys()].sort(), [
    "/M.inputs:a -> /M.inputs:b",
    "/M.inputs:b -> /M.inputs:a",
    "/M/Loop.outputs:r -> /M/Surface.inputs:roughness",
    "/M/Surface.outputs:out -> /M/Loop.inputs:st",
  ]);
  assert.equal(cycles.get("/M.inputs:a -> /M.inputs:b"), "it is on a cycle: M inputs → M inputs");
  assert.equal(cycles.get("/M/Surface.outputs:out -> /M/Loop.inputs:st"), "it is on a cycle: Surface → Loop → Surface");
  const issues = readIssues(stage, network);
  const diffuse = issues.get("/M/Surface.inputs:diffuseColor") ?? [];
  assert.equal(diffuse.length, 1);
  assert.equal(diffuse[0]!.kind, "invalidTarget");
  assert.match(diffuse[0]!.reason, /no prim is at \/M\/Gone/);
  assert.equal(issues.get("/M/Loop.inputs:st")?.[0]?.kind, "cycle");
  // The interface cycle, which the stage finds following the connections.
  assert.ok((issues.get("/M/Surface.inputs:opacity") ?? []).some((issue) => issue.kind === "cycle" && issue.source === null));
  // The folded drawing keeps a row whose connection names nothing.
  assert.ok(connectedOnly(network).network.nodes.some((node) => node.ports.some((each) => each.path === "/M/Surface.inputs:diffuseColor")));
});

/** The labels of a menu's entries, a submenu's after a `>`. */
function labels(entries: readonly MenuEntry[]): string[] {
  return entries.flatMap((entry) => [entry.label, ...(entry.entries === undefined ? [] : labels(entry.entries).map((label) => `> ${label}`))]);
}

test("the menus offer the edits that apply, with what they author", () => {
  const network = readNetwork(sample(), CHECKER);
  // A connected input: connect (to another source), disconnect, explain.
  const diffuse = portMenu(network, `${CHECKER}/Surface.inputs:diffuseColor`);
  assert.deepEqual(
    diffuse.map((entry) => entry.label),
    ["Connect to…", "Disconnect", "Explain its value"],
  );
  assert.deepEqual(diffuse[1]!.action, { kind: "disconnect", destination: `${CHECKER}/Surface.inputs:diffuseColor`, source: `${CHECKER}/Texture.outputs:rgb` });
  assert.equal(diffuse[1]!.detail, "authors Surface.inputs:diffuseColor.connect without it");
  // An unauthored input: what connecting authors says it creates the input.
  const metallic = portMenu(network, `${CHECKER}/Surface.inputs:metallic`);
  const connectTo = metallic[0]!.entries!;
  const channel = connectTo.find((entry) => entry.id === `${CHECKER}/Texture.outputs:r`)!;
  assert.deepEqual(channel.action, { kind: "connect", source: `${CHECKER}/Texture.outputs:r`, destination: `${CHECKER}/Surface.inputs:metallic` });
  assert.equal(channel.detail, "authors Surface.inputs:metallic.connect, creating the input");
  assert.ok(!connectTo.some((entry) => entry.id === `${CHECKER}/Surface.outputs:surface`), "a cycle is not offered");
  // An output lists the inputs that could read it.
  assert.ok(portMenu(network, `${CHECKER}/Reader.outputs:result`)[0]!.entries!.some((entry) => entry.id === `${CHECKER}/Texture.inputs:st`));
  assert.deepEqual(edgeMenu(network, `${CHECKER}/Texture.outputs:rgb`, `${CHECKER}/Surface.inputs:diffuseColor`)[0]!.action, {
    kind: "disconnect",
    destination: `${CHECKER}/Surface.inputs:diffuseColor`,
    source: `${CHECKER}/Texture.outputs:rgb`,
  });
  // A shader's menu says what deleting it takes with it.
  const texture = nodeMenu(network, `${CHECKER}/Texture`, false, 10);
  assert.deepEqual(texture.map((entry) => entry.label), ["Select Texture", "Show its 10 unconnected ports", "Delete shader"]);
  assert.equal(texture[2]!.detail, `deletes ${CHECKER}/Texture and the 3 connections to and from it`);
  assert.deepEqual(nodeMenu(network, `${CHECKER}#outputs`, false, 0).map((entry) => entry.label), ["Select Checker"]);
  // A drop on a node: each input, refused ones disabled with why.
  const drop = dropMenu(network, `${CHECKER}/Texture.outputs:a`, `${CHECKER}/Surface`);
  assert.equal(drop.find((entry) => entry.id === `${CHECKER}/Surface.inputs:metallic`)?.disabled, false);
  assert.ok(drop.every((entry) => entry.action?.kind === "connect"));
  // The background adds shaders, by family.
  assert.deepEqual(labels(backgroundMenu()).slice(0, 4), ["Add shader", "> UsdPreviewSurface", "> UsdUVTexture", "> Primvar readers"]);
});
