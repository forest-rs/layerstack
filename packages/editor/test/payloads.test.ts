// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Opening with payloads unloaded and loading them from the session: what
// the asset loop fetches, and the budget an automatic open weighs before
// each round.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import { type Fetcher, OverBudget, Session } from "../src/model/session.ts";

const layerstack = await loadLayerstack();
const text = new TextEncoder();

const FILES: Record<string, string> = {
  "scene.usda": `#usda 1.0
def Xform "World"
{
    def Xform "Rock" (
        prepend payload = @./rock.usda@</Rock>
    )
    {
    }
    def Xform "Tree" (
        prepend payload = @./tree.usda@</Tree>
    )
    {
    }
}
`,
  "rock.usda": `#usda 1.0
def Sphere "Rock"
{
}
`,
  "tree.usda": `#usda 1.0
def Xform "Tree"
{
    def Cube "Trunk"
    {
    }
}
`,
};

/** The files above, remembering what was fetched. */
function files(): { fetch: Fetcher; fetched: string[]; size: (identifier: string) => Promise<number | null> } {
  const fetched: string[] = [];
  return {
    fetched,
    async fetch(identifier) {
      fetched.push(identifier);
      const file = FILES[identifier];
      return file === undefined ? null : text.encode(file);
    },
    async size(identifier) {
      const file = FILES[identifier];
      return file === undefined ? null : text.encode(file).byteLength;
    },
  };
}

describe("payloads in a session", () => {
  it("opens with payloads unloaded, fetching none of them, and loads one prim's", async () => {
    const source = files();
    using session = await Session.open(layerstack, "scene.usda", source.fetch, { open: { load: "none" } });
    assert.deepEqual(source.fetched, ["scene.usda"]);
    assert.deepEqual(session.stage.findLoadable(), ["/World/Rock", "/World/Tree"]);
    assert.equal(session.stage.prim("/World/Tree")?.loaded, false);
    assert.equal(session.stage.prim("/World/Tree")?.hasAuthoredPayloads, true);

    assert.deepEqual(await session.loadPayloads(["/World/Tree"], []), { changed: true, refused: [] });
    assert.deepEqual(source.fetched, ["scene.usda", "tree.usda"]);
    assert.equal(session.stage.prim("/World/Tree")?.loaded, true);
    assert.deepEqual(session.stage.children("/World/Tree"), ["/World/Tree/Trunk"]);
    assert.equal(session.loops.runs.at(-1)?.cause, "load /World/Tree");

    assert.equal((await session.loadPayloads([], ["/World/Tree"])).changed, true);
    assert.equal(session.stage.prim("/World/Tree/Trunk"), null);
  });

  it("stops before a round that would pass the budget, fetching nothing of it", async () => {
    const source = files();
    using session = await Session.open(layerstack, "scene.usda", source.fetch, { open: { load: "none" } });
    const budget = { bytes: 40, size: source.size };
    await assert.rejects(session.loadPayloads(["/"], [], "withDescendants", budget), (error) => {
      assert.ok(error instanceof OverBudget);
      assert.ok(error.bytes > 40);
      return true;
    });
    assert.deepEqual(source.fetched, ["scene.usda"], "nothing of the round was fetched");
    session.stage.setLoadRules("none");
    assert.deepEqual(session.stage.unavailable(), []);

    // Within the budget, everything loads.
    await session.loadPayloads(["/"], [], "withDescendants", { bytes: 1024, size: source.size });
    assert.deepEqual(session.stage.loadSet(), ["/World/Rock", "/World/Tree"]);
  });
});
