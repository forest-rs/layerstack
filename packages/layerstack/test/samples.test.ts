// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Sample times, counts, bracketing samples and unions against usd-core
// 26.11's answers for `layerstack_conformance/fixtures/sample_brackets`
// (`sample_brackets_oracle.py`, which the Rust conformance tests replay
// too): layer offsets, blocks, an animation block, a spline, sparse array
// edits, a schema fallback and value clips.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { loadLayerstack } from "../dist/index.js";
import type { AssetSource, BracketingSamples, Layerstack, Stage, TimeInterval } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();
const base = new URL("../../../layerstack_conformance/fixtures/sample_brackets/", import.meta.url);

type Interval = [number | null, number | null, boolean, boolean];
type Bracket = [number, number] | "none" | "error" | false;
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8")) as {
  intervals: Interval[];
  times: number[];
  attributes: {
    attribute: string;
    samples: number[];
    count: number;
    in: number[][];
    bracket: Bracket[];
    nan: Bracket;
  }[];
  unions: { attributes: string[]; all: number[]; in: number[][] }[];
};

// Where layerstack deliberately answers differently (see
// `tests/conformance/sample_brackets.rs`): OpenUSD lets a weaker sample
// through when the dense sample masking it lies on an open interval bound;
// layerstack's interval reads are always runs of the full grid.
const divergences = new Set(["/Mixed.e 3", "union /Sparse.e /Mixed.e 3"]);

const source: AssetSource = {
  async fetch(identifier: string) {
    try {
      return new Uint8Array(readFileSync(new URL(identifier, base)));
    } catch {
      return null;
    }
  },
};

async function open(): Promise<Stage> {
  const stage = layerstack.open({ identifier: "root.usda", bytes: readFileSync(new URL("root.usda", base)) });
  await stage.resolve(source);
  for (const { attribute } of oracle.attributes) await stage.resolveClipQuery(attribute, source);
  return stage;
}

function interval([min, max, minClosed, maxClosed]: Interval): TimeInterval {
  return { ...(min === null ? {} : { min }), ...(max === null ? {} : { max }), minClosed, maxClosed };
}

function contains([min, max, minClosed, maxClosed]: Interval, time: number): boolean {
  const lo = min ?? -Infinity;
  const hi = max ?? Infinity;
  return (time > lo || (minClosed && time === lo)) && (time < hi || (maxClosed && time === hi));
}

function expectedIn(label: string, index: number, cpp: number[], full: number[]): number[] {
  if (!divergences.has(`${label} ${index}`)) return cpp;
  const grid = full.filter((time) => contains(oracle.intervals[index] as Interval, time));
  assert.notDeepEqual(cpp, grid, `${label}: OpenUSD agrees now; drop the divergence`);
  return grid;
}

function expected(bracket: Bracket): BracketingSamples {
  if (Array.isArray(bracket)) return { kind: "samples", lower: bracket[0], upper: bracket[1] };
  return bracket === "none" ? { kind: "noSamples" } : { kind: "noValue" };
}

describe("sample-time reads", () => {
  it("match OpenUSD per attribute", async () => {
    using stage = await open();
    for (const reads of oracle.attributes) {
      const attribute = stage.attribute(reads.attribute);
      assert.ok(attribute, reads.attribute);
      assert.deepEqual(attribute.timeSamples(), reads.samples, `${reads.attribute} samples`);
      assert.equal(attribute.sampleCount(), reads.count, `${reads.attribute} count`);
      oracle.intervals.forEach((bounds, index) => {
        assert.deepEqual(
          attribute.timeSamplesInInterval(interval(bounds)),
          expectedIn(reads.attribute, index, reads.in[index] as number[], reads.samples),
          `${reads.attribute} in ${JSON.stringify(bounds)}`,
        );
      });
      oracle.times.forEach((time, index) => {
        assert.deepEqual(
          attribute.bracketingSamples(time),
          expected(reads.bracket[index] as Bracket),
          `${reads.attribute} at ${time}`,
        );
      });
      assert.deepEqual(attribute.bracketingSamples(NaN), expected(reads.nan), `${reads.attribute} at NaN`);
    }
    // One read for every attribute, as a renderer brackets a frame.
    const paths = oracle.attributes.map((reads) => reads.attribute);
    assert.deepEqual(
      stage.bracketingSamples(paths, 7),
      oracle.attributes.map((reads) => expected(reads.bracket[oracle.times.indexOf(7)] as Bracket)),
    );
    assert.deepEqual(stage.bracketingSamples(["/Nowhere.a"], 1), [{ kind: "noValue" }]);
  });

  it("unions match OpenUSD", async () => {
    using stage = await open();
    for (const union of oracle.unions) {
      const label = `union ${union.attributes.join(" ")}`;
      assert.deepEqual(stage.unionedTimeSamples(union.attributes), union.all, label);
      oracle.intervals.forEach((bounds, index) => {
        assert.deepEqual(
          stage.unionedTimeSamples(union.attributes, interval(bounds)),
          expectedIn(label, index, union.in[index] as number[], union.all),
          `${label} in ${JSON.stringify(bounds)}`,
        );
      });
    }
  });
});
