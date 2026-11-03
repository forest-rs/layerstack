// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A texture packaged in a USDZ is read from the stage that holds the
// package; anything else from the scene's source.

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { assetExists, packagedSearch, readAsset, type PackagedAssets, type SceneSource } from "../src/model/sources.ts";

const text = new TextEncoder();

/** A source holding `files`, recording what it was asked for. */
function source(files: Record<string, string>): SceneSource & { readonly asked: string[] } {
  const asked: string[] = [];
  return {
    label: "test",
    root: "set/model.usdz",
    asked,
    async fetch(identifier) {
      asked.push(identifier);
      return identifier in files ? text.encode(files[identifier]) : null;
    },
    async exists(identifier) {
      asked.push(identifier);
      return identifier in files;
    },
    async size() {
      return null;
    },
  };
}

/** A stage holding the package `set/model.usdz` with one texture. */
const stage: PackagedAssets = {
  packageMember: (identifier) => (identifier === "set/model.usdz[tex/wood.png]" ? text.encode("wood") : null),
};

describe("readAsset", () => {
  it("reads a package member from the stage, not the source", async () => {
    const files = source({ "set/model.usdz": "the archive" });
    const bytes = await readAsset(files, stage, "set/model.usdz[tex/wood.png]", "set/model.usdz");
    assert.equal(new TextDecoder().decode(bytes!), "wood");
    assert.deepEqual(files.asked, [], "the source is not asked for a member it cannot answer");
    assert.equal(await assetExists(files, stage, "set/model.usdz[tex/wood.png]", "set/model.usdz"), true);
  });

  it("looks for a search path in the package that authors it", async () => {
    const files = source({ "tex/wood.png": "outside" });
    const bytes = await readAsset(files, stage, "tex/wood.png", "set/model.usdz");
    assert.equal(new TextDecoder().decode(bytes!), "wood", "the package's member, not the file the host has");
    assert.deepEqual(files.asked, []);
    assert.deepEqual(packagedSearch("tex/wood.png", "set/model.usdz[geo/rock.usda]"), ["set/model.usdz[geo/tex/wood.png]", "set/model.usdz[tex/wood.png]"]);
    assert.deepEqual(packagedSearch("tex/wood.png", "set/model.usdz[rock.usda]"), ["set/model.usdz[tex/wood.png]"]);
    assert.deepEqual(packagedSearch("./tex/wood.png", "set/model.usdz"), [], "an anchored path is already the member");
    assert.deepEqual(packagedSearch("tex/wood.png", "set/scene.usda"), [], "not in a package");
  });

  it("reads anything else from the source", async () => {
    const files = source({ "./set/tex/oak.png": "oak" });
    const bytes = await readAsset(files, stage, "./set/tex/oak.png", "set/scene.usda");
    assert.equal(new TextDecoder().decode(bytes!), "oak");
    assert.equal(await readAsset(files, null, "./set/tex/oak.png", "set/scene.usda").then((b) => b?.byteLength), 3, "without a stage");
    assert.equal(await assetExists(files, stage, "set/model.usdz[tex/none.png]", "set/model.usdz"), false);
  });
});
