// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A player reading value clips in a browser: it fetches only the clips a
// query needs, as the stage names them, and once they are supplied its
// answers are OpenUSD's.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { AssetSource, Layerstack, Stage } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();

// usd-core 26.11's answers for `clip_schedules/root.usda`
// (`clip_schedules_oracle.py`); `loaded` is what OpenUSD had opened by
// then, cumulatively.
const base = new URL("../../../layerstack_conformance/fixtures/clip_schedules/", import.meta.url);
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8")) as {
  open: string[];
  times: number[];
  vary: { attribute: string; loaded: string[]; might_vary: boolean }[];
  get: { attribute: string; loaded: string[]; values: (number | "error" | null)[] }[];
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

/** Opens the fixture and resolves its layer stack, which names no clip. */
async function open(source: AssetSource): Promise<Stage> {
  const stage = layerstack.open({ identifier: "root.usda", bytes: readFileSync(new URL("root.usda", base)) });
  await stage.resolve(source);
  return stage;
}

/** The clip layers the stage read, each within what OpenUSD had opened. */
function assertReadWithin(stage: Stage, loaded: readonly string[], label: string): void {
  for (const layer of stage.layers()) {
    if (layer.identifier === null || ["root.usda", "weak.usda"].includes(layer.identifier)) continue;
    assert.ok(loaded.includes(layer.identifier), `${label}: read ${layer.identifier}, which OpenUSD did not open`);
  }
}

describe("a player preparing value clips", () => {
  it("answers whether each attribute might vary as OpenUSD does, once the untimed plan is loaded", async () => {
    const source = directory();
    using stage = await open(source);
    assertReadWithin(stage, oracle.open, "open");
    // Clips are not arcs: the stage wants none to compose, and reads only
    // those a query needs.
    assert.deepEqual(stage.unavailable(), []);
    for (const vary of oracle.vary) {
      // Before the host supplies what the plan names, the answer is unknown.
      const before = stage.prepareClipQuery(vary.attribute);
      if (before.unavailable.length > 0) {
        assert.equal(stage.attribute(vary.attribute)?.mightBeTimeVarying(), true, `${vary.attribute}: unknown is conservative`);
        assert.equal(stage.valueSourcesMightBeTimeVarying([vary.attribute])[0], true);
      }
      const prepared = await stage.resolveClipQuery(vary.attribute, source);
      assert.deepEqual(prepared.failures, [], vary.attribute);
      assert.deepEqual(prepared.unavailable, [], vary.attribute);
      assert.ok(
        prepared.demands.every((demand) => demand.status === "loaded" || demand.status === "missing"),
        vary.attribute,
      );
      assert.equal(stage.attribute(vary.attribute)?.mightBeTimeVarying(), vary.might_vary, vary.attribute);
      assertReadWithin(stage, vary.loaded, vary.attribute);
    }
  });

  it("reads each attribute at each time as OpenUSD does, fetching only the clips it needs", async () => {
    const source = directory();
    using stage = await open(source);
    for (const get of oracle.get) {
      for (const [index, time] of oracle.times.entries()) {
        const expected = get.values[index] ?? null;
        const label = `${get.attribute} at ${time}`;
        const prepared = await stage.resolveClipQuery(get.attribute, source, { time });
        if (expected === "error") {
          // OpenUSD raises where the read needs a clip that does not parse;
          // the preparation reports the failed read.
          assert.ok(prepared.failures.length > 0 && !prepared.complete, label);
          assert.equal(prepared.failures[0]?.code, "io.rejected");
          continue;
        }
        const value = stage.attribute(get.attribute)?.get(time);
        const number = value?.kind === "scalar" ? (value.value as number) : null;
        if (expected === null || number === null) {
          assert.equal(number, expected, label);
        } else {
          assert.ok(Math.abs(number - expected) < 1e-6, `${label}: ${number} != ${expected}`);
        }
      }
      assertReadWithin(stage, get.loaded, get.attribute);
    }
  });

  it("names the clips it lacks, reads them once supplied, and says why it needed each", () => {
    using stage = layerstack.open({ identifier: "root.usda", bytes: readFileSync(new URL("root.usda", base)) });
    const first = stage.prepareClipQuery("/SingleTwo.size", { time: 0 });
    assert.equal(first.complete, false);
    assert.deepEqual(
      first.unavailable.map((asset) => [asset.identifier, asset.assetPath, asset.role, asset.owner, asset.clipSet]),
      [["manifest.usda", "./manifest.usda", "manifest", "/SingleTwo", "default"]],
    );
    assert.equal(first.unavailable[0]?.anchor, stage.root);
    assert.equal(first.loadAttempts, 1);
    stage.supply("manifest.usda", readFileSync(new URL("manifest.usda", base)));
    const second = stage.prepareClipQuery("/SingleTwo.size", { time: 0 });
    assert.deepEqual(
      second.unavailable.map((asset) => asset.identifier),
      ["clipFive.usda"],
    );
    assert.deepEqual(
      second.demands.map((demand) => [demand.assetPath, demand.reason, demand.status]),
      [
        ["./manifest.usda", "manifest", "loaded"],
        ["./clipFive.usda", "activeInterval", "unanswered"],
      ],
    );
    stage.supply("clipFive.usda", readFileSync(new URL("clipFive.usda", base)));
    const done = stage.prepareClipQuery("/SingleTwo.size", { time: 0 });
    assert.equal(done.complete, true);
    assert.deepEqual(done.unavailable, []);
    assert.equal(done.error, null);
    // The clips are read, never wanted as an arc is: the stage still wants
    // only the sublayer this test did not resolve.
    assert.deepEqual(
      stage.unavailable().map((asset) => asset.identifier),
      ["weak.usda"],
    );
    assert.ok(stage.layers().some((layer) => layer.identifier === "clipFive.usda"));
  });

  it("completes with a gap for a clip confirmed absent", () => {
    using stage = layerstack.open({ identifier: "root.usda", bytes: readFileSync(new URL("root.usda", base)) });
    stage.supply("manifest.usda", readFileSync(new URL("manifest.usda", base)));
    stage.supply("clipFive.usda", null);
    const prepared = stage.prepareClipQuery("/SingleTwo.size", { time: 0 });
    assert.equal(prepared.complete, true);
    assert.equal(prepared.demands.find((demand) => demand.assetPath === "./clipFive.usda")?.status, "missing");
  });

  it("fails as resolve does when a fetch fails", async () => {
    using stage = layerstack.open({ identifier: "root.usda", bytes: readFileSync(new URL("root.usda", base)) });
    const failing = mockSource({});
    await assert.rejects(
      stage.resolveClipQuery("/SingleTwo.size", failing),
      (error: unknown) =>
        error instanceof LayerstackError && error.code === "resolve.fetch" && failing.asked.includes("manifest.usda"),
    );
    assert.throws(
      () => stage.prepareClipQuery("/SingleTwo"),
      (error: unknown) => error instanceof LayerstackError && error.code === "path.invalid",
    );
  });
});
