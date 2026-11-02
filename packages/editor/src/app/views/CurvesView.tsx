// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The views of curves and points, beside the mesh's: what the draw list
// made of them, read from its buffers' reports (nothing is evaluated
// again). Curves swept as tubes or ribbons are previewed as a mesh is, on
// the viewport's own geometry; curves drawn as lines, and points, are
// described only.
import type { ChannelState, CurvesReport, PointsReport } from "@forest-rs/layerstack_stagehand";
import { formatNumber } from "../../model/values.ts";
import type { PresentProps } from "../../presentations/registry.ts";
import { Section } from "../Section.tsx";
import { Fact, Preview } from "./MeshView.tsx";

/** A channel's fate, as text: `vertex`, `constant, indexed`, `absent`, `dropped: why`. */
function channel(state: ChannelState): string {
  if (state.state === "used") return `${state.interpolation}${state.indexed ? ", indexed" : ""}`;
  return state.state === "absent" ? "absent" : `dropped: ${state.reason}`;
}

/** The extent to frame a preview on: the prim's untransformed bounds. */
function extent({ object, context }: PresentProps<"prim">): number[] {
  const box = context.stage.untransformedBounds([object.prim.path], { purposes: ["default", "proxy", "render"], ...(context.time === undefined ? {} : { time: context.time }) }).alignedRanges;
  return box[0]! <= box[3]! ? Array.from(box.subarray(0, 6)) : [-1, -1, -1, 1, 1, 1];
}

const NO_STYLE = { checker: false, wireframe: false, normals: false, doubleSided: true, highlight: null } as const;

export function CurvesView(props: PresentProps<"prim">) {
  const { object, context } = props;
  const view = context.store.scene.items.find((candidate) => candidate.item.prim === object.prim.path && candidate.buffers.curves !== null);
  const report: CurvesReport | null = view?.buffers.curves?.report ?? null;
  if (view === undefined || report === null) {
    return (
      <Section title="Curves">
        <p className="text-[11px] text-muted-foreground">The viewport does not draw these curves now (the draw scope, or the budget): what it makes of them shows once it does.</p>
      </Section>
    );
  }
  const shared = view.kind === "tubes" ? (context.store.viewport?.sharedGeometry(view.buffers.key) ?? null) : null;
  const evaluation =
    report.evaluation.kind === "basis" ? `${report.evaluation.type}${report.evaluation.basis === null ? "" : ` ${report.evaluation.basis}`}, ${report.evaluation.wrap}` : report.evaluation.kind;
  const segments = report.segments.reduce((sum, count) => sum + count, 0);
  return (
    <Section title="Curves" aside={<span>{view.kind === "tubes" ? "the viewport's buffers" : "drawn as lines"}</span>}>
      {shared !== null && <Preview source={{ shared, cpu: null, bounds: extent(props) }} style={NO_STYLE} />}
      <dl className="mt-2 grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[11px]">
        <Fact label="curves" value={`${report.curves.toLocaleString()}${report.drawnCurves !== report.curves ? ` (${report.drawnCurves.toLocaleString()} drawn)` : ""}`} />
        <Fact label="evaluated as" value={evaluation} />
        <Fact label="segments" value={`${segments.toLocaleString()}, ${report.samples} samples each`} />
        <Fact label="samples" value={report.vertices.toLocaleString()} />
        <Fact label="swept as" value={report.geometry.kind === "tubes" ? `tubes of ${report.geometry.sides} sides` : report.geometry.kind} />
        <Fact label="triangles" value={report.triangles.toLocaleString()} />
        <Fact label="widths" value={channel(report.channels.widths)} />
        <Fact label="normals" value={channel(report.channels.normals)} />
        <Fact label="displayColor" value={channel(report.channels.displayColor)} />
        {report.fallbacks.map((fallback) => (
          <Fact key={fallback.attribute} label={fallback.attribute} value={`${fallback.token}, drawn as ${fallback.used}`} />
        ))}
        {report.notes.length > 0 && <Fact label="notes" value={`${report.notes.length} curves drawn otherwise than authored`} />}
      </dl>
    </Section>
  );
}

export function PointsView({ object, context }: PresentProps<"prim">) {
  const view = context.store.scene.items.find((candidate) => candidate.item.prim === object.prim.path && candidate.buffers.points !== null);
  const points = view?.buffers.points ?? null;
  const report: PointsReport | null = points?.report ?? null;
  if (points === null || report === null) {
    return (
      <Section title="Points">
        <p className="text-[11px] text-muted-foreground">The viewport does not draw these points now (the draw scope, or the budget): what it makes of them shows once it does.</p>
      </Section>
    );
  }
  let smallest = Infinity;
  let largest = -Infinity;
  for (const width of points.widths) {
    smallest = Math.min(smallest, width);
    largest = Math.max(largest, width);
  }
  return (
    <Section title="Points">
      <dl className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[11px]">
        <Fact label="points" value={report.points.toLocaleString()} />
        <Fact label="drawn as" value={report.shape} />
        <Fact label="widths" value={`${channel(report.channels.widths)}${report.points > 0 ? `, ${formatNumber(smallest)} to ${formatNumber(largest)}` : ""}`} />
        <Fact label="normals" value={channel(report.channels.normals)} />
        <Fact label="displayColor" value={channel(report.channels.displayColor)} />
        <Fact label="displayOpacity" value={channel(report.channels.displayOpacity)} />
        <Fact label="ids" value={channel(report.channels.ids)} />
        {report.extent !== null && <Fact label="extent" value={`(${report.extent[0].map(formatNumber).join(", ")}) to (${report.extent[1].map(formatNumber).join(", ")})`} />}
      </dl>
    </Section>
  );
}
