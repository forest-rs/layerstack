// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The session's asset loop, timed: each run's rounds and each asset's way
// from the stage naming it to the stage reading it, as the waterfall
// shows them.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import { Session, type Fetcher } from "../src/model/session.ts";
import { summarize, type AssetTiming, type LoopRun } from "../src/model/timeline.ts";

const layerstack = await loadLayerstack();
const text = new TextEncoder();

const FILES: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    subLayers = [@./lib.usda@]
)
def Xform "World" (
    variants = {
        string look = "plain"
    }
    prepend variantSets = "look"
)
{
    def "Robot" (
        prepend references = @./robot.usda@
    )
    {
    }
    def "Lamp" (
        prepend references = @./missing.usda@
    )
    {
    }
    variantSet "look" = {
        "plain" {
        }
        "rock" (
            prepend references = @./rock.usda@
        ) {
        }
    }
}
`,
  "lib.usda": `#usda 1.0
def Scope "Lib"
{
}
`,
  "robot.usda": `#usda 1.0
(
    defaultPrim = "Robot"
)
def Xform "Robot" (
    prepend payload = @./parts.usda@
)
{
}
`,
  "parts.usda": `#usda 1.0
(
    defaultPrim = "Parts"
)
def Xform "Parts"
{
}
`,
  "rock.usda": `#usda 1.0
(
    defaultPrim = "Rock"
)
def Sphere "Rock"
{
}
`,
};

/** The files above, each a few milliseconds away; `failing` throws. */
function fetcher(failing: string | null = null): Fetcher {
  return async (identifier) => {
    await new Promise((resolve) => setTimeout(resolve, 2));
    if (identifier === failing) throw new Error("connection reset");
    const file = FILES[identifier];
    return file === undefined ? null : text.encode(file);
  };
}

function asset(run: LoopRun, identifier: string): AssetTiming {
  const found = run.assets.find((candidate) => candidate.identifier === identifier);
  assert.ok(found !== undefined, `the run has ${identifier}`);
  return found;
}

describe("the session's timed asset loop", () => {
  it("times the open round by round, from want to read", async () => {
    using session = await Session.open(layerstack, "scene.usda", fetcher());
    assert.equal(session.loops.runs.length, 1);
    const run = session.loops.runs[0]!;
    assert.equal(run.cause, "open");
    assert.equal(run.error, null);
    assert.deepEqual(
      run.rounds.map((round) => round.index),
      [0, 1, 2],
    );
    assert.deepEqual(
      run.assets.map((candidate) => [candidate.identifier, candidate.round, candidate.roles.join(","), candidate.outcome]).sort(),
      [
        ["scene.usda", 0, "root", "read"],
        ["lib.usda", 1, "sublayer", "read"],
        ["robot.usda", 1, "reference", "read"],
        ["missing.usda", 1, "reference", "absent"],
        ["parts.usda", 2, "payload", "read"],
      ].sort(),
    );
    assert.deepEqual(asset(run, "robot.usda").prims, ["/World/Robot"]);
    assert.deepEqual(asset(run, "robot.usda").mentions, [{ assetPath: "./robot.usda", anchor: "scene.usda", usedBy: "reference" }]);
    assert.equal(asset(run, "parts.usda").mentions[0]?.anchor, "robot.usda");
    assert.equal(asset(run, "missing.usda").bytes, null);
    assert.equal(asset(run, "missing.usda").source, "absent");
    for (const timing of run.assets) {
      const round = run.rounds.find((candidate) => candidate.index === timing.round)!;
      const marks = [run.startedAt, timing.wantedAt, timing.fetchStart, timing.fetchEnd, timing.suppliedAt, round.readStart, round.readEnd, run.endedAt];
      assert.ok(marks.every((mark) => mark !== null), `${timing.identifier} has every mark`);
      for (let index = 1; index < marks.length; index += 1) {
        assert.ok(marks[index - 1]! <= marks[index]!, `${timing.identifier}'s marks are in order: ${marks.join(", ")}`);
      }
    }
    assert.ok(run.settle !== null, "the last `unavailable()` is timed");
    const summary = summarize(run);
    assert.equal(summary.assets, 5);
    assert.equal(summary.rounds, 2);
    assert.equal(summary.absent, 1);
    assert.equal(summary.bytes, ["scene.usda", "lib.usda", "robot.usda", "parts.usda"].reduce((sum, name) => sum + text.encode(FILES[name]!).byteLength, 0));
    assert.ok(summary.fetching > 0 && summary.stage > 0);
    assert.ok(summary.fetching + summary.stage <= summary.wall + 1e-6);
    // Each layer read has its own read, inside its round's `loadSupplied`;
    // the root, read by `Layerstack.open`, and the absent asset have none.
    for (const name of ["lib.usda", "robot.usda", "parts.usda"]) {
      const timing = asset(run, name);
      const round = run.rounds.find((candidate) => candidate.index === timing.round)!;
      const read = timing.read;
      assert.ok(read !== null && read.start !== null && read.end !== null && read.own !== null, `${name} has its read`);
      assert.equal(read.bytes, text.encode(FILES[name]!).byteLength);
      assert.ok(read.prims > 0, `${name} made prims`);
      assert.ok(round.readStart! <= read.start && read.start <= read.end && read.end <= round.readEnd!, `${name} is read in its round`);
      assert.ok(read.own <= read.end - read.start + 1e-6);
    }
    assert.equal(asset(run, "scene.usda").read, null);
    assert.equal(asset(run, "missing.usda").read, null);
    // Each round's compositions follow its reads, inside the call.
    for (const round of run.rounds.filter((candidate) => candidate.index > 0)) {
      assert.ok(round.reading !== null && round.compositions.length > 0, `round ${round.index} reports its reading and composing`);
      for (const composition of round.compositions) {
        assert.ok(round.readStart! <= composition.start && composition.start <= composition.end && composition.end <= round.readEnd!);
      }
    }
    assert.ok(summary.reading + summary.composing <= summary.stage + 1e-6);
  });

  it("keeps a run for each action that fetched, and none for one that did not", async () => {
    using session = await Session.open(layerstack, "scene.usda", fetcher());
    await session.selectVariant("/World", "look", "rock");
    assert.deepEqual(
      session.loops.runs.map((run) => run.cause),
      ["open", "look=rock"],
    );
    const run = session.loops.runs[1]!;
    assert.deepEqual(
      run.rounds.map((round) => round.index),
      [1],
    );
    assert.deepEqual(asset(run, "rock.usda").prims, ["/World"]);
    assert.equal(asset(run, "rock.usda").outcome, "read");
    await session.undo();
    assert.equal(session.loops.runs.length, 2, "an undo that wants nothing records no run");
    assert.equal(session.loops.current, null);
  });

  it("reads what a round's other fetches returned before a failed fetch fails the open", async () => {
    await assert.rejects(Session.open(layerstack, "scene.usda", fetcher("robot.usda")), (error: Error) => {
      assert.equal(error.message, "fetching robot.usda failed");
      assert.equal((error.cause as Error).message, "connection reset");
      return true;
    });
  });
});
