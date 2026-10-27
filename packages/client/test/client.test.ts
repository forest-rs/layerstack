// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import test from "node:test";
import { Stage, LayerstackError, type StageTransport } from "../src/index.ts";

function endpoint(realm: string) {
  const requests: any[] = [];
  const transport: StageTransport = { request(raw) {
    const request = JSON.parse(raw); requests.push(request);
    const layers = [{ id: `${realm}/layer/18446744073709551615`, name: "edits.usda", muted: false }];
    let result: unknown = { rootLayer: layers[0].id, layers, schemaTypes: [], capabilities: [] };
    if (request.op.type === "transaction" || request.op.type === "undo") result = { receipt: `${realm}/undo/${requests.length}`, changes: { created: [], removed: [], resynced: [], changedInfoOnly: [] } };
    return JSON.stringify({ protocol: "layerstack", version: 1, stage: `${realm}/stage`, store: `${realm}/store`, ok: true, result });
  } };
  return { transport, requests };
}

test("typed native transactions preserve exact IDs and integer encodings", async () => {
  const host = endpoint("realm-a");
  const stage = await Stage.connect(host.transport);
  const { layers } = await stage.inspect();
  const edit = await stage.transaction(layers[0], [{ type: "setAttribute", path: "/Part", name: "serial", value: { type: "uint64", value: "18446744073709551615" } }]);
  const request = host.requests.at(-1);
  assert.equal(request.store, "realm-a/store");
  assert.equal(request.op.target, "realm-a/layer/18446744073709551615");
  assert.equal(request.op.edits[0].value.value, "18446744073709551615");
  await stage.undo(edit.receipt);
  await assert.rejects(stage.undo(edit.receipt), { code: "receipt.affinity" });
});

test("foreign layers, reference targets and inverse receipts fail before dispatch", async () => {
  const a = endpoint("realm-a"), b = endpoint("realm-b");
  const first = await Stage.connect(a.transport), second = await Stage.connect(b.transport);
  const own = (await first.inspect()).layers[0], foreign = (await second.inspect()).layers[0];
  const edit = await second.transaction(foreign, []);
  const count = a.requests.length;
  await assert.rejects(first.transaction(foreign, []), { code: "layer.affinity" });
  await assert.rejects(first.transaction(own, [{ type: "addReference", path: "/Copy", layer: foreign, primPath: "/Part" }]), { code: "layer.affinity" });
  await assert.rejects(first.undo(edit.receipt), { code: "receipt.affinity" });
  assert.equal(a.requests.length, count);
});

test("disposal is explicit and idempotent", async () => {
  const host = endpoint("realm"); const stage = await Stage.connect(host.transport);
  await stage.dispose(); const count = host.requests.length;
  await stage.dispose();
  await assert.rejects(stage.inspect(), { code: "stage.disposed" });
  assert.equal(host.requests.length, count);
});

test("inverse retention can be released without applying undo", async () => {
  const host = endpoint("realm"); const stage = await Stage.connect(host.transport);
  const layer = (await stage.inspect()).layers[0];
  const edit = await stage.transaction(layer, []);
  await stage.releaseReceipt(edit.receipt);
  assert.equal(host.requests.at(-1).op.type, "releaseReceipt");
  const count = host.requests.length;
  await assert.rejects(stage.undo(edit.receipt), { code: "receipt.affinity" });
  assert.equal(host.requests.length, count);
});

test("malformed endpoints and mismatched response affinity refuse use", async () => {
  await assert.rejects(Stage.connect({ request: () => "{}" }), { code: "protocol.invalid" });
  const host = endpoint("realm"); const stage = await Stage.connect(host.transport);
  host.transport.request = () => JSON.stringify({ protocol: "layerstack", version: 1, stage: "other/stage", store: "other/store", ok: true, result: {} });
  await assert.rejects(stage.inspect(), { code: "stage.affinity" });
  assert.equal(new LayerstackError("native.refusal", "detail").name, "LayerstackError");
});
