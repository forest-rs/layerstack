// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { Layerstack, LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Changes, Stage } from "../dist/index.js";
import { EditBatch } from "../wasm/layerstack.js";
import * as nativeModule from "../wasm/layerstack.js";
import type { Stage as GeneratedStage } from "../wasm/layerstack.js";
import { bytes } from "./fixtures.ts";

const layerstack = await loadLayerstack();

function openScene(): Stage {
  return layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
}

// A same-module native consumer. Passing the callback's inferred NativeStage
// here also checks the published declarations against the actual generated
// class, without an assertion or access to the wrapper's internal methods.
function publish(native: GeneratedStage, layer: bigint, path: string): void {
  using batch = new EditBatch();
  batch.definePrim(path, "def", "Xform");
  using receipt = native.apply(layer, batch);
}

// A native transaction the extension makes, with no receipt kept.
function commit(native: GeneratedStage, layer: bigint, fill: (batch: EditBatch) => void): void {
  using batch = new EditBatch();
  fill(batch);
  using receipt = native.apply(layer, batch);
}

function setSize(native: GeneratedStage, layer: bigint, size: number): void {
  commit(native, layer, (batch) => batch.setValue("/Ball.size", null, batch.valueNumbers("double", new Float64Array([size]))));
}

function size(stage: Stage): unknown {
  const value = stage.attribute("/Ball.size")?.get();
  return value?.kind === "scalar" ? value.value : value;
}

describe("same-module native extensions", () => {
  it("opens with explicit native provenance and validates JavaScript callers", () => {
    const forwarded: boolean[] = [];
    const module = new Layerstack({
      ...nativeModule,
      Stage: {
        open(...args: Parameters<typeof nativeModule.Stage.open>) {
          forwarded.push(args[7]);
          return nativeModule.Stage.open(...args);
        },
      },
    });
    for (const provenance of [undefined, false, true]) {
      using stage = module.open(
        { identifier: "scene.usda", bytes: bytes("scene.usda") },
        provenance === undefined ? {} : { provenance },
      );
      assert.ok(stage.attribute("/Ball.size")?.explain());
    }
    assert.deepEqual(forwarded, [false, false, true]);
    for (const provenance of [false, true]) {
      using stage = module.open({ identifier: "scene.usda", bytes: bytes("scene.usda") }, { provenance });
      assert.equal(stage.provenance, provenance);
    }
    using plain = openScene();
    assert.equal(plain.provenance, false);
    for (const provenance of [null, 0, 1, "true", {}]) {
      assert.throws(
        () => Reflect.apply(layerstack.open, layerstack, [
          { identifier: "scene.usda", bytes: bytes("scene.usda") }, { provenance },
        ]),
        (error: unknown) => error instanceof LayerstackError &&
          error.code === "argument.invalid" && error.operation === "open",
      );
    }
  });

  it("borrows the real stage and returns an extension result", () => {
    using stage = openScene();
    assert.deepEqual(
      stage.withNative("extension.inspect", (native) => native.children("/Garden")),
      stage.children("/Garden"),
    );
    assert.equal(stage.withNative("extension.scalar", () => 17), 17);
    assert.deepEqual(stage.withNative("extension.data", () => ({ then: "ordinary data" })), {
      then: "ordinary data",
    });
  });

  it("delivers a native edit before returning and leaves read-only calls quiet", () => {
    using stage = openScene();
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    stage.withNative("extension.publish", (native) => publish(native, stage.root.id, "/Extension"));
    assert.equal(stage.prim("/Extension")?.typeName, "Xform");
    assert.deepEqual(heard.map((changes) => changes.created), [["/Extension"]]);
    stage.withNative("extension.inspect", (native) => native.prim("/Extension"));
    assert.equal(heard.length, 1);
  });

  it("flushes committed edits on failure and preserves the callback error", () => {
    using stage = openScene();
    const heard: Changes[] = [];
    const reported: unknown[] = [];
    const host = globalThis as { reportError?: ((error: unknown) => void) | undefined };
    const saved = host.reportError;
    host.reportError = (error) => reported.push(error);
    const original = new Error("extension failed after publishing");
    try {
      stage.onChange(() => {
        throw new Error("listener failed too");
      });
      stage.onChange((changes) => heard.push(changes));
      assert.throws(
        () => stage.withNative("extension.publish", (native) => {
          publish(native, stage.root.id, "/Committed");
          throw original;
        }),
        (error) => error === original,
      );
      assert.equal(stage.prim("/Committed")?.typeName, "Xform");
      assert.deepEqual(heard.map((changes) => changes.created), [["/Committed"]]);
      assert.equal(reported.length, 1);
      assert.ok(reported[0] instanceof LayerstackError);
      assert.equal(reported[0].code, "listener.failed");
    } finally {
      host.reportError = saved;
    }
  });

  it("dispatches a listener's own native extension after the current changes", () => {
    using stage = openScene();
    const heard: (readonly string[])[] = [];
    stage.onChange((changes) => {
      heard.push(changes.created);
      if (changes.created.includes("/Outer")) {
        stage.withNative("extension.inner", (native) => publish(native, stage.root.id, "/Inner"));
      }
    });
    stage.withNative("extension.outer", (native) => publish(native, stage.root.id, "/Outer"));
    assert.deepEqual(heard, [["/Outer"], ["/Inner"]]);
  });

  it("keeps the callback failure primary when change reporting also throws", () => {
    using stage = openScene();
    const host = globalThis as { reportError?: ((error: unknown) => void) | undefined };
    const saved = host.reportError;
    const original = new Error("extension failed after its edit");
    host.reportError = () => { throw new Error("reporting failed too"); };
    try {
      stage.onChange(() => { throw new Error("listener failed"); });
      assert.throws(
        () => stage.withNative("extension.publish", (native) => {
          publish(native, stage.root.id, "/StillCommitted");
          throw original;
        }),
        (error) => error === original,
      );
      assert.equal(stage.prim("/StillCommitted")?.typeName, "Xform");
    } finally {
      host.reportError = saved;
    }
  });

  it("refuses async results while retaining and reporting edits already committed", () => {
    using stage = openScene();
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    assert.throws(
      () => stage.withNative("extension.async", async (native) => {
        publish(native, stage.root.id, "/BeforePromise");
      }),
      (error) => error instanceof LayerstackError && error.code === "argument.invalid" && error.operation === "extension.async",
    );
    assert.deepEqual(heard.map((changes) => changes.created), [["/BeforePromise"]]);
    assert.equal(stage.prim("/BeforePromise")?.typeName, "Xform");
    assert.throws(
      () => stage.withNative("extension.thenable", () => ({ then() {} })),
      (error) => error instanceof LayerstackError && error.code === "argument.invalid",
    );
  });

  it("checks disposed stages before invoking the extension", () => {
    const stage = openScene();
    stage.free();
    assert.throws(
      () => stage.withNative("extension.closed", () => assert.fail("must not run")),
      (error) => error instanceof LayerstackError && error.code === "stage.disposed" && error.operation === "extension.closed",
    );
  });

  it("retains the extension operation and path on native refusals", () => {
    using stage = openScene();
    assert.throws(
      () => stage.withNative("extension.inspect", (native) => native.children("not-absolute")),
      (error) => error instanceof LayerstackError && error.code === "path.invalid" && error.operation === "extension.inspect" && error.path === "not-absolute",
    );
    assert.equal(stage.prim("/Ball")?.typeName, "Sphere", "a refusal does not poison the module");
  });

  it("requires an operation name and a callback", () => {
    using stage = openScene();
    for (const arguments_ of [["", () => undefined], ["extension.invalid", null]]) {
      assert.throws(
        () => Reflect.apply(stage.withNative, stage, arguments_),
        (error) => error instanceof LayerstackError && error.code === "argument.invalid" && error.operation === "withNative",
      );
    }
  });

  it("preserves the extension operation when a trap prevents change draining", () => {
    // This separate wrapper shares the real module but isolates the wrapper's
    // poisoned state. The synthetic trap exercises translation without trapping
    // the native instance used by the rest of this package's tests.
    const module = new Layerstack(nativeModule);
    using stage = module.open({ identifier: "scene.usda", bytes: bytes("scene.usda") });
    const trap = new WebAssembly.RuntimeError("extension trapped");
    assert.throws(
      () => stage.withNative("extension.trap", () => { throw trap; }),
      (error) => error instanceof LayerstackError && error.code === "module.trapped" && error.operation === "extension.trap" && error.cause === trap,
    );
    assert.throws(
      () => stage.status(),
      (error) => error instanceof LayerstackError && error.code === "module.trapped",
    );
  });
});

describe("undo across native transactions", () => {
  it("undoes a user edit around native content elsewhere, and redoes it", () => {
    using stage = openScene();
    const receipt = stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 3 } }]);
    stage.withNative("extension.publish", (native) => publish(native, stage.root.id, "/Derived"));
    using redo = receipt.undo();
    assert.equal(size(stage), 2);
    using again = redo.undo();
    assert.equal(size(stage), 3);
    assert.equal(stage.prim("/Derived")?.typeName, "Xform", "undo leaves native content alone");
  });

  it("refuses an undo over a spec native code changed, until it is put back", () => {
    using stage = openScene();
    const define = stage.apply([{ type: "definePrim", path: "/Thing", typeName: "Xform" }]);
    stage.withNative("extension.publish", (native) => publish(native, stage.root.id, "/Thing/Derived"));
    assert.throws(
      () => define.undo(),
      (error) => error instanceof LayerstackError && error.code === "edit.stale" && error.operation === "undo",
    );
    assert.equal(stage.prim("/Thing/Derived")?.typeName, "Xform", "a refused undo changes nothing");
    stage.withNative("extension.unpublish", (native) =>
      commit(native, stage.root.id, (batch) => batch.removePrim("/Thing/Derived")),
    );
    using redo = define.undo();
    assert.equal(stage.prim("/Thing"), null);

    const set = stage.apply([{ type: "setValue", path: "/Ball.size", value: { type: "double", value: 3 } }]);
    stage.withNative("extension.set", (native) => setSize(native, stage.root.id, 7));
    assert.throws(() => set.undo(), (error) => error instanceof LayerstackError && error.code === "edit.stale");
    assert.equal(size(stage), 7);
    stage.withNative("extension.set", (native) => setSize(native, stage.root.id, 3));
    using back = set.undo();
    assert.equal(size(stage), 2);
  });

  it("keeps undo free of native content in another layer", () => {
    using stage = layerstack.open({ identifier: "scene.usda", bytes: bytes("scene.usda") }, { editLayer: "session" });
    const session = stage.editLayer;
    assert.ok(session !== null && session !== stage.root);
    const define = stage.apply([{ type: "definePrim", path: "/Thing", typeName: "Xform" }], stage.root);
    stage.withNative("extension.publish", (native) => publish(native, session.id, "/Thing/Derived"));
    using redo = define.undo();
    assert.equal(stage.prim("/Thing")?.defined, false, "only the session's derived opinion is left");
    using again = redo.undo();
    assert.equal(stage.prim("/Thing")?.typeName, "Xform");
    assert.equal(stage.prim("/Thing/Derived")?.typeName, "Xform");
    assert.equal(stage.isDirty(session), true);
    assert.deepEqual(
      stage.save().written.map((layer) => layer.identifier),
      ["scene.usda"],
      "saving never writes the derived layer",
    );
    assert.equal(stage.isDirty(session), true);
  });
});
