// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, UndoHistory, loadLayerstack } from "../dist/index.js";
import type { Edit, EditReceipt, Layerstack, Stage } from "../dist/index.js";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

const SCENE = `#usda 1.0
def Xform "World" {
    double size = 2
    double spin = 0
    def Sphere "Rock" {
        double radius = 1
    }
}
`;

function open(): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: text.encode(SCENE) });
}

/** The root layer's USDA bytes. */
function bytes(stage: Stage): string {
  return new TextDecoder().decode(stage.exportLayer(stage.root, "usda"));
}

function set(path: string, value: number): Edit {
  return { type: "setValue", path, value: { type: "double", value } };
}

function double(value: number): unknown {
  return { kind: "scalar", type: "double", value };
}

function apply(stage: Stage, ...edits: Edit[]): EditReceipt {
  return stage.apply(edits, stage.root);
}

/** Runs `f` and returns the `LayerstackError` it throws. */
function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

/** A deterministic xorshift, so a failing sequence replays. */
function rng(seed: number): (n: number) => number {
  let state = seed;
  return (n) => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) % n;
  };
}

/** A random edit of the scene: values, new prims and attributes, removals; some are refused. */
function randomEdit(below: (n: number) => number): Edit {
  const prims = ["/World", "/World/Rock", "/World/Pebble", "/World/Rock/Moss"];
  const prim = prims[below(prims.length)]!;
  const attribute = `${prim}.${["size", "spin", "radius"][below(3)]!}`;
  switch (below(6)) {
    case 0:
      return { type: "definePrim", path: prim, typeName: "Sphere" };
    case 1:
      return { type: "removePrim", path: prim };
    case 2:
      return { type: "clearValue", path: attribute };
    default:
      return set(attribute, below(100));
  }
}

describe("UndoHistory", () => {
  it("undoes and redoes to byte-identical layers, steps alone and grouped", () => {
    let undos = 0;
    let groups = 0;
    for (let seed = 1; seed <= 12; seed += 1) {
      using stage = open();
      using history = new UndoHistory(stage);
      const below = rng(seed * 7919);
      // The root's bytes after each undo step, the current state last,
      // and the bytes the redo steps get back to, the next last.
      const done = [bytes(stage)];
      const undone: string[] = [];
      for (let action = 0; action < 40; action += 1) {
        const choice = below(10);
        if (choice < 6) {
          let receipt: EditReceipt;
          try {
            receipt = apply(stage, randomEdit(below), ...(below(2) === 0 ? [randomEdit(below)] : []));
          } catch (error) {
            assert.ok(error instanceof LayerstackError);
            continue;
          }
          const recorded = history.record("edit", receipt);
          assert.equal(receipt.released, true, "the history takes the receipt");
          if (recorded.kind === "step") {
            done.push(bytes(stage));
            undone.length = 0;
          } else if (recorded.kind === "grouped") {
            undone.length = 0;
          }
        } else if (choice === 6 && history.groupDepth === 0) {
          const before = history.undoSteps.length;
          history.group("group", () => {
            for (let edit = 0; edit < 3; edit += 1) {
              try {
                history.record("in group", apply(stage, randomEdit(below)));
              } catch (error) {
                assert.ok(error instanceof LayerstackError);
              }
            }
          });
          if (history.undoSteps.length > before) {
            done.push(bytes(stage));
            undone.length = 0;
            groups += 1;
            assert.ok(history.nextUndo!.receipts >= 1);
          }
        } else if (choice < 9 && history.canUndo) {
          history.undo();
          undone.push(done.pop()!);
          assert.equal(bytes(stage), done[done.length - 1], `seed ${seed}: undo restores the bytes`);
          undos += 1;
        } else if (history.canRedo) {
          history.redo();
          done.push(undone.pop()!);
          assert.equal(bytes(stage), done[done.length - 1], `seed ${seed}: redo restores the bytes`);
        }
        assert.equal(history.undoSteps.length, done.length - 1);
        assert.equal(history.redoSteps.length, undone.length);
      }
      while (history.canRedo) history.redo();
      const end = bytes(stage);
      while (history.canUndo) history.undo();
      assert.equal(bytes(stage), done[0], `seed ${seed}: back to the start`);
      while (history.canRedo) history.redo();
      assert.equal(bytes(stage), end, `seed ${seed}: forward to the end`);
    }
    assert.ok(undos > 40 && groups > 5, `${undos} undos, ${groups} groups`);
  });

  it("undoes a group atomically, and reports a stale step without skipping it", () => {
    using stage = open();
    using history = new UndoHistory(stage);
    const start = bytes(stage);
    history.group("define and set radius", () => {
      history.record("define", apply(stage, { type: "definePrim", path: "/World/Ball", typeName: "Sphere" }));
      history.record("set radius", apply(stage, set("/World/Ball.radius", 2)));
      history.record("resize", apply(stage, set("/World.size", 3)));
    });
    assert.deepEqual(history.undoSteps, [{ label: "define and set radius", layers: [stage.root], receipts: 3 }]);

    // Not recorded: the slot the newest edit wrote changes.
    const outside = apply(stage, set("/World.size", 4));
    const after = bytes(stage);
    const stale = failure(() => history.undo());
    assert.equal(stale.code, "history.stale");
    assert.equal(stale.operation, "undo");
    assert.match(stale.message, /^undo: "define and set radius" is stale: /);
    assert.ok(stale.cause instanceof LayerstackError && stale.cause.code === "edit.stale");
    assert.equal(bytes(stage), after, "nothing was undone, not even the prim");
    assert.equal(history.undoSteps.length, 1, "the step stays");
    assert.equal(history.canUndo, true);

    outside.undo().release();
    const step = history.nextUndo;
    const undone = history.undo();
    assert.equal(history.nextRedo, step, "the same step, on the redo stack");
    assert.equal(bytes(stage), start, "all three undone as one");
    assert.deepEqual(undone.changes.removed, ["/World/Ball"]);
    assert.deepEqual(history.redoSteps.map((step) => step.label), ["define and set radius"]);
    history.redo();
    assert.deepEqual(stage.attribute("/World/Ball.radius")?.get(), double(2));
    assert.equal(failure(() => history.redo()).code, "history.empty");
  });

  it("abandons a group whose function throws, and nests groups", () => {
    using stage = open();
    using history = new UndoHistory(stage);
    const start = bytes(stage);
    assert.throws(
      () =>
        history.group("broken", () => {
          history.record("size", apply(stage, set("/World.size", 7)));
          history.beginGroup("left open");
          history.record("spin", apply(stage, set("/World.spin", 7)));
          throw new Error("tool failed");
        }),
      /tool failed/,
    );
    assert.equal(bytes(stage), start, "what the group applied is undone");
    assert.equal(history.groupDepth, 0);
    assert.equal(history.undoSteps.length, 0);

    history.beginGroup("outer");
    history.record("size", apply(stage, set("/World.size", 5)));
    const afterSize = bytes(stage);
    history.group("inner", () => {
      history.record("spin", apply(stage, set("/World.spin", 5)));
    });
    assert.equal(history.groupDepth, 1);
    assert.equal(history.openGroup, "outer");
    assert.equal(failure(() => history.undo()).code, "history.group_open");
    assert.equal(history.canUndo, false);
    history.beginGroup("dropped");
    history.record("radius", apply(stage, set("/World/Rock.radius", 5)));
    const abandoned = history.abandonGroup();
    assert.equal(abandoned?.released, true);
    assert.deepEqual(stage.attribute("/World/Rock.radius")?.get(), double(1));
    assert.deepEqual(history.endGroup(), { kind: "step", dropped: null });
    assert.deepEqual(history.undoSteps, [{ label: "outer", layers: [stage.root], receipts: 2 }]);
    assert.notEqual(bytes(stage), afterSize);
    history.undo();
    assert.equal(bytes(stage), start);
    assert.equal(failure(() => history.endGroup()).code, "history.no_group");
    assert.equal(failure(() => history.abandonGroup()).code, "history.no_group");
    history.beginGroup("empty");
    assert.deepEqual(history.endGroup(), { kind: "nothing", dropped: null });
    assert.equal(history.redoSteps.length, 1, "an empty group keeps the redo stack");
  });

  it("bounds the steps and reports each one it drops", () => {
    using stage = open();
    using history = new UndoHistory(stage, { limit: 2 });
    assert.equal(history.limit, 2);
    assert.equal(history.record("size 3", apply(stage, set("/World.size", 3))).dropped, null);
    history.record("size 4", apply(stage, set("/World.size", 4)));
    const recorded = history.record("size 5", apply(stage, set("/World.size", 5)));
    assert.equal(recorded.kind, "step");
    assert.equal(recorded.dropped?.label, "size 3");
    assert.equal(history.dropped, 1);
    assert.deepEqual(
      history.undoSteps.map((step) => step.label),
      ["size 4", "size 5"],
    );
    history.undo();
    history.undo();
    assert.equal(failure(() => history.undo()).code, "history.empty");
    assert.deepEqual(stage.attribute("/World.size")?.get(), double(3), "the dropped step stays applied");
    assert.equal(history.nextRedo?.label, "size 4");
    assert.equal(failure(() => new UndoHistory(stage, { limit: -1 })).code, "argument.invalid");
  });

  it("unwinds to a mark, and refuses a mark whose step is gone", () => {
    using stage = open();
    using history = new UndoHistory(stage);
    const start = bytes(stage);
    const empty = history.mark();
    history.record("size", apply(stage, set("/World.size", 3)));
    const afterSize = bytes(stage);
    const mark = history.mark();
    assert.equal(history.unwindTo(mark), null);
    history.record("spin", apply(stage, set("/World.spin", 1)));
    history.record("radius", apply(stage, set("/World/Rock.radius", 4)));
    const unwound = history.unwindTo(mark);
    assert.deepEqual(unwound?.changes.changedInfoOnly, ["/World", "/World/Rock"]);
    assert.equal(bytes(stage), afterSize);
    assert.deepEqual(history.undoSteps.map((step) => step.label), ["size"]);
    assert.equal(history.canRedo, false, "unwound steps are forgotten");

    history.undo();
    assert.equal(failure(() => history.unwindTo(mark)).code, "history.mark_gone");
    history.redo();
    history.unwindTo(empty);
    assert.equal(bytes(stage), start);
    history.record("size", apply(stage, set("/World.size", 6)));
    const before = history.mark();
    history.reset();
    assert.equal(history.undoSteps.length, 0);
    assert.equal(failure(() => history.unwindTo(before)).code, "history.mark_gone");
    using other = new UndoHistory(stage);
    assert.equal(failure(() => other.unwindTo(before)).code, "argument.invalid");
  });

  it("takes the receipts it records, and records nothing for a no-op", () => {
    using stage = open();
    using other = open();
    using history = new UndoHistory(stage);
    const receipt = apply(stage, { type: "clearValue", path: "/World/Rock.size" });
    assert.deepEqual(receipt.layers, [], "nothing was authored to clear");
    assert.deepEqual(history.record("no-op", receipt), { kind: "nothing", dropped: null });
    assert.equal(receipt.released, true);
    const foreign = apply(other, set("/World.size", 8));
    assert.equal(failure(() => history.record("foreign", foreign)).code, "receipt.affinity");
    assert.equal(foreign.released, false, "a refused receipt stays the caller's");
    const spent = apply(stage, set("/World.size", 9));
    spent.undo().release();
    assert.equal(failure(() => history.record("spent", spent)).code, "receipt.released");
  });
});
