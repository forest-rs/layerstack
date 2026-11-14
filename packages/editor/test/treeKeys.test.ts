// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a key on a focused prim row does: F frames (react-aria's
// type-to-find would otherwise take it), Delete deletes, F2 and Enter
// rename, and a letter that continues a search stays the tree's.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { rowAction, rowAfterRemoval, treeRefusal } from "../src/app/treeKeys.ts";
import { harness, sampleSource } from "./support/storeHarness.ts";

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
  it("opens the row's menu on Shift+F10 and the Menu key", () => {
    assert.equal(rowAction(press("F10", { shiftKey: true }), false), "menu");
    assert.equal(rowAction(press("ContextMenu"), false), "menu");
    assert.equal(rowAction(press("F10"), false), null);
    assert.equal(rowAction(press("F10", { shiftKey: true, metaKey: true }), false), null);
  });
  it("clears the selection on Escape", () => {
    assert.equal(rowAction(press("Escape"), false), "deselect");
    assert.equal(rowAction(press("Escape", { shiftKey: true }), false), null);
  });
  it("leaves other keys to the tree", () => {
    for (const key of ["a", "ArrowDown", " ", "Backspace"]) assert.equal(rowAction(press(key), false), null, key);
    assert.equal(rowAction(press("Enter", { shiftKey: true }), false), null);
  });
});

describe("rowAfterRemoval", () => {
  const paths = ["/World", "/World/Lamp", "/World/Robot", "/World/Robot/Arm", "/World/Robot/Arm/Hand", "/World/Top", "/Looks"];
  it("is the next row after the removed prim and its descendants", () => {
    assert.equal(rowAfterRemoval(paths, "/World/Lamp"), "/World/Robot");
    assert.equal(rowAfterRemoval(paths, "/World/Robot"), "/World/Top");
    assert.equal(rowAfterRemoval(paths, "/World"), "/Looks");
  });
  it("is the row before when nothing follows", () => {
    assert.equal(rowAfterRemoval(paths, "/Looks"), "/World/Top");
    assert.equal(rowAfterRemoval(["/World", "/World/Robot", "/World/Robot/Arm"], "/World/Robot"), "/World");
  });
  it("is nothing for the only row, or a row not listed", () => {
    assert.equal(rowAfterRemoval(["/World"], "/World"), null);
    assert.equal(rowAfterRemoval(paths, "/Missing"), null);
  });
});

describe("treeRefusal", () => {
  it("says a refused drag and a refused Delete key in the same place, each for what it was", async () => {
    // The store's refusal of a delete while edits go inside a variant: what the tree says under its rows.
    const h = await harness();
    await h.open(sampleSource());
    h.store.editInsideVariant("/World/Robot", "finish", "painted");
    const why = h.store.delete("/World/Rock");
    assert.ok(why !== undefined);
    assert.match(treeRefusal("delete", why), /^Not deleted: Renaming and deleting edit the opinions of the stage's own layer stack/);
    assert.equal(treeRefusal("move", "edits go inside a variant."), "Not moved to another parent: edits go inside a variant.");
    assert.equal(treeRefusal("reorder", "the children of /World do not take that order."), "Not reordered: the children of /World do not take that order.");
  });
});
