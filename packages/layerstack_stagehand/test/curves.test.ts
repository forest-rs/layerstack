// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Curves and points: against usd-core's answers (`stagehand_curves_oracle.py`),
// hand-computed points, and the draw list's refills and layouts, as
// `layerstack_stagehand`'s `curve_tests` and draw list tests check them.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { loadLayerstack } from "@forest-rs/layerstack";
import type { Changes, Edit } from "@forest-rs/layerstack";
import {
  basisDataSizes,
  basisSegmentCounts,
  curvesExtent,
  fillPoints,
  interpolationForSize,
  loadStagehand,
  pointsExtent,
  readCurves,
  readPointSet,
} from "../dist/index.js";
import type { CurvesInput } from "../dist/index.js";

const layerstack = await loadLayerstack();
const stagehand = await loadStagehand();

const base = new URL("../../../layerstack_conformance/fixtures/stagehand_curves/", import.meta.url);
const oracle = JSON.parse(readFileSync(new URL("oracle.json", base), "utf8"));

type Extent = [[number, number, number], [number, number, number]] | null;
const near = (a: ArrayLike<number>, b: ArrayLike<number>, tolerance = 1e-6) =>
  a.length === b.length && Array.from(a).every((v, i) => Math.abs(v - b[i]!) < tolerance);

describe("curves", () => {
  it("count segments and size primvars as OpenUSD does", () => {
    const rows = oracle.basisCurves as {
      path: string;
      type: "linear" | "cubic";
      basis: "bezier" | "bspline" | "catmullRom";
      wrap: "nonperiodic" | "periodic" | "pinned";
      curveVertexCounts: number[];
      segmentCounts: number[];
      uniform: number;
      varying: number | null;
      vertex: number;
      interpolationForSize: Record<string, string>;
      widths: number[] | null;
      extent: Extent;
    }[];
    assert.ok(rows.length > 40);
    for (const row of rows) {
      const counts = Int32Array.from(row.curveVertexCounts);
      assert.deepEqual(basisSegmentCounts(row.type, row.basis, row.wrap, counts), row.segmentCounts, row.path);
      const sizes = basisDataSizes(row.type, row.basis, row.wrap, counts)!;
      assert.equal(sizes.uniform, row.uniform, row.path);
      assert.equal(sizes.vertex, row.vertex, row.path);
      // `null`: C++'s varying size wraps around below 4 vertices (B035).
      if (row.varying !== null) assert.equal(sizes.varying, row.varying, row.path);
      for (const [size, token] of Object.entries(row.interpolationForSize)) {
        assert.equal(interpolationForSize(sizes, Number(size)) ?? "", token, `${row.path} at ${size}`);
      }
      const total = row.curveVertexCounts.reduce((a, b) => a + b, 0);
      const points = new Float32Array(total * 3);
      for (let i = 0; i < total; i++) points.set([i, (i * i) % 5, 0.5 * i], i * 3);
      const widths = row.widths === null ? null : Float32Array.from(row.widths);
      assert.deepEqual(curvesExtent(points, widths), row.extent, row.path);
    }
    for (const row of oracle.points as { path: string; points: number[][]; widths: number[] | null; extent: Extent; extentFromPlugins: Extent }[]) {
      const points = Float32Array.from(row.points.flat());
      const widths = row.widths === null ? null : Float32Array.from(row.widths);
      if (widths !== null) assert.deepEqual(pointsExtent(points, widths), row.extent, row.path);
      assert.deepEqual(pointsExtent(points, widths), row.extentFromPlugins, row.path);
    }
  });

  it("read off a stage, with their schema fallbacks and the extent C++ computes", () => {
    const stage = layerstack.open({ identifier: "curves.usda", bytes: readFileSync(new URL("curves.usda", base)) });
    for (const row of oracle.basisCurves as { path: string; type: string; basis: string; wrap: string; extent: Extent }[]) {
      const input = readCurves(stage, row.path);
      assert.ok(input !== null && input.form.kind === "basis", row.path);
      assert.deepEqual([input.form.type, input.form.basis, input.form.wrap], [row.type, row.basis, row.wrap], row.path);
      const curves = stagehand.buildCurves(input);
      try {
        assert.deepEqual(curves.report.extent, row.extent, row.path);
      } finally {
        curves.dispose();
      }
    }
    const quarter = readCurves(stage, "/Nurbs/quarter");
    assert.ok(quarter !== null && quarter.form.kind === "nurbs");
    const curves = stagehand.buildCurves(quarter);
    try {
      assert.equal(curves.report.evaluation.kind, "nurbs");
      assert.deepEqual(curves.report.segments, [1]);
      for (let i = 0; i < curves.positions.length; i += 3) {
        assert.ok(Math.abs(Math.hypot(curves.positions[i]!, curves.positions[i + 1]!) - 1) < 1e-6);
      }
    } finally {
      curves.dispose();
    }
    const sparks = readPointSet(stage, "/Points/sparks");
    assert.ok(sparks !== null);
    const filled = fillPoints(sparks);
    assert.deepEqual(Array.from(filled.ids), [10n, 20n, 30n, 40n, 1099511627776n]);
    const row = (oracle.points as { path: string; extent: Extent }[]).find((row) => row.path === "/Points/sparks")!;
    assert.deepEqual(filled.report.extent, row.extent);
  });

  it("evaluate their bases at hand-computed points", () => {
    const input: CurvesInput = {
      form: { kind: "basis", type: "cubic", basis: "catmullRom", wrap: "nonperiodic" },
      curveVertexCounts: Int32Array.of(5),
      points: Float32Array.of(0, 0, 0, 1, 1, 0, 2, 0, 0, 3, 1, 0, 4, 0, 0),
    };
    const curves = stagehand.buildCurves(input);
    try {
      assert.deepEqual(curves.report.segments, [2]);
      assert.equal(curves.report.vertices, 17);
      assert.deepEqual(Array.from(curves.positions.subarray(0, 3)), [1, 1, 0]);
      // (-P0 + 9 P1 + 9 P2 - P3) / 16 at the first segment's middle.
      assert.ok(near(curves.positions.subarray(12, 15), [1.5, 0.5, 0]));
      assert.deepEqual(Array.from(curves.positions.subarray(48, 51)), [3, 1, 0]);
      assert.equal(curves.lines.length, 32);
      assert.equal(curves.swept.indices.length, 0);
    } finally {
      curves.dispose();
    }
    // Pinned: begins and ends at the end vertices, n - 1 segments.
    const pinned = stagehand.buildCurves({ ...input, form: { kind: "basis", type: "cubic", basis: "bspline", wrap: "pinned" } });
    try {
      assert.deepEqual(pinned.report.segments, [4]);
      assert.ok(near(pinned.positions.subarray(0, 3), [0, 0, 0]));
      assert.ok(near(pinned.positions.subarray(-3), [4, 0, 0]));
    } finally {
      pinned.dispose();
    }
    // Swept: a tube of 6 sides, ring by ring.
    const swept = stagehand.buildCurves(
      { ...input, widths: { values: Float32Array.of(0.2), interpolation: "constant" } },
      { style: "swept", samples: 4, sides: 6 },
    );
    try {
      assert.deepEqual(swept.report.geometry, { kind: "tubes", sides: 6 });
      assert.equal(swept.swept.positions.length / 3, 9 * 6);
      assert.equal(swept.report.triangles, 8 * 6 * 2);
      assert.deepEqual(swept.report.channels.widths, { state: "used", interpolation: "constant", indexed: false });
    } finally {
      swept.dispose();
    }
    // An unknown basis is drawn as linear, and said so.
    const fallback = stagehand.buildCurves({ ...input, form: { kind: "basis", type: "cubic", basis: "power", wrap: "nonperiodic" } });
    try {
      assert.deepEqual(fallback.report.fallbacks, [{ attribute: "basis", token: "power", used: "linear" }]);
      assert.deepEqual(Array.from(fallback.positions), Array.from(input.points));
    } finally {
      fallback.dispose();
    }
    // A NURBS jump: both sides sampled, no line segment across it; a range
    // outside the domain is noted by its code.
    const nurbs = (ranges: Float64Array): CurvesInput => ({
      form: { kind: "nurbs", order: Int32Array.of(2), knots: Float64Array.of(0, 0, 1, 1, 2, 2), ranges, weights: new Float64Array() },
      curveVertexCounts: Int32Array.of(4),
      points: Float32Array.of(0, 0, 0, 1, 0, 0, 2, 1, 0, 3, 1, 0),
    });
    const jump = stagehand.buildCurves(nurbs(new Float64Array()), { samples: 2 });
    try {
      assert.deepEqual(Array.from(jump.positions), [0, 0, 0, 0.5, 0, 0, 1, 0, 0, 2, 1, 0, 2.5, 1, 0, 3, 1, 0]);
      assert.deepEqual(Array.from(jump.lines), [0, 1, 1, 2, 3, 4, 4, 5]);
    } finally {
      jump.dispose();
    }
    const outside = stagehand.buildCurves(nurbs(Float64Array.of(5, 6)));
    try {
      assert.deepEqual(outside.report.notes, [{ curve: 0, kind: "nurbs", code: "emptyRange" }]);
    } finally {
      outside.dispose();
    }
    // Catmull-Rom widths (1, 0, 0, 0) undershoot inside the segment: drawn as 0, and counted.
    const thin = stagehand.buildCurves({
      ...input,
      curveVertexCounts: Int32Array.of(4),
      points: input.points.subarray(0, 12),
      widths: { values: Float32Array.of(1, 0, 0, 0), interpolation: "vertex" },
    });
    try {
      assert.equal(thin.report.clampedWidths, 7);
      assert.ok(thin.widths.every((width) => width === 0));
    } finally {
      thin.dispose();
    }
  });

  it("follow a Hermite curve's tangents", () => {
    const curves = stagehand.buildCurves({
      form: { kind: "hermite", tangents: Float32Array.of(0, 1, 0, 0, -1, 0) },
      curveVertexCounts: Int32Array.of(2),
      points: Float32Array.of(0, 0, 0, 1, 0, 0),
    });
    try {
      assert.ok(near(curves.positions.subarray(12, 15), [0.5, 0.25, 0]));
    } finally {
      curves.dispose();
    }
  });
});

const SCENE = `#usda 1.0
def Xform "World" {
    def BasisCurves "Path" {
        uniform token type = "cubic"
        uniform token basis = "catmullRom"
        int[] curveVertexCounts = [5]
        point3f[] points.timeSamples = {
            1: [(0, 0, 0), (1, 0, 0), (2, 0, 1), (3, 0, 1), (4, 0, 0)],
            2: [(0, 1, 0), (1, 1, 0), (2, 1, 1), (3, 1, 1), (4, 1, 0)],
        }
        float[] widths = [0.25] (interpolation = "constant")
        color3f[] primvars:displayColor = [(0.5, 0.25, 0)]
    }
    def Points "Sparks" {
        point3f[] points = [(0, 2, 0), (1, 2, 0), (2, 2, 0)]
        float[] widths = [0.1, 0.2, 0.3]
        int64[] ids = [7, 8, 9]
    }
}
`;

describe("curves and points in a draw list", () => {
  it("refill for new values and lay out again for a new topology", () => {
    const stage = layerstack.open({ identifier: "scene.usda", bytes: new TextEncoder().encode(SCENE) });
    const heard: Changes[] = [];
    stage.onChange((changes) => heard.push(changes));
    const list = stagehand.drawList(stage, { time: 1 });
    try {
      const first = list.refresh();
      assert.equal(first.added.length, 2);
      assert.equal(first.work.curvesLaidOut, 1);
      assert.equal(first.work.pointsRebuilt, 1);
      const edit = (edits: Edit[]) => {
        stage.apply(edits, stage.root);
        const diffs = heard.splice(0).map((changes) => list.applyChanges(changes));
        assert.equal(diffs.length, 1);
        return diffs[0]!;
      };
      const bufferOf = (prim: string) => list.items().find((item) => item.prim === prim)!.buffer;
      const [path, sparks] = [bufferOf("/World/Path"), bufferOf("/World/Sparks")];
      const curves = list.buffers(path)!.curves!;
      assert.equal(curves.report.evaluation.kind, "basis");
      assert.deepEqual(curves.report.segments, [2]);
      assert.deepEqual(Array.from(list.buffers(sparks)!.points!.ids), [7n, 8n, 9n]);

      const widths = edit([{ type: "setValue", path: "/World/Path.widths", value: { type: "float[]", value: Float32Array.of(0.5) } }]);
      assert.deepEqual(widths.refilled, [path]);
      assert.equal(widths.work.curvesRefilled, 1);
      assert.equal(widths.work.curvesLaidOut, 0);
      assert.equal(widths.work.itemsRead, 1);
      assert.equal(list.buffers(path)!.curves!.widths[0], 0.5);

      const basis = edit([{ type: "setValue", path: "/World/Path.basis", value: { type: "token", value: "bspline" } }]);
      assert.deepEqual(basis.rebuilt, [path]);
      assert.deepEqual(basis.refilled, []);
      assert.equal(basis.work.curvesLaidOut, 1);
      assert.equal(basis.work.itemsRead, 1);

      const moved = edit([
        { type: "setValue", path: "/World/Sparks.points", value: { type: "point3f[]", value: Float32Array.of(0, 3, 0, 1, 3, 0, 2, 3, 0) } },
      ]);
      assert.deepEqual(moved.refilled, [sparks]);
      assert.equal(moved.work.pointsRefilled, 1);
      const fewer = edit([{ type: "setValue", path: "/World/Sparks.points", value: { type: "point3f[]", value: Float32Array.of(0, 3, 0, 1, 3, 0) } }]);
      assert.deepEqual(fewer.rebuilt, [sparks]);
      assert.equal(fewer.work.pointsRebuilt, 1);
      assert.equal(list.buffers(sparks)!.points!.report.channels.ids.state, "dropped");

      const later = list.setTime(2);
      assert.deepEqual(later.refilled, [path]);
      assert.equal(later.work.itemsRead, 1);
      assert.equal(later.work.itemsTimeInvariant, 1);
    } finally {
      list.dispose();
    }
  });
});
