// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A layer as text, read-only: what `Stage.exportLayer(layer, "usda")`
// writes, read again after each edit, undo and redo, with the lines the
// last change touched marked and scrolled to. After an edit, the edit
// layer's text shows the opinion it authored. Lines are drawn as they
// scroll into view, so a large layer scrolls as a small one; a layer whose
// read is slow is read again only when asked.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Layer } from "@forest-rs/layerstack";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";
import { type LayerTextRead, MAX_TEXT_BYTES } from "../model/layerText.ts";
import { type UsdaTokenKind, usdaTokens } from "../model/usdaSyntax.ts";
import type { EditorStore } from "../store.ts";
import { useRevision } from "./context.ts";
import { exportLayerFile } from "./download.ts";
import { formatBytes, layerName } from "./format.ts";
import { countRender } from "./renders.ts";

/** A line's height, in pixels. */
const ROW = 18;
/** Lines drawn past each edge of the view. */
const OVERSCAN = 30;
/** A read slower than this (ms) is not repeated after each edit: the tab asks for a refresh instead. */
const SLOW_MS = 150;

/** How each kind of token is coloured. */
const COLOURS: Record<UsdaTokenKind, string> = {
  comment: "text-muted-foreground italic",
  string: "text-emerald-700 dark:text-emerald-400",
  asset: "text-sky-700 dark:text-sky-400",
  path: "text-violet-700 dark:text-violet-400",
  keyword: "text-rose-700 dark:text-rose-400",
  type: "text-amber-700 dark:text-amber-400",
  number: "text-cyan-700 dark:text-cyan-400",
  plain: "",
};

/** The first line of each run of changes: what Previous and Next step between. */
function changeStarts(read: LayerTextRead): number[] {
  const starts = new Set<number>();
  const added = new Set(read.changes.added);
  for (const line of read.changes.added) if (!added.has(line - 1)) starts.add(line);
  for (const line of read.changes.removedBefore) if (!added.has(line - 1) && !added.has(line)) starts.add(Math.min(line, Math.max(0, read.lines.length - 1)));
  return [...starts].sort((a, b) => a - b);
}

/** The Text tab: the layer chosen (a layer's "Show text"), else the edit layer. */
export function LayerTextView({ store }: { store: EditorStore }) {
  countRender("LayerTextView");
  // Edits, undo and redo change the text; `view` the layer shown.
  const revision = useRevision(store, "composition", "history", "view");
  const layer = store.textLayer;
  const stage = store.session?.stage;
  const [refreshed, setRefreshed] = useState(0);
  const last = layer === null ? null : store.lastLayerText(layer);
  const slow = last !== null && last.ms > SLOW_MS;
  // The revision a slow layer was last read at, to say its text may be behind.
  const readAt = useRef(revision);
  const read = useMemo(() => {
    if (layer === null) return null;
    readAt.current = revision;
    return store.layerText(layer);
    // A slow layer is read again on Refresh, not on every change.
  }, [store, layer, slow ? refreshed : revision]);
  if (stage === undefined || layer === null) {
    return <p className="p-3 text-xs text-muted-foreground">Open a scene, then choose "Show text" on any layer (its context menu, in the Layers tab, Explain or History) to read it as USDA.</p>;
  }
  const editLayer = stage.editLayer;
  const target = store.session?.target;
  const editsHere = target === undefined ? layer === editLayer : "kind" in target ? target.layer === layer : target === layer;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs">
        <LayerPicker store={store} layer={layer} />
        <span className="text-muted-foreground">
          as USDA, its unsaved edits included{editsHere ? "; edits go here" : ""}
          {store.isEdited(layer) ? "; edited" : ""}
        </span>
        <Button variant="ghost" size="xs" className="ml-auto" onPress={() => exportLayerFile(store, layer, "usda")}>
          Export .usda
        </Button>
        {slow && (
          <Button variant="outline" size="xs" onPress={() => setRefreshed((count) => count + 1)}>
            Refresh
          </Button>
        )}
      </div>
      {typeof read === "string" || read === null ? (
        <p className="p-3 text-xs text-muted-foreground">This layer cannot be written as text: {read}</p>
      ) : (
        <TextBody read={read} note={slow && readAt.current !== revision ? `Read in ${Math.round(read.ms)} ms, so it is read again on Refresh, not after every edit; it may be behind.` : null} />
      )}
    </div>
  );
}

/** Chooses the layer shown, among those the stage holds. */
function LayerPicker({ store, layer }: { store: EditorStore; layer: Layer }) {
  const layers = store.session?.stage.layers() ?? [];
  return (
    <Select
      aria-label="The layer shown as text"
      value={String(layer.id)}
      onChange={(key) => {
        const chosen = layers.find((each) => String(each.id) === key);
        if (chosen !== undefined) store.showLayerText(chosen);
      }}
      className="max-w-80"
    >
      <SelectTrigger size="sm" className="font-mono text-[11px]">
        <SelectValue className="truncate" />
      </SelectTrigger>
      <SelectContent>
        {layers.map((each) => (
          <SelectItem key={String(each.id)} id={String(each.id)} textValue={layerName(each.identifier)}>
            <span className="font-mono text-[11px]">{layerName(each.identifier)}</span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/** What changed, the way between the changes, and the lines. */
function TextBody({ read, note }: { read: LayerTextRead; note: string | null }) {
  const starts = useMemo(() => changeStarts(read), [read]);
  // Which change is in view: a new read shows its first.
  const [at, setAt] = useState<{ read: LayerTextRead; index: number }>({ read, index: 0 });
  const index = at.read === read ? at.index : 0;
  const focus = starts.length === 0 ? null : starts[Math.min(index, starts.length - 1)]!;
  const { added, removed } = read.changes;
  const step = (by: number) => setAt({ read, index: (index + by + starts.length) % starts.length });
  return (
    <>
      <div className="flex flex-wrap items-center gap-2 border-b px-3 py-1 text-[11px] text-muted-foreground">
        {starts.length === 0 ? (
          <span>{read.lines.length.toLocaleString()} lines. The lines an edit changes are marked here after it.</span>
        ) : (
          <>
            <span className="text-foreground">
              The last change: {added.length > 0 && `${added.length} ${added.length === 1 ? "line" : "lines"} new or changed`}
              {added.length > 0 && removed > 0 && ", "}
              {removed > 0 && `${removed} ${removed === 1 ? "line" : "lines"} gone`}
            </span>
            {starts.length > 1 && (
              <>
                <Button variant="ghost" size="xs" onPress={() => step(-1)}>
                  Previous
                </Button>
                <span>
                  {index + 1} of {starts.length}
                </span>
                <Button variant="ghost" size="xs" onPress={() => step(1)}>
                  Next
                </Button>
              </>
            )}
          </>
        )}
        {read.truncated && (
          <span className="text-amber-700 dark:text-amber-400">
            Showing the first {formatBytes(MAX_TEXT_BYTES)} of {formatBytes(read.bytes)}; export the layer for all of it.
          </span>
        )}
        {note !== null && <span className="text-amber-700 dark:text-amber-400">{note}</span>}
      </div>
      <Lines read={read} focus={focus} />
    </>
  );
}

/** The lines, drawn as they scroll into view, the changed ones marked; scrolled to `focus` when it changes. */
function Lines({ read, focus }: { read: LayerTextRead; focus: number | null }) {
  const ref = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState(0);
  const [height, setHeight] = useState(300);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null) return;
    setHeight(element.clientHeight);
    const observer = new ResizeObserver(() => setHeight(element.clientHeight));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  // A change, or stepping to another, scrolls it a third of the way down the view.
  useEffect(() => {
    const element = ref.current;
    if (element === null || focus === null) return;
    const top = focus * ROW;
    if (top < element.scrollTop || top > element.scrollTop + element.clientHeight - ROW) element.scrollTop = Math.max(0, top - element.clientHeight / 3);
  }, [read, focus]);
  const added = useMemo(() => new Set(read.changes.added), [read]);
  const removedBefore = useMemo(() => new Set(read.changes.removedBefore), [read]);
  const lines = read.lines;
  const first = Math.max(0, Math.floor(scroll / ROW) - OVERSCAN);
  const last = Math.min(lines.length, Math.ceil((scroll + height) / ROW) + OVERSCAN);
  const gutter = `${String(lines.length).length + 1}ch`;
  const rows = [];
  for (let line = first; line < last; line++) {
    const removedHere = removedBefore.has(line) || (line === lines.length - 1 && removedBefore.has(lines.length));
    rows.push(
      <div
        key={line}
        className={cn("flex min-w-max", added.has(line) && "bg-amber-500/15", removedHere && "shadow-[inset_0_2px_0_0] shadow-red-500/70")}
        style={{ height: ROW }}
      >
        <span className="shrink-0 pr-3 text-right text-muted-foreground/70 select-none" style={{ width: gutter }}>
          {line + 1}
        </span>
        <span className="pr-4 whitespace-pre">
          {usdaTokens(lines[line]!).map((token, at) => (
            <span key={at} className={COLOURS[token.kind]}>
              {token.text}
            </span>
          ))}
        </span>
      </div>,
    );
  }
  return (
    <div
      ref={ref}
      tabIndex={0}
      role="region"
      aria-label="The layer's text"
      className="min-h-0 flex-1 overflow-auto font-mono text-[11px] leading-[18px] outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
      onScroll={(event) => setScroll(event.currentTarget.scrollTop)}
    >
      <div className="relative min-w-max" style={{ height: lines.length * ROW }}>
        <div className="absolute inset-x-0" style={{ top: first * ROW }}>
          {rows}
        </div>
      </div>
    </div>
  );
}
