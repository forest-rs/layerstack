// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The point instancer's view: its instances read as columns once per change
// and time (`model/instancer.ts`), its prototypes with how many instances
// each places (the `protoIndices` histogram), the mask (`invisibleIds`,
// `inactiveIds`), the positions' and scales' ranges, and a table of every
// instance: virtualized (only the rows in view are made, each formatted
// from the typed columns as it scrolls in), sortable by an index
// permutation. Choosing a row selects the instance, which the viewport
// outlines (double-click frames it); picking an instance in the viewport
// chooses its row. Each row hides or deactivates its instance, an edit of
// `invisibleIds` or of the `inactiveIds` metadata where edits go, undone as
// any. Each row, and the chosen instance below the table, is a live
// presentation of its instance (`presentations/instances.tsx`): a context
// menu on it (or Shift+F10 on its index) offers the instance's commands.
import { useEffect, useMemo, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { PowerIcon, ViewIcon, ViewOffIcon } from "@hugeicons/core-free-icons";
import { cn } from "@/lib/utils";
import { histogram, instanceId, type InstanceColumn, type InstancerColumns, ranges, sortOrder } from "../../model/instancer.ts";
import { formatNumber } from "../../model/values.ts";
import type { PresentationContext, PresentProps } from "../../presentations/registry.ts";
import type { EditorStore } from "../../store.ts";
import { presentedInstance } from "../presentations/instances.tsx";
import { CommandMenu, Live, Present } from "../presentations/Present.tsx";
import { Section } from "../Section.tsx";
import { useTimeOf } from "../context.ts";
import { countRender } from "../renders.ts";

/** A table row's height in pixels, and the table's. */
const ROW = 20;
const TABLE = 320;
/** Rows made beyond those in view, above and below. */
const OVERSCAN = 6;

/** The table's columns: what each shows, and the key it sorts by (`null` for none). */
const COLUMNS: readonly { readonly label: string; readonly sort: InstanceColumn | null; readonly width: string; readonly title: string }[] = [
  { label: "#", sort: "index", width: "minmax(2.5rem,0.8fr)", title: "Its index in the instancer's arrays" },
  { label: "id", sort: "id", width: "minmax(2.5rem,0.8fr)", title: "Its id: its `ids` entry, or its index" },
  { label: "prototype", sort: "prototype", width: "minmax(3rem,1.4fr)", title: "The prototype it places" },
  { label: "x", sort: "x", width: "minmax(2.6rem,1fr)", title: "positions" },
  { label: "y", sort: "y", width: "minmax(2.6rem,1fr)", title: "positions" },
  { label: "z", sort: "z", width: "minmax(2.6rem,1fr)", title: "positions" },
  { label: "scale", sort: "scale", width: "minmax(2.6rem,1fr)", title: "scales, as their geometric mean (the chosen instance's below the table)" },
  { label: "", sort: null, width: "2.4rem", title: "Hide (invisibleIds) or deactivate (inactiveIds)" },
];
const GRID = { gridTemplateColumns: COLUMNS.map((column) => column.width).join(" ") };

/** A number for a narrow cell. */
const cell = (value: number | undefined) => (value === undefined ? "" : formatNumber(Number(value.toPrecision(4))));

export function InstancerView({ object, context }: PresentProps<"prim">) {
  countRender("InstancerView");
  const { store } = context;
  const path = object.prim.path;
  // Its columns are read at the time: again as it moves only when one of its attributes might vary.
  useTimeOf(store, context.stage, path, { attributes: null });
  const columns = store.instancerColumns(path);
  if (columns === null) return null;
  return <Instances store={store} path={path} columns={columns} context={context} />;
}

function Instances({ store, path, columns, context }: { store: EditorStore; path: string; columns: InstancerColumns; context: PresentProps<"prim">["context"] }) {
  const counts = useMemo(() => histogram(columns.protoIndices, columns.prototypes.length), [columns]);
  const spread = useMemo(() => ({ positions: ranges(columns.positions, 3), scales: ranges(columns.scales, 3) }), [columns]);
  const [sort, setSort] = useState<{ by: InstanceColumn; descending: boolean }>({ by: "index", descending: false });
  const order = useMemo(() => sortOrder(columns, sort.by, sort.descending), [columns, sort]);
  const hidden = useMemo(() => columns.mask.reduce((sum, drawn) => sum + (drawn === 0 ? 1 : 0), 0), [columns]);
  const largest = Math.max(1, ...counts.counts);
  const selected = store.instance?.instancer === path ? store.instance.index : null;
  const invisible = useMemo(() => new Set(columns.invisibleIds), [columns]);
  const inactive = useMemo(() => new Set(columns.inactiveIds), [columns]);

  const toggleInvisible = (index: number) => {
    const id = instanceId(columns, index);
    store.setInstanceVisible(path, id, invisible.has(id));
  };
  const toggleInactive = (index: number) => {
    const id = instanceId(columns, index);
    store.setInstanceActive(path, id, inactive.has(id));
  };

  return (
    <Section title="Point instancer" aside={<span>{columns.count.toLocaleString()} instances</span>}>
      <div className="mb-2 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground">
        <span>{columns.prototypes.length} prototypes</span>
        <span>{columns.ids === null ? "no ids (an instance's id is its index)" : "ids authored"}</span>
        <span>
          {hidden.toLocaleString()} masked: {columns.invisibleIds.length} invisibleIds, {columns.inactiveIds.length} inactiveIds
        </span>
        {counts.outOfRange > 0 && <span className="text-destructive">{counts.outOfRange.toLocaleString()} name no prototype</span>}
      </div>
      <ol className="mb-2 flex flex-col gap-0.5">
        {columns.prototypes.map((prototype, index) => (
          <li key={prototype} className="grid grid-cols-[minmax(0,1fr)_4.5rem_5rem] items-center gap-2 text-[11px]">
            <Present object={{ kind: "primPath", path: prototype }} context={context} />
            <span className="h-2 overflow-hidden rounded-sm bg-muted" title={`${counts.counts[index]} of ${columns.count}`}>
              <span className="block h-full bg-primary/70" style={{ width: `${(100 * counts.counts[index]!) / largest}%` }} />
            </span>
            <span className="text-right tabular-nums text-muted-foreground">{counts.counts[index]!.toLocaleString()}</span>
          </li>
        ))}
      </ol>
      {(spread.positions !== null || spread.scales !== null) && (
        <div className="mb-2 grid grid-cols-[4rem_minmax(0,1fr)] gap-x-2 gap-y-0.5 font-mono text-[11px] text-muted-foreground">
          {spread.positions !== null && (
            <>
              <span className="font-sans">positions</span>
              <span className="truncate" title="minimum to maximum, per axis">
                {[0, 1, 2].map((axis) => `${"xyz"[axis]} ${cell(spread.positions!.min[axis])}…${cell(spread.positions!.max[axis])}`).join("  ")}
              </span>
            </>
          )}
          {spread.scales !== null && (
            <>
              <span className="font-sans">scales</span>
              <span className="truncate" title="minimum to maximum, and the mean, per axis">
                {[0, 1, 2].map((axis) => `${cell(spread.scales!.min[axis])}…${cell(spread.scales!.max[axis])}`).join("  ")} (mean {spread.scales.mean.map(cell).join(", ")})
              </span>
            </>
          )}
        </div>
      )}
      <Table
        store={store}
        context={context}
        path={path}
        columns={columns}
        order={order}
        sort={sort}
        setSort={setSort}
        selected={selected}
        invisible={invisible}
        inactive={inactive}
        toggleInvisible={toggleInvisible}
        toggleInactive={toggleInactive}
      />
      {selected !== null && selected < columns.count && <Chosen store={store} context={context} path={path} columns={columns} index={selected} />}
    </Section>
  );
}

/** The chosen instance in full: its id, prototype, position, orientation and scale, and framing it. */
function Chosen({ store, context, path, columns, index }: { store: EditorStore; context: PresentationContext; path: string; columns: InstancerColumns; index: number }) {
  const triple = (values: Float32Array | null, fallback: string) => (values === null ? fallback : `(${[0, 1, 2].map((c) => cell(values[index * 3 + c])).join(", ")})`);
  const q = columns.orientations;
  return (
    <div className="mt-1.5 grid grid-cols-[5.5rem_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[11px]">
      <span className="text-muted-foreground">instance</span>
      <span className="flex min-w-0 items-center gap-2 font-mono">
        <Present object={presentedInstance(path, columns, index)} context={context} />
        <button type="button" className="ml-auto font-sans text-sky-600 hover:underline dark:text-sky-400" onClick={() => store.selectInstance(path, index, true)}>
          Frame
        </button>
      </span>
      <span className="text-muted-foreground">position</span>
      <span className="font-mono">{triple(columns.positions, "none authored")}</span>
      <span className="text-muted-foreground">orientation</span>
      <span className="font-mono">{q === null ? "none authored" : `(${[0, 1, 2, 3].map((c) => cell(q[index * 4 + c])).join(", ")}) i j k r`}</span>
      <span className="text-muted-foreground">scale</span>
      <span className="font-mono">{triple(columns.scales, "none authored (1, 1, 1)")}</span>
    </div>
  );
}

function Table({
  store,
  context,
  path,
  columns,
  order,
  sort,
  setSort,
  selected,
  invisible,
  inactive,
  toggleInvisible,
  toggleInactive,
}: {
  store: EditorStore;
  context: PresentationContext;
  path: string;
  columns: InstancerColumns;
  order: Uint32Array;
  sort: { by: InstanceColumn; descending: boolean };
  setSort: (sort: { by: InstanceColumn; descending: boolean }) => void;
  selected: number | null;
  invisible: ReadonlySet<bigint>;
  inactive: ReadonlySet<bigint>;
  toggleInvisible: (index: number) => void;
  toggleInactive: (index: number) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(0);
  // The row a context menu is open on, and the element it opened at.
  const [menu, setMenu] = useState<number | null>(null);
  const menuAt = useRef<Element | null>(null);
  const frame = useRef(0);
  // A selection made elsewhere (a pick) scrolls its row into view.
  useEffect(() => {
    const element = scroller.current;
    if (selected === null || element === null) return;
    const position = order.indexOf(selected);
    if (position < 0) return;
    const y = position * ROW;
    if (y < element.scrollTop || y + ROW > element.scrollTop + element.clientHeight) element.scrollTop = Math.max(0, y - element.clientHeight / 2);
  }, [selected, order]);
  useEffect(() => () => cancelAnimationFrame(frame.current), []);
  const first = Math.max(0, Math.floor(top / ROW) - OVERSCAN);
  const last = Math.min(columns.count, Math.ceil((top + TABLE) / ROW) + OVERSCAN);
  const rows: React.ReactNode[] = [];
  for (let position = first; position < last; position++) {
    const index = order[position]!;
    const id = instanceId(columns, index);
    const p = columns.positions;
    const s = columns.scales;
    const prototype = columns.prototypes[columns.protoIndices[index]!];
    const isInvisible = invisible.has(id);
    const isInactive = inactive.has(id);
    rows.push(
      <div
        key={index}
        role="row"
        aria-selected={selected === index}
        onClick={() => store.selectInstance(path, index)}
        onDoubleClick={() => store.selectInstance(path, index, true)}
        onContextMenu={(event) => {
          event.preventDefault();
          menuAt.current = event.currentTarget;
          setMenu(index);
        }}
        className={cn(
          "absolute inset-x-0 grid cursor-default items-center select-none gap-x-1 px-1 font-mono text-[11px] tabular-nums hover:bg-muted",
          selected === index && "bg-accent text-accent-foreground",
          (isInvisible || isInactive) && "text-muted-foreground",
        )}
        style={{ ...GRID, top: position * ROW, height: ROW }}
      >
        <Live object={presentedInstance(path, columns, index)} context={context} className="text-right text-inherit hover:no-underline dark:text-inherit">
          {index}
        </Live>
        <span className="text-right">{String(id)}</span>
        <span className="truncate font-sans" title={prototype}>
          {prototype === undefined ? "none" : prototype.slice(prototype.lastIndexOf("/") + 1)}
        </span>
        <span className="truncate text-right">{cell(p?.[index * 3])}</span>
        <span className="truncate text-right">{cell(p?.[index * 3 + 1])}</span>
        <span className="truncate text-right">{cell(p?.[index * 3 + 2])}</span>
        <span className="truncate text-right" title={s === null ? "no scales" : `${s[index * 3]}, ${s[index * 3 + 1]}, ${s[index * 3 + 2]}`}>
          {s === null ? "" : cell(Math.cbrt(Math.abs(s[index * 3]! * s[index * 3 + 1]! * s[index * 3 + 2]!)))}
        </span>
        <span className="flex justify-end gap-0.5">
          <button
            type="button"
            aria-label={isInvisible ? `Show instance ${id}` : `Hide instance ${id}`}
            title={isInvisible ? "Invisible (invisibleIds): show it" : "Hide it (invisibleIds)"}
            onClick={(event) => {
              event.stopPropagation();
              toggleInvisible(index);
            }}
            className="rounded p-0.5 hover:bg-foreground/10"
          >
            <HugeiconsIcon icon={isInvisible ? ViewOffIcon : ViewIcon} className="size-3" />
          </button>
          <button
            type="button"
            aria-label={isInactive ? `Activate instance ${id}` : `Deactivate instance ${id}`}
            title={isInactive ? "Inactive (inactiveIds): activate it" : "Deactivate it (inactiveIds)"}
            onClick={(event) => {
              event.stopPropagation();
              toggleInactive(index);
            }}
            className={cn("rounded p-0.5 hover:bg-foreground/10", isInactive && "text-destructive")}
          >
            <HugeiconsIcon icon={PowerIcon} className="size-3" />
          </button>
        </span>
      </div>,
    );
  }
  return (
    <div className="overflow-x-auto rounded-md border">
      <div className="min-w-[23rem]">
        <div role="row" className="grid gap-x-1 border-b px-1 py-0.5 text-[10px] text-muted-foreground" style={GRID}>
          {COLUMNS.map((column, at) =>
            column.sort === null ? (
              <span key={at} title={column.title} className="truncate text-right">
                {column.label}
              </span>
            ) : (
              <button
                key={at}
                type="button"
                title={`${column.title}: sort by it`}
                onClick={() => setSort({ by: column.sort!, descending: sort.by === column.sort ? !sort.descending : false })}
                className={cn("truncate text-right hover:text-foreground", column.sort === "prototype" && "text-left", sort.by === column.sort && "text-foreground")}
              >
                {column.label}
                {sort.by === column.sort && (sort.descending ? " ↓" : " ↑")}
              </button>
            ),
          )}
        </div>
        <div
          ref={scroller}
          role="grid"
          aria-label="Instances"
          aria-rowcount={columns.count}
          className="relative overflow-y-auto"
          style={{ height: Math.min(TABLE, columns.count * ROW + 2) }}
          onScroll={(event) => {
            const y = event.currentTarget.scrollTop;
            cancelAnimationFrame(frame.current);
            frame.current = requestAnimationFrame(() => setTop(y));
          }}
        >
          <div style={{ height: columns.count * ROW }} />
          {rows}
        </div>
      </div>
      {menu !== null && menu < columns.count && (
        <CommandMenu key={menu} object={presentedInstance(path, columns, menu)} context={context} triggerRef={menuAt} onClose={() => setMenu(null)} />
      )}
    </div>
  );
}
