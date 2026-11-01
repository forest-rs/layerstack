// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The asset loop, two ways: a waterfall of each run (every asset on one
// time axis, from the stage naming it to the stage reading it) and the
// step-by-step log it has always been. The waterfall is the default; the
// log is a toggle away.
import { useState } from "react";
import { ToggleButton, ToggleButtonGroup } from "react-aria-components";
import { buttonVariants } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import type { Session } from "../model/session.ts";
import { summarize, type LoopRun } from "../model/timeline.ts";
import type { EditorStore } from "../store.ts";
import { formatBytes } from "./format.ts";
import { formatDuration, LoopWaterfall } from "./LoopWaterfall.tsx";

/** The most log rows shown, newest first. */
const SHOWN = 400;

/** The asset loop of the open (or opening) session: its waterfall, or its log. */
export function AssetLoopView({ store }: { store: EditorStore }) {
  const session = store.session ?? store.opening;
  const [mode, setMode] = useState<"waterfall" | "log">("waterfall");
  if (session === null || session.loopLog.length === 0) {
    return <p className="p-3 text-xs text-muted-foreground">The stage has asked for nothing yet.</p>;
  }
  const option = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "xs" });
  const toggle = (
    <ToggleButtonGroup
      aria-label="Show the asset loop as"
      selectionMode="single"
      disallowEmptySelection
      selectedKeys={[mode]}
      onSelectionChange={(keys) => setMode([...keys][0] === "log" ? "log" : "waterfall")}
      className="flex shrink-0 gap-0.5"
    >
      <ToggleButton id="waterfall" className={option}>
        Waterfall
      </ToggleButton>
      <ToggleButton id="log" className={option}>
        Log
      </ToggleButton>
    </ToggleButtonGroup>
  );
  return mode === "waterfall" ? <WaterfallPane store={store} session={session} toggle={toggle} /> : <LogPane session={session} toggle={toggle} />;
}

/** A run's waterfall, the run chosen from the session's (the newest by default), and its totals. */
function WaterfallPane({ store, session, toggle }: { store: EditorStore; session: Session; toggle: React.ReactNode }) {
  const runs = session.loops.runs;
  // `null` follows the newest run, so a new one (a variant selected) shows itself.
  const [chosen, setChosen] = useState<LoopRun | null>(null);
  const run = chosen !== null && runs.includes(chosen) ? chosen : (runs.at(-1) ?? null);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-1.5">
        {toggle}
        {runs.length > 1 && run !== null && (
          <Select
            aria-label="Run of the loop"
            selectedKey={String(runs.indexOf(run))}
            onSelectionChange={(key) => setChosen(key === String(runs.length - 1) ? null : (runs[Number(key)] ?? null))}
          >
            <SelectTrigger size="sm" className="max-w-56">
              <SelectValue className="truncate" />
            </SelectTrigger>
            <SelectContent>
              {runs.map((candidate, index) => (
                <SelectItem key={index} id={String(index)} textValue={candidate.cause}>
                  {candidate.cause} · {candidate.assets.length} assets
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        {run !== null && <Summary run={run} />}
      </div>
      {run === null ? (
        <p className="p-3 text-xs text-muted-foreground">No run of the loop fetched anything.</p>
      ) : (
        <LoopWaterfall key={runs.indexOf(run)} store={store} session={session} run={run} version={run.version} selection={store.selection} />
      )}
    </div>
  );
}

/** A run's totals: what it fetched, how many rounds, and where the time went. */
function Summary({ run }: { run: LoopRun }) {
  const summary = summarize(run);
  const other = Math.max(0, summary.wall - summary.fetching - summary.stage);
  const part = (label: string, value: string, title?: string) => (
    <span title={title}>
      <span className="text-foreground tabular-nums">{value}</span> {label}
    </span>
  );
  return (
    <div className="flex min-w-0 flex-wrap items-baseline gap-x-3 text-[11px] text-muted-foreground">
      {part(summary.assets === 1 ? "asset" : "assets", summary.assets.toLocaleString())}
      {part("", formatBytes(summary.bytes))}
      {part(summary.rounds === 1 ? "round" : "rounds", String(summary.rounds), "Rounds of want, fetch, supply and read after the root's")}
      {part(run.endedAt === null ? "so far" : "wall", formatDuration(summary.wall), "From the first request to the stage wanting nothing more")}
      <span className="flex items-baseline gap-1.5" title="Waiting on fetches, in the stage (unavailable, supply, loadSupplied, the root's open), and the rest (the page's own work)">
        <span className="inline-block size-2 translate-y-px rounded-sm bg-muted-foreground/40" />
        {part("fetching", formatDuration(summary.fetching))}
        <span className="inline-block size-2 translate-y-px rounded-sm bg-foreground/70" />
        {part("in the stage", formatDuration(summary.stage))}
        {other >= 1 && part("other", formatDuration(other))}
      </span>
      {summary.failed > 0 && <span className="text-destructive">{summary.failed} failed</span>}
      {summary.absent > 0 && <span>{summary.absent} absent</span>}
      {run.error !== null && <span className="text-destructive">stopped: {run.error}</span>}
    </div>
  );
}

/** The loop's steps, newest first: what the stage wanted, what the host fetched and supplied, what it read. */
function LogPane({ session, toggle }: { session: Session; toggle: React.ReactNode }) {
  const log = session.loopLog;
  const tone: Record<string, string> = {
    want: "text-sky-400",
    fetch: "text-muted-foreground",
    supply: "text-foreground",
    load: "text-emerald-400",
    fail: "text-destructive",
  };
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-3 border-b px-3 py-1.5">
        {toggle}
        <p className="text-[11px] text-muted-foreground">
          {log.length} steps{log.length > SHOWN && `, the newest ${SHOWN} shown`}
        </p>
      </div>
      <ol className="min-h-0 flex-1 overflow-y-auto font-mono text-[11px]">
        {log
          .slice(-SHOWN)
          .reverse()
          .map((entry, index) => (
            <li key={log.length - index} className="flex gap-3 border-b px-3 py-1">
              <span className={cn("w-12 shrink-0", tone[entry.phase])}>{entry.phase}</span>
              <span className="w-56 shrink-0 truncate" title={entry.identifier}>
                {entry.identifier}
              </span>
              <span className="truncate text-muted-foreground">{entry.detail}</span>
              <span className="ml-auto shrink-0 text-muted-foreground">{entry.cause}</span>
            </li>
          ))}
      </ol>
    </div>
  );
}
