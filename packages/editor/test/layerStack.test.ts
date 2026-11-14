// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The pure parts of the Layers, Stage and Health tabs: the layer stack as
// a tree in strength order, how an asset path anchors and is written,
// offsets, the stage metadata's units and sources, and the health report.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { healthReport, issueKind, wantingPrims } from "../src/model/health.ts";
import {
  anchoredIdentifier,
  ancestorsOf,
  composeOffsets,
  IDENTITY,
  layerStack,
  movedSublayers,
  offsetMeaning,
  offsetText,
  offsetUsda,
  relativeAsset,
  type SublayerEntry,
  sublayerCandidates,
  sublayerIndex,
} from "../src/model/layerStack.ts";
import { defaultPrimPath, fieldSource, inheritedFrom, metadataLayer, unitPreset, UNIT_PRESETS } from "../src/model/stageMetadata.ts";

/** A layer as the tree reads one. */
interface FakeLayer {
  readonly identifier: string | null;
  readonly readOnly: boolean;
}

const layer = (identifier: string | null, readOnly = false): FakeLayer => ({ identifier, readOnly });
const entry = (asset: string, offset = 0, scale = 1): SublayerEntry => ({ asset, layerOffset: { offset, scale } });

describe("asset paths", () => {
  it("anchor beside the layer that authors them, a bare path as a `./` one: the host has no search paths", () => {
    assert.equal(anchoredIdentifier("set/root.usda", "./lib.usda"), "set/lib.usda");
    assert.equal(anchoredIdentifier("set/root.usda", "../shared/lib.usda"), "shared/lib.usda");
    assert.equal(anchoredIdentifier("set/root.usda", "lib.usda"), "set/lib.usda");
    assert.equal(anchoredIdentifier("root.usda", "./a/./b/../c.usda"), "a/c.usda");
    assert.equal(anchoredIdentifier("root.usda", "../b.usda"), "../b.usda");
    assert.equal(anchoredIdentifier(null, "./lib.usda"), "lib.usda");
    assert.equal(anchoredIdentifier("set/root.usda", "/abs/./lib.usda"), "/abs/lib.usda");
    assert.equal(anchoredIdentifier("set/root.usda", "https://host/lib.usda"), "https://host/lib.usda");
    // As the host's storage identifies them (`identify` in `layerstack_wasm/src/storage.rs`): the root's slash and a URL's host are kept.
    assert.equal(anchoredIdentifier("/a/scene.usda", "./lib.usda"), "/a/lib.usda");
    assert.equal(anchoredIdentifier("/a/scene.usda", "../../c.usda"), "/c.usda");
    assert.equal(anchoredIdentifier("https://host/set/scene.usda", "../tex/d.usda"), "https://host/tex/d.usda");
    assert.equal(anchoredIdentifier("https://host/set/scene.usda", "../../../d.usda"), "https://host/d.usda");
  });

  it("anchor inside a package at the member that authors them", () => {
    assert.equal(anchoredIdentifier("set.usdz[geo/rock.usda]", "./moss.usda"), "set.usdz[geo/moss.usda]");
    assert.equal(anchoredIdentifier("set.usdz[geo/rock.usda]", "moss.usda"), "set.usdz[geo/moss.usda]");
  });

  it("are written from one layer to another as relative paths", () => {
    assert.equal(relativeAsset("scene.usda", "library.usda"), "./library.usda");
    assert.equal(relativeAsset("scene.usda", "assets/robot.usda"), "./assets/robot.usda");
    assert.equal(relativeAsset("assets/robot.usda", "library.usda"), "../library.usda");
    assert.equal(relativeAsset("a/b/c.usda", "a/d/e.usda"), "../d/e.usda");
    assert.equal(relativeAsset(null, "shot.usda"), "./shot.usda");
    // What it writes anchors back to the file.
    for (const [from, to] of [
      ["a/b/c.usda", "a/d/e.usda"],
      ["scene.usda", "assets/robot.usda"],
      ["assets/robot.usda", "library.usda"],
    ] as const) {
      assert.equal(anchoredIdentifier(from, relativeAsset(from, to)), to);
    }
  });
});

describe("layer offsets", () => {
  it("are explained as the parent layer's time, which is what they author, not the stage's", () => {
    assert.equal(
      offsetMeaning({ offset: 5, scale: 3 }, "a.usda"),
      "A sample at time t in the sublayer is at 3 × t + 5 in a.usda's time. The stage reads a.usda through its own offsets in turn, and scales between layers whose timeCodesPerSecond differ.",
    );
    assert.match(offsetMeaning({ offset: -4, scale: 1 }, "root.usda"), /is at t - 4 in root\.usda's time/);
    assert.match(offsetMeaning(IDENTITY, "root.usda"), /is at t in root\.usda's time/);
  });

  it("compose outer after inner, and are said only when they change time", () => {
    assert.deepEqual(composeOffsets({ offset: 10, scale: 2 }, { offset: 5, scale: 0.5 }), { offset: 20, scale: 1 });
    assert.deepEqual(composeOffsets(IDENTITY, { offset: 3, scale: 1 }), { offset: 3, scale: 1 });
    assert.equal(offsetText(IDENTITY), null);
    assert.equal(offsetText({ offset: 10, scale: 1 }), "offset 10");
    assert.equal(offsetText({ offset: 0, scale: 2 }), "scale 2");
    assert.equal(offsetText({ offset: -4, scale: 0.5 }), "offset -4, scale 0.5");
    assert.equal(offsetUsda(IDENTITY), "");
    assert.equal(offsetUsda({ offset: 10, scale: 2 }), " (offset = 10; scale = 2)");
  });
});

describe("the layer stack", () => {
  const root = layer("scene.usda");
  const session = layer(null);
  const library = layer("library.usda");
  const shot = layer("shots/shot.usda");
  const anim = layer("shots/anim.usda");
  const robot = layer("assets/robot.usda");
  const subs = new Map<FakeLayer, SublayerEntry[]>([
    [root, [entry("./shots/shot.usda", 10), entry("./library.usda"), entry("./missing.usda")]],
    [shot, [entry("./anim.usda", 0, 2)]],
    [session, [entry("./override.usda")]],
  ]);
  const input = (muted: readonly string[] = []) => ({
    root,
    session,
    held: [root, session, library, shot, anim, robot],
    sublayers: (each: FakeLayer) => subs.get(each) ?? [],
    muted: (each: FakeLayer | string) => muted.includes(typeof each === "string" ? each : (each.identifier ?? "")),
  });

  it("is in strength order: the session layer's stack, then the root's, each layer's sublayers after it", () => {
    const tree = layerStack(input());
    assert.deepEqual(
      tree.rows.map((row) => [row.identifier, row.role, row.depth, row.strength, row.state]),
      [
        [null, "session", 0, 1, "read"],
        ["override.usda", "sublayer", 1, 0, "unread"],
        ["scene.usda", "root", 0, 2, "read"],
        ["shots/shot.usda", "sublayer", 1, 3, "read"],
        ["shots/anim.usda", "sublayer", 2, 4, "read"],
        ["library.usda", "sublayer", 1, 5, "read"],
        ["missing.usda", "sublayer", 1, 0, "unread"],
      ],
    );
    // The layers outside the stack are reached through arcs.
    assert.deepEqual(tree.reached, [robot]);
  });

  it("carries each sublayer's parent, place and offsets, composed down the tree", () => {
    const rows = layerStack(input()).rows;
    const animRow = rows.find((row) => row.identifier === "shots/anim.usda")!;
    assert.equal(animRow.parent, shot);
    assert.equal(animRow.asset, "./anim.usda");
    assert.deepEqual([animRow.index, animRow.siblings], [0, 1]);
    assert.deepEqual(animRow.offset, { offset: 0, scale: 2 });
    assert.deepEqual(animRow.composed, { offset: 10, scale: 2 });
    const libraryRow = rows.find((row) => row.identifier === "library.usda")!;
    assert.deepEqual([libraryRow.parent, libraryRow.index, libraryRow.siblings], [root, 1, 3]);
    assert.deepEqual(ancestorsOf(rows, animRow), ["shots/shot.usda", "scene.usda"]);
  });

  it("shows a muted layer without its sublayers, which do not compose", () => {
    const tree = layerStack(input(["shots/shot.usda"]));
    const identifiers = tree.rows.map((row) => row.identifier);
    assert.ok(!identifiers.includes("shots/anim.usda"));
    assert.equal(tree.rows.find((row) => row.identifier === "shots/shot.usda")!.state, "muted");
    // The muted one keeps its row in the stack; its sublayer, held but composing nowhere, is listed with the layers outside the stack.
    assert.deepEqual(tree.reached, [anim, robot]);
  });

  it("stops at a cycle", () => {
    const a = layer("a.usda");
    const b = layer("b.usda");
    const cyclic = new Map([
      [a, [entry("./b.usda")]],
      [b, [entry("./a.usda")]],
    ]);
    const tree = layerStack({ root: a, session: null, held: [a, b], sublayers: (each) => cyclic.get(each) ?? [], muted: () => false });
    assert.deepEqual(
      tree.rows.map((row) => [row.identifier, row.state]),
      [
        ["a.usda", "read"],
        ["b.usda", "read"],
        ["a.usda", "cycle"],
      ],
    );
  });

  it("reorders a layer's sublayers by moving one, offsets kept", () => {
    const list = [entry("./a.usda", 1), entry("./b.usda"), entry("./c.usda")];
    assert.deepEqual(movedSublayers(list, 2, -1)?.map((each) => each.asset), ["./a.usda", "./c.usda", "./b.usda"]);
    assert.deepEqual(movedSublayers(list, 0, 1), [entry("./b.usda"), entry("./a.usda", 1), entry("./c.usda")]);
    assert.equal(movedSublayers(list, 0, -1), null);
    assert.equal(movedSublayers(list, 2, 1), null);
  });

  it("finds the sublayer a row named where it is now, by its asset path when the index names another", () => {
    const list = [entry("./a.usda", 1), entry("./b.usda"), entry("./c.usda")];
    assert.equal(sublayerIndex(list, 1, "./b.usda"), 1);
    // One was inserted in front, or removed, since the row was built.
    assert.equal(sublayerIndex(list, 0, "./b.usda"), 1);
    assert.equal(sublayerIndex(list, 5, "./c.usda"), 2);
    // Gone: no other is taken for it.
    assert.equal(sublayerIndex(list, 1, "./d.usda"), -1);
    // Listed twice (a file may): the index tells them apart while it still names one, and nothing does once it does not.
    const twice = [entry("./a.usda"), entry("./b.usda"), entry("./a.usda", 4)];
    assert.equal(sublayerIndex(twice, 2, "./a.usda"), 2);
    assert.equal(sublayerIndex(twice, 1, "./a.usda"), -1);
  });

  it("offers the source's layers as sublayers, saying why one cannot be added", () => {
    const candidates = sublayerCandidates(["assets/robot.usda", "library.usda", "scene.usda", "shots/shot.usda"], "shots/shot.usda", ["shots/anim.usda"], ["scene.usda"]);
    assert.deepEqual(
      candidates.map((each) => [each.identifier, each.asset, each.refusal]),
      [
        ["assets/robot.usda", "../assets/robot.usda", null],
        ["library.usda", "../library.usda", null],
        ["scene.usda", "../scene.usda", "above it in the stack: a cycle"],
        ["shots/shot.usda", "./shot.usda", "the layer itself"],
      ],
    );
    assert.equal(sublayerCandidates(["shots/anim.usda"], "shots/shot.usda", ["shots/anim.usda"], [])[0]!.refusal, "listed already");
  });
});

describe("stage metadata", () => {
  it("names the common units, within a layer's precision", () => {
    assert.equal(unitPreset(0.01)?.name, "centimeters");
    assert.equal(unitPreset(1)?.short, "m");
    assert.equal(unitPreset(0.0254)?.name, "inches");
    assert.equal(unitPreset(0.30480000001)?.name, "feet");
    assert.equal(unitPreset(0.5), null);
    // Every preset is told from the others.
    for (const preset of UNIT_PRESETS) assert.equal(unitPreset(preset.metersPerUnit), preset);
  });

  it("is read from the session layer over the root, else falls back", () => {
    const root = new Set(["upAxis", "startTimeCode"]);
    assert.equal(fieldSource("upAxis", new Set(["upAxis"]), root), "session");
    assert.equal(fieldSource("upAxis", new Set(), root), "root");
    assert.equal(fieldSource("startTimeCode", null, root), "root");
    assert.equal(fieldSource("metersPerUnit", null, root), "fallback");
  });

  it("is authored where edits go when the stage reads it there, else in the root", () => {
    const root = layer("scene.usda");
    const session = layer(null);
    const library = layer("library.usda");
    const name = (each: FakeLayer) => each.identifier ?? "the session layer";
    assert.deepEqual(metadataLayer(root, root, null, name), { layer: root, note: null });
    assert.deepEqual(metadataLayer(session, root, session, name), { layer: session, note: null });
    const elsewhere = metadataLayer(library, root, session, name);
    assert.equal(elsewhere.layer, root);
    assert.match(elsewhere.note!, /Edits go to library\.usda, but the stage reads its metadata only from the root and session layers: this is authored in scene\.usda/);
    assert.match(metadataLayer(null, root, null, name).note!, /an edit target through an arc/);
    // A read-only root (a package) takes none: the session layer does.
    const packaged = layer("scene.usdz", true);
    assert.equal(metadataLayer(packaged, packaged, session, name).layer, session);
  });
});

describe("health", () => {
  const root = layer("scene.usda");
  const empty = { prims: 12, errors: [], unresolved: [], unavailable: [], wanting: new Map(), lastNamespaceEdit: null, layers: 3 };

  it("says what it checked when nothing is wrong", () => {
    const report = healthReport<FakeLayer>(empty);
    assert.equal(report.count, 0);
    assert.deepEqual(report.groups, []);
    assert.equal(report.checked, "No composition errors among 12 prims; every asset of 3 layers resolved; no move or delete to repair.");
    const moved = healthReport<FakeLayer>({ ...empty, prims: 1, layers: 1, lastNamespaceEdit: { label: "move /A to /B", unrepaired: [] } });
    assert.equal(moved.checked, "No composition errors among 1 prim; every asset of 1 layer resolved; move /A to /B left no path naming the old one.");
  });

  it("groups errors by prim in namespace order, the layer stacks' first, and marks those prims", () => {
    const report = healthReport<FakeLayer>({
      ...empty,
      errors: [
        { code: "composition.unresolved_asset", message: "b", prim: "/World/Robot" },
        { code: "composition.unresolved_sublayer", message: "s", prim: null },
        { code: "composition.unresolved_prim_path", message: "a", prim: "/World/A" },
        { code: "composition.arc_cycle", message: "c", prim: "/World/Robot" },
        { code: "composition.unresolved_asset", message: "d", prim: "/World/A/Child" },
      ],
    });
    assert.deepEqual(
      report.groups.map((group) => [group.prim, group.issues.map((issue) => issue.message)]),
      [
        [null, ["s"]],
        ["/World/A", ["a"]],
        ["/World/A/Child", ["d"]],
        ["/World/Robot", ["b", "c"]],
      ],
    );
    assert.deepEqual([...report.errorPrims].sort(), ["/World/A", "/World/A/Child", "/World/Robot"]);
    assert.equal(report.count, 5);
  });

  it("names the prims composed without a missing asset from the loop's record, and counts what waits and what was left unrepaired", () => {
    const wanting = wantingPrims([
      { assets: [{ identifier: "assets/gone.usda", prims: ["/World/B"] }] },
      { assets: [{ identifier: "assets/gone.usda", prims: ["/World/A", "/World/B"] }, { identifier: "x.usda", prims: [] }] },
    ]);
    assert.deepEqual(wanting.get("assets/gone.usda"), ["/World/B", "/World/A"]);
    const report = healthReport<FakeLayer>({
      ...empty,
      wanting,
      unresolved: [{ assetPath: "./assets/gone.usda", anchor: root, usedBy: "reference", identifier: "assets/gone.usda", reason: "io.not_found" }],
      unavailable: [{ assetPath: "./later.usda", anchor: root, usedBy: "sublayer", identifier: "later.usda", reason: null, prims: ["/"] }],
      lastNamespaceEdit: { label: "move /World/A to /World/C", unrepaired: [{ layer: root, spec: "/Other", field: "targets", path: "/World/A" }] },
    });
    assert.deepEqual(report.missing[0]!.prims, ["/World/A", "/World/B"]);
    assert.deepEqual(report.waiting[0]!.prims, ["/"]);
    assert.equal(report.count, 2);
  });

  it("says what an error code is in words", () => {
    assert.equal(issueKind("composition.unresolved_asset"), "unresolved asset");
    assert.equal(issueKind("other"), "other");
  });

  it("reads the default prim from the root layer alone, as a root prim's name or a prim's path", () => {
    // `UsdStage::GetDefaultPrim` reads the root layer: a session layer's opinion names nothing.
    assert.equal(fieldSource("defaultPrim", new Set(["defaultPrim"]), new Set(["defaultPrim"])), "root");
    assert.equal(fieldSource("defaultPrim", new Set(["defaultPrim"]), new Set()), "fallback");
    assert.equal(fieldSource("upAxis", new Set(["upAxis"]), new Set(["upAxis"])), "session");
    assert.equal(defaultPrimPath("World"), "/World");
    assert.equal(defaultPrimPath("World/Robot"), "/World/Robot");
    assert.equal(defaultPrimPath("/World/Robot"), "/World/Robot");
    assert.equal(defaultPrimPath("Größe"), "/Größe");
    assert.equal(defaultPrimPath(""), null);
    assert.equal(defaultPrimPath("World//Robot"), null);
    assert.equal(defaultPrimPath("World.size"), null);
    assert.equal(defaultPrimPath("../World"), null);
  });


  it("says a rate no layer authors is the frame rate one does, before it is the fallback", () => {
    const sources = (entries: Record<string, "session" | "root" | "fallback">) => new Map(Object.entries(entries));
    assert.deepEqual(inheritedFrom("timeCodesPerSecond", sources({ timeCodesPerSecond: "fallback", framesPerSecond: "root" })), { key: "framesPerSecond", source: "root" });
    assert.deepEqual(inheritedFrom("timeCodesPerSecond", sources({ timeCodesPerSecond: "fallback", framesPerSecond: "session" })), { key: "framesPerSecond", source: "session" });
    // The root's rate is over the session layer's frame rate.
    assert.equal(inheritedFrom("timeCodesPerSecond", sources({ timeCodesPerSecond: "root", framesPerSecond: "session" })), null);
    assert.equal(inheritedFrom("timeCodesPerSecond", sources({ timeCodesPerSecond: "fallback", framesPerSecond: "fallback" })), null);
    assert.equal(inheritedFrom("framesPerSecond", sources({ timeCodesPerSecond: "root", framesPerSecond: "fallback" })), null);
  });

});
