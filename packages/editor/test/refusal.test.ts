// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a field needs to say an input was refused: numbers parsed strictly.
// (A stage's refusal is returned by the store's edit: `store.test.ts`.)
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { NOT_A_NUMBER, parseNumber, rowRefusal } from "../src/model/refusal.ts";

describe("parseNumber", () => {
  it("accepts finite numbers", () => {
    assert.equal(parseNumber("1.5"), 1.5);
    assert.equal(parseNumber(" -2 "), -2);
    assert.equal(parseNumber("1e3"), 1000);
  });
  it("refuses empty, words and non-finite numbers", () => {
    for (const text of ["", "  ", "abc", "Infinity", "NaN", "1,5"]) assert.equal(parseNumber(text), null, text);
  });
});

describe("a refusal said under its row", () => {
  it("names the field and why, under the row that caused it, until a commit is taken", () => {
    assert.deepEqual(rowRefusal("translate", "Translate X", NOT_A_NUMBER), { row: "translate", text: "Translate X: Not a number" });
    assert.equal(rowRefusal("translate", "Translate X", undefined), null);
  });
});
