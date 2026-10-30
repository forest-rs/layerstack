// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Layerstack, Stage } from "../dist/index.js";
import { bytes, mockSource, scene, usdz } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const decoder = new TextDecoder();

/** The fixture scene, every dependency resolved. */
async function resolvedScene(root: Uint8Array = bytes("scene.usda"), identifier = "scene.usda"): Promise<Stage> {
  const stage = layerstack.open({ identifier, bytes: root });
  await stage.resolve(mockSource(scene));
  assert.ok(stage.status().complete);
  return stage;
}

/** What the stage composes, as text: its flattened USDA. */
function composed(stage: Stage): string {
  const flat = stage.exportFlattened("usda");
  assert.ok(flat.lossless, JSON.stringify(flat.lost));
  return decoder.decode(flat.bytes);
}

async function failure(f: () => unknown): Promise<LayerstackError> {
  try {
    await f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

describe("saving", () => {
  it("writes an asset edited through its reference, and lists dirty layers it skips", async () => {
    using stage = await resolvedScene();
    const radius = stage.attribute("/Ball.radius")?.explain()?.opinions[0];
    assert.equal(radius?.layer.identifier, "parts/ball.usda");
    assert.ok(radius?.editTarget);
    stage.apply([{ type: "setValue", path: "/Ball.radius", value: { type: "double", value: 0.75 } }], radius.editTarget).release();
    const saved = stage.save();
    assert.deepEqual(saved.written.map((layer) => layer.identifier), ["parts/ball.usda"]);
    assert.match(decoder.decode(saved.written[0]?.bytes), /double radius = 0.75/);

    stage.apply([{ type: "setValue", path: "/Ball.radius", value: { type: "double", value: 1 } }], radius.editTarget).release();
    const sub = stage.layers().find((layer) => layer.identifier === "sub.usda");
    assert.ok(sub);
    stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 6 } }], sub).release();
    stage.muteLayer(sub);
    stage.apply([{ type: "removeReference", path: "/Ball", asset: "./parts/ball.usda", primPath: "/Ball" }]).release();
    const skipping = stage.save();
    assert.deepEqual(skipping.written.map((layer) => layer.identifier), ["scene.usda"]);
    assert.deepEqual(
      skipping.skipped.map(({ layer, reason }) => `${layer.identifier} ${reason}`).sort(),
      ["parts/ball.usda unused", "sub.usda muted"],
    );
    assert.equal(stage.isDirty(radius.layer), true, "left dirty");
    assert.deepEqual(stage.save(radius.layer).written.map((layer) => layer.identifier), ["parts/ball.usda"]);
  });

  it("hands the host each dirty layer, and reopening the saved bytes gives the same stage", async () => {
    using stage = await resolvedScene();
    stage
      .apply([
        { type: "setValue", path: "/Ball.size", value: { type: "double", value: 7 } },
        { type: "definePrim", path: "/Garden/Clover", typeName: "Xform" },
      ], stage.root)
      .release();
    assert.ok(stage.isDirty(stage.root));
    const before = layerstack.transfer().bytesOut;
    const saved = stage.save();
    assert.deepEqual(
      saved.written.map((layer) => layer.identifier),
      ["scene.usda"],
      "only the edited root was dirty",
    );
    assert.deepEqual(saved.failures, []);
    assert.deepEqual(saved.anonymous, []);
    assert.deepEqual(saved.skipped, []);
    const [root] = saved.written;
    assert.ok(root);
    assert.equal(root.layer, stage.root);
    assert.equal(saved.bytes, root.bytes.byteLength);
    assert.equal(layerstack.transfer().bytesOut - before, saved.bytes);
    assert.equal(stage.isDirty(stage.root), false);
    assert.match(decoder.decode(root.bytes), /double size = 7/);
    assert.deepEqual(stage.save().written, [], "nothing is dirty now");

    using reopened = await resolvedScene(root.bytes);
    assert.equal(composed(reopened), composed(stage));
    assert.deepEqual(reopened.attribute("/Ball.size")?.get(), { kind: "scalar", type: "double", value: 7 });
    assert.ok(reopened.prim("/Garden/Clover"));
  });

  it("saves one layer when asked, and refuses another stage's", async () => {
    using stage = await resolvedScene();
    const sub = stage.layers().find((layer) => layer.identifier === "sub.usda");
    assert.ok(sub);
    stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 6 } }], sub).release();
    stage.apply([{ type: "setActive", path: "/Garden", active: false }], stage.root).release();
    const saved = stage.save(sub);
    assert.deepEqual(
      saved.written.map((layer) => layer.identifier),
      ["sub.usda"],
    );
    assert.ok(stage.isDirty(stage.root), "the root was not asked for");
    using other = await resolvedScene();
    assert.equal((await failure(() => other.save(sub))).code, "layer.affinity");
  });

  it("lists the session layer as anonymous, and exports it", async () => {
    const pack = usdz([
      ["root.usda", '#usda 1.0\ndef Sphere "Ball" {\n    double radius = 1\n}\n'],
    ]);
    using stage = layerstack.open({ identifier: "ball.usdz", bytes: pack }, { editLayer: "session" });
    const session = stage.editLayer;
    assert.ok(session?.anonymous);
    stage.apply([{ type: "setValue", path: "/Ball.radius", value: { type: "double", value: 2 } }], session).release();
    const saved = stage.save();
    assert.deepEqual(saved.written, []);
    assert.deepEqual(saved.failures, [], "the package was not edited");
    assert.deepEqual(saved.anonymous, [], "a stage's save leaves its session layer alone");
    assert.deepEqual(stage.save(session).anonymous, [session]);
    const text = decoder.decode(stage.exportLayer(session, "usda"));
    assert.match(text, /double radius = 2/);
    assert.ok(stage.isDirty(session), "an export changes nothing");
    const packaged = stage.exportPackage();
    assert.deepEqual(packaged.omitted, [session]);
    assert.ok(packaged.bytes);
    using repackaged = layerstack.open({ identifier: "again.usdz", bytes: packaged.bytes });
    assert.deepEqual(repackaged.attribute("/Ball.radius")?.get(), { kind: "scalar", type: "double", value: 1 }, "the package as read");
    using flat = layerstack.open({ identifier: "flat.usda", bytes: stage.exportFlattened().bytes });
    assert.deepEqual(flat.attribute("/Ball.radius")?.get(), { kind: "scalar", type: "double", value: 2 }, "the flattened stage has the edit");
  });
});

describe("exporting", () => {
  it("exports a layer in each format, which reopens", async () => {
    using stage = await resolvedScene();
    for (const format of ["usda", "usdc"] as const) {
      const exported = stage.exportLayer(stage.root, format);
      using copy = await resolvedScene(exported, `copy.${format}`);
      assert.equal(composed(copy), composed(stage), format);
    }
    assert.match(decoder.decode(stage.exportLayer(stage.root, "usda")), /^#usda 1\.0/);
    assert.equal(decoder.decode(stage.exportLayer(stage.root, "usdc").subarray(0, 8)), "PXR-USDC");
    assert.equal((await failure(() => stage.exportLayer(stage.root, "usdz" as "usda"))).code, "argument.invalid");
  });

  it("flattens the composed stage in each format, which reopens with nothing unavailable", async () => {
    using stage = await resolvedScene();
    for (const format of ["usda", "usdc"] as const) {
      const flat = stage.exportFlattened(format);
      assert.ok(flat.lossless);
      using reopened = layerstack.open({ identifier: `flat.${format}`, bytes: flat.bytes });
      assert.deepEqual(reopened.unavailable(), [], "no arcs are left");
      assert.equal(composed(reopened), composed(stage), format);
      assert.deepEqual(reopened.attribute("/Ball.tag")?.get(), { kind: "scalar", type: "token", value: "trim" });
    }
  });

  it("packages a layered stage as a USDZ that reopens complete", async () => {
    using stage = await resolvedScene();
    stage.apply([{ type: "setValue", path: "/Ball.radius", value: { type: "double", value: 3 } }], stage.root).release();
    const packaged = stage.exportPackage();
    assert.ok(packaged.bytes);
    assert.equal(packaged.layers, 4, "the root, its sublayer and two referenced layers");
    assert.deepEqual(packaged.unresolved, []);
    assert.deepEqual(packaged.omitted, []);
    assert.ok(stage.isDirty(stage.root), "packaging saves nothing");
    using reopened = layerstack.open({ identifier: "scene.usdz", bytes: packaged.bytes });
    assert.ok(reopened.status().complete, "every layer is inside the package");
    assert.equal(composed(reopened), composed(stage), "with the unsaved edit");
  });

  it("reports what keeps a package from being made", async () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
    const packaged = stage.exportPackage();
    assert.equal(packaged.bytes, null);
    assert.deepEqual(packaged.unresolved.map((use) => use.assetPath).sort(), ["./parts/ball.usda", "./sub.usda"]);
    assert.ok(packaged.unresolved.every((use) => use.layer === stage.root && use.reason.length > 0));
  });
});
