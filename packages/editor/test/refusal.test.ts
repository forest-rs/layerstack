// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a field needs to say an input was refused: numbers parsed strictly,
// and a stage refusal (a store notice) read back as the reason.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { parseNumber, refusedBy } from "../src/model/refusal.ts";

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

describe("refusedBy", () => {
  it("is the new notice's message when the author pushed one", () => {
    const notices: { message: string }[] = [{ message: "old" }];
    assert.equal(refusedBy(notices, () => notices.push({ message: "value.range" })), "value.range");
  });
  it("is undefined when it pushed none, whatever was there before", () => {
    assert.equal(
      refusedBy([{ message: "old" }], () => {}),
      undefined,
    );
  });
});
