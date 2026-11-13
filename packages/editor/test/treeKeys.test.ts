// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a key on a focused prim row does: F frames (react-aria's
// type-to-find would otherwise take it), Delete deletes, F2 and Enter
// rename, and a letter that continues a search stays the tree's.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { rowAction } from "../src/app/treeKeys.ts";

const press = (key: string, mods: { metaKey?: boolean; ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean } = {}) => ({
  key,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});

describe("rowAction", () => {
  it("frames on F, unless a search is under way", () => {
    assert.equal(rowAction(press("f"), false), "frame");
    assert.equal(rowAction(press("f"), true), null);
  });
  it("leaves F with a modifier alone", () => {
    for (const mods of [{ metaKey: true }, { ctrlKey: true }, { altKey: true }, { shiftKey: true }]) assert.equal(rowAction(press("f", mods), false), null);
  });
  it("deletes on Delete and renames on F2 and Enter", () => {
    assert.equal(rowAction(press("Delete"), false), "delete");
    assert.equal(rowAction(press("F2"), false), "rename");
    assert.equal(rowAction(press("Enter"), false), "rename");
  });
  it("deletes on Command+Backspace, where a keyboard has no Delete key", () => {
    assert.equal(rowAction(press("Backspace", { metaKey: true }), false), "delete");
    assert.equal(rowAction(press("Backspace", { metaKey: true, shiftKey: true }), false), null);
  });
  it("leaves other keys, and Shift+F10, to the tree", () => {
    for (const key of ["a", "ArrowDown", " ", "Backspace"]) assert.equal(rowAction(press(key), false), null, key);
    assert.equal(rowAction(press("F10", { shiftKey: true }), false), null);
    assert.equal(rowAction(press("Enter", { shiftKey: true }), false), null);
  });
});
