// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The common transform read and edits against usd-core 26.11's answers
// for `layerstack_conformance/fixtures/common_xform_api`
// (`common_xform_api_oracle.py`, which the Rust conformance tests replay
// too): `GetXformVectorsByAccumulation` and the API's validity for every
// prim, and `SetXformVectors`, `SetTranslate`, `SetRotate`, `SetScale`
// and `SetPivot` called alone on every prim, with what the root layer
// then authors, or the refusal.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { LayerstackError, loadLayerstack } from "../dist/index.js";
import type { Edit, Layerstack, RotationOrder, Stage } from "../dist/index.js";
import { mockSource } from "./fixtures.ts";

const layerstack: Layerstack = await loadLayerstack();
const base = new URL("../../../layerstack_conformance/fixtures/common_xform_api/", import.meta.url);
const read = (name: string) => readFileSync(new URL(name, base), "utf8");

type Vector = [number, number, number];
interface Vectors {
  readonly translation: Vector;
  readonly rotation: Vector;
  readonly scale: Vector;
  readonly pivot: Vector;
  readonly rotation_order: RotationOrder;
}
/** A number, a vector, or an array of vectors. */
type Numbers = number | number[] | number[][];
interface Authored {
  readonly type: string;
  readonly default: Numbers | null;
  readonly samples: Record<string, Numbers>;
}
interface Oracle {
  readonly reads: Record<string, { compatible: boolean; accumulated_default: Vectors; accumulated_2: Vectors }>;
  readonly calls: {
    readonly prim: string;
    readonly call: unknown[];
    readonly result: boolean | null;
    readonly after: { readonly order: string[] | null; readonly attributes: Record<string, Authored> };
  }[];
}
const oracle = JSON.parse(read("oracle.json")) as Oracle;

/** The scene over `weak.usda`, its sublayer, as the oracle opened it. */
async function openScene(): Promise<Stage> {
  const root = read("scene.usda").replace("#usda 1.0", "#usda 1.0\n(\n    subLayers = [@weak.usda@]\n)");
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(root) });
  await stage.resolve(mockSource({ "weak.usda": read("weak.usda") }));
  return stage;
}

function close(actual: readonly number[], expected: readonly number[], what: string): void {
  assert.equal(actual.length, expected.length, what);
  actual.forEach((value, index) => {
    const want = expected[index]!;
    assert.ok(Math.abs(value - want) <= 1e-5 * Math.max(1, Math.abs(want)), `${what}: ${actual} != ${expected}`);
  });
}

/** An oracle call as an edit of the prim at `path`. */
function asEdit(path: string, call: unknown[]): Edit {
  const time = (value: unknown) => (value === null ? {} : { time: value as number });
  const v = (value: unknown) => value as Vector;
  switch (call[0]) {
    case "vectors":
      return {
        type: "setCommonTransform",
        path,
        translate: v(call[1]),
        rotate: v(call[2]),
        scale: v(call[3]),
        pivot: v(call[4]),
        rotationOrder: call[5] as RotationOrder,
        ...time(call[6]),
      };
    case "translate":
      return { type: "setTranslate", path, value: v(call[1]), ...time(call[2]) };
    case "rotate":
      return { type: "setRotate", path, value: v(call[1]), rotationOrder: call[2] as RotationOrder, ...time(call[3]) };
    case "scale":
      return { type: "setScale", path, value: v(call[1]), ...time(call[2]) };
    case "pivot":
      return { type: "setPivot", path, value: v(call[1]), ...time(call[2]) };
    default:
      throw new Error(`no edit for ${String(call[0])}`);
  }
}

/** A value read as the oracle writes it: a number, or a vector's numbers. */
function numbers(value: unknown): number[] | number | null {
  const read = value as { kind: string; value: unknown } | null;
  if (read === null) return null;
  if (read.kind === "tuple") return [...(read.value as number[])];
  if (read.kind === "scalar") return Number(read.value);
  if (read.kind === "array") {
    const { array } = read as unknown as { array: { copy(): ArrayLike<number>; release(): void } };
    const values = [...Array.from(array.copy())];
    array.release();
    return values;
  }
  return null;
}

describe("common transforms", () => {
  it("read each prim as GetXformVectorsByAccumulation does, and its compatibility", async () => {
    using stage = await openScene();
    for (const [path, reads] of Object.entries(oracle.reads)) {
      const xformable = stage.xformable(path);
      assert.ok(xformable !== null, path);
      for (const [time, expected] of [
        [undefined, reads.accumulated_default],
        [2, reads.accumulated_2],
      ] as const) {
        const read = xformable.commonTransform(time);
        const what = `${path} at ${time ?? "default"}`;
        close(read.translation, expected.translation, `${what} translation`);
        close(read.rotation, expected.rotation, `${what} rotation`);
        close(read.scale, expected.scale, `${what} scale`);
        close(read.pivot, expected.pivot, `${what} pivot`);
        assert.equal(read.rotationOrder, expected.rotation_order, `${what} order`);
        assert.equal(read.compatible, reads.compatible, `${what} compatible`);
      }
    }
  });

  it("author each call OpenUSD accepts as it does, and refuse each it refuses with nothing authored", async () => {
    const calls = oracle.calls.filter((record) => record.call[0] !== "make_matrix");
    assert.ok(calls.length > 100);
    for (const record of calls) {
      using stage = await openScene();
      const what = `${record.prim} ${JSON.stringify(record.call)}`;
      const root = stage.layers()[0]!;
      const edit = asEdit(record.prim, record.call);
      if (record.result !== true) {
        assert.throws(() => stage.apply([edit]), (error: unknown) => error instanceof LayerstackError && error.code.startsWith("xform."), what);
        assert.equal(stage.isDirty(root), false, `${what}: nothing authored`);
        continue;
      }
      stage.apply([edit]);
      // What the root layer authors: the layer alone, its sublayer not read.
      using alone = layerstack.open({ identifier: "alone.usda", bytes: stage.exportLayer(root, "usda") });
      const prim = alone.prim(record.prim);
      assert.ok(prim !== null, what);
      const authored = prim.attributeNames().filter((name) => name.startsWith("xformOp:") && alone.attribute(`${record.prim}.${name}`)?.authored);
      assert.deepEqual(authored.sort(), Object.keys(record.after.attributes).sort(), `${what}: the ops authored`);
      for (const [name, expected] of Object.entries(record.after.attributes)) {
        const attribute = alone.attribute(`${record.prim}.${name}`)!;
        assert.equal(attribute.typeName, expected.type, `${what} ${name} type`);
        const samples = attribute.timeSamples();
        assert.deepEqual(samples, Object.keys(expected.samples).map(Number).sort((a, b) => a - b), `${what} ${name} samples`);
        const byTime = new Map(Object.entries(expected.samples).map(([key, value]) => [Number(key), value]));
        for (const time of samples) {
          const want = byTime.get(time)!;
          close([numbers(attribute.get(time))].flat() as number[], [want].flat(2), `${what} ${name} at ${time}`);
        }
        if (expected.default !== null) close([numbers(attribute.get())].flat() as number[], [expected.default].flat(2), `${what} ${name} default`);
      }
      const order = alone.attribute(`${record.prim}.xformOpOrder`);
      const orderValue = order?.authored ? order.get() : null;
      const names = orderValue?.kind === "array" ? (orderValue.array.copy() as string[]) : null;
      assert.deepEqual(names === null ? null : [...names], record.after.order, `${what}: xformOpOrder`);
    }
  });
});
