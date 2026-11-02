// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What drawing costs, item by item: the draw list's measurement of what
// it holds (`DrawList.cost`), its estimate of the whole scene from counts
// (`DrawList.estimate`), or the items the budget simplified
// (`DrawList.budgetReport`), each row selecting the prim it draws. Bytes uploaded are the buffer's (f32 attributes, u32
// indices) plus 64 per instance; bytes held are the JavaScript arrays the
// list keeps. A buffer two items share is counted in both rows, once in
// the totals.
import { useState } from "react";
import type { BudgetGrant, BudgetReport, DrawCost, ItemCost } from "@forest-rs/layerstack_stagehand";
import { cn } from "@/lib/utils";
import type { EditorStore } from "../store.ts";
import { formatBytes } from "./format.ts";

/** The most rows shown, after sorting. */
const SHOWN = 1000;

/** A column: its heading, how a row reads it, and whether it is a number. */
interface Column {
  readonly id: string;
  readonly label: string;
  readonly title: string;
  readonly value: (item: ItemCost) => number | string;
  readonly text?: (item: ItemCost) => string;
}

const COLUMNS: readonly Column[] = [
  { id: "prim", label: "Prim", title: "The prim whose geometry is drawn", value: (item) => item.prim },
  { id: "kind", label: "Drawn as", title: "A gprim, a native prototype's gprim, a point instancer's prototype, or a draw mode stand-in", value: (item) => item.source.kind },
  { id: "instances", label: "Instances", title: "Instances drawn", value: (item) => item.instances },
  { id: "triangles", label: "Triangles", title: "The buffer's triangles", value: (item) => item.geometry.triangles },
  { id: "drawn", label: "Drawn", title: "Triangles drawn: the buffer's times its instances", value: (item) => item.trianglesDrawn },
  { id: "vertices", label: "Vertices", title: "The buffer's vertices", value: (item) => item.geometry.vertices },
  {
    id: "gpu",
    label: "Uploaded",
    title: "Bytes uploaded: the buffer's, and 64 per instance",
    value: (item) => item.geometry.gpuBytes + item.instanceBytes,
    text: (item) => formatBytes(item.geometry.gpuBytes + item.instanceBytes),
  },
  { id: "cpu", label: "Held", title: "Bytes of JavaScript arrays the buffer holds", value: (item) => item.geometry.cpuBytes, text: (item) => formatBytes(item.geometry.cpuBytes) },
];

/** The prim a row selects: the gprim drawn (a native prototype's in its first instance). */
function selects(item: ItemCost): string {
  return item.prim;
}

/** The cost table: `cost`'s items, sortable, and its totals. */
export function CostTable({ store, cost, caption }: { store: EditorStore; cost: DrawCost; caption: string }) {
  const [sort, setSort] = useState<{ id: string; descending: boolean }>({ id: "gpu", descending: true });
  const column = COLUMNS.find((candidate) => candidate.id === sort.id) ?? COLUMNS[0]!;
  const rows = [...cost.items].sort((a, b) => {
    const [x, y] = [column.value(a), column.value(b)];
    const order = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
    return sort.descending ? -order : order;
  });
  const total = cost.total;
  return (
    <div className="text-[11px]">
      <p className="px-3 py-1.5 text-muted-foreground">
        {caption}: {total.items.toLocaleString()} items over {total.buffers.toLocaleString()} buffers, {total.instances.toLocaleString()} instances,{" "}
        {total.trianglesDrawn.toLocaleString()} triangles drawn ({total.triangles.toLocaleString()} held), {total.vertices.toLocaleString()} vertices,{" "}
        {formatBytes(total.gpuBytes)} uploaded ({formatBytes(total.instanceBytes)} of instance matrices), {formatBytes(total.cpuBytes)} held in JavaScript.
        {rows.length > SHOWN && ` The ${SHOWN} first rows are shown.`}
      </p>
      <table className="w-full table-fixed border-collapse tabular-nums">
        <thead className="sticky top-0 bg-background text-left text-muted-foreground">
          <tr>
            {COLUMNS.map((candidate) => (
              <th
                key={candidate.id}
                className={cn("px-3 py-1 font-normal", candidate.id === "prim" ? "w-[40%]" : candidate.id === "kind" ? "w-28" : "text-right")}
                title={candidate.title}
              >
                <button
                  type="button"
                  className="hover:text-foreground"
                  aria-sort={sort.id === candidate.id ? (sort.descending ? "descending" : "ascending") : "none"}
                  onClick={() => setSort({ id: candidate.id, descending: sort.id === candidate.id ? !sort.descending : candidate.id !== "prim" && candidate.id !== "kind" })}
                >
                  {candidate.label}
                  {sort.id === candidate.id && (sort.descending ? " ↓" : " ↑")}
                </button>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, SHOWN).map((item, index) => {
            const path = selects(item);
            return (
              <tr
                key={`${item.item ?? index} ${item.prim}`}
                className={cn("cursor-pointer border-t hover:bg-muted/50", store.selection === path && "bg-muted")}
                onClick={() => store.select(path)}
              >
                {COLUMNS.map((candidate) => {
                  const value = candidate.value(item);
                  return (
                    <td key={candidate.id} className={cn("px-3 py-0.5", candidate.id === "prim" ? "max-w-0 truncate font-mono" : candidate.id !== "kind" && "text-right")} title={candidate.id === "prim" ? item.prim : undefined}>
                      {candidate.text?.(item) ?? (typeof value === "number" ? value.toLocaleString() : value)}
                    </td>
                  );
                })}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/** What a degraded item is drawn as, in words. */
function rungLabel(grant: BudgetGrant): string {
  return grant.rung === "standin" ? "box" : grant.rung;
}

/** The items the budget simplified, in admission order: what each asked for, what it was granted, and why. */
export function BudgetTable({ store, report }: { store: EditorStore; report: BudgetReport }) {
  const megatriangles = (count: number) => (count >= 1e6 ? `${(count / 1e6).toFixed(1)} M` : count.toLocaleString());
  return (
    <div className="text-[11px]">
      <p className="px-3 py-1.5 text-muted-foreground">
        {report.degraded.length.toLocaleString()} of {report.items.toLocaleString()} items simplified to fit{" "}
        {report.budget.gpuBytes === undefined ? "any bytes" : formatBytes(report.budget.gpuBytes)} and{" "}
        {report.budget.triangles === undefined ? "any number of" : megatriangles(report.budget.triangles)} triangles: granted {formatBytes(report.granted.gpuBytes)} and {megatriangles(report.granted.triangles)} triangles
        of the {formatBytes(report.requested.gpuBytes)} and {megatriangles(report.requested.triangles)} asked for (estimated from counts). Every item is a
        box first; then the selection, and after it the items worth most per share of the budget (bound size times instances, over their share of bytes
        or triangles), are drawn in full while it holds.
        {report.degraded.length > SHOWN && ` The ${SHOWN} first rows are shown.`}
      </p>
      <table className="w-full table-fixed border-collapse tabular-nums">
        <thead className="sticky top-0 bg-background text-left text-muted-foreground">
          <tr>
            <th className="w-[40%] px-3 py-1 font-normal">Prim</th>
            <th className="w-20 px-3 py-1 font-normal" title="A box of its extent at each instance (one box over a point instancer's), or skipped">
              Drawn as
            </th>
            <th className="w-28 px-3 py-1 font-normal" title="The limits its full geometry would have passed">
              Over
            </th>
            <th className="px-3 py-1 text-right font-normal">Instances</th>
            <th className="px-3 py-1 text-right font-normal" title="Triangles its full geometry would draw">
              Triangles asked
            </th>
            <th className="px-3 py-1 text-right font-normal" title="Bytes its full geometry would upload">
              Uploaded asked
            </th>
            <th className="px-3 py-1 text-right font-normal" title="Bytes its rung uploads">
              Granted
            </th>
          </tr>
        </thead>
        <tbody>
          {report.degraded.slice(0, SHOWN).map((grant) => (
            <tr
              key={`${grant.order} ${grant.prim}`}
              className={cn("cursor-pointer border-t hover:bg-muted/50", store.selection === grant.prim && "bg-muted")}
              onClick={() => store.select(grant.prim)}
            >
              <td className="max-w-0 truncate px-3 py-0.5 font-mono" title={grant.prim}>
                {grant.prim}
              </td>
              <td className="px-3 py-0.5">{rungLabel(grant)}</td>
              <td className="px-3 py-0.5">{grant.over.join(", ")}</td>
              <td className="px-3 py-0.5 text-right">{grant.requested.instances.toLocaleString()}</td>
              <td className="px-3 py-0.5 text-right">{grant.requested.triangles.toLocaleString()}</td>
              <td className="px-3 py-0.5 text-right">{formatBytes(grant.requested.gpuBytes)}</td>
              <td className="px-3 py-0.5 text-right">{formatBytes(grant.granted.gpuBytes)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * The bottom panel's draw cost tab: what the drawn items hold, measured;
 * what drawing the whole scene in full would, estimated (from counts,
 * building nothing); or the items the draw budget simplified.
 */
export function CostView({ store }: { store: EditorStore }) {
  const [choice, setChoice] = useState<"drawn" | "estimated" | "simplified" | null>(null);
  const drawn = store.cost !== null && store.cost.items.length > 0 ? store.cost : null;
  const showing = choice ?? (drawn !== null ? "drawn" : "estimated");
  const cost = showing === "drawn" ? drawn : showing === "estimated" ? store.sceneEstimate() : null;
  return (
    <div>
      <div role="group" aria-label="Cost of" className="flex gap-1 px-3 pt-1.5 text-[11px]">
        {(
          [
            ["drawn", "Drawn, measured"],
            ["estimated", "Whole scene, estimated"],
            ["simplified", `Simplified${store.budget !== null && store.budget.degraded.length > 0 ? ` (${store.budget.degraded.length.toLocaleString()})` : ""}`],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={showing === id}
            onClick={() => setChoice(id)}
            className={cn("rounded px-2 py-0.5 text-muted-foreground hover:text-foreground", showing === id && "bg-muted text-foreground")}
          >
            {label}
          </button>
        ))}
      </div>
      {showing === "simplified" ? (
        store.budget === null ? (
          <p className="p-3 text-xs text-muted-foreground">No budget: everything is drawn in full.</p>
        ) : store.budget.degraded.length === 0 ? (
          <p className="p-3 text-xs text-muted-foreground">Everything fits the budget: nothing is simplified.</p>
        ) : (
          <BudgetTable store={store} report={store.budget} />
        )
      ) : cost === null ? (
        <p className="p-3 text-xs text-muted-foreground">{showing === "drawn" ? "Nothing is drawn." : "No estimate: no scene is open."}</p>
      ) : (
        <CostTable
          store={store}
          cost={cost}
          caption={
            showing === "drawn"
              ? "Drawn, measured"
              : "The whole scene in full, estimated from counts (a mesh's vertices are its points, raised where a primvar is face-varying)"
          }
        />
      )}
    </div>
  );
}
