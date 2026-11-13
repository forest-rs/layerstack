// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which keys a focused widget keeps from the global shortcuts: text entry
// keeps all of them, but the hidden input behind a switch or a slider is not
// text entry; Space belongs to what presses, Left and Right to what moves.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { isTextEntry, type KeyTarget, widgetKeeps } from "../src/app/keys.ts";

/** An element of `tag` (and input `type`) inside ancestors with `roles`; `closest` knows the few selectors the rules use by their roles and tags. */
function element(tag: string, options: { type?: string; roles?: string[]; editable?: boolean } = {}): KeyTarget {
  const roles = options.roles ?? [];
  const self: KeyTarget = {
    tagName: tag.toUpperCase(),
    ...(options.type === undefined ? {} : { type: options.type }),
    isContentEditable: options.editable ?? false,
    closest(selector) {
      const wanted = selector.split(",").map((part) => part.trim());
      const matches = (part: string) => {
        if (part.startsWith("[contenteditable]")) return options.editable === true;
        const role = /^\[role="(.+)"\]$/.exec(part)?.[1];
        if (role !== undefined) return roles.includes(role);
        const typed = /^input\[type="(.+)"\]$/.exec(part)?.[1];
        if (typed !== undefined) return tag === "input" && options.type === typed;
        return part === tag;
      };
      return wanted.some(matches) ? self : null;
    },
  };
  return self;
}

describe("isTextEntry", () => {
  it("is text inputs, textareas and contenteditable", () => {
    assert.ok(isTextEntry(element("input", { type: "text" })));
    assert.ok(isTextEntry(element("input")));
    assert.ok(isTextEntry(element("input", { type: "search" })));
    assert.ok(isTextEntry(element("textarea")));
    assert.ok(isTextEntry(element("div", { editable: true })));
  });
  it("is not the hidden input of a switch or a slider, or a button", () => {
    for (const type of ["checkbox", "range", "radio", "button"]) assert.equal(isTextEntry(element("input", { type })), false, type);
    assert.equal(isTextEntry(element("button")), false);
    assert.equal(isTextEntry(null), false);
  });
});

describe("widgetKeeps", () => {
  it("keeps every key in text", () => {
    for (const key of ["f", " ", "Escape", "ArrowLeft"]) assert.ok(widgetKeeps(element("input", { type: "text" }), key), key);
  });
  it("leaves the shortcuts alive after a switch or a slider", () => {
    const hidden = [element("input", { type: "checkbox" }), element("input", { type: "range", roles: ["slider"] })];
    for (const target of hidden) for (const key of ["f", "Escape", "z"]) assert.equal(widgetKeeps(target, key), false, key);
  });
  it("lets Space play from a slider but toggle a switch", () => {
    assert.equal(widgetKeeps(element("input", { type: "range" }), " "), false);
    assert.ok(widgetKeeps(element("input", { type: "checkbox" }), " "));
  });
  it("keeps Space on buttons, tabs and rows", () => {
    assert.ok(widgetKeeps(element("button"), " "));
    assert.ok(widgetKeeps(element("div", { roles: ["tab"] }), " "));
    assert.ok(widgetKeeps(element("div", { roles: ["row"] }), " "));
    assert.equal(widgetKeeps(element("div"), " "), false);
  });
  it("keeps Left and Right inside tabs, lists, trees and selects", () => {
    for (const role of ["tablist", "grid", "tree", "treegrid", "listbox", "slider"]) assert.ok(widgetKeeps(element("div", { roles: [role] }), "ArrowLeft"), role);
    assert.ok(widgetKeeps(element("select"), "ArrowRight"));
    assert.equal(widgetKeeps(element("div"), "ArrowRight"), false);
    assert.equal(widgetKeeps(element("button"), "ArrowRight"), false);
  });
});
