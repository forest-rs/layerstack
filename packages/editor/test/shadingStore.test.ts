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
import { portMenu, unshownConnection } from "../src/model/networkEdits.ts";
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
  const looks = stage.layers().find((layer) => layer.identifier.endsWith("looks.usda"))!;
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
