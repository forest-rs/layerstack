// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { AssetSource, Changes, Layer, Stage } from "../dist/index.js";

const layerstack = await loadLayerstack();

// The layers `layer_reload_oracle.py` records usd-core 26.11 reloading;
// `layer_reload.rs` replays its cases call by call. This module has no
// search paths, so `asset.usda` is supplied where the root anchors it.
const base = new URL("../../../layerstack_conformance/fixtures/layer_reload/layers/", import.meta.url);
const files = new Map([
  ["root.usda", "root.usda"],
  ["sub.usda", "sub.usda"],
  ["ref.usda", "ref.usda"],
  ["other.usda", "other.usda"],
  ["asset.usda", "second/asset.usda"],
]);

/** The fixture's files, rewritable, as an asset source counting its fetches. */
function source(): AssetSource & { readonly text: Map<string, string>; readonly fetched: string[] } {
  const text = new Map([...files].map(([name, path]) => [name, readFileSync(new URL(path, base), "utf8")]));
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

async function open(files: ReturnType<typeof source>): Promise<Stage> {
  const root = new TextEncoder().encode(files.text.get("root.usda"));
  const stage = layerstack.open({ identifier: "root.usda", bytes: root });
  await stage.resolve(files);
  files.fetched.length = 0;
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

/** Supplies `text` as the source `identifier`, as a host does when its file changes. */
function rewrite(stage: Stage, identifier: string, text: string): void {
  stage.supply(identifier, new TextEncoder().encode(text));
}

describe("reloading", () => {
  it("reads nothing and is heard as nothing when no source changed", async () => {
    using stage = await open(source());
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const result = stage.reload();
    assert.deepEqual(result.reloaded, []);
    assert.deepEqual(result.reads, [], "nothing decoded");
    assert.deepEqual(heard, []);
  });

  it("publishes only what a changed source changed", async () => {
    const files = source();
    using stage = await open(files);
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    rewrite(stage, "sub.usda", files.text.get("sub.usda")!.replace("y = 2", "y = 20"));
    const result = stage.reload();
    assert.deepEqual(result.reloaded, [named(stage, "sub.usda")]);
    assert.equal(double(stage, "/B.y"), 20);
    assert.equal(heard.length, 1);
    const [changes] = heard;
    // OpenUSD: info-only `/` (the layer read again) and `/B.y`.
    assert.deepEqual(changes!.resynced, []);
    assert.deepEqual(changes!.changedInfoOnly, ["/"]);
    assert.deepEqual(changes!.primFields, []);
    assert.deepEqual(changes!.changedProperties, [{ property: "/B.y", field: "default", metadata: null }]);
    assert.equal(stage.isDirty(named(stage, "sub.usda")), false);
  });

  it("does not read a source supplied again unchanged, released or not", async () => {
    const files = source();
    using stage = await open(files);
    rewrite(stage, "sub.usda", files.text.get("sub.usda")!);
    assert.deepEqual(stage.reload().reloaded, []);
    // A version the host gives is what the reload compares.
    stage.supply("sub.usda", new TextEncoder().encode(files.text.get("sub.usda")!), "rev-2");
    assert.deepEqual(stage.reload().reloaded, [named(stage, "sub.usda")]);
    stage.supply("sub.usda", new TextEncoder().encode(files.text.get("sub.usda")!), "rev-2");
    assert.deepEqual(stage.reload().reloaded, []);
  });

  it("refuses to lose edits unless told to, as OpenUSD always does", async () => {
    using stage = await open(source());
    stage.apply([{ type: "setValue", path: "/A.x", value: { type: "double", value: 100 } }], stage.root);
    try {
      stage.reload();
      assert.fail("expected a LayerstackError");
    } catch (error) {
      assert.ok(error instanceof LayerstackError);
      assert.equal(error.code, "io.dirty_reload");
    }
    assert.equal(double(stage, "/A.x"), 100);
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    // The root's bytes were released once read: supplied again, unchanged.
    rewrite(stage, "root.usda", source().text.get("root.usda")!);
    const result = stage.reload({ discardDirty: true });
    assert.deepEqual(result.reloaded, [stage.root]);
    assert.equal(double(stage, "/A.x"), 1);
    assert.equal(stage.isDirty(stage.root), false);
    assert.deepEqual(
      heard.map((changes) => [changes.changedInfoOnly, changes.changedProperties.map((change) => change.property)]),
      [[["/"], ["/A.x"]]],
    );
  });

  it("reloads chosen layers, forced or not", async () => {
    const files = source();
    using stage = await open(files);
    const sub = named(stage, "sub.usda");
    assert.deepEqual(stage.reloadLayers([sub]).reloaded, []);
    rewrite(stage, "sub.usda", files.text.get("sub.usda")!);
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    assert.deepEqual(stage.reloadLayers([sub], { force: true }).reloaded, [sub]);
    // OpenUSD: a forced reload of an unchanged file is heard at `/` alone.
    assert.deepEqual(heard.map((changes) => [changes.resynced, changes.changedInfoOnly]), [[[], ["/"]]]);
  });

  it("keeps a layer that does not parse, and reloads the others", async () => {
    const files = source();
    using stage = await open(files);
    rewrite(stage, "sub.usda", '#usda 1.0\ndef "B"\n{\n    double y = \n');
    rewrite(stage, "ref.usda", files.text.get("ref.usda")!.replace("r = 6", "r = 60"));
    const result = stage.reload();
    assert.deepEqual(result.reloaded, [named(stage, "ref.usda")]);
    assert.deepEqual(
      result.failures.map((failure) => [failure.assetPath, failure.anchor, failure.code]),
      [["sub.usda", null, "io.rejected"]],
    );
    assert.equal(double(stage, "/B.y"), 2, "as it was");
    assert.equal(double(stage, "/R.r"), 60);
    rewrite(stage, "sub.usda", files.text.get("sub.usda")!.replace("y = 2", "y = 20"));
    assert.deepEqual(stage.reload().reloaded, [named(stage, "sub.usda")]);
    assert.equal(double(stage, "/B.y"), 20);
  });

  it("refuses another stage's layer", async () => {
    using stage = await open(source());
    using other = await open(source());
    try {
      stage.reloadLayers([other.root]);
      assert.fail("expected a LayerstackError");
    } catch (error) {
      assert.ok(error instanceof LayerstackError);
      assert.equal(error.code, "layer.affinity");
    }
  });
});
