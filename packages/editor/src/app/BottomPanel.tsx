// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What happened, and why: the focused property's value and every opinion
// the stage consulted for it, the recompositions each action caused, the
// undo history, the asset loop's requests and answers, what drawing
// costs, and what the stage refused.
import type { Explanation, Opinion } from "@forest-rs/layerstack";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { formatValue, releaseValue } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { AssetLoopView } from "./AssetLoop.tsx";
import { CostView } from "./CostView.tsx";
import { useStore } from "./context.ts";
import { formatTime, layerName } from "./format.ts";
import { ShadingView } from "./ShadingView.tsx";
import { targetText } from "./TargetPicker.tsx";

/** The most log rows shown, newest first. */
const SHOWN = 400;

/** The bottom panel; `onTab` hears which tab is chosen, so the frame can give a tab that draws a graph more room. */
export function BottomPanel({ onTab }: { onTab?: (tab: string) => void }) {
  const store = useStore();
  const notices = store.notices.length;
  return (
    <Tabs className="flex h-full flex-col gap-0" onSelectionChange={(key) => onTab?.(String(key))}>
      <TabsList variant="line" className="w-full justify-start border-b px-2">
        <TabsTrigger className="flex-none px-2.5" id="explain">Explain</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="changes">Changes</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="history">History</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="assets">Asset loop</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="shading">Shading</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="cost">Draw cost</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="notices">
          Notices
          {notices > 0 && <Badge variant="destructive">{notices}</Badge>}
        </TabsTrigger>
      </TabsList>
      <TabsContent id="explain" className="min-h-0 overflow-y-auto">
        <ExplainView store={store} />
      </TabsContent>
      <TabsContent id="changes" className="min-h-0 overflow-y-auto">
        <ChangesView store={store} />
      </TabsContent>
      <TabsContent id="history" className="min-h-0 overflow-y-auto">
        <HistoryView store={store} />
      </TabsContent>
      <TabsContent id="assets" className="min-h-0">
        <AssetLoopView store={store} />
      </TabsContent>
      <TabsContent id="shading" className="min-h-0">
        <ShadingView />
      </TabsContent>
      <TabsContent id="cost" className="min-h-0 overflow-y-auto">
        <CostView store={store} />
      </TabsContent>
      <TabsContent id="notices" className="min-h-0 overflow-y-auto">
        <NoticesView store={store} />
      </TabsContent>
    </Tabs>
  );
}

/** The focused property's resolved value, where it comes from, and each opinion strongest first. */
function ExplainView({ store }: { store: EditorStore }) {
  const stage = store.session?.stage;
  const path = store.property;
  if (stage === undefined || path === null) {
    return <p className="p-3 text-xs text-muted-foreground">Choose a property's name in the inspector to see why it has its value.</p>;
  }
  const attribute = stage.attribute(path);
  const explanation: Explanation | null = attribute?.explain(store.time) ?? null;
  if (attribute === null || explanation === null) {
    return <p className="p-3 text-xs text-muted-foreground">{path} has no value to explain.</p>;
  }
  const value = formatValue(explanation.value);
  releaseValue(explanation.value);
  const samples = attribute.timeSamples();
  return (
    <div className="p-3 text-xs">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="font-mono">{path}</span>
        {store.time !== undefined && <span className="text-muted-foreground">at {formatTime(store.time)}</span>}
        <span className="text-muted-foreground">=</span>
        <span className="font-mono">{value}</span>
      </div>
      <div className="mt-1 text-muted-foreground">
        from <span className="text-foreground">{explanation.source}</span>
        {explanation.samples !== null && ` (between samples ${explanation.samples.lower} and ${explanation.samples.upper})`}
        {samples.length > 0 && ` · ${samples.length} time samples, ${samples[0]} to ${samples.at(-1)}`}
      </div>
      <table className="mt-3 w-full border-collapse text-[11px]">
        <thead className="text-left text-muted-foreground">
          <tr>
            <th className="py-1 pr-3 font-normal">Opinion</th>
            <th className="py-1 pr-3 font-normal">Layer</th>
            <th className="py-1 pr-3 font-normal">Spec</th>
            <th className="py-1 pr-3 font-normal">Through</th>
            <th className="py-1 font-normal" />
          </tr>
        </thead>
        <tbody>
          {explanation.opinions.map((opinion, index) => (
            <OpinionRow key={index} store={store} opinion={opinion} />
          ))}
        </tbody>
      </table>
      {explanation.opinions.length === 0 && <p className="mt-2 text-muted-foreground">No layer has an opinion: the value is the schema's fallback.</p>}
    </div>
  );
}

function OpinionRow({ store, opinion }: { store: EditorStore; opinion: Opinion }) {
  const role = opinion.role === "ignored" ? `ignored (${opinion.cause})` : opinion.role === "contributed" ? `contributed ${opinion.contribution}` : "block";
  const target = opinion.editTarget;
  const current = store.session?.target;
  const editing = target !== null && current !== undefined && "kind" in current && targetText(current) === targetText(target);
  return (
    <tr className={cn("border-t", opinion.role === "ignored" && "text-muted-foreground")}>
      <td className="py-1 pr-3">{role}</td>
      <td className="py-1 pr-3 font-mono">{layerName(opinion.layer.identifier)}</td>
      <td className="py-1 pr-3 font-mono">{opinion.specPath}</td>
      <td className="py-1 pr-3">{opinion.arc ?? "the root layer stack"}</td>
      <td className="py-1 text-right">
        {target !== null && (
          <Button variant={editing ? "secondary" : "ghost"} size="xs" onPress={() => store.setTarget(target)}>
            {editing ? "Editing here" : "Edit here"}
          </Button>
        )}
      </td>
    </tr>
  );
}

/** Each recomposition, newest first, with the action that caused it. */
function ChangesView({ store }: { store: EditorStore }) {
  const log = store.session?.changeLog ?? [];
  if (log.length === 0) return <p className="p-3 text-xs text-muted-foreground">No recompositions yet.</p>;
  return (
    <ol className="flex flex-col text-[11px]">
      {log
        .slice(-SHOWN)
        .reverse()
        .map(({ cause, changes }, index) => {
          const parts: [string, readonly string[]][] = [
            ["created", changes.created],
            ["removed", changes.removed],
            ["resynced", changes.resynced],
            ["info changed", changes.changedInfoOnly],
            ["properties resynced", changes.resyncedProperties],
            ["muted", changes.mutedIdentifiers],
            ["unmuted", changes.unmutedIdentifiers],
          ];
          return (
            <li key={log.length - index} className="border-b px-3 py-1.5">
              <span className="font-medium">{cause}</span>
              {parts
                .filter(([, paths]) => paths.length > 0)
                .map(([label, paths]) => (
                  <span key={label} className="ml-3 text-muted-foreground">
                    {label} <span className="font-mono text-foreground">{paths.slice(0, 4).join(", ")}</span>
                    {paths.length > 4 && ` and ${paths.length - 4} more`}
                  </span>
                ))}
            </li>
          );
        })}
    </ol>
  );
}

/** The undo stack (newest first), then what was undone. */
function HistoryView({ store }: { store: EditorStore }) {
  const session = store.session;
  if (session === null) return null;
  const undo = [...session.history.undoSteps].reverse();
  const redo = [...session.history.redoSteps].reverse();
  if (undo.length === 0 && redo.length === 0) return <p className="p-3 text-xs text-muted-foreground">No edits yet.</p>;
  const row = (label: string, layers: readonly { identifier: string | null }[], state: "done" | "undone", key: string) => (
    <li key={key} className={cn("flex items-center gap-2 border-b px-3 py-1.5 text-[11px]", state === "undone" && "text-muted-foreground")}>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="font-mono text-muted-foreground">{layers.map((layer) => layerName(layer.identifier)).join(", ")}</span>
    </li>
  );
  return (
    <div>
      <div className="flex gap-1 px-3 py-1.5">
        <Button variant="outline" size="xs" isDisabled={undo.length === 0} onPress={() => void store.undo()}>
          Undo
        </Button>
        <Button variant="outline" size="xs" isDisabled={redo.length === 0} onPress={() => void store.redo()}>
          Redo
        </Button>
      </div>
      <ol>
        {redo.map((step, index) => row(step.label, step.layers, "undone", `redo-${index}`))}
        {undo.map((step, index) => row(step.label, step.layers, "done", `undo-${index}`))}
      </ol>
    </div>
  );
}

/** What actions reported: refused edits, failed fetches. */
function NoticesView({ store }: { store: EditorStore }) {
  if (store.notices.length === 0) return <p className="p-3 text-xs text-muted-foreground">Nothing to report.</p>;
  return (
    <ol>
      {[...store.notices].reverse().map((notice) => (
        <li key={notice.id} className="flex items-start gap-2 border-b px-3 py-1.5 text-[11px]">
          <span className="font-medium">{notice.action}</span>
          {notice.code !== null && <Badge variant="outline">{notice.code}</Badge>}
          <span className="min-w-0 flex-1">{notice.message}</span>
          <Button variant="ghost" size="xs" onPress={() => store.dismiss(notice.id)}>
            Dismiss
          </Button>
        </li>
      ))}
    </ol>
  );
}
