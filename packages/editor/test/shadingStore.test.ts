// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Editing the sample's checker material through the store, as the
// shading view and the inspector do: connecting and disconnecting ports,
// setting an input its shader's definition has and the prim does not,
// adding a shader and deleting one; each one action in the history where
// edits go, undone and redone.
import assert from "node:assert/strict";
import { test } from "node:test";
import { readNetwork } from "../src/model/network.ts";
import { nodeMenu, portMenu, unshownConnection } from "../src/model/networkEdits.ts";
import { groupInputs, inputEditTime } from "../src/model/shadingInputs.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

const CHECKER = "/World/Looks/Checker";

test("connecting from an output the prim does not author creates it, as one undoable action", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  const opacity = `${CHECKER}/Surface.inputs:opacity`;
  const alpha = `${CHECKER}/Texture.outputs:a`;
  assert.equal(stage.port(alpha), null);
  assert.equal(store.connect(opacity, alpha), undefined);
  assert.deepEqual(stage.port(opacity)?.connections, [alpha]);
  assert.equal(stage.port(alpha)?.typeName, "float");
  assert.equal(store.session!.history.nextUndo?.label, `connect ${opacity} to ${alpha}`);
  assert.equal(store.session!.undoStack.at(-1)!.label, `connect ${opacity} to ${alpha}`, "the history's entry has its report");
  // One step undoes both: the connection and the output it created.
  await h.settle(store.undo());
  assert.equal(stage.port(opacity), null);
  assert.equal(stage.port(alpha), null);
  await h.settle(store.redo());
  assert.deepEqual(stage.port(opacity)?.connections, [alpha]);
  assert.deepEqual(store.notices, []);
});

test("disconnecting a curve removes that source, and undo brings it back", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  const diffuse = `${CHECKER}/Surface.inputs:diffuseColor`;
  const rgb = `${CHECKER}/Texture.outputs:rgb`;
  assert.equal(store.disconnect(diffuse, rgb), undefined);
  assert.deepEqual(stage.port(diffuse)?.connections, []);
  assert.equal(store.session!.history.nextUndo?.label, `disconnect ${diffuse} from ${rgb}`);
  await h.settle(store.undo());
  assert.deepEqual(stage.port(diffuse)?.connections, [rgb]);
  // Reconnecting replaces what the input read.
  store.connect(diffuse, `${CHECKER}/Place.outputs:result`);
  assert.deepEqual(stage.port(diffuse)?.connections, [`${CHECKER}/Place.outputs:result`]);
});

test("an input only the shader's definition has is created by its first edit; a mistyped one is refused", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  const metallic = `${CHECKER}/Surface.inputs:metallic`;
  assert.equal(stage.attribute(metallic), null);
  assert.equal(store.setShaderInput(metallic, { type: "float", value: 0.75 }), undefined);
  const value = stage.attribute(metallic)?.get() ?? null;
  assert.equal(value?.kind === "scalar" ? value.value : null, 0.75);
  assert.equal(store.session!.history.nextUndo?.label, `set ${metallic}`);
  const refusal = store.setShaderInput(metallic, { type: "color3f", value: [1, 0, 0] });
  assert.equal(typeof refusal, "string", "the input is a float now");
  assert.equal(store.notices.at(-1)?.message, refusal);
  await h.settle(store.undo());
  assert.equal(stage.attribute(metallic), null);
});

test("adding a shader defines it with its id in one action; deleting one takes the connections to it", async () => {
  const h = await harness();
  await h.open(sampleSource());
  const { store } = h;
  const stage = store.session!.stage;
  const path = `${CHECKER}/UVTexture`;
  store.select(CHECKER);
  assert.equal(store.addShader(path, "UsdUVTexture"), undefined);
  assert.equal(stage.shader(path)?.id, "UsdUVTexture");
  assert.equal(store.selection, CHECKER, "the selection stays on the material");
  assert.equal(store.session!.history.nextUndo?.label, `add UsdUVTexture ${path}`);
  // Its outputs are the definition's: connecting from one creates it.
  store.connect(`${CHECKER}/Surface.inputs:roughness`, `${path}.outputs:g`);
  assert.equal(stage.port(`${path}.outputs:g`)?.typeName, "float");
  await h.settle(store.undo());
  await h.settle(store.undo());
  assert.equal(stage.prim(path), null);

  // Deleting the texture the surface reads leaves the surface's input reading nothing.
  const texture = `${CHECKER}/Texture`;
  assert.equal(store.delete(texture), undefined);
  assert.equal(stage.prim(texture), null);
  assert.deepEqual(stage.port(`${CHECKER}/Surface.inputs:diffuseColor`)?.connections ?? [], []);
  await h.settle(store.undo());
  assert.deepEqual(stage.port(`${CHECKER}/Surface.inputs:diffuseColor`)?.connections, [`${texture}.outputs:rgb`]);
  assert.deepEqual(store.notices, []);
});

/** A root over a sublayer that defines a material; the root authors the surface's roughness and what its colour reads. */
const STACKED = {
  "stacked.usda": `#usda 1.0
(
    subLayers = [@./looks.usda@]
)
over "Looks"
{
    over "M"
    {
        over "Surface"
        {
            color3f inputs:diffuseColor.connect = </Looks/M/Tex.outputs:rgb>
            float inputs:roughness = 0.75
        }
    }
}
`,
  "looks.usda": `#usda 1.0
def Scope "Looks"
{
    def Material "M"
    {
        token outputs:surface.connect = </Looks/M/Surface.outputs:surface>
        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Looks/M/Tex.outputs:rgb>
            float inputs:roughness = 0.2
            float inputs:metallic = 0.1
            float inputs:ior.connect = </Looks/M/Gone.outputs:r>
            token outputs:surface
        }
        def Shader "Tex"
        {
            uniform token info:id = "UsdUVTexture"
            float3 outputs:rgb
        }
        def Shader "Tex2"
        {
            uniform token info:id = "UsdUVTexture"
            float3 outputs:rgb
        }
    }
}
`,
};

/** The stacked scene open, with its sublayer. */
async function stacked() {
  const h = await harness();
  await h.open(sampleSource("stacked.usda", STACKED));
  const stage = h.store.session!.stage;
  const looks = stage.layers().find((layer) => layer.identifier?.endsWith("looks.usda"))!;
  return { h, store: h.store, stage, looks };
}

test("a shading input's edit that a stronger layer hides is said not to show, and one that shows clears what was said", async () => {
  const { store, stage, looks } = await stacked();
  const roughness = "/Looks/M/Surface.inputs:roughness";
  const metallic = "/Looks/M/Surface.inputs:metallic";
  store.setTarget(looks);
  // The root's 0.75 still gives the value: the edit is in the sublayer, and does not show.
  assert.equal(store.setShaderInput(roughness, { type: "float", value: 0.5 }), undefined);
  const value = stage.attribute(roughness)?.get() ?? null;
  assert.equal(value?.kind === "scalar" ? value.value : null, 0.75);
  assert.equal(store.shadow?.path, roughness);
  assert.equal(store.shadow?.layer.identifier, stage.root.identifier);
  // An edit that shows: nothing is shadowed any more.
  assert.equal(store.setShaderInput(metallic, { type: "float", value: 0.4 }), undefined);
  assert.equal(store.shadow, null);
});

test("a connection or disconnection that a stronger layer hides is a notice saying what the port still reads", async () => {
  const { h, store, stage, looks } = await stacked();
  const diffuse = "/Looks/M/Surface.inputs:diffuseColor";
  const tex = "/Looks/M/Tex.outputs:rgb";
  const tex2 = "/Looks/M/Tex2.outputs:rgb";
  store.setTarget(looks);
  // The root authors what the colour reads: a connection made in the sublayer is authored there, and changes nothing read.
  assert.equal(store.connect(diffuse, tex2), undefined);
  const authored = stage.layerSpec(looks, diffuse);
  assert.deepEqual(authored?.type === "attribute" ? authored.connections?.explicit : null, [tex2]);
  assert.deepEqual(stage.port(diffuse)?.connections, [tex]);
  assert.equal(unshownConnection(stage, diffuse, tex2, true), `${diffuse}.connect is authored where edits go, and a stronger opinion still gives what it reads: ${tex}`);
  assert.equal(store.notices.at(-1)?.action, `connect ${diffuse} to ${tex2}`);
  assert.match(store.notices.at(-1)?.message ?? "", /a stronger opinion still gives what it reads/);
  // Disconnecting there does not show either.
  const before = store.notices.length;
  assert.equal(store.disconnect(diffuse, tex), undefined);
  assert.deepEqual(stage.port(diffuse)?.connections, [tex]);
  assert.equal(store.notices.length, before + 1);
  // Where edits go to the root, both show, and nothing more is said.
  await h.settle(store.undo());
  await h.settle(store.undo());
  store.setTarget(undefined);
  assert.equal(store.connect(diffuse, tex2), undefined);
  assert.deepEqual(stage.port(diffuse)?.connections, [tex2]);
  assert.equal(unshownConnection(stage, diffuse, tex2, true), null);
  assert.equal(store.disconnect(diffuse, tex2), undefined);
  assert.equal(unshownConnection(stage, diffuse, tex2, false), null);
  assert.equal(store.notices.length, before + 1);
});

test("a connection that names nothing is disconnected from its port's menu, as one it reads is", async () => {
  const { h, store, stage } = await stacked();
  const ior = "/Looks/M/Surface.inputs:ior";
  const gone = "/Looks/M/Gone.outputs:r";
  assert.deepEqual(stage.port(ior)?.connections, []);
  assert.deepEqual(stage.port(ior)?.invalidConnections, [gone]);
  const menu = portMenu(readNetwork(stage, "/Looks/M"), ior);
  const item = menu.find((entry) => entry.id === "disconnect");
  assert.deepEqual(item?.action, { kind: "disconnect", destination: ior, source: gone });
  assert.equal(store.disconnect(ior, gone), undefined);
  assert.deepEqual(stage.port(ior)?.invalidConnections, []);
  assert.deepEqual(store.notices, []);
  await h.settle(store.undo());
  assert.deepEqual(stage.port(ior)?.invalidConnections, [gone]);
  // Beside a source it reads, each is named, the one that names nothing said so.
  assert.equal(store.apply("add a source", [{ type: "connectToSource", path: ior, source: "/Looks/M/Tex.outputs:rgb", position: "append" }]), undefined);
  const both = portMenu(readNetwork(stage, "/Looks/M"), ior).filter((entry) => entry.action?.kind === "disconnect");
  assert.deepEqual(both.map((entry) => entry.label).sort(), ["Disconnect from Gone.outputs:r (names nothing)", "Disconnect from Tex.outputs:rgb"]);
});

test("deleting a shader edits every layer of the stack that has it, and its menu says so", async () => {
  const { h, store, stage, looks } = await stacked();
  const tex = "/Looks/M/Tex";
  const diffuse = "/Looks/M/Surface.inputs:diffuseColor";
  // Edits go to the root, which has no spec of the texture: the sublayer defines it.
  assert.equal(stage.editLayer?.identifier, stage.root.identifier);
  assert.equal(stage.layerSpec(stage.root, tex), null);
  const item = nodeMenu(readNetwork(stage, "/Looks/M"), tex, false, 0).find((entry) => entry.id === "delete")!;
  assert.equal(item.detail, `deletes ${tex} and the 1 connection to and from it, in every layer of the layer stack that has them`);
  assert.equal(store.delete(tex), undefined);
  assert.equal(stage.prim(tex), null);
  assert.equal(stage.layerSpec(looks, tex), null, "the sublayer's definition is gone");
  for (const layer of [stage.root, looks]) {
    const spec = stage.layerSpec(layer, diffuse);
    assert.deepEqual(spec?.type === "attribute" ? spec.connections?.explicit : null, [], `${layer.identifier}'s connection to it is gone`);
  }
  assert.equal(stage.isDirty(looks), true);
  await h.settle(store.undo());
  assert.notEqual(stage.layerSpec(looks, tex), null);
  assert.deepEqual(stage.port(diffuse)?.connections, [`${tex}.outputs:rgb`]);
});

test("two shaders added while an action fetches are two shaders, each where it was asked for", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const stage = store.session!.stage;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  // Both are named from the children there are now: the same path.
  const path = `${CHECKER}/UVTexture`;
  assert.equal(store.addShader(path, "UsdUVTexture", [1, 2]), undefined);
  assert.equal(store.addShader(path, "UsdUVTexture", [8, 9]), undefined);
  assert.equal(store.queued, 2);
  release();
  await h.settle(variant);
  await h.until(() => store.running === null && store.queued === 0, "the additions behind the variant");
  const place = (prim: string) => {
    const value = stage.attribute(`${prim}.ui:nodegraph:node:pos`)?.get() ?? null;
    return value?.kind === "tuple" ? [...value.value] : null;
  };
  assert.deepEqual(place(path), [1, 2]);
  assert.deepEqual(place(`${path}1`), [8, 9]);
  assert.equal(store.session!.history.nextUndo?.label, `add UsdUVTexture ${path}1`);
  // Each its own step.
  await h.settle(store.undo());
  assert.equal(stage.prim(`${path}1`), null);
  assert.deepEqual(place(path), [1, 2]);
  assert.deepEqual(store.notices, []);
});

test("a connection asked for while an action fetches is authored where edits went when it was asked for", async () => {
  const h = await harness();
  const source = sampleSource();
  await h.open(source);
  const { store } = h;
  const stage = store.session!.stage;
  const library = stage.layers().find((layer) => layer.identifier?.endsWith("library.usda"))!;
  const opacity = `${CHECKER}/Surface.inputs:opacity`;
  const alpha = `${CHECKER}/Texture.outputs:a`;
  const release = source.hold("assets/chrome.usda");
  const variant = store.selectVariant("/World/Robot", "finish", "chrome");
  await h.until(() => source.askedFor("assets/chrome.usda") > 0, "the variant's fetch");
  // Asked for with edits going to the root; then edits are sent to the sublayer, before it runs.
  assert.equal(store.connect(opacity, alpha), undefined);
  store.setTarget(library);
  release();
  await h.settle(variant);
  await h.until(() => store.running === null && store.queued === 0, "the connection behind the variant");
  assert.deepEqual(stage.port(opacity)?.connections, [alpha]);
  assert.notEqual(stage.layerSpec(stage.root, opacity), null, "the root, where edits went, has the connection");
  assert.equal(stage.layerSpec(library, opacity), null, "the sublayer chosen since has nothing of it");
});

test("an input a referenced material sets at the same path is not marked as set here until this prim sets it", async () => {
  const h = await harness();
  await h.open(
    sampleSource("refs.usda", {
      "refs.usda": `#usda 1.0
def "Stone" (references = @./base.usda@</Stone>)
{
}
`,
      "base.usda": `#usda 1.0
def Material "Stone"
{
    float inputs:roughness = 0.5
}
`,
    }),
  );
  const { store } = h;
  const stage = store.session!.stage;
  const own = () => groupInputs(stage, "/Stone", stage.material("/Stone")!.inputs())[0]!.inputs[0]!.own;
  // The asset's spec is at /Stone too: the same path in another layer, through a reference, is not this prim's own.
  assert.equal(own(), false);
  assert.equal(store.setShaderInput("/Stone.inputs:roughness", { type: "float", value: 0.25 }), undefined);
  assert.equal(own(), true);
  await h.settle(store.undo());
  assert.equal(own(), false);
});

test("an input with one sample is edited as a sample at the time now, not as a default under it", async () => {
  const h = await harness();
  await h.open(
    sampleSource("keyed.usda", {
      "keyed.usda": `#usda 1.0
(
    startTimeCode = 0
    endTimeCode = 24
)
def Material "M"
{
    float inputs:gain.timeSamples = {
        0: 0.5,
    }
    float inputs:plain = 0.5
}
`,
    }),
  );
  const { store } = h;
  const stage = store.session!.stage;
  const gain = "/M.inputs:gain";
  // One sample: the value holds still, so the row is not an animated one.
  assert.deepEqual(stage.valuesMightBeTimeVarying([gain]), [false]);
  store.setTime(12);
  assert.equal(inputEditTime(stage, gain, store.time), 12);
  assert.equal(inputEditTime(stage, "/M.inputs:plain", store.time), null, "an input without samples edits its default");
  assert.equal(inputEditTime(stage, "/M.inputs:none", store.time), null);
  assert.equal(store.setShaderInput(gain, { type: "float", value: 0.25 }, inputEditTime(stage, gain, store.time)), undefined);
  const spec = stage.layerSpec(stage.root, gain);
  assert.deepEqual(spec?.type === "attribute" ? [spec.default, spec.timeSamples] : null, [null, [0, 12]]);
  const value = stage.attribute(gain)?.get(12) ?? null;
  assert.equal(value?.kind === "scalar" ? value.value : null, 0.25, "and it shows");
  // The time is read when the edit is committed: after the time moves, the next edit is there.
  store.setTime(20);
  assert.equal(inputEditTime(stage, gain, store.time), 20);
});
