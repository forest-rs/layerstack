// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The pure parts of what leaves the page: file names, what a package
// still asks the host for, why a root is edited in a session layer, the
// lines a change touched, and USDA's tokens.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { Layer, UnpackagedAsset } from "@forest-rs/layerstack";
import { fileName, fileNames, isPackage, isSaveKey, packageWants, saveKeyOpens, sessionLayerReason, suppliable } from "../src/model/files.ts";
import { drawnLine, lineChanges, MAX_LINE_CHARS } from "../src/model/layerText.ts";
import { usdaTokens } from "../src/model/usdaSyntax.ts";

describe("file names", () => {
  it("are an identifier's last segment, a package member's own, or the session layer's", () => {
    assert.equal(fileName("assets/robot.usda"), "robot.usda");
    assert.equal(fileName("set.usdz[geo/rock.usdc]"), "rock.usdc");
    assert.equal(fileName(null), "session.usda");
    assert.equal(fileName("assets/robot.usda", "usdc"), "robot.usdc");
    assert.equal(fileName("scene.usdz", "usdz"), "scene.usdz");
    assert.equal(fileName("scene", "usda"), "scene.usda");
  });

  it("of several layers are told apart by their folders where two would share one", () => {
    assert.deepEqual(fileNames(["scene.usda", "assets/robot.usda", "parts/robot.usda", null]), [
      "scene.usda",
      "assets_robot.usda",
      "parts_robot.usda",
      "session.usda",
    ]);
  });

  it("of several layers are never the same, whatever their folders are called", () => {
    // Joined by `_`, a folder's file takes the name another file already has.
    assert.deepEqual(fileNames(["a/model.usda", "b/model.usda", "a_model.usda"]), ["a_model.usda", "b_model.usda", "a_model-2.usda"]);
    // A layer above the root's folder beside one beneath it.
    assert.deepEqual(fileNames(["../a/model.usda", "a/model.usda"]), ["a_model.usda", "a_model-2.usda"]);
    // A package member keeps its extension.
    assert.deepEqual(fileNames(["set.usdz[geo/rock.usda]", "rock.usda"]), ["set.usdz_geo_rock.usda", "rock.usda"]);
    const names = fileNames(["a/b/model.usda", "a_b/model.usda", "a_b_model.usda", "/a/b/model.usda"]);
    assert.equal(new Set(names).size, names.length, JSON.stringify(names));
  });

  it("know a package by its extension", () => {
    assert.equal(isPackage("set/Model.USDZ"), true);
    assert.equal(isPackage("set/model.usda"), false);
  });
});

describe("a session layer", () => {
  const zip = new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0]);
  const text = new TextEncoder().encode("#usda 1.0\n");

  it("takes the edits of a package root, by its name or by its bytes", () => {
    assert.match(sessionLayerReason("model.usdz", text) ?? "", /read-only/);
    assert.match(sessionLayerReason("model.usd", zip) ?? "", /read-only/);
    assert.equal(sessionLayerReason("model.usda", text), null);
  });
});

describe("what a package asks of the host", () => {
  const layer = {} as Layer;
  const unresolved = (assetPath: string, reason: string): UnpackagedAsset => ({ layer, identifier: "scene.usda", assetPath, reason });

  it("reads the identifier from a dependency the host has not supplied, or let go", () => {
    assert.equal(suppliable("textures/checker.png: the host has not supplied it yet"), "textures/checker.png");
    assert.equal(suppliable("assets/chrome.usda: the host's bytes were released after the stage read them; supply them again"), "assets/chrome.usda");
    assert.equal(suppliable("textures/gone.png: the host confirmed this source absent"), null);
  });

  it("is each asset to fetch, every other tile of a UDIM set, and what no fetch resolves", () => {
    const wants = packageWants([
      unresolved("./textures/checker.png", "textures/checker.png: the host has not supplied it yet"),
      unresolved("./textures/tile.<UDIM>.png", "textures/tile.1001.png: the host has not supplied it yet"),
      unresolved("./gone.png", "gone.png: the host confirmed this source absent"),
    ]);
    assert.deepEqual(wants.fetch, ["textures/checker.png", "textures/tile.1001.png"]);
    assert.equal(wants.tiles.length, 99);
    assert.equal(wants.tiles[0], "textures/tile.1002.png");
    assert.equal(wants.tiles.at(-1), "textures/tile.1100.png");
    assert.deepEqual(
      wants.unresolved.map((each) => each.assetPath),
      ["./gone.png"],
    );
  });
});

describe("the lines a change touched", () => {
  const lines = (text: string) => text.split("\n");

  it("are none for the same text", () => {
    assert.deepEqual(lineChanges(lines("a\nb\nc"), lines("a\nb\nc")), { added: [], removedBefore: [], removed: 0 });
  });

  it("are the lines an edit added, wherever they are", () => {
    const before = lines('over "World"\n{\n}\n');
    const after = lines('over "World"\n{\n    over "Robot"\n    {\n        token visibility = "invisible"\n    }\n}\n');
    assert.deepEqual(lineChanges(before, after), { added: [2, 3, 4, 5], removedBefore: [], removed: 0 });
  });

  it("are a changed line, and where lines went", () => {
    assert.deepEqual(lineChanges(lines("a\nb\nc\nd"), lines("a\nB\nc")), { added: [1], removedBefore: [1, 3], removed: 2 });
    assert.deepEqual(lineChanges(lines("a\nb\nc"), lines("a\nc")), { added: [], removedBefore: [1], removed: 1 });
  });

  it("are two changes far apart, not everything between them", () => {
    const before = Array.from({ length: 200 }, (_, index) => `line ${index}`);
    const after = [...before];
    after[10] = "changed";
    after[190] = "changed too";
    assert.deepEqual(lineChanges(before, after).added, [10, 190]);
  });

  it("past the most differences traced, are everything between the common head and tail", () => {
    const before = ["head", "a", "b", "c", "tail"];
    const after = ["head", "x", "y", "z", "tail"];
    assert.deepEqual(lineChanges(before, after, 1), { added: [1, 2, 3], removedBefore: [4], removed: 3 });
  });
});

describe("the save shortcut", () => {
  const press = (key: string, held: Partial<Record<"metaKey" | "ctrlKey" | "shiftKey" | "altKey", boolean>> = {}) => ({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...held });

  it("is ⌘S or Ctrl+S, and nothing else", () => {
    assert.equal(isSaveKey(press("s", { metaKey: true })), true);
    assert.equal(isSaveKey(press("S", { ctrlKey: true })), true);
    assert.equal(isSaveKey(press("s")), false);
    assert.equal(isSaveKey(press("s", { metaKey: true, shiftKey: true })), false);
    assert.equal(isSaveKey(press("s", { metaKey: true, altKey: true })), false);
    assert.equal(isSaveKey(press("z", { metaKey: true })), false);
  });

  it("opens the save's confirmation with a scene open, and does nothing over a dialog", () => {
    const save = press("s", { metaKey: true });
    assert.equal(saveKeyOpens(save, true, false), true);
    assert.equal(saveKeyOpens(save, false, false), false, "nothing is open");
    assert.equal(saveKeyOpens(save, true, true), false, "a dialog is being answered: no second one on top of it");
    assert.equal(saveKeyOpens(press("s"), true, false), false);
  });
});

describe("a line as the Text tab draws it", () => {
  it("is whole when short, and its start with what is left out when it is a mesh's array", () => {
    assert.deepEqual(drawnLine("    double size = 1"), { text: "    double size = 1", more: 0 });
    // A mesh's points on one line of USDA: over a million characters, 600,000 tokens.
    const points = `    point3f[] points = [${Array.from({ length: 100_000 }, (_, index) => `(${index}.5, -1e-5, 2)`).join(", ")}]`;
    const drawn = drawnLine(points);
    assert.equal(drawn.text.length, MAX_LINE_CHARS);
    assert.equal(drawn.text.length + drawn.more, points.length);
    assert.ok(points.startsWith(drawn.text));
    assert.ok(usdaTokens(drawn.text).length < MAX_LINE_CHARS, "no more elements than a row can hold");
  });
});

describe("USDA's tokens", () => {
  const kinds = (line: string) => usdaTokens(line).filter((token) => token.kind !== "plain").map((token) => [token.kind, token.text]);

  it("rejoin to the line", () => {
    const line = '        prepend references = @./assets/robot.usda@</Robot> # the robot';
    assert.equal(usdaTokens(line).map((token) => token.text).join(""), line);
  });

  it("are keywords, types, strings, numbers, asset and scene paths and comments", () => {
    assert.deepEqual(kinds('def Xform "World" (\n'), [
      ["keyword", "def"],
      ["string", '"World"'],
    ]);
    assert.deepEqual(kinds("    uniform token[] xformOpOrder = [\"xformOp:translate\"]"), [
      ["keyword", "uniform"],
      ["type", "token[]"],
      ["string", '"xformOp:translate"'],
    ]);
    assert.deepEqual(kinds("double3 xformOp:translate = (3, -0.5, 1e-3)"), [
      ["type", "double3"],
      ["number", "3"],
      ["number", "-0.5"],
      ["number", "1e-3"],
    ]);
    assert.deepEqual(kinds("rel material:binding = </World/Looks/Paint> # bound"), [
      ["keyword", "rel"],
      ["path", "</World/Looks/Paint>"],
      ["comment", "# bound"],
    ]);
    assert.deepEqual(kinds("asset inputs:file = @./textures/tile.<UDIM>.png@"), [
      ["type", "asset"],
      ["asset", "@./textures/tile.<UDIM>.png@"],
    ]);
  });
});
