// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a field needs to say an input was refused: numbers parsed strictly.
// (A stage's refusal is returned by the store's edit: `store.test.ts`.)
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseNumber } from "../src/model/refusal.ts";

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
