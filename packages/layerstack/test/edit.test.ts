// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { EditReceipt, LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Changes, Edit, Layerstack, Stage, Value } from "../dist/index.js";
import { mockSource, usdz } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const text = new TextEncoder();

/** A layered scene: a variant set whose `dark` variant references an asset fetched on demand, and a referenced part. */
const files: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    defaultPrim = "World"
)
def Xform "World" (
    variants = {
        string shade = "light"
    }
    prepend variantSets = "shade"
) {
    double size = 2
    double spin.timeSamples = {
        1: 10,
        2: 20,
    }
    rel lookAt = </World/Rock>

    def Sphere "Rock" {
    }

    variantSet "shade" = {
        "dark" (
            prepend references = @./dark.usda@</Dark>
        ) {
        }
        "light" {
            color3f tint = (1, 1, 1)
        }
    }
}

def "Spare" (
    doc = "a spare part"
    prepend references = @./spare.usda@</Part>
) {
}
`,
  "spare.usda": `#usda 1.0\ndef "Part" {\n    int count = 3\n}\n`,
  "dark.usda": `#usda 1.0\ndef "Dark" {\n    color3f tint = (0.25, 0.25, 0.25)\n}\n`,
};

async function openScene(): Promise<Stage> {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: text.encode(files["scene.usda"]) });
  await stage.resolve(mockSource({ "spare.usda": files["spare.usda"] as string }));
  return stage;
}

/** Runs `f` and returns the `LayerstackError` it throws. */
function failure(f: () => unknown): LayerstackError {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error;
  }
  assert.fail("expected a LayerstackError");
}

/** Every composed prim with its fields, values, samples, targets, variant sets and kind, as text. */
function snapshot(stage: Stage): string {
  const lines: string[] = [];
  const visit = (path: string): void => {
    const prim = stage.prim(path);
    lines.push(`${path} ${JSON.stringify(prim, plain)}`);
    if (path !== "/" && prim !== null) {
      lines.push(`  variants ${JSON.stringify(prim.variantSets())} kind ${JSON.stringify(prim.metadata("kind"), plain)}`);
      for (const name of prim.attributeNames()) {
        const attribute = prim.attribute(name);
        lines.push(
          `  .${name} ${JSON.stringify(attribute, plain)} ${JSON.stringify(attribute?.get(), plain)} ${attribute?.timeSamples()}`,
        );
      }
      lines.push(`  doc ${JSON.stringify(prim.metadata("documentation"))}`);
      for (const name of ["lookAt", "aim"]) {
        lines.push(`  ${name} ${JSON.stringify(stage.relationship(`${path}.${name}`)?.targets())}`);
      }
    }
    for (const child of stage.children(path)) visit(child);
  };
  visit("/");
  return lines.join("\n");
}

function plain(key: string, value: unknown): unknown {
  if (key === "stage") return undefined;
  if (typeof value === "bigint") return `${value}n`;
  if (value !== null && typeof value === "object" && "copy" in value) {
    return Array.from((value as { copy(): ArrayLike<unknown> }).copy() as ArrayLike<number>);
  }
  return value;
}

/**
 * Applies `edits` to the root, checks `effect`, undoes and checks the
 * composed stage is exactly as before, then redoes and checks `effect`
 * again.
 */
async function roundTrip(edits: Edit[], effect: (stage: Stage) => void): Promise<Stage> {
  const stage = await openScene();
  const before = snapshot(stage);
  const receipt = stage.apply(edits, stage.root);
  assert.notEqual(snapshot(stage), before, "the edits change something");
  effect(stage);
  const redo = receipt.undo();
  assert.equal(receipt.released, true, "undoing spends the receipt");
  assert.equal(snapshot(stage), before, "undo restores the composed stage");
  redo.undo();
  effect(stage);
  return stage;
}

const double = (value: number): Value => ({ kind: "scalar", type: "double", value });

describe("edit kinds", () => {
  it("definePrim defines the prim and its ancestors", async () => {
    using stage = await roundTrip([{ type: "definePrim", path: "/Garden/Moss", typeName: "Cube" }], (stage) => {
      assert.equal(stage.prim("/Garden/Moss")?.typeName, "Cube");
      assert.equal(stage.prim("/Garden")?.specifier, "def");
    });
    using over = await roundTrip([{ type: "definePrim", path: "/Elsewhere", specifier: "over" }], (stage) => {
      assert.equal(stage.prim("/Elsewhere")?.specifier, "over");
    });
  });

  it("definePrim's over and class leave a defined prim's def, as OverridePrim and CreateClassPrim do", async () => {
    // `OverridePrim` then `SetTypeName`: the `def` spec stays a `def`.
    using stage = await roundTrip(
      [{ type: "definePrim", path: "/World/Rock", specifier: "over", typeName: "Cube" }],
      (stage) => {
        assert.equal(stage.prim("/World/Rock")?.specifier, "def");
        assert.equal(stage.prim("/World/Rock")?.typeName, "Cube");
      },
    );
    using classes = await roundTrip(
      [
        { type: "definePrim", path: "/Base", specifier: "class" },
        { type: "definePrim", path: "/Base/Inner", specifier: "over" },
        { type: "definePrim", path: "/Base/Inner", specifier: "class" },
      ],
      (stage) => {
        assert.equal(stage.prim("/Base")?.specifier, "class");
        assert.equal(stage.prim("/Base/Inner")?.specifier, "over", "already in a class");
      },
    );
    const refused = failure(() =>
      stage.apply([{ type: "definePrim", path: "/World/Rock", specifier: "class" }], stage.root),
    );
    assert.equal(refused.code, "prim.not_class");
    assert.equal(refused.edit, 0);
    assert.equal(stage.prim("/World/Rock")?.specifier, "def", "nothing is authored");
  });

  it("definePrim gives later edits of the same apply its type's attributes", async () => {
    using stage = await roundTrip(
      [
        { type: "definePrim", path: "/Garden/Ball", typeName: "Sphere" },
        { type: "setValue", path: "/Garden/Ball.radius", value: { type: "double", value: 2 } },
        { type: "definePrim", path: "/World/Rock", typeName: "Cube" },
        { type: "setValue", path: "/World/Rock.size", value: { type: "double", value: 3 } },
      ],
      (stage) => {
        const radius = stage.attribute("/Garden/Ball.radius");
        assert.equal(radius?.typeName, "double");
        assert.equal(radius?.custom, false);
        assert.deepEqual(radius?.get(), double(2));
        assert.equal(stage.prim("/World/Rock")?.typeName, "Cube");
        assert.deepEqual(stage.attribute("/World/Rock.size")?.get(), double(3));
      },
    );
    const cases: [Edit, string, string][] = [
      [
        { type: "setValue", path: "/Pebble.bogus", value: { type: "double", value: 1 } },
        "edit.rejected.undeclared_type",
        "apply: edit 1 rejected: /Pebble.bogus declares no type; create it with a type first",
      ],
      [
        { type: "setValue", path: "/Pebble.radius", value: { type: "float", value: 1 } },
        "edit.rejected.type_mismatch",
        "apply: edit 1 rejected: the value is a `float`, but /Pebble.radius is a `double`",
      ],
    ];
    for (const [edit, code, message] of cases) {
      const define: Edit = { type: "definePrim", path: "/Pebble", typeName: "Sphere" };
      const error = failure(() => stage.apply([define, edit], stage.root));
      assert.equal(error.code, code, JSON.stringify(edit));
      assert.equal(error.message, message, "the message names paths, not ids, and the edit sent");
      assert.equal(error.edit, 1, "the error names the edit");
      assert.equal(stage.prim("/Pebble"), null, "nothing applied");
    }
  });

  it("removePrim and removeProperty remove the target layer's specs", async () => {
    using stage = await roundTrip([{ type: "removePrim", path: "/World/Rock" }], (stage) => {
      assert.equal(stage.prim("/World/Rock"), null);
    });
    using again = await roundTrip([{ type: "removeProperty", path: "/World.size" }], (stage) => {
      assert.equal(stage.attribute("/World.size"), null);
    });
  });

  it("createAttribute declares type, variability, custom and a default", async () => {
    using stage = await roundTrip(
      [
        {
          type: "createAttribute",
          path: "/World/Rock.glow",
          typeName: "color3f",
          variability: "uniform",
          custom: false,
          value: { type: "color3f", value: [0.5, 0.25, 1] },
        },
      ],
      (stage) => {
        const glow = stage.attribute("/World/Rock.glow");
        assert.equal(glow?.typeName, "color3f");
        assert.equal(glow?.variability, "uniform");
        assert.equal(glow?.custom, false);
        assert.deepEqual(glow?.get(), { kind: "tuple", type: "color3f", value: [0.5, 0.25, 1] });
      },
    );
  });

  it("setValue authors the default and time samples", async () => {
    using stage = await roundTrip(
      [
        { type: "setValue", path: "/World.size", value: { type: "double", value: 3 } },
        { type: "setValue", path: "/World.spin", time: 5, value: { type: "double", value: 50 } },
      ],
      (stage) => {
        assert.deepEqual(stage.attribute("/World.size")?.get(), double(3));
        assert.deepEqual(stage.attribute("/World.spin")?.timeSamples(), [1, 2, 5]);
        assert.deepEqual(stage.attribute("/World.spin")?.get(5), double(50));
      },
    );
  });

  it("clearValue and blockValue", async () => {
    using stage = await roundTrip(
      [
        { type: "clearValue", path: "/World.size" },
        { type: "clearValue", path: "/World.spin", time: 1 },
      ],
      (stage) => {
        assert.equal(stage.attribute("/World.size")?.get(), null);
        assert.deepEqual(stage.attribute("/World.spin")?.timeSamples(), [2]);
      },
    );
    using blocked = await roundTrip([{ type: "blockValue", path: "/World.spin" }], (stage) => {
      assert.deepEqual(stage.attribute("/World.spin")?.timeSamples(), []);
      assert.equal(stage.attribute("/World.spin")?.get(1), null);
    });
  });

  it("setTargets sets a relationship's targets", async () => {
    using stage = await roundTrip(
      [{ type: "setTargets", path: "/World.lookAt", targets: ["/Spare", "/World.size"] }],
      (stage) => {
        assert.deepEqual(stage.relationship("/World.lookAt")?.targets(), ["/Spare", "/World.size"]);
      },
    );
    using created = await roundTrip([{ type: "setTargets", path: "/Spare.aim", targets: ["/World"] }], (stage) => {
      assert.equal(stage.relationship("/Spare.aim")?.custom, true);
    });
  });

  it("adds and removes references and payloads", async () => {
    using stage = await roundTrip(
      [
        {
          type: "addReference",
          path: "/World/Rock",
          asset: "./spare.usda",
          primPath: "/Part",
          position: "frontOfPrepend",
        },
      ],
      (stage) => {
        assert.deepEqual(stage.attribute("/World/Rock.count")?.get(), { kind: "scalar", type: "int", value: 3 });
      },
    );
    using internal = await roundTrip(
      [{ type: "addPayload", path: "/World/Rock", primPath: "/Spare", position: "backOfAppend" }],
      (stage) => {
        assert.deepEqual(stage.attribute("/World/Rock.count")?.get(), { kind: "scalar", type: "int", value: 3 });
      },
    );
    using removed = await roundTrip(
      [{ type: "removeReference", path: "/Spare", asset: "./spare.usda", primPath: "/Part" }],
      (stage) => {
        assert.equal(stage.attribute("/Spare.count")?.get() ?? null, null);
      },
    );
  });

  it("setVariantSelection, setActive, setInstanceable and setKind", async () => {
    using variant = await roundTrip(
      [{ type: "setVariantSelection", path: "/World", set: "shade", variant: null }],
      (stage) => {
        assert.equal(stage.prim("/World")?.variantSets()[0]?.selection, null);
      },
    );
    using active = await roundTrip([{ type: "setActive", path: "/World/Rock", active: false }], (stage) => {
      assert.equal(stage.prim("/World/Rock")?.active, false);
    });
    using instance = await roundTrip([{ type: "setInstanceable", path: "/Spare", instanceable: true }], (stage) => {
      assert.equal(stage.prim("/Spare")?.instance, true);
    });
    using kind = await roundTrip([{ type: "setKind", path: "/World", kind: "assembly" }], (stage) => {
      assert.deepEqual(stage.prim("/World")?.metadata("kind"), { kind: "scalar", type: "token", value: "assembly" });
    });
  });

  it("setMetadata and clearMetadata on prims and properties", async () => {
    using stage = await roundTrip(
      [
        { type: "setMetadata", path: "/World", key: "documentation", value: { type: "string", value: "the main one" } },
        { type: "setMetadata", path: "/World.size", key: "hidden", value: { type: "bool", value: true } },
      ],
      (stage) => {
        assert.deepEqual(stage.prim("/World")?.metadata("documentation"), {
          kind: "scalar",
          type: "string",
          value: "the main one",
        });
        assert.deepEqual(stage.attribute("/World.size")?.metadata("hidden"), { kind: "scalar", type: "bool", value: true });
      },
    );
    using cleared = await roundTrip([{ type: "clearMetadata", path: "/Spare", key: "documentation" }], (stage) => {
      assert.equal(stage.prim("/Spare")?.metadata("documentation"), null);
    });
  });

  it("setLayerMetadata and clearLayerMetadata author the target layer's metadata", async () => {
    using stage = await openScene();
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const text = () => new TextDecoder().decode(stage.exportLayer(stage.root, "usda"));
    const receipt = stage.apply(
      [
        { type: "setLayerMetadata", key: "documentation", value: { type: "string", value: "moss on every rock" } },
        { type: "setLayerMetadata", key: "metersPerUnit", value: { type: "double", value: 0.01 } },
      ],
      stage.root,
    );
    assert.match(text(), /\(\n    defaultPrim = "World"\n    doc = "moss on every rock"\n/);
    assert.match(text(), /metersPerUnit = 0\.01/);
    // Stage metadata composes nothing: `/` changes info-only.
    assert.deepEqual([heard[0]?.resynced, heard[0]?.changedInfoOnly], [[], ["/"]]);
    receipt.undo().release();
    assert.doesNotMatch(text(), /moss on every rock|metersPerUnit/);
    stage.apply([{ type: "setLayerMetadata", key: "documentation", value: { type: "string", value: "kept" } }], stage.root).release();
    stage.apply([{ type: "clearLayerMetadata", key: "documentation" }], stage.root).release();
    assert.doesNotMatch(text(), /kept/);
  });
});

describe("values", () => {
  it("take typed arrays, nested arrays, bigints and values read back", async () => {
    using stage = await openScene();
    const receipt = stage.apply([
      { type: "createAttribute", path: "/World.points", typeName: "point3f[]" },
      { type: "setValue", path: "/World.points", value: { type: "point3f[]", value: new Float32Array([0, 1, 2, 3, 4, 5]) } },
      { type: "createAttribute", path: "/World.ids", typeName: "int64[]", value: { type: "int64[]", value: [9007199254740993n, -1n] } },
      { type: "createAttribute", path: "/World.big", typeName: "uint64", value: { type: "uint64", value: 18446744073709551615n } },
      { type: "createAttribute", path: "/World.uvs", typeName: "texCoord2f[]", value: { type: "texCoord2f[]", value: [[0, 1], [1, 0]] } },
      { type: "createAttribute", path: "/World.tags", typeName: "token[]", value: { type: "token[]", value: ["a", "b"] } },
      { type: "createAttribute", path: "/World.halves", typeName: "half[]", value: { type: "half[]", value: new Uint16Array([0x3c00]) } },
    ], stage.root);
    assert.deepEqual(Array.from(stage.attribute("/World.points")?.array()?.copy() as Float32Array), [0, 1, 2, 3, 4, 5]);
    assert.deepEqual(Array.from(stage.attribute("/World.ids")?.array()?.copy() as BigInt64Array), [9007199254740993n, -1n]);
    assert.deepEqual(stage.attribute("/World.big")?.get(), { kind: "scalar", type: "uint64", value: 18446744073709551615n });
    assert.deepEqual(Array.from(stage.attribute("/World.uvs")?.array()?.copy() as Float32Array), [0, 1, 1, 0]);
    assert.deepEqual(stage.attribute("/World.tags")?.array()?.copy(), ["a", "b"]);
    assert.deepEqual(Array.from(stage.attribute("/World.halves")?.array()?.copy() as Uint16Array), [0x3c00]);
    // A value read is sent back as it is.
    const points = stage.attribute("/World.points")?.get();
    assert.ok(points);
    stage.apply([{ type: "createAttribute", path: "/Spare.points", typeName: "point3f[]", value: points }], stage.root);
    assert.deepEqual(Array.from(stage.attribute("/Spare.points")?.array()?.copy() as Float32Array), [0, 1, 2, 3, 4, 5]);
    receipt.release();
  });

  it("fail with value codes naming the edit", async () => {
    using stage = await openScene();
    const cases: [Edit, string][] = [
      [{ type: "setValue", path: "/World.size", value: { type: "dubble", value: 1 } }, "value.unknown_type"],
      [{ type: "setValue", path: "/World.size", value: { type: "double3", value: [1, 2] } }, "value.shape"],
      [{ type: "setValue", path: "/World.size", value: { type: "int", value: 1.5 } }, "value.range"],
      [{ type: "setValue", path: "/World.size", value: { type: "int64", value: 2 ** 53 + 2 } }, "value.range"],
      [{ type: "setValue", path: "/World.size", value: { type: "double", value: "two" } }, "value.kind"],
      [{ type: "setValue", path: "/World.size", value: { type: "double", value: [1, "x"] } }, "value.kind"],
      [{ type: "setValue", path: "/World.size", value: { type: "float", value: 2 } }, "edit.rejected.type_mismatch"],
    ];
    for (const [edit, code] of cases) {
      const error = failure(() => stage.apply([{ type: "setActive", path: "/World", active: true }, edit], stage.root));
      assert.equal(error.code, code, JSON.stringify(edit));
      assert.equal(error.edit, 1, "the error names the edit");
      assert.equal(error.path, "/World.size");
      assert.equal(error.operation, "apply");
    }
    assert.equal(stage.prim("/World")?.active, true);
    assert.deepEqual(stage.attribute("/World.size")?.get(), double(2), "nothing applied");
  });
});

describe("applying", () => {
  it("checks edit shapes, layers and paths with codes", async () => {
    using stage = await openScene();
    using other = await openScene();
    const bad = [
      [{ type: "explode", path: "/World" }, "argument.invalid"],
      [{ type: "setActive", path: "/World", active: "yes" }, "argument.invalid"],
      [{ type: "definePrim", path: "/A", specifier: "maybe" }, "argument.invalid"],
      [{ type: "removePrim", path: "World" }, "path.invalid"],
      [{ type: "removePrim", path: "/World.size" }, "path.invalid"],
    ] as const;
    for (const [edit, code] of bad) {
      const error = failure(() => stage.apply([edit as unknown as Edit], stage.root));
      assert.equal(error.code, code, JSON.stringify(edit));
      assert.equal(error.edit, 0);
    }
    assert.equal(failure(() => stage.apply([], other.root)).code, "layer.affinity");
    const spare = stage.layers().find((layer) => layer.identifier === "spare.usda");
    assert.ok(spare);
    const outside = failure(() => stage.apply([{ type: "setActive", path: "/Part", active: false }], spare));
    assert.equal(outside.code, "layer.outside_stack");
    assert.match(outside.message, /editTarget/, "the error says how to edit it through its arc");
  });

  it("undoes once, refuses another stage's receipt and a stale undo", async () => {
    using stage = await openScene();
    using other = await openScene();
    const first = stage.apply([{ type: "setValue", path: "/World.size", value: { type: "double", value: 3 } }], stage.root);
    assert.ok(first instanceof EditReceipt);
    assert.equal(first.target.layer, stage.root);
    assert.equal(first.target, stage.editTargetForLayer(stage.root), "the receipt names the target, interned");
    assert.deepEqual(first.layers, [stage.root]);
    assert.deepEqual(first.changes.changedInfoOnly, ["/World"]);
    assert.deepEqual(
      first.changes.properties,
      [{ prim: "/World", fields: [{ property: "size", field: "default", metadata: null }] }],
      "the inventory of what changed on the prim",
    );
    assert.equal(failure(() => other.undo(first)).code, "receipt.affinity");
    using later = stage.apply([{ type: "setValue", path: "/World.size", value: { type: "double", value: 4 } }], stage.root);
    const stale = failure(() => first.undo());
    assert.equal(stale.code, "edit.stale");
    assert.equal(first.released, false, "a stale receipt stays undoable");
    assert.deepEqual(stage.attribute("/World.size")?.get(), double(4));
    later.undo().release();
    first.undo().release();
    assert.deepEqual(stage.attribute("/World.size")?.get(), double(2));
    assert.equal(failure(() => first.undo()).code, "receipt.released");
  });

  it("undoes several receipts as one atomic transaction", async () => {
    using stage = await openScene();
    const usda = () => new TextDecoder().decode(stage.exportLayer(stage.root, "usda"));
    const set = (path: string, value: number) =>
      stage.apply([{ type: "setValue", path, value: { type: "double", value } }], stage.root);
    const start = usda();
    const size = set("/World.size", 3);
    const spin = set("/World.spin", 3);
    assert.equal(failure(() => stage.undo([])).code, "argument.invalid");
    assert.equal(failure(() => stage.undo([size, size])).code, "argument.invalid");
    const outside = set("/World.size", 4);
    const after = usda();
    assert.equal(failure(() => stage.undo([size, spin])).code, "edit.stale");
    assert.equal(usda(), after, "the newer receipt was not undone either");
    assert.equal(size.released || spin.released, false, "a refused undo spends neither");
    outside.undo().release();
    const redo = stage.undo([size, spin]);
    assert.equal(usda(), start);
    assert.equal(size.released && spin.released, true);
    redo.undo().release();
    assert.deepEqual(stage.attribute("/World.spin")?.get(), double(3));
  });

  it("switches a variant and fetches the assets it needs", async () => {
    using stage = await openScene();
    const world = stage.prim("/World");
    assert.ok(world);
    assert.deepEqual(world.variantSets(), [{ name: "shade", selection: "light", variants: ["dark", "light"] }]);
    const receipt = stage.setVariantSelection(world, "shade", "dark");
    assert.deepEqual(
      receipt.unavailable.map((need) => [need.identifier, need.usedBy]),
      [["dark.usda", "reference"]],
    );
    assert.equal(stage.attribute("/World.tint")?.get() ?? null, null, "the variant's asset is not read yet");
    const source = mockSource(files);
    await stage.resolve(source);
    assert.deepEqual(source.asked, ["dark.usda"]);
    assert.deepEqual(stage.attribute("/World.tint")?.get(), { kind: "tuple", type: "color3f", value: [0.25, 0.25, 0.25] });
    receipt.undo().release();
    assert.deepEqual(stage.attribute("/World.tint")?.get(), { kind: "tuple", type: "color3f", value: [1, 1, 1] });
  });
});

describe("change listeners", () => {
  it("hear every change in order, after each call", async () => {
    using stage = await openScene();
    const heard: Changes[] = [];
    const stop = stage.onChange((changes) => heard.push(changes));
    const receipt = stage.apply([{ type: "definePrim", path: "/Garden" }], stage.root);
    assert.equal(heard.length, 1, "one recomposition");
    assert.deepEqual(heard[0]?.created, ["/Garden"]);
    assert.deepEqual(heard[0]?.removed, []);
    assert.deepEqual(heard[0]?.resynced, ["/Garden"]);
    receipt.undo().release();
    assert.deepEqual(heard[1]?.removed, ["/Garden"]);
    stage.setVariantSelection("/World", "shade", "dark").release();
    const before = heard.length;
    await stage.resolve(mockSource(files));
    assert.ok(heard.length > before, "loading the variant's asset is heard");
    stop();
    stage.apply([{ type: "setActive", path: "/Spare", active: false }], stage.root).release();
    assert.equal(heard.length, before + 1, "a removed listener hears nothing");
  });

  it("hear a new property spec as a resync of the property, not its prim", async () => {
    using stage = await openScene();
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const receipt = stage.apply(
      [{ type: "createAttribute", path: "/World.weight", typeName: "double", value: { type: "double", value: 1 } }],
      stage.root,
    );
    assert.deepEqual(receipt.changes.resynced, []);
    assert.deepEqual(receipt.changes.changedInfoOnly, []);
    assert.deepEqual(receipt.changes.resyncedProperties, ["/World.weight"]);
    assert.deepEqual(heard.at(-1)?.resyncedProperties, ["/World.weight"]);
    receipt.undo().release();
    assert.deepEqual(heard.at(-1)?.resyncedProperties, ["/World.weight"], "removing the spec resyncs it too");
    stage.apply([{ type: "setValue", path: "/World.size", value: { type: "double", value: 3 } }], stage.root).release();
    assert.deepEqual(heard.at(-1)?.resyncedProperties, [], "a value of an existing spec is info-only");
  });

  it("keep going when one throws, and report it", async () => {
    using stage = await openScene();
    const reported: unknown[] = [];
    const host = globalThis as { reportError?: ((error: unknown) => void) | undefined };
    const saved = host.reportError;
    host.reportError = (error) => reported.push(error);
    try {
      const order: string[] = [];
      stage.onChange(() => {
        order.push("first");
        throw new Error("listener bug");
      });
      stage.onChange(() => order.push("second"));
      stage.apply([{ type: "setActive", path: "/Spare", active: false }], stage.root).release();
      assert.deepEqual(order, ["first", "second"]);
      assert.equal(reported.length, 1);
      const error = reported[0];
      assert.ok(error instanceof LayerstackError);
      assert.equal(error.code, "listener.failed");
      assert.ok(error.cause instanceof Error);
    } finally {
      host.reportError = saved;
    }
  });

  it("hear a listener's own edits after the current changes", async () => {
    using stage = await openScene();
    const heard: (readonly string[])[] = [];
    stage.onChange((changes) => {
      heard.push(changes.created);
      if (changes.created.includes("/Garden")) {
        stage.apply([{ type: "definePrim", path: "/Garden/Moss" }], stage.root).release();
      }
    });
    stage.apply([{ type: "definePrim", path: "/Garden" }], stage.root).release();
    assert.deepEqual(heard, [["/Garden"], ["/Garden/Moss"]]);
  });
});

describe("read-only roots", () => {
  const pack = (): Uint8Array =>
    usdz([
      ["root.usda", `#usda 1.0\ndef "Model" {\n    float height = 3\n}\n`],
    ]);

  it("refuse edits without a session layer", () => {
    using stage = layerstack.open({ identifier: "model.usdz", bytes: pack() });
    assert.equal(stage.editLayer, null);
    const error = failure(() => stage.apply([{ type: "setActive", path: "/Model", active: false }], stage.root));
    assert.equal(error.code, "layer.read_only");
    assert.equal(error.identifier, "model.usdz");
    assert.equal(failure(() => stage.setVariantSelection("/Model", "look", "a")).code, "layer.read_only");
  });

  it("take edits in an explicit session layer in front", () => {
    using stage = layerstack.open({ identifier: "model.usdz", bytes: pack() }, { editLayer: "session" });
    const session = stage.editLayer;
    assert.ok(session);
    assert.notEqual(session, stage.root);
    assert.equal(session.anonymous, true);
    assert.equal(session.readOnly, false);
    const receipt = stage.apply([{ type: "setValue", path: "/Model.height", value: { type: "float", value: 5 } }], session);
    assert.deepEqual(stage.attribute("/Model.height")?.get(), { kind: "scalar", type: "float", value: 5 });
    assert.equal(stage.isDirty(session), true);
    assert.equal(stage.isDirty(stage.root), false, "the package stays as it was read");
    const strongest = stage.attribute("/Model.height")?.explain()?.opinions[0];
    assert.equal(strongest?.layer, session);
    receipt.undo().release();
    assert.deepEqual(stage.attribute("/Model.height")?.get(), { kind: "scalar", type: "float", value: 3 });
  });
});

describe("dictionary values", () => {
  it("author a nested dictionary whole, read it back and undo it", async () => {
    using stage = await openScene();
    const receipt = stage.apply([
      {
        type: "setMetadata",
        path: "/World",
        key: "customData",
        value: {
          type: "dictionary",
          value: {
            note: { type: "string", value: "moss" },
            inner: { type: "dictionary", value: new Map([["scale", { type: "double3", value: [1, 2, 3] }]]) },
          },
        },
      },
    ], stage.root);
    const read = stage.prim("/World")?.metadata("customData");
    assert.ok(read?.kind === "dictionary");
    assert.deepEqual([...read.value.keys()], ["inner", "note"], "ordered by key");
    assert.deepEqual(read.value.get("note"), { kind: "scalar", type: "string", value: "moss" });
    const inner = read.value.get("inner");
    assert.ok(inner?.kind === "dictionary");
    assert.deepEqual(inner.value.get("scale"), { kind: "tuple", type: "double3", value: [1, 2, 3] });
    receipt.undo().release();
    assert.equal(stage.prim("/World")?.metadata("customData"), null);
  });

  it("send a dictionary read from a stage back as it is", async () => {
    using stage = await openScene();
    stage
      .apply([
        {
          type: "setMetadata",
          path: "/World",
          key: "assetInfo",
          value: { type: "dictionary", value: { name: { type: "string", value: "rock" }, version: { type: "int", value: 2 } } },
        },
      ], stage.root)
      .release();
    const info = stage.prim("/World")?.metadata("assetInfo");
    assert.ok(info);
    stage.apply([{ type: "setMetadata", path: "/Spare", key: "assetInfo", value: info }], stage.root).release();
    assert.deepEqual(stage.prim("/Spare")?.metadata("assetInfo"), info);
    const saved = stage.save().written[0];
    assert.ok(saved);
    assert.match(new TextDecoder().decode(saved.bytes), /string name = "rock"/);
  });

  it("fail naming the entry and the edit", async () => {
    using stage = await openScene();
    const bad = await failure(() =>
      stage.apply([
        { type: "setKind", path: "/World", kind: "group" },
        {
          type: "setMetadata",
          path: "/World",
          key: "customData",
          value: { type: "dictionary", value: { count: { type: "int", value: 1.5 } } },
        },
      ], stage.root),
    );
    assert.equal(bad.code, "value.range");
    assert.equal(bad.edit, 1);
    assert.match(bad.message, /`count`/);
    const notMap = await failure(() =>
      stage.apply([
        { type: "setMetadata", path: "/World", key: "customData", value: { type: "dictionary", value: [1, 2] } as never },
      ], stage.root),
    );
    assert.equal(notMap.code, "value.kind");
    const cycle: { type: "dictionary"; value: Record<string, unknown> } = { type: "dictionary", value: {} };
    cycle.value.self = cycle;
    const deep = await failure(() =>
      stage.apply([{ type: "setMetadata", path: "/World", key: "customData", value: cycle as never }], stage.root),
    );
    assert.equal(deep.code, "value.shape");
    assert.equal(stage.prim("/World")?.metadata("customData"), null, "nothing was authored");
  });
});
