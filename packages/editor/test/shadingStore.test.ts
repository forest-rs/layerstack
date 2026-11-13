// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Editing the sample's checker material through the store, as the
// shading view and the inspector do: connecting and disconnecting ports,
// setting an input its shader's definition has and the prim does not,
// adding a shader and deleting one; each one action in the history where
// edits go, undone and redone.
import assert from "node:assert/strict";
import { test } from "node:test";
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
  assert.equal(stage.attribute(metallic)?.get()?.kind === "scalar" ? stage.attribute(metallic)?.get()?.value : null, 0.75);
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
