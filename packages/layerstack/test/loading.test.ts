// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { AssetSource, Changes, Layerstack, LoadOutcome, LoadPolicy, LoadRule, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();

// usd-core 26.11's answers, call by call, for `payload_loading/scene.usda`
// opened with every payload loaded or none (`payload_loading_oracle.py`).
const base = new URL("../../../layerstack_conformance/fixtures/payload_loading/", import.meta.url);
type State = {
  rules: [string, LoadRule["rule"]][];
  loadSet: string[];
  loadable: string[];
  usedPayloadLayers: string[];
  prims: Record<string, [boolean, boolean, boolean]>;
};
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8")) as {
  paths: string[];
  cases: {
    load: "all" | "none";
    opened: State;
    steps: { call: unknown[]; result: string | null; refused: [string, string][]; after: State }[];
  }[];
};

/** The fixture directory as an asset source, remembering what it was asked for. */
function directory(): AssetSource & { readonly fetched: Set<string> } {
  const fetched = new Set<string>();
  return {
    fetched,
    async fetch(identifier: string) {
      fetched.add(identifier);
      try {
        return new Uint8Array(readFileSync(new URL(identifier, base)));
      } catch {
        return null;
      }
    },
  };
}

async function open(load: "all" | "none", source: AssetSource): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: readFileSync(new URL("scene.usda", base)) }, { load });
  await stage.resolve(source);
  return stage;
}

/** What the oracle records, read through the package. */
function state(stage: Stage): Omit<State, "usedPayloadLayers"> {
  const prims: State["prims"] = {};
  for (const path of oracle.paths) {
    const prim = stage.prim(path);
    prims[path] = prim === null ? [false, false, false] : [true, prim.loaded, prim.hasAuthoredPayloads];
  }
  return {
    rules: stage.loadRules().map(({ path, rule }) => [path, rule]),
    loadSet: stage.loadSet(),
    loadable: stage.findLoadable(),
    prims,
  };
}

function expected({ usedPayloadLayers: _, ...rest }: State): Omit<State, "usedPayloadLayers"> {
  return rest;
}

/** Makes the step's call: its result as OpenUSD's spells it, and the paths it refused with their codes, sorted. */
function call(stage: Stage, step: unknown[]): [string | null, [string, string][]] {
  const [kind] = step as [string];
  let outcome: LoadOutcome = { changed: false, refused: [] };
  if (kind === "load") {
    const [, path, policy] = step as [string, string, LoadPolicy];
    outcome = stage.load(path, policy);
  } else if (kind === "unload") outcome = stage.unload(step[1] as string);
  else if (kind === "loadAndUnload") {
    const [, load, unload, policy] = step as [string, string[], string[], LoadPolicy];
    outcome = stage.loadAndUnload(load, unload, policy);
  } else if (kind === "setLoadRules") {
    stage.setLoadRules((step[1] as [string, LoadRule["rule"]][]).map(([path, rule]) => ({ path, rule })));
  } else throw new Error(`no call ${kind}`);
  const refused = outcome.refused.map(({ path, code }): [string, string] => [path, code]).sort();
  const result = kind === "load" ? (stage.prim(step[1] as string) === null ? "invalid" : "prim") : null;
  return [result, refused];
}

describe("payload loading", () => {
  for (const testCase of oracle.cases) {
    it(`answers as OpenUSD does, call by call, opened with ${testCase.load} payloads loaded`, async () => {
      const source = directory();
      using stage = await open(testCase.load, source);
      assert.deepEqual(state(stage), expected(testCase.opened), "open");
      if (testCase.load === "none") {
        assert.deepEqual([...source.fetched], [], "no payload's asset is asked for");
      }
      for (const step of testCase.steps) {
        const [result, refused] = call(stage, step.call);
        assert.equal(result, step.result, JSON.stringify(step.call));
        assert.deepEqual(refused, step.refused, JSON.stringify(step.call));
        // The assets the call newly wants, fetched as an open fetches them.
        await stage.resolve(source);
        assert.deepEqual(state(stage), expected(step.after), JSON.stringify(step.call));
        for (const layer of step.after.usedPayloadLayers) {
          assert.ok(source.fetched.has(layer), `${layer} fetched by ${JSON.stringify(step.call)}`);
        }
      }
    });
  }

  it("asks for a payload's asset once it loads, and listeners hear the prims it resyncs", async () => {
    const source = directory();
    using stage = await open("none", source);
    assert.deepEqual(stage.unavailable(), []);
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));

    assert.deepEqual(stage.load("/World/Tree", "withoutDescendants"), { changed: true, refused: [] });
    assert.equal(heard.length, 1, "one recomposition, heard before load returned");
    assert.ok(heard[0]!.resynced.includes("/World/Tree"));
    assert.deepEqual(
      stage.unavailable().map(({ identifier, usedBy, prims }) => [identifier, usedBy, prims]),
      [["tree.usda", "payload", ["/World/Tree"]]],
    );
    await stage.resolve(source);
    // As usd-core 26.11 orders them: the payload's children first.
    assert.deepEqual(stage.children("/World/Tree"), ["/World/Tree/Trunk", "/World/Tree/Branch", "/World/Tree/Local"]);
    assert.deepEqual([...source.fetched], ["tree.usda"], "the branch's leaf is not loaded");

    const before = heard.length;
    assert.equal(stage.load("/World/Tree", "withoutDescendants").changed, false, "already loaded: no change");
    assert.equal(stage.unload("/World/Rock").changed, false, "already unloaded: no change");
    assert.equal(heard.length, before, "no change is heard");
  });

  it("skips the paths OpenUSD refuses, applying the rest, and throws only for malformed arguments", async () => {
    using stage = await open("none", directory());
    const failure = (f: () => unknown): string => {
      try {
        f();
      } catch (error) {
        assert.ok(error instanceof LayerstackError, String(error));
        return error.code;
      }
      return "no error";
    };
    assert.deepEqual(stage.loadAndUnload(["/World/Rock", "/World/Off"], ["/__Prototype_1"]), {
      changed: true,
      refused: [
        { path: "/World/Off", code: "inactive" },
        { path: "/__Prototype_1", code: "prototype" },
      ],
    });
    assert.deepEqual(stage.loadRules(), [
      { path: "/", rule: "none" },
      { path: "/World/Rock", rule: "all" },
    ]);
    assert.deepEqual(stage.load("/Nowhere/Deep"), { changed: false, refused: [{ path: "/Nowhere/Deep", code: "not_on_stage" }] });
    assert.equal(failure(() => stage.load("/World.size")), "path.invalid");
    assert.equal(failure(() => stage.load("/World", "everything" as LoadPolicy)), "argument.invalid");
    assert.equal(failure(() => stage.setLoadRules([{ path: "/World", rule: "some" as "all" }])), "argument.invalid");
    assert.equal(failure(() => layerstack.open({ identifier: "a.usda", bytes: new Uint8Array() }, { load: [{ path: "World", rule: "all" }] })), "path.invalid");
    assert.equal(stage.setLoadRules("all"), true);
    assert.deepEqual(stage.loadRules(), []);
  });
});
