// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { AssetSource, Changes, Layer, Stage } from "../dist/index.js";

const layerstack = await loadLayerstack();

// The layers `layer_release_oracle.py` records usd-core 26.11 freeing and
// reopening; `layer_release.rs` replays its cases call by call.
const base = new URL("../../../layerstack_conformance/fixtures/layer_release/layers/", import.meta.url);
const names = ["root.usda", "sub.usda", "extra.usda", "payload.usda", "payload_sub.usda", "ref.usda"];

/** The fixture's files, rewritable, as an asset source counting its fetches. */
function files(): AssetSource & { readonly text: Map<string, string>; readonly fetched: string[] } {
  const text = new Map(names.map((name) => [name, readFileSync(new URL(name, base), "utf8")]));
  const fetched: string[] = [];
  return {
    text,
    fetched,
    async fetch(identifier: string) {
      fetched.push(identifier);
      const found = text.get(identifier);
      return found === undefined ? null : new TextEncoder().encode(found);
    },
  };
}

async function open(source: ReturnType<typeof files>): Promise<Stage> {
  const root = new TextEncoder().encode(source.text.get("root.usda"));
  const stage = layerstack.open({ identifier: "root.usda", bytes: root });
  await stage.resolve(source);
  source.fetched.length = 0;
  return stage;
}

function double(stage: Stage, path: string): unknown {
  const value = stage.attribute(path)?.get();
  return value?.kind === "scalar" ? value.value : (value ?? null);
}

function named(stage: Stage, identifier: string): Layer {
  const layer = stage.layers().find((candidate) => candidate.identifier === identifier);
  assert.ok(layer, identifier);
  return layer;
}

function identifiers(stage: Stage): string[] {
  return stage
    .layers()
    .map((layer) => layer.identifier ?? "")
    .sort();
}

describe("releasing unused layers", () => {
  it("releases an unloaded payload, and reads its rewritten file when loaded again", async () => {
    const source = files();
    using stage = await open(source);
    stage.unload("/P");
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const released = stage.releaseUnusedLayers();
    assert.deepEqual(
      released.released.map((layer) => layer.identifier).sort(),
      ["payload.usda", "payload_sub.usda"],
    );
    assert.deepEqual(released.keptDirty, []);
    assert.equal(
      released.sourceBytes,
      source.text.get("payload.usda")!.length + source.text.get("payload_sub.usda")!.length,
    );
    assert.deepEqual(heard, [], "a release is heard as nothing");
    assert.ok(!identifiers(stage).includes("payload.usda"));
    assert.deepEqual(stage.releaseUnusedLayers().released, [], "once");

    source.text.set("payload.usda", source.text.get("payload.usda")!.replace("p = 4", "p = 44"));
    source.text.set("payload_sub.usda", source.text.get("payload_sub.usda")!.replace("q = 5", "q = 55"));
    stage.load("/P");
    assert.deepEqual(
      stage.unavailable().map((asset) => asset.identifier),
      ["payload.usda"],
      "the host supplies it again",
    );
    await stage.resolve(source);
    assert.deepEqual(source.fetched.sort(), ["payload.usda", "payload_sub.usda"]);
    assert.equal(double(stage, "/P.p"), 44);
    assert.equal(double(stage, "/P.q"), 55);
    assert.equal(stage.isDirty(stage.root), false);
  });

  it("releases a muted layer, and reads its rewritten file when unmuted", async () => {
    const source = files();
    using stage = await open(source);
    const sub = named(stage, "sub.usda");
    stage.muteLayer(sub);
    const released = stage.releaseUnusedLayers();
    assert.deepEqual(released.released, [
      { identifier: "sub.usda", sourceBytes: source.text.get("sub.usda")!.length, muted: true },
    ]);
    assert.deepEqual(stage.mutedIdentifiers(), ["sub.usda"]);
    source.text.set("sub.usda", '#usda 1.0\ndef "B"\n{\n    double y = 20\n}\n');
    stage.unmuteLayer("sub.usda");
    await stage.resolve(source);
    assert.equal(double(stage, "/B.y"), 20);
    assert.equal(stage.prim("/S"), null);
    assert.equal(stage.isDirty(stage.root), false);
  });

  it("keeps the layers it is asked to keep, and unused dirty layers", async () => {
    const source = files();
    using stage = await open(source);
    const payload = named(stage, "payload.usda");
    const through = stage.prim("/P")!.compositionArcs().find((arc) => arc.arc === "payloads")!;
    stage.apply([{ type: "setValue", path: "/P.p", value: { type: "double", value: 40 } }], through.editTarget(payload));
    const ref = named(stage, "ref.usda");
    stage.unload("/P");
    stage.apply([{ type: "removeReference", path: "/R", asset: "ref.usda", primPath: "/R" }], stage.root);
    const released = stage.releaseUnusedLayers([ref]);
    // The dirty payload is kept, its clean sublayer released: holding a
    // layer does not hold what it sublayers.
    assert.deepEqual(
      released.released.map((layer) => layer.identifier),
      ["payload_sub.usda"],
    );
    assert.deepEqual(released.keptDirty, [payload]);
    assert.ok(identifiers(stage).includes("ref.usda"), "kept");
    stage.load("/P");
    await stage.resolve(source);
    assert.equal(double(stage, "/P.p"), 40, "OpenUSD would have lost the edit");
  });

  it("refuses another stage's layer", async () => {
    using stage = await open(files());
    using other = await open(files());
    try {
      stage.releaseUnusedLayers([other.root]);
      assert.fail("expected a LayerstackError");
    } catch (error) {
      assert.ok(error instanceof LayerstackError);
      assert.equal(error.code, "layer.affinity");
    }
  });
});
