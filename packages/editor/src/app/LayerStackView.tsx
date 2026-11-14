// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The Layers tab: the stage's own layer stack as a tree in strength order
// (`model/layerStack.ts`), then the layers reached through references and
// payloads, then muted layers the stage released. A layer of the stack
// that takes edits offers adding a sublayer, and each of its sublayers
// moving stronger or weaker, a new offset, or removal: each one undoable
// edit of that layer's `subLayers`, after which the asset loop reads what
// it names. Every layer is live, as anywhere: a click shows its text, a
// context menu offers the rest. Reading the layers again from the source
// picks up files changed on disk.
import { useMemo, useState } from "react";
import type { Layer } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon, Refresh01Icon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { ancestorsOf, layerStack, offsetText, offsetUsda, type StackRow } from "../model/layerStack.ts";
import type { EditorStore, ReloadReport } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { layerName } from "./format.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { countRender } from "./renders.ts";
import { AddSublayerDialog, SublayerOffsetDialog } from "./SublayerDialogs.tsx";

/** A dialog the tab has open. */
type Open = { readonly kind: "add"; readonly row: StackRow<Layer> } | { readonly kind: "offset"; readonly row: StackRow<Layer> } | null;

/** The layers of the stage: its stack in strength order, the layers its arcs reach, and the muted ones it released. */
export function LayerStackView() {
  countRender("LayerStackView");
  const store = useEditorStore();
  // The stack is the stage's (`composition`); a save clears the edited marks (`history`); the tab in front decides what a click on a layer does (`view`).
  const composition = useRevision(store, "composition");
  useRevision(store, "history", "view", "status");
  const [open, setOpen] = useState<Open>(null);
  // The last refusal, by the row whose action caused it.
  const [refused, setRefused] = useState<{ readonly key: string; readonly message: string } | null>(null);
  const session = store.session;
  const stage = session?.stage;
  const tree = useMemo(() => {
    if (session === null || stage === undefined) return null;
    return layerStack({
      root: stage.root,
      session: session.sessionLayer,
      held: stage.layers(),
      sublayers: (layer) => stage.sublayers(layer),
      muted: (layer) => stage.isMuted(layer),
    });
  }, [session, stage, composition]);
  if (session === null || stage === undefined || tree === null) return null;
  const context = presentationContext(store);
  const health = store.health;
  const missing = new Set((health?.missing ?? []).flatMap((asset) => (asset.identifier === null ? [] : [asset.identifier])));
  const shown = new Set([...tree.rows.map((row) => row.identifier), ...tree.reached.map((layer) => layer.identifier)]);
  const released = stage.mutedIdentifiers().filter((identifier) => !shown.has(identifier));
  const editable = (layer: Layer | null): layer is Layer => layer !== null && !layer.readOnly && stage.hasLocalLayer(layer);
  const act = (row: StackRow<Layer>, work: Promise<string | undefined>) => {
    setRefused(null);
    void work.then((message) => message !== undefined && setRefused({ key: row.key, message }));
  };
  return (
    <div className="flex flex-col gap-2 p-1 pb-3">
      <StackHeader store={store} />
      <ol aria-label="The root layer stack, strongest first" className="flex flex-col">
        {tree.rows.map((row) => (
          <StackRowView
            key={row.key}
            store={store}
            row={row}
            missing={row.identifier !== null && missing.has(row.identifier)}
            context={context}
            canAdd={row.state === "read" && editable(row.layer)}
            canEditParent={editable(row.parent)}
            refusal={refused?.key === row.key ? refused.message : null}
            onAdd={() => setOpen({ kind: "add", row })}
            onOffset={() => setOpen({ kind: "offset", row })}
            onMove={(by) => row.parent !== null && row.asset !== null && act(row, store.moveSublayer(row.parent, row.index, by, row.asset))}
            onRemove={() => row.parent !== null && row.asset !== null && act(row, store.removeSublayer(row.parent, row.index, row.asset))}
          />
        ))}
      </ol>
      {tree.reached.length > 0 && (
        <section aria-label="Reached by references and payloads">
          <h3 className="px-2 pt-2 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">Reached by references and payloads</h3>
          <p className="px-2 pb-1 text-[11px] text-muted-foreground">Not in the stack: their opinions come in through the prims whose arcs name them, weaker than the stack's on those prims.</p>
          <ul className="flex flex-col">
            {tree.reached.map((layer) => (
              <li key={String(layer.id)} className="flex items-center gap-2 rounded-md px-2 py-1 text-xs hover:bg-muted">
                <LayerName store={store} layer={layer} muted={stage.isMuted(layer)} />
                <LayerBadges store={store} layer={layer} />
                <MuteSwitch store={store} layer={layer} muted={stage.isMuted(layer)} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {released.length > 0 && (
        <section aria-label="Muted and released">
          <h3 className="px-2 pt-2 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">Muted and released</h3>
          <ul className="flex flex-col">
            {released.map((identifier) => (
              <li key={identifier} className="flex items-center gap-2 rounded-md px-2 py-1 text-xs hover:bg-muted">
                <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground line-through" title={identifier}>
                  {layerName(identifier)}
                </span>
                <Badge variant="outline">released</Badge>
                <Switch size="sm" aria-label={`Unmute ${identifier}`} isSelected={false} onChange={() => void store.setMuted(identifier, false)} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {open?.kind === "add" && open.row.layer !== null && (
        <AddSublayerDialog
          store={store}
          parent={open.row.layer}
          present={tree.rows.filter((row) => row.parent === open.row.layer && row.identifier !== null).map((row) => row.identifier!)}
          above={ancestorsOf(tree.rows, open.row)}
          onClose={() => setOpen(null)}
        />
      )}
      {open?.kind === "offset" && open.row.parent !== null && open.row.asset !== null && (
        <SublayerOffsetDialog store={store} parent={open.row.parent} index={open.row.index} asset={open.row.asset} onClose={() => setOpen(null)} />
      )}
    </div>
  );
}

/** What the order means, and reading the layers again from the source. */
function StackHeader({ store }: { store: EditorStore }) {
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<ReloadReport | null>(null);
  const reload = async () => {
    setBusy(true);
    setReport(await store.reload(null));
    setBusy(false);
  };
  const said =
    report === null
      ? null
      : report.refusal !== null
        ? report.refusal
        : report.reloaded.length === 0
          ? `Nothing changed in the source${report.skipped.length > 0 ? `; ${report.skipped.length} with unsaved edits left as they are` : ""}.`
          : `Read again: ${report.reloaded.map((layer) => layerName(layer.identifier)).join(", ")}${report.skipped.length > 0 ? `; ${report.skipped.length} with unsaved edits left as they are` : ""}.`;
  return (
    <div className="flex flex-col gap-1 px-2 pt-1">
      <div className="flex items-start gap-2">
        <p className="min-w-0 flex-1 text-[11px] text-muted-foreground">
          <span className="font-medium text-foreground">Stronger above weaker.</span> Where two layers of the stack have an opinion of the same thing, the one higher here wins: the
          session layer over the root, a layer over its sublayers, a sublayer over those listed after it.
        </p>
        <TooltipTrigger>
          <Button variant="outline" size="xs" isDisabled={busy || store.status.kind !== "ready"} onPress={() => void reload()}>
            {busy ? <Spinner className="size-3" data-icon="inline-start" /> : <HugeiconsIcon icon={Refresh01Icon} data-icon="inline-start" />}
            Reload
          </Button>
          <Tooltip>Read every layer again from the source: files changed on disk come in, and only what changed recomposes. A layer with unsaved edits is left as it is.</Tooltip>
        </TooltipTrigger>
      </div>
      {said !== null && (
        <p role="status" className={cn("text-[11px]", report?.refusal !== null ? "text-destructive" : "text-muted-foreground")}>
          {said}
        </p>
      )}
    </div>
  );
}

/** A layer's name, live when there is a stage. */
function LayerName({ store, layer, muted }: { store: EditorStore; layer: Layer; muted: boolean }) {
  const context = presentationContext(store);
  return (
    <span className={cn("flex min-w-0 flex-1", muted && "line-through opacity-60")}>
      {context === null ? <span className="truncate font-mono text-[11px]">{layerName(layer.identifier)}</span> : <Present object={{ kind: "layer", layer }} context={context} />}
    </span>
  );
}

/** What a layer's row says of it besides its name: edited, read-only. */
function LayerBadges({ store, layer }: { store: EditorStore; layer: Layer }) {
  return (
    <>
      {store.isEdited(layer) && <Badge variant="secondary">edited</Badge>}
      {layer.readOnly && <Badge variant="outline">read-only</Badge>}
    </>
  );
}

/** The switch that mutes or unmutes a layer: the stage's state, not an edit. */
function MuteSwitch({ store, layer, muted }: { store: EditorStore; layer: Layer; muted: boolean }) {
  const name = layerName(layer.identifier);
  return <Switch size="sm" aria-label={muted ? `Unmute ${name}` : `Mute ${name}`} isSelected={!muted} onChange={(on) => void store.setMuted(layer, !on)} />;
}

/** One layer of the stack: its place in the strength order, its name, how its parent lists it, and what can be done to it. */
function StackRowView({
  store,
  row,
  missing,
  context,
  canAdd,
  canEditParent,
  refusal,
  onAdd,
  onOffset,
  onMove,
  onRemove,
}: {
  store: EditorStore;
  row: StackRow<Layer>;
  missing: boolean;
  context: ReturnType<typeof presentationContext>;
  canAdd: boolean;
  canEditParent: boolean;
  refusal: string | null;
  onAdd: () => void;
  onOffset: () => void;
  onMove: (by: number) => void;
  onRemove: () => void;
}) {
  const { layer } = row;
  const muted = row.state === "muted";
  const offset = offsetText(row.offset);
  const parentName = row.parent === null ? null : layerName(row.parent.identifier);
  const name = layer === null ? (row.identifier ?? row.asset ?? "") : layerName(layer.identifier);
  const sublayer = row.role === "sublayer";
  const hasMenu = canAdd || (sublayer && canEditParent);
  return (
    <li className="group flex flex-col rounded-md px-2 py-1 text-xs hover:bg-muted" style={{ paddingLeft: `${row.depth * 14 + 8}px` }}>
      <div className="flex min-w-0 items-center gap-1.5">
        <span
          className="w-4 shrink-0 text-right font-mono text-[10px] text-muted-foreground tabular-nums"
          title={row.strength > 0 ? `${ordinal(row.strength)} strongest in the stack` : "Composes nothing"}
        >
          {row.strength > 0 ? row.strength : "–"}
        </span>
        {layer === null ? (
          <span className={cn("min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground", muted && "line-through")} title={row.identifier ?? undefined}>
            {name}
          </span>
        ) : context === null ? (
          <span className="min-w-0 flex-1 truncate font-mono text-[11px]">{name}</span>
        ) : (
          <span className={cn("flex min-w-0 flex-1", muted && "line-through opacity-60")}>
            <Present object={{ kind: "layer", layer }} context={context} />
          </span>
        )}
        {row.role === "session" && <Badge variant="outline">session</Badge>}
        {row.role === "root" && <Badge variant="outline">root</Badge>}
        {layer !== null && <LayerBadges store={store} layer={layer} />}
        {row.state === "unread" && <Badge variant={missing ? "destructive" : "outline"}>{missing ? "missing" : "not read"}</Badge>}
        {row.state === "cycle" && <Badge variant="destructive">cycle</Badge>}
        {offset !== null && (
          <Badge variant="outline" className="font-mono" title={`Read through ${offset}: a sample at t in it is at ${row.offset.scale === 1 ? "t" : `${row.offset.scale}t`}${row.offset.offset === 0 ? "" : ` + ${row.offset.offset}`} on the stage`}>
            {offset}
          </Badge>
        )}
        {sublayer && (layer !== null || muted) && (
          <MuteToggle store={store} layer={layer} identifier={row.identifier} muted={muted} />
        )}
        {hasMenu && (
          <DropdownMenuTrigger>
            <Button variant="ghost" size="icon-xs" aria-label={`Change the stack at ${name}`} className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[pressed]:opacity-100">
              <HugeiconsIcon icon={MoreHorizontalIcon} />
            </Button>
            <DropdownMenu placement="bottom end" className="min-w-52">
              {canAdd && <DropdownMenuItem onAction={onAdd}>Add a sublayer to {name}…</DropdownMenuItem>}
              {canAdd && sublayer && canEditParent && <DropdownMenuSeparator />}
              {sublayer && canEditParent && (
                <>
                  <DropdownMenuItem onAction={() => onMove(-1)} isDisabled={row.index === 0}>
                    {row.index === 0 ? `Stronger (the strongest of ${parentName}'s)` : "Make stronger (move up)"}
                  </DropdownMenuItem>
                  <DropdownMenuItem onAction={() => onMove(1)} isDisabled={row.index === row.siblings - 1}>
                    {row.index === row.siblings - 1 ? `Weaker (the weakest of ${parentName}'s)` : "Make weaker (move down)"}
                  </DropdownMenuItem>
                  <DropdownMenuItem onAction={onOffset}>Set its offset…</DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onAction={onRemove}>Remove from {parentName}</DropdownMenuItem>
                </>
              )}
            </DropdownMenu>
          </DropdownMenuTrigger>
        )}
      </div>
      {sublayer && (
        <div className="truncate pl-[22px] font-mono text-[10px] text-muted-foreground" title={`Authored in ${parentName}'s subLayers, ${ordinal(row.index + 1)} of ${row.siblings}`}>
          @{row.asset}@{offsetUsda(row.offset)} <span className="font-sans">in {parentName}</span>
        </div>
      )}
      {refusal !== null && (
        <p role="alert" className="pl-[22px] text-[11px] text-destructive">
          {refusal}
        </p>
      )}
    </li>
  );
}

/** Mutes a sublayer: by its layer, or by identifier for one the stage has not read. */
function MuteToggle({ store, layer, identifier, muted }: { store: EditorStore; layer: Layer | null; identifier: string | null; muted: boolean }) {
  const name = layerName(layer?.identifier ?? identifier);
  const target = layer ?? identifier;
  if (target === null) return null;
  return <Switch size="sm" aria-label={muted ? `Unmute ${name}` : `Mute ${name}`} isSelected={!muted} onChange={(on) => void store.setMuted(target, !on)} />;
}

/** `1st`, `2nd`, `3rd`, `4th`, ... */
function ordinal(count: number): string {
  const tens = count % 100;
  const suffix = tens >= 11 && tens <= 13 ? "th" : (["th", "st", "nd", "rd"][count % 10] ?? "th");
  return `${count}${suffix}`;
}
