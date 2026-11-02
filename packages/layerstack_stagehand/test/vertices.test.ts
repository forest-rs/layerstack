// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Vertex formats: the compact encoding's error bounds, and the cost model
// in both formats.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes } from "@forest-rs/layerstack";
import { HALF_ST_LIMIT, attributeValues, encodeVertices, fromHalf, loadStagehand, toHalf, vertexPositions } from "../dist/index.js";
import type { DrawList, VertexSource } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

/** A deterministic generator of numbers in [0, 1). */
function random(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

/** `count` vertices: points in an uneven box away from the origin, unit normals, `st` in [-1, 1.9]. */
function source(count: number, seed: number): VertexSource {
  const next = random(seed);
  const positions = new Float32Array(3 * count);
  const normals = new Float32Array(3 * count);
  const st = new Float32Array(2 * count);
  for (let v = 0; v < count; v++) {
    positions.set([1000 + 37.5 * next(), -3 + 0.25 * next(), 12 * next() - 6], 3 * v);
    const [x, y, z] = [next() - 0.5, next() - 0.5, next() - 0.5];
    const length = Math.hypot(x, y, z) || 1;
    normals.set([x / length, y / length, z / length], 3 * v);
    st.set([2.9 * next() - 1, 2.9 * next() - 1], 2 * v);
  }
  const indices = Uint32Array.from({ length: 3 * count }, (_, i) => i % count);
  return { positions, normals, st, colors: null, uvSets: [], indices };
}

describe("compact vertices", () => {
  it("decode positions within half a step of the bounds' largest extent / 65534", () => {
    const input = source(2000, 7);
    const vertices = encodeVertices(input, "compact");
    assert.equal(vertices.position.type, "snorm16");
    assert.equal(vertices.position.stride, 4);
    assert.ok(vertices.position.array instanceof Int16Array);
    const min = [Infinity, Infinity, Infinity];
    const max = [-Infinity, -Infinity, -Infinity];
    for (let at = 0; at < input.positions.length; at++) {
      min[at % 3] = Math.min(min[at % 3]!, input.positions[at]!);
      max[at % 3] = Math.max(max[at % 3]!, input.positions[at]!);
    }
    const extent = Math.max(...max.map((value, axis) => value - min[axis]!));
    assert.equal(vertices.decode.scale, extent / 2);
    const step = extent / 65534;
    const decoded = vertexPositions(vertices);
    let worst = 0;
    for (let at = 0; at < decoded.length; at++) worst = Math.max(worst, Math.abs(decoded[at]! - input.positions[at]!));
    // Half a step, and the f32 rounding of points near 1000.
    assert.ok(worst <= step / 2 + 2 ** -13, `${worst} > ${step / 2}`);
  });

  it("keep normals within snorm8's rounding", () => {
    const input = source(2000, 11);
    const vertices = encodeVertices(input, "compact");
    assert.equal(vertices.normal.type, "snorm8");
    const normals = attributeValues(vertices.normal);
    for (let at = 0; at < normals.length; at++) assert.ok(Math.abs(normals[at]! - input.normals[at]!) <= 0.5 / 127 + 1e-7);
  });

  it("store st as half floats below the limit, within half a half-float step", () => {
    const input = source(2000, 13);
    const vertices = encodeVertices(input, "compact");
    assert.equal(vertices.st?.type, "float16");
    const st = attributeValues(vertices.st!);
    for (let at = 0; at < st.length; at++) {
      const value = input.st![at]!;
      // A half float's step is 2^-10 of the power of two below the value.
      const bound = Math.abs(value) < 1 ? 2 ** -12 : 2 ** -11;
      assert.ok(Math.abs(st[at]! - value) <= bound, `${value} -> ${st[at]}`);
    }
    // Coordinates a half float holds exactly come back exactly.
    for (const value of [0, 0.5, -0.25, 1, 1.5, 0.125]) assert.equal(fromHalf(toHalf(value)), value);
  });

  it("keep st as f32 when a coordinate reaches the limit, exactly", () => {
    const input = source(16, 17);
    const st = Float32Array.from(input.st!);
    st[5] = HALF_ST_LIMIT;
    const vertices = encodeVertices({ ...input, st }, "compact");
    assert.equal(vertices.st?.type, "float32");
    assert.deepEqual([...attributeValues(vertices.st!)], [...st]);
  });

  it("round to the nearest half float, ties to even, as a GPU converts", () => {
    assert.equal(toHalf(1 + 2 ** -11), toHalf(1), "a tie rounds to the even mantissa");
    assert.equal(fromHalf(toHalf(1 + 3 * 2 ** -11)), 1 + 2 ** -9);
    assert.equal(fromHalf(toHalf(65504)), 65504);
    assert.equal(fromHalf(toHalf(65520)), Infinity);
    assert.equal(fromHalf(toHalf(2 ** -24)), 2 ** -24, "the smallest subnormal");
    assert.equal(fromHalf(toHalf(2 ** -26)), 0);
    assert.equal(fromHalf(toHalf(-2)), -2);
    assert.ok(Number.isNaN(fromHalf(toHalf(NaN))));
  });

  it("convert as the runtime's Float16Array does, where it has one", (context) => {
    const Float16 = (globalThis as { Float16Array?: new (values: Float32Array) => ArrayBufferView }).Float16Array;
    if (Float16 === undefined) return context.skip("no Float16Array");
    const next = random(23);
    const values = Float32Array.from({ length: 4096 }, (_, i) => (next() - 0.5) * 2 ** ((i % 40) - 26));
    const native = new Uint16Array(new Float16(values).buffer);
    for (let i = 0; i < values.length; i++) assert.equal(toHalf(values[i]!), native[i], `${values[i]}`);
  });

  it("index with u16 below 65535 vertices, u32 from it", () => {
    assert.ok(encodeVertices(source(65534, 3), "compact").indices instanceof Uint16Array);
    assert.ok(encodeVertices(source(65535, 3), "compact").indices instanceof Uint32Array);
    assert.ok(encodeVertices(source(10, 3), "float32").indices instanceof Uint32Array);
  });

  it("share the mesh's arrays in float32, with an identity decode", () => {
    const input = source(10, 5);
    const vertices = encodeVertices(input, "float32");
    assert.equal(vertices.position.array, input.positions);
    assert.equal(vertices.indices, input.indices);
    assert.deepEqual(vertices.decode, { scale: 1, offset: [0, 0, 0] });
  });

  it("decode a flat or single-point mesh", () => {
    const flat = encodeVertices({ ...source(3, 1), positions: Float32Array.from([1, 2, 3, 1, 2, 3, 1, 2, 3]) }, "compact");
    assert.equal(flat.decode.scale, 1);
    assert.deepEqual([...vertexPositions(flat)], [1, 2, 3, 1, 2, 3, 1, 2, 3]);
  });
});

const SCENE = `#usda 1.0
def Mesh "Tile" {
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
    texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
        interpolation = "vertex"
    )
}
def Mesh "Wave" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points.timeSamples = {
        0: [(0, 0, 0), (1, 0, 0), (0, 1, 0)],
        1: [(0, 0, 1), (1, 0, 1), (0, 1, 1)],
    }
}
def Sphere "Ball" {
    double radius = 0.5
}
`;

function open(format: "compact" | "float32"): { stage: ReturnType<typeof layerstack.open>; list: DrawList; heard: Changes[] } {
  const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
  const heard: Changes[] = [];
  stage.onChange((changes) => heard.push(changes));
  const list = stagehand.drawList(stage, { time: 0, vertexFormat: format });
  list.refresh();
  return { stage, list, heard };
}

function bufferOf(list: DrawList, prim: string): number {
  return list.items().find((item) => item.prim === prim)!.buffer;
}

describe("vertex formats in a draw list", () => {
  it("halve each mesh's uploaded bytes, measured and estimated alike", () => {
    const compact = open("compact");
    const float = open("float32");
    try {
      const tile = (list: DrawList) => list.cost().items.find((item) => item.prim === "/Tile")!.geometry;
      // f32: 4 × (12 + 12 + 8) + 6 × 4; compact: 4 × (8 + 4 + 4) + 6 × 2.
      assert.equal(tile(float.list).gpuBytes, 152);
      assert.equal(tile(compact.list).gpuBytes, 76);
      for (const { list } of [compact, float]) {
        assert.deepEqual(list.estimate().total, list.cost().total);
      }
      assert.ok(compact.list.cost().total.gpuBytes < float.list.cost().total.gpuBytes * 0.6);
      // The compact list holds its encoded arrays and vertex points only: the mesh's own are released.
      const buffers = compact.list.buffers(bufferOf(compact.list, "/Tile"))!;
      assert.ok(buffers.mesh.released);
      assert.equal(buffers.mesh.positions.length, 0);
      assert.equal(buffers.mesh.ranges.length, 1);
      assert.equal(tile(compact.list).cpuBytes, 76 + 16);
    } finally {
      for (const { list, stage } of [compact, float]) {
        list.dispose();
        stage.free();
      }
    }
  });

  it("re-encode a refilled mesh against its new bounds", () => {
    const { stage, list } = open("compact");
    try {
      const key = bufferOf(list, "/Wave");
      const diff = list.setTime(0.5);
      assert.deepEqual(diff.refilled, [key]);
      const buffers = list.buffers(key)!;
      assert.deepEqual([...vertexPositions(buffers.vertices)], [0, 0, 0.5, 1, 0, 0.5, 0, 1, 0.5]);
      assert.deepEqual([...buffers.vertices.indices], [0, 1, 2]);
    } finally {
      list.dispose();
      stage.free();
    }
  });
});
