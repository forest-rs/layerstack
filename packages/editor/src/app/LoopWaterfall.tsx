// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One run of the asset loop as a waterfall: a row per asset on a shared
// time axis, from the stage naming it (want) through the host's fetch and
// the wait for the rest of its round to the stage reading its layer
// (within the round's `loadSupplied`), colored by what uses it. A track
// above the rows shows each round's phases: fetching, the stage's own
// time, and within it the compositions after the reads. Rows are a
// virtualized React Aria grid list, so the thousands of assets of a
// production scene scroll as a dozen do; selecting one shows where it is
// authored and the prims composed without it until it loaded.
import { memo, useMemo, useState } from "react";
import { GridList, GridListItem, ListLayout, Virtualizer } from "react-aria-components";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { Session } from "../model/session.ts";
import type { AssetRole, AssetTiming, LoopRun, RoundTiming } from "../model/timeline.ts";
import type { EditorStore } from "../store.ts";
import { formatBytes } from "./format.ts";

/** The columns, shared by the header, the round track and the rows. */
const COLUMNS = "grid grid-cols-[minmax(8rem,2fr)_5rem_2.5rem_4rem_3.5rem_3.5rem_minmax(10rem,5fr)] items-center gap-x-2 px-3";
const ROW_HEIGHT = 22;
/** The most prims the selection's details list. */
const PRIMS_SHOWN = 200;

/** The stage's own time, and a layer's read, as a bar. */
export const STAGE_COLOR = "bg-foreground/70";
/** A composition after a round's reads, as a bar. */
export const COMPOSE_COLOR = "bg-rose-400";

/** Each use's color, as a bar and as a dot. */
const ROLE_COLOR: Record<AssetRole, string> = {
  root: "bg-emerald-500",
  sublayer: "bg-sky-500",
  reference: "bg-violet-500",
  payload: "bg-amber-500",
};

type SortKey = "start" | "name" | "round" | "size" | "time" | "read";

interface Sort {
  readonly key: SortKey;
  readonly ascending: boolean;
}

/** The run's time axis: where it starts, how long it is shown, and its labelled ticks (offsets from the start). */
interface Axis {
  readonly start: number;
  readonly total: number;
  readonly ticks: readonly number[];
}

/** A duration as the waterfall shows it. */
export function formatDuration(ms: number): string {
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 10) return `${ms.toFixed(1)} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

/** An offset on the axis as a tick shows it: `0`, `250 ms`, `1.5 s`. */
function formatTick(ms: number): string {
  if (ms === 0) return "0";
  return ms < 1000 ? `${Math.round(ms)} ms` : `${Number((ms / 1000).toFixed(2))} s`;
}

/** The smallest of 1, 2 or 5 times a power of ten that is at least `raw`. */
function niceStep(raw: number): number {
  const power = 10 ** Math.floor(Math.log10(raw));
  return ([1, 2, 5, 10].find((multiple) => multiple * power >= raw) ?? 10) * power;
}

function axisOf(run: LoopRun, now: number): Axis {
  const total = Math.max((run.endedAt ?? now) - run.startedAt, 1);
  const step = niceStep(total / 6);
  const ticks: number[] = [];
  // A label needs room: none closer to the end than a few percent.
  for (let tick = 0; tick < total * 0.95; tick += step) ticks.push(tick);
  return { start: run.startedAt, total, ticks };
}

/** Where `time` falls on the axis, in percent. */
function at(axis: Axis, time: number): number {
  return ((time - axis.start) / axis.total) * 100;
}

/** A bar from `start` to `end`, at least a pixel wide. */
function bar(axis: Axis, start: number, end: number): React.CSSProperties {
  return { left: `${at(axis, start)}%`, width: `max(1px, ${at(axis, end) - at(axis, start)}%)` };
}

/** `identifier` relative to the root's directory, split into its name and the directory it is in. */
function nameOf(identifier: string, base: string): { readonly name: string; readonly directory: string } {
  const relative = identifier.startsWith(base) ? identifier.slice(base.length) : identifier;
  const slash = relative.lastIndexOf("/");
  return { name: relative.slice(slash + 1), directory: slash < 0 ? "" : relative.slice(0, slash + 1) };
}

/** How long the host took to fetch it; `null` before it settled. */
function fetchTime(asset: AssetTiming): number | null {
  return asset.fetchStart === null || asset.fetchEnd === null ? null : asset.fetchEnd - asset.fetchStart;
}

function compare(key: SortKey, a: AssetTiming, b: AssetTiming): number {
  switch (key) {
    case "start":
      return a.wantedAt - b.wantedAt || (a.fetchStart ?? Infinity) - (b.fetchStart ?? Infinity);
    case "name":
      return a.identifier < b.identifier ? -1 : a.identifier > b.identifier ? 1 : 0;
    case "round":
      return a.round - b.round;
    case "size":
      return (a.bytes ?? -1) - (b.bytes ?? -1);
    case "time":
      return (fetchTime(a) ?? Infinity) - (fetchTime(b) ?? Infinity);
    case "read":
      return (a.read?.own ?? Infinity) - (b.read?.own ?? Infinity);
  }
}

/** The run's assets whose identifiers contain `query`, in `sort`'s order (ties in the order the stage wanted them). */
function rowsOf(run: LoopRun, query: string, sort: Sort): AssetTiming[] {
  const needle = query.trim().toLowerCase();
  const rows = needle === "" ? [...run.assets] : run.assets.filter((asset) => asset.identifier.toLowerCase().includes(needle));
  const sign = sort.ascending ? 1 : -1;
  // Rows without the measure (the root's read, a fetch under way) stay last either way.
  const missing = (asset: AssetTiming) => (sort.key === "read" ? asset.read?.own == null : sort.key === "time" && fetchTime(asset) === null);
  return rows.sort((a, b) => Number(missing(a)) - Number(missing(b)) || sign * compare(sort.key, a, b));
}

/** What the waterfall draws from: a run of the session's loop, recorded in place (its `version` says when it changed), and the selected prim. */
interface WaterfallProps {
  readonly store: EditorStore;
  readonly session: Session;
  readonly run: LoopRun;
  readonly version: number;
  readonly selection: string | null;
}

/**
 * The waterfall of `run`, one of `session`'s. It draws again only when the
 * run or the selected prim changes, not on every revision of the store
 * (a frame of playback).
 */
export const LoopWaterfall = memo(function LoopWaterfall({ store, session, run, version }: WaterfallProps) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<Sort>({ key: "start", ascending: true });
  const [selected, setSelected] = useState<string | null>(null);
  const rows = useMemo(() => rowsOf(run, query, sort), [run, version, query, sort]);
  const axis = axisOf(run, performance.now());
  const base = session.root.slice(0, session.root.lastIndexOf("/") + 1);
  const rounds = new Map(run.rounds.map((round) => [round.index, round]));
  const boundaries = run.rounds.filter((round) => round.index > 1).map((round) => at(axis, round.wantStart));
  const asset = selected === null ? undefined : run.assets.find((candidate) => candidate.identifier === selected);
  const sortBy = (key: SortKey) => setSort((current) => ({ key, ascending: current.key === key ? !current.ascending : key === "start" || key === "name" }));
  const heading = (key: SortKey, label: string, className?: string) => (
    <button
      type="button"
      onClick={() => sortBy(key)}
      aria-label={`Sort by ${label.toLowerCase()}`}
      className={cn("truncate text-left outline-none hover:text-foreground focus-visible:text-foreground", sort.key === key && "text-foreground", className)}
    >
      {label}
      {sort.key === key && (sort.ascending ? " ↑" : " ↓")}
    </button>
  );
  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col">
        <div className={cn(COLUMNS, "h-8 shrink-0 border-b text-[11px] text-muted-foreground")}>
          <Input aria-label="Filter assets by name" placeholder="Filter by name" value={query} onChange={(event) => setQuery(event.target.value)} className="h-6 text-xs" />
          <span>Use</span>
          {heading("round", "Round")}
          {heading("size", "Size", "text-right")}
          {heading("time", "Fetch", "text-right")}
          {heading("read", "Read", "text-right")}
          <div className="relative h-full">
            <button type="button" onClick={() => sortBy("start")} aria-label="Sort by start" className="absolute inset-0 outline-none" />
            {axis.ticks.map((tick) => (
              <span key={tick} className="pointer-events-none absolute top-0 flex h-full items-center border-l pl-1 text-[10px] whitespace-nowrap tabular-nums" style={{ left: `${at(axis, axis.start + tick)}%` }}>
                {formatTick(tick)}
                {tick === 0 && sort.key === "start" && (sort.ascending ? " ↑" : " ↓")}
              </span>
            ))}
          </div>
        </div>
        <RoundTrack run={run} axis={axis} />
        <Virtualizer layout={ListLayout} layoutOptions={{ rowHeight: ROW_HEIGHT }}>
          <GridList
            aria-label="Assets of the loop"
            items={rows}
            selectionMode="single"
            selectedKeys={selected === null ? [] : [selected]}
            onSelectionChange={(keys) => {
              const [first] = keys === "all" ? [] : [...keys];
              setSelected(first === undefined ? null : String(first));
            }}
            renderEmptyState={() => <p className="p-3 text-xs text-muted-foreground">No asset is named like that.</p>}
            className="min-h-0 flex-1 overflow-y-auto outline-none"
          >
            {(row) => (
              <GridListItem id={row.identifier} textValue={row.identifier} className={rowClass}>
                <AssetRow asset={row} round={rounds.get(row.round)} axis={axis} base={base} boundaries={boundaries} />
              </GridListItem>
            )}
          </GridList>
        </Virtualizer>
        <p className="shrink-0 border-t px-3 py-0.5 text-[10px] text-muted-foreground">
          {rows.length === run.assets.length ? `${rows.length.toLocaleString()} assets` : `${rows.length.toLocaleString()} of ${run.assets.length.toLocaleString()} assets`}
          {" · "}a bar is the fetch, colored by use; the line after it waits on the round's other fetches; the dark bar is the stage reading its layer, and on the round's track, rose is composing
        </p>
      </div>
      {asset !== undefined && (
        <AssetDetails store={store} session={session} asset={asset} run={run} round={rounds.get(asset.round)} base={base} onClose={() => setSelected(null)} />
      )}
    </div>
  );
});

const rowClass = ({ isSelected, isFocusVisible }: { isSelected: boolean; isFocusVisible: boolean }) =>
  cn(
    COLUMNS,
    "h-[22px] cursor-default text-[11px] outline-none select-none hover:bg-muted/60",
    isSelected && "bg-accent text-accent-foreground hover:bg-accent",
    isFocusVisible && "ring-1 ring-ring ring-inset",
  );

/** The run's rounds in words: `the root and 3 rounds`, `2 rounds`. */
function roundsText(run: LoopRun): string {
  const root = run.rounds[0]?.index === 0;
  const rounds = run.rounds.length - (root ? 1 : 0);
  const counted = `${rounds} ${rounds === 1 ? "round" : "rounds"}`;
  return root ? (rounds === 0 ? "the root" : `the root and ${counted}`) : counted;
}

/** Each round's phases: fetching, then the stage's want, supply and read. */
function RoundTrack({ run, axis }: { run: LoopRun; axis: Axis }) {
  // Rounds that start close together (the quick early ones) share the first one's label.
  let last = -Infinity;
  const labelled = run.rounds.map((round) => {
    const left = at(axis, round.wantStart);
    if (left - last < 2.5) return false;
    last = left;
    return true;
  });
  return (
    <div className={cn(COLUMNS, "h-[22px] shrink-0 border-b bg-muted/30 text-[11px]")}>
      <span className="col-span-6 truncate text-muted-foreground">
        {run.cause} · {roundsText(run)}
      </span>
      <div className="relative h-full">
        {run.rounds.map((round, index) => (
          <RoundBar key={round.index} round={round} axis={axis} labelled={labelled[index] ?? false} />
        ))}
        {run.settle !== null && <span className="absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px] bg-foreground/70" style={bar(axis, run.settle.start, run.settle.end)} />}
      </div>
    </div>
  );
}

function RoundBar({ round, axis, labelled }: { round: RoundTiming; axis: Axis; labelled: boolean }) {
  const fetchEnd = round.fetchEnd ?? performance.now();
  const end = round.readEnd ?? fetchEnd;
  const spans: [string, number | null, number | null][] = [
    ["Want (unavailable)", round.wantStart, round.wantEnd],
    ["Fetching", round.wantEnd, round.fetchEnd ?? fetchEnd],
    ["Supply", round.fetchEnd, round.supplyEnd],
    [round.index === 0 ? "Read (Layerstack.open)" : "Read (loadSupplied)", round.readStart, round.readEnd],
  ];
  // What `loadSupplied` reports of its time: decoding layers, then composing.
  const composing = round.compositions.reduce((sum, composition) => sum + (composition.end - composition.start), 0);
  const within: string[] = [];
  if (round.reading !== null) within.push(`reading ${formatDuration(round.reading)}`);
  if (round.compositions.length > 0) within.push(`composing ${formatDuration(composing)}`);
  const describe = spans
    .filter(([, start, finish]) => start !== null && finish !== null)
    .map(([label, start, finish]) => `${label} ${formatDuration(finish! - start!)}`)
    .concat(within)
    .join(", ");
  return (
    <>
      <span className="absolute top-1/2 h-1.5 -translate-y-1/2 bg-muted-foreground/40" style={bar(axis, round.wantEnd, fetchEnd)} />
      {round.wantEnd - round.wantStart > 0 && <span className="absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px] bg-foreground/70" style={bar(axis, round.wantStart, round.wantEnd)} />}
      {round.fetchEnd !== null && round.supplyEnd !== null && (
        <span className="absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px] bg-foreground/70" style={bar(axis, round.fetchEnd, round.supplyEnd)} />
      )}
      {round.readStart !== null && round.readEnd !== null && (
        <span className={cn("absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px]", STAGE_COLOR)} style={bar(axis, round.readStart, round.readEnd)} />
      )}
      {round.compositions.map((composition, index) => (
        <span key={index} className={cn("absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px]", COMPOSE_COLOR)} style={bar(axis, composition.start, composition.end)} />
      ))}
      {labelled && (
        <span className="pointer-events-none absolute top-0 text-[9px] leading-none text-muted-foreground" style={{ left: `${at(axis, round.wantStart)}%` }}>
          {round.index === 0 ? "root" : round.index}
        </span>
      )}
      <TooltipTrigger delay={200} closeDelay={0}>
        <span role="img" tabIndex={-1} aria-label={`Round ${round.index}: ${describe}`} className="absolute inset-y-0 min-w-1 outline-none" style={bar(axis, round.wantStart, end)} />
        <Tooltip className="max-w-sm flex-col items-start gap-0.5">
          <span className="font-medium">{round.index === 0 ? "The root" : `Round ${round.index}`}</span>
          <span>
            {round.assets.toLocaleString()} {round.assets === 1 ? "asset" : "assets"} wanted, {round.loaded.toLocaleString()} layers read
          </span>
          {spans.map(([label, start, finish]) =>
            start === null || finish === null ? null : (
              <span key={label} className="tabular-nums">
                {label}: {formatDuration(finish - start)}
              </span>
            ),
          )}
          {within.length > 0 && <span className="text-muted-foreground tabular-nums">of which {within.join(", ")}</span>}
        </Tooltip>
      </TooltipTrigger>
    </>
  );
}

/** The stage's read of an asset's layer, in words: its own time and work, and what it read as it went. */
function readLines(asset: AssetTiming, round: RoundTiming | undefined): string[] {
  const read = asset.read;
  if (read === null) {
    if (asset.round === 0 && round?.readStart != null && round.readEnd != null) return [`read by the open in ${formatDuration(round.readEnd - round.readStart)}`];
    return [];
  }
  const prims = `${read.prims.toLocaleString()} ${read.prims === 1 ? "prim" : "prims"}`;
  if (read.own === null || read.start === null || read.end === null) return [`read ${formatBytes(read.bytes)} into ${prims}`];
  const lines = [`read in ${formatDuration(read.own)}: ${formatBytes(read.bytes)} into ${prims}`];
  const nested = read.end - read.start - read.own;
  if (nested >= 0.01) lines.push(`and ${formatDuration(nested)} reading the layers it names`);
  return lines;
}

/** What an asset came to, in words. */
function outcomeText(asset: AssetTiming): string {
  switch (asset.outcome) {
    case "pending":
      return asset.fetchEnd === null ? "fetching" : "waiting for the round";
    case "read":
      return asset.source === "saved" ? "loaded, from the saved layers" : "loaded";
    case "absent":
      return "absent: the host has no such asset";
    case "failed":
      return `failed: ${asset.failure ?? "unknown"}`;
  }
}

function AssetRow({ asset, round, axis, base, boundaries }: { asset: AssetTiming; round: RoundTiming | undefined; axis: Axis; base: string; boundaries: readonly number[] }) {
  const { name, directory } = nameOf(asset.identifier, base);
  const now = performance.now();
  const fetchStart = asset.fetchStart;
  const fetchEnd = asset.fetchEnd ?? (fetchStart === null ? null : now);
  // Its own read when the stage reported one; the root's is its round's (`Layerstack.open`).
  const read = asset.read;
  const ownRead = read !== null && read.start !== null && read.end !== null;
  const readStart = ownRead ? read.start : asset.round === 0 ? (round?.readStart ?? null) : null;
  const readEnd = ownRead ? read.end : asset.round === 0 ? (round?.readEnd ?? null) : null;
  const end = readEnd ?? round?.readEnd ?? asset.suppliedAt ?? fetchEnd ?? asset.wantedAt;
  const fetched = fetchTime(asset);
  const role = asset.roles[0] ?? "reference";
  const fetchColor = asset.outcome === "failed" ? "bg-destructive" : asset.outcome === "absent" ? "bg-muted-foreground/50" : ROLE_COLOR[role];
  const lines: string[] = [
    `${asset.roles.join(", ")} · round ${asset.round}`,
    asset.bytes === null ? (asset.source === "absent" ? "absent" : "no bytes") : formatBytes(asset.bytes),
  ];
  if (fetched !== null && fetchStart !== null) lines.push(`fetch ${formatDuration(fetched)} (from +${formatDuration(fetchStart - axis.start)})`);
  if (asset.fetchEnd !== null && asset.suppliedAt !== null) lines.push(`waited ${formatDuration(asset.suppliedAt - asset.fetchEnd)} for the round, then supplied`);
  lines.push(...readLines(asset, round));
  lines.push(outcomeText(asset));
  return (
    <>
      <span className="min-w-0 truncate">
        <span className={cn(asset.outcome === "failed" && "text-destructive", asset.outcome === "absent" && "text-muted-foreground line-through")}>{name}</span>
        {directory !== "" && <span className="ml-1.5 text-muted-foreground">{directory}</span>}
      </span>
      <span className="flex min-w-0 items-center gap-1 truncate text-muted-foreground">
        <span className={cn("size-1.5 shrink-0 rounded-full", ROLE_COLOR[role])} />
        {role}
        {asset.roles.length > 1 && ` +${asset.roles.length - 1}`}
      </span>
      <span className="text-muted-foreground tabular-nums">{asset.round}</span>
      <span className="text-right text-muted-foreground tabular-nums">{asset.bytes === null ? "–" : formatBytes(asset.bytes)}</span>
      <span className="text-right text-muted-foreground tabular-nums">{fetched === null ? "–" : formatDuration(fetched)}</span>
      <span className="text-right text-muted-foreground tabular-nums">{read?.own == null ? "–" : formatDuration(read.own)}</span>
      <div className="relative h-full">
        {boundaries.map((left) => (
          <span key={left} className="absolute inset-y-0 w-px bg-border" style={{ left: `${left}%` }} />
        ))}
        {fetchStart !== null && fetchStart > asset.wantedAt && <span className="absolute top-1/2 h-px bg-muted-foreground/50" style={bar(axis, asset.wantedAt, fetchStart)} />}
        {fetchStart !== null && fetchEnd !== null && <span className={cn("absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px]", fetchColor)} style={bar(axis, fetchStart, fetchEnd)} />}
        {asset.fetchEnd !== null && asset.suppliedAt !== null && <span className="absolute top-1/2 h-px bg-muted-foreground/50" style={bar(axis, asset.fetchEnd, asset.suppliedAt)} />}
        {readStart !== null && readEnd !== null && <span className={cn("absolute top-1/2 h-2 -translate-y-1/2 rounded-[1px]", STAGE_COLOR)} style={bar(axis, readStart, readEnd)} />}
        <TooltipTrigger delay={200} closeDelay={0}>
          <span role="img" tabIndex={-1} aria-label={`${name}: ${lines.join(", ")}`} className="absolute inset-y-0 min-w-1 outline-none" style={bar(axis, asset.wantedAt, end)} />
          <Tooltip className="max-w-sm flex-col items-start gap-0.5">
            <span className="font-medium break-all">{name}</span>
            {lines.map((line) => (
              <span key={line} className="tabular-nums">
                {line}
              </span>
            ))}
          </Tooltip>
        </TooltipTrigger>
      </div>
    </>
  );
}

/** The selected asset: where it is authored, its timings, and the prims composed without it until it loaded. */
function AssetDetails({
  store,
  session,
  asset,
  run,
  round,
  base,
  onClose,
}: {
  store: EditorStore;
  session: Session;
  asset: AssetTiming;
  run: LoopRun;
  round: RoundTiming | undefined;
  base: string;
  onClose: () => void;
}) {
  const { name } = nameOf(asset.identifier, base);
  // Prims can be selected once the session is the open one, not while it opens.
  const live = store.session === session;
  const offset = (time: number | null) => (time === null ? "–" : `+${formatDuration(time - run.startedAt)}`);
  const timings: [string, string][] = [
    ["Wanted", offset(asset.wantedAt)],
    ["Fetch", asset.fetchStart === null ? "–" : `${offset(asset.fetchStart)} for ${asset.fetchEnd === null ? "…" : formatDuration(asset.fetchEnd - asset.fetchStart)}`],
    ["Supplied", offset(asset.suppliedAt)],
    [
      "Read",
      asset.read?.start != null && asset.read.own !== null
        ? `${offset(asset.read.start)} for ${formatDuration(asset.read.own)}`
        : round === undefined || round.readStart === null || round.readEnd === null || asset.round !== 0
          ? "–"
          : `${offset(round.readStart)} for ${formatDuration(round.readEnd - round.readStart)}, by the open`,
    ],
    ["Made", asset.read === null ? "–" : `${asset.read.prims.toLocaleString()} ${asset.read.prims === 1 ? "prim" : "prims"} from ${formatBytes(asset.read.bytes)}`],
    [
      "Its round",
      round === undefined || round.readStart === null || round.readEnd === null
        ? "–"
        : `read ${round.loaded.toLocaleString()} ${round.loaded === 1 ? "layer" : "layers"} in ${formatDuration(round.readEnd - round.readStart)}`,
    ],
    ["Size", asset.bytes === null ? "–" : formatBytes(asset.bytes)],
    ["Became", outcomeText(asset)],
  ];
  return (
    <aside className="flex w-72 shrink-0 flex-col border-l text-[11px]" aria-label={`${name} in the loop`}>
      <div className="flex items-start gap-2 border-b px-3 py-1.5">
        <div className="min-w-0 flex-1">
          <p className="truncate font-medium">{name}</p>
          <p className="font-mono text-[10px] break-all text-muted-foreground">{asset.identifier}</p>
        </div>
        <Button variant="ghost" size="xs" onPress={onClose}>
          Close
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        <dl className="grid grid-cols-[4.5rem_1fr] gap-x-2 gap-y-0.5">
          {timings.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-muted-foreground">{label}</dt>
              <dd className="tabular-nums break-words">{value}</dd>
            </div>
          ))}
        </dl>
        {asset.mentions.length > 0 && (
          <>
            <p className="mt-3 mb-1 text-muted-foreground">Authored</p>
            <ul className="flex flex-col gap-0.5">
              {asset.mentions.map((mention, index) => (
                <li key={index} className="break-all">
                  <span className="text-muted-foreground">{mention.usedBy}</span> <span className="font-mono">@{mention.assetPath}@</span>{" "}
                  <span className="text-muted-foreground">in</span> <span className="font-mono">{nameOf(mention.anchor, base).name}</span>
                </li>
              ))}
            </ul>
          </>
        )}
        {asset.prims.length > 0 && (
          <>
            <p className="mt-3 mb-1 text-muted-foreground">
              {asset.prims.length === 1 ? "The prim" : `${asset.prims.length.toLocaleString()} prims`} composed without it until it loaded
            </p>
            <ul className="flex flex-col">
              {asset.prims.slice(0, PRIMS_SHOWN).map((prim) =>
                live ? (
                  <li key={prim}>
                    <button
                      type="button"
                      onClick={() => store.select(prim)}
                      className={cn("w-full truncate rounded-sm px-1 text-left font-mono hover:bg-muted", store.selection === prim && "bg-accent text-accent-foreground")}
                    >
                      {prim}
                    </button>
                  </li>
                ) : (
                  <li key={prim} className="truncate px-1 font-mono">
                    {prim}
                  </li>
                ),
              )}
            </ul>
            {asset.prims.length > PRIMS_SHOWN && <p className="mt-1 text-muted-foreground">and {asset.prims.length - PRIMS_SHOWN} more</p>}
          </>
        )}
      </div>
    </aside>
  );
}
