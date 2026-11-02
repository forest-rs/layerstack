// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which draws the viewport records into render bundles, and when a bundle
// is recorded again: every change to a bundled draw records its bundle, a
// draw is direct until its compile is done, and a draw time moves stays
// direct.
import assert from "node:assert/strict";
import { test } from "node:test";
import { DrawBundles, type Bundle, type Holder } from "../src/viewport/bundles.ts";

/** A scene or bundle that lists what it holds; a bundle counts its recordings asked for. */
class Fake implements Bundle<string> {
  readonly held = new Set<string>();
  version = 0;
  set needsUpdate(value: boolean) {
    if (value) this.version += 1;
  }
  get needsUpdate(): boolean {
    return false;
  }
  add(object: string): void {
    assert.ok(!this.held.has(object), `${object} added twice`);
    this.held.add(object);
  }
  remove(object: string): void {
    assert.ok(this.held.delete(object), `${object} removed but not held`);
  }
}

function setup(capacity = 2): { scene: Holder<string> & { held: Set<string> }; bundles: Fake[]; draws: DrawBundles<string> } {
  const scene = new Fake();
  const bundles: Fake[] = [];
  const draws = new DrawBundles<string>(
    scene,
    () => {
      const bundle = new Fake();
      bundles.push(bundle);
      return bundle;
    },
    capacity,
  );
  return { scene, bundles, draws };
}

const always = () => true;

test("a new draw is direct until the compile that met it is done", () => {
  const { scene, bundles, draws } = setup();
  draws.drawDirectly("a");
  assert.deepEqual([...scene.held], ["a"]);
  assert.equal(bundles.length, 0);
  const met = draws.compiling();
  assert.deepEqual(met, ["a"]);
  // Met by no later compile: nothing waits.
  assert.deepEqual(draws.compiling(), []);
  draws.settle(met, always);
  assert.deepEqual([...scene.held], []);
  assert.deepEqual([...bundles[0]!.held], ["a"]);
  assert.equal(bundles[0]!.version, 1);
  assert.ok(draws.isBundled("a"));
});

test("bundles fill to their capacity, and a new one takes the rest", () => {
  const { bundles, draws } = setup(2);
  for (const object of ["a", "b", "c"]) draws.drawDirectly(object);
  draws.settle(draws.compiling(), always);
  assert.deepEqual(
    bundles.map((bundle) => [...bundle.held]),
    [["a", "b"], ["c"]],
  );
  assert.deepEqual(draws.counts, { bundles: 2, bundled: 3, direct: 0, pending: 0, dynamic: 0 });
});

test("touching a bundled draw records its bundle alone; a direct draw's touch records nothing", () => {
  const { bundles, draws } = setup(2);
  for (const object of ["a", "b", "c"]) draws.drawDirectly(object);
  draws.settle(draws.compiling(), always);
  const before = bundles.map((bundle) => bundle.version);
  draws.touch("c");
  assert.deepEqual(
    bundles.map((bundle, i) => bundle.version - before[i]!),
    [0, 1],
  );
  draws.drawDirectly("d");
  draws.touch("d");
  assert.deepEqual(
    bundles.map((bundle, i) => bundle.version - before[i]!),
    [0, 1],
  );
  draws.touchAll();
  assert.deepEqual(
    bundles.map((bundle, i) => bundle.version - before[i]!),
    [1, 2],
  );
});

test("new materials take a draw out of its bundle until they compile", () => {
  const { scene, bundles, draws } = setup();
  draws.drawDirectly("a");
  draws.settle(draws.compiling(), always);
  const version = bundles[0]!.version;
  draws.drawDirectly("a");
  assert.equal(bundles[0]!.version, version + 1, "its bundle is recorded without it");
  assert.deepEqual([...scene.held], ["a"]);
  assert.ok(!draws.isBundled("a"));
  draws.settle(draws.compiling(), always);
  assert.ok(draws.isBundled("a"));
});

test("a draw changed again during its compile waits for the next one", () => {
  const { draws } = setup();
  draws.drawDirectly("a");
  const met = draws.compiling();
  draws.drawDirectly("a");
  draws.settle(met, always);
  assert.ok(!draws.isBundled("a"));
  draws.settle(draws.compiling(), always);
  assert.ok(draws.isBundled("a"));
});

test("a draw removed during its compile is not bundled", () => {
  const { scene, bundles, draws } = setup();
  draws.drawDirectly("a");
  const met = draws.compiling();
  draws.remove("a");
  draws.settle(met, always);
  assert.equal(bundles.length, 0);
  assert.deepEqual([...scene.held], []);
});

test("an ineligible draw (transparent) stays direct", () => {
  const { scene, draws } = setup();
  draws.drawDirectly("glass");
  draws.drawDirectly("wall");
  draws.settle(draws.compiling(), (object) => object !== "glass");
  assert.deepEqual([...scene.held], ["glass"]);
  assert.ok(draws.isBundled("wall"));
});

test("a draw time moves leaves its bundle for good", () => {
  const { scene, bundles, draws } = setup();
  draws.drawDirectly("a");
  draws.settle(draws.compiling(), always);
  draws.keepDirect("a");
  assert.ok(draws.isDynamic("a"));
  assert.deepEqual([...scene.held], ["a"]);
  assert.deepEqual([...bundles[0]!.held], []);
  // New materials later (a selection) do not bundle it again.
  draws.drawDirectly("a");
  draws.settle(draws.compiling(), always);
  assert.ok(!draws.isBundled("a"));
  assert.deepEqual(draws.counts, { bundles: 1, bundled: 0, direct: 1, pending: 0, dynamic: 1 });
});

test("removing lets a draw go from wherever it is, and its bundle's room is used again", () => {
  const { scene, bundles, draws } = setup(1);
  draws.drawDirectly("a");
  draws.drawDirectly("b");
  draws.settle(draws.compiling(), always);
  draws.drawDirectly("c");
  draws.remove("a");
  draws.remove("c");
  assert.deepEqual([...scene.held], []);
  assert.deepEqual(
    bundles.map((bundle) => [...bundle.held]),
    [[], ["b"]],
  );
  draws.drawDirectly("d");
  draws.settle(draws.compiling(), always);
  assert.deepEqual(
    bundles.map((bundle) => [...bundle.held]),
    [["d"], ["b"]],
  );
  // Removing what is not held does nothing.
  draws.remove("a");
});

test("a bundle holds at least one draw", () => {
  assert.throws(() => setup(0), RangeError);
});
