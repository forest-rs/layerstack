// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What happened, and why: the focused property's value and every opinion
// the stage consulted for it, the recompositions each action caused, the
// undo history, the asset loop's requests and answers, what drawing
// costs, and what the stage refused.
import { useEffect } from "react";
import type { Attribute, Explanation, Layer, Opinion } from "@forest-rs/layerstack";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";

import { bracketNote } from "../model/keyTimes.ts";
import type { EditorStore } from "../store.ts";
import { BOTTOM_TABS } from "../store/view.ts";
import { AssetLoopView } from "./AssetLoop.tsx";
import { CostView } from "./CostView.tsx";
import { useEditorStore, useRevision, useStoreSelector, useTime } from "./context.ts";
import { shown } from "./fields.tsx";
import { formatTime, layerName } from "./format.ts";
import { LayerTextView } from "./LayerTextView.tsx";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { ShadingView } from "./ShadingView.tsx";
import { targetText } from "./TargetPicker.tsx";
import { countRender } from "./renders.ts";

/** The most log rows shown, newest first. */
const SHOWN = 400;

/** The bottom panel; `onTab` hears which tab is chosen, so the frame can give a tab that draws a graph more room. */
export function BottomPanel({ onTab }: { onTab?: (tab: string) => void }) {
  countRender("BottomPanel");
  const store = useEditorStore();
  // The tabs themselves show only how many notices there are; each view listens for what it shows.
  const notices = useStoreSelector(store, ["notices"], (at) => at.notices.length);
  // The tab in front is the store's, so explaining a property brings its explanation forward.
  const tab = useStoreSelector(store, ["view"], (at) => at.bottomTab);
  // The frame hears of the tab however it was chosen.
  useEffect(() => onTab?.(tab), [tab]);
  return (
    <Tabs
      className="flex h-full flex-col gap-0"
      selectedKey={tab}
      onSelectionChange={(key) => {
        const chosen = BOTTOM_TABS.find((each) => each === key);
        if (chosen !== undefined) store.showBottomTab(chosen);
      }}
    >
      <TabsList variant="line" className="w-full justify-start border-b px-2">
        <TabsTrigger className="flex-none px-2.5" id="explain">Explain</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="changes">Changes</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="history">History</TabsTrigger>
        <TabsTrigger className="flex-none px-2.5" id="text">Text</TabsTrigger>
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
      <TabsContent id="text" className="min-h-0">
        <LayerTextView store={store} />
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
  countRender("ExplainView");
  useRevision(store, "selection", "composition", "target");
  const stage = store.session?.stage;
  const path = store.property;
  const attribute = stage === undefined || path === null ? null : stage.attribute(path);
  // Only an animated property is explained again as time moves.
  const time = useTime(store, attribute?.mightBeTimeVarying() === true);
  if (stage === undefined || path === null) {
    return <p className="p-3 text-xs text-muted-foreground">Choose a property's name in the inspector to see why it has its value.</p>;
  }
  const explanation: Explanation | null = attribute?.explain(time) ?? null;
  if (attribute === null || explanation === null) {
    return <p className="p-3 text-xs text-muted-foreground">{path} has no value to explain.</p>;
  }
  const typeName = attribute.typeName ?? explanation.value?.type ?? "";
  const value = shown(explanation.value);
  const context = presentationContext(store);
  const samples = attribute.timeSamples();
  return (
    <div className="p-3 text-xs">
      <div className="flex flex-wrap items-baseline gap-x-2">
        {context !== null ? <Present object={{ kind: "propertyPath", path }} context={context} /> : <span className="font-mono">{path}</span>}
        {time !== undefined && <span className="text-muted-foreground">at {formatTime(time)}</span>}
        <span className="text-muted-foreground">=</span>
        {context !== null ? <Present object={{ kind: "value", attribute, typeName, shown: value, at: null }} context={context} /> : <span className="font-mono">{value.text}</span>}
      </div>
      <div className="mt-1 text-muted-foreground">
        from <span className="text-foreground">{explanation.source}</span>
        {samples.length > 0 && ` · ${samples.length} time samples, ${samples[0]} to ${samples.at(-1)}`}
      </div>
      {time !== undefined && <SampleNote store={store} attribute={attribute} time={time} typeName={typeName} />}
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

/**
 * Where the time falls among the attribute's samples, each sample's time a
 * button that moves the timeline there. The stage does not say whether it
 * interpolates or holds, so only a type USD always holds is said to be held.
 */
function SampleNote({ store, attribute, time, typeName }: { store: EditorStore; attribute: Attribute; time: number; typeName: string }) {
  const note = bracketNote(attribute.bracketingSamples(time), time, typeName);
  if (note.kind === "none") return null;
  const at = (sample: number) => (
    <button type="button" className="font-mono text-foreground underline decoration-dotted underline-offset-2 hover:decoration-solid" onClick={() => store.seek(sample)}>
      {formatTime(sample)}
    </button>
  );
  return (
    <div className="mt-1 text-muted-foreground">
      At {formatTime(time)}:{" "}
      {note.kind === "on" && <>on the sample at {at(note.time)}</>}
      {note.kind === "before" && <>before the first sample, so its value: the sample at {at(note.time)}</>}
      {note.kind === "after" && <>after the last sample, so its value: the sample at {at(note.time)}</>}
      {note.kind === "between" && (
        <>
          between the samples at {at(note.lower)} and {at(note.upper)}
          {note.held && `; a ${typeName} is held at the earlier one, never interpolated`}
        </>
      )}
    </div>
  );
}

function OpinionRow({ store, opinion }: { store: EditorStore; opinion: Opinion }) {
  const role = opinion.role === "ignored" ? `ignored (${opinion.cause})` : opinion.role === "contributed" ? `contributed ${opinion.contribution}` : "block";
  const target = opinion.editTarget;
  const current = store.session?.target;
  const editing = target !== null && current !== undefined && "kind" in current && targetText(current) === targetText(target);
  const context = presentationContext(store);
  return (
    <tr className={cn("border-t", opinion.role === "ignored" && "text-muted-foreground")}>
      <td className="py-1 pr-3">{role}</td>
      <td className="py-1 pr-3 font-mono">{context === null ? layerName(opinion.layer.identifier) : <Present object={{ kind: "layer", layer: opinion.layer }} context={context} />}</td>
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
  useRevision(store, "composition");
  const log = store.session?.changeLog ?? [];
  const context = presentationContext(store);
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
            ["properties changed", [...new Set(changes.changedProperties.map((change) => change.property))]],
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
                    {label}{" "}
                    {context === null || label === "muted" || label === "unmuted" ? (
                      <span className="font-mono text-foreground">{paths.slice(0, 4).join(", ")}</span>
                    ) : (
                      paths.slice(0, 4).map((path, at) => (
                        <span key={path}>
                          {at > 0 && ", "}
                          <Present object={path.includes(".") ? { kind: "propertyPath", path } : { kind: "primPath", path }} context={context} />
                        </span>
                      ))
                    )}
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
  useRevision(store, "history", "composition");
  const session = store.session;
  if (session === null) return null;
  const undo = [...session.history.undoSteps].reverse();
  const redo = [...session.history.redoSteps].reverse();
  const context = presentationContext(store);
  if (undo.length === 0 && redo.length === 0) return <p className="p-3 text-xs text-muted-foreground">No edits yet.</p>;
  const row = (label: string, layers: readonly Layer[], state: "done" | "undone", key: string) => (
    <li key={key} className={cn("flex items-center gap-2 border-b px-3 py-1.5 text-[11px]", state === "undone" && "text-muted-foreground")}>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="flex gap-2 font-mono text-muted-foreground">
        {layers.map((layer, at) => (context === null ? <span key={at}>{layerName(layer.identifier)}</span> : <Present key={at} object={{ kind: "layer", layer }} context={context} />))}
      </span>
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
  useRevision(store, "notices");
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
