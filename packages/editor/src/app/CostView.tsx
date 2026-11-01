// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What drawing costs, item by item: the draw list's measurement of what
// it holds (`DrawList.cost`), sortable by any column, each row selecting
// the prim it draws. Bytes uploaded are the buffer's (f32 attributes, u32
// indices) plus 64 per instance; bytes held are the JavaScript arrays the
// list keeps. A buffer two items share is counted in both rows, once in
// the totals.
import { useState } from "react";
import type { DrawCost, ItemCost } from "@forest-rs/layerstack_stagehand";
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
      <table className="w-full border-collapse tabular-nums">
        <thead className="sticky top-0 bg-background text-left text-muted-foreground">
          <tr>
            {COLUMNS.map((candidate) => (
              <th key={candidate.id} className={cn("px-3 py-1 font-normal", candidate.id !== "prim" && candidate.id !== "kind" && "text-right")} title={candidate.title}>
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

/** The bottom panel's draw cost tab: what the drawn items hold. */
export function CostView({ store }: { store: EditorStore }) {
  const cost = store.cost;
  if (cost === null || cost.items.length === 0) return <p className="p-3 text-xs text-muted-foreground">Nothing is drawn.</p>;
  return <CostTable store={store} cost={cost} caption="Drawn, measured" />;
}
