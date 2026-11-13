// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Adding a sublayer to a layer of the stage's own stack, and changing the
// offset one is read through. The asset is picked from the layers the
// source lists (or typed, for one it cannot list), written as the layer
// authors a path to it (`./lib.usda`); the position is a place among the
// layer's sublayers, strongest first; the offset maps the sublayer's
// times to the stage's. Each dialog shows the `subLayers` it will author
// and where, and says why the stage refused it.
import { useEffect, useMemo, useState } from "react";
import { ListBox, ListBoxItem } from "react-aria-components";
import type { Layer } from "@forest-rs/layerstack";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogSubmit, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { IDENTITY, type LayerOffset, offsetUsda, type SublayerEntry, sublayerCandidates } from "../model/layerStack.ts";
import { parseNumber } from "../model/refusal.ts";
import type { EditorStore } from "../store.ts";
import { layerName } from "./format.ts";

const itemClass = ({ isSelected, isFocused, isDisabled }: { isSelected: boolean; isFocused: boolean; isDisabled: boolean }) =>
  cn(
    "flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none",
    isFocused && "bg-muted",
    isSelected && "bg-accent text-accent-foreground",
    isDisabled && "opacity-50",
  );

/** `subLayers` as USDA writes it, `marked` (an index) flagged as the one the edit authors. */
function SublayersUsda({ sublayers, marked }: { sublayers: readonly SublayerEntry[]; marked: number | null }) {
  return (
    <pre className="overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-[11px] text-muted-foreground" aria-label="What is authored, in USDA">
      {sublayers.length === 0 ? (
        "subLayers = []"
      ) : (
        <>
          {"subLayers = [\n"}
          {sublayers.map((entry, index) => (
            <span key={index} className={cn(index === marked && "text-foreground")}>
              {`    @${entry.asset}@${offsetUsda(entry.layerOffset)}${index < sublayers.length - 1 ? "," : ""}`}
              {index === marked && "   # this edit"}
              {"\n"}
            </span>
          ))}
          {"]"}
        </>
      )}
    </pre>
  );
}

/** Offset and scale fields; `onChange` hears the offset, or `null` while either is not a number (or the scale not positive). */
function OffsetFields({ initial, onChange }: { initial: LayerOffset; onChange: (offset: LayerOffset | null) => void }) {
  const [offset, setOffset] = useState(String(initial.offset));
  const [scale, setScale] = useState(String(initial.scale));
  const parsedOffset = parseNumber(offset);
  const parsedScale = parseNumber(scale);
  const valid = parsedOffset !== null && parsedScale !== null && parsedScale > 0;
  useEffect(() => onChange(valid ? { offset: parsedOffset, scale: parsedScale } : null), [offset, scale]);
  return (
    <div className="flex flex-col gap-1 text-xs">
      <div className="grid grid-cols-2 gap-2">
        <label className="flex flex-col gap-1">
          Offset
          <Input aria-label="Offset" inputMode="decimal" value={offset} onChange={(event) => setOffset(event.target.value)} className="h-7 text-right font-mono text-xs tabular-nums" />
        </label>
        <label className="flex flex-col gap-1">
          Scale
          <Input aria-label="Scale" inputMode="decimal" value={scale} onChange={(event) => setScale(event.target.value)} className="h-7 text-right font-mono text-xs tabular-nums" />
        </label>
      </div>
      <span className={cn("text-[11px]", valid ? "text-muted-foreground" : "text-destructive")}>
        {valid
          ? `A sample at time t in the sublayer is read at stage time ${parsedScale === 1 ? "t" : `${parsedScale} × t`}${parsedOffset === 0 ? "" : ` + ${parsedOffset}`}.`
          : "The offset is a number, the scale a number above 0."}
      </span>
    </div>
  );
}

/** The dialog that adds a sublayer to `parent`, a layer of the stage's own stack that takes edits. */
export function AddSublayerDialog({
  store,
  parent,
  present,
  above,
  onClose,
}: {
  store: EditorStore;
  parent: Layer;
  /** The identifiers of the sublayers it lists. */
  present: readonly string[];
  /** The identifiers of the layers above it in the stack. */
  above: readonly (string | null)[];
  onClose: () => void;
}) {
  const stage = store.session?.stage;
  const sublayers = useMemo(() => stage?.sublayers(parent) ?? [], [stage, parent]);
  const [listed, setListed] = useState<readonly string[] | null>(null);
  const [filter, setFilter] = useState("");
  const [chosen, setChosen] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const [index, setIndex] = useState(sublayers.length);
  const [offset, setOffset] = useState<LayerOffset | null>(IDENTITY);
  const [busy, setBusy] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const source = store.source;
  useEffect(() => {
    let live = true;
    const list = source?.layers;
    if (list === undefined) setListed([]);
    else
      void list().then(
        (layers) => live && setListed(layers),
        () => live && setListed([]),
      );
    return () => {
      live = false;
    };
  }, [source]);
  const held = useMemo(() => new Map((stage?.layers() ?? []).flatMap((layer) => (layer.identifier === null ? [] : [[layer.identifier, layer] as const]))), [stage]);
  const candidates = useMemo(
    () =>
      sublayerCandidates(
        listed ?? [],
        parent.identifier,
        present,
        above.filter((each) => each !== null),
      ).filter((candidate) => candidate.identifier.toLowerCase().includes(filter.toLowerCase())),
    [listed, parent, present, above, filter],
  );
  const asset = typed.trim() !== "" ? typed.trim() : (candidates.find((candidate) => candidate.identifier === chosen)?.asset ?? null);
  const after = asset === null || offset === null ? sublayers : [...sublayers.slice(0, index), { asset, layerOffset: offset }, ...sublayers.slice(index)];
  const name = layerName(parent.identifier);
  const add = async () => {
    if (asset === null || offset === null) return;
    setBusy(true);
    const why = await store.insertSublayer(parent, asset, index, offset);
    setBusy(false);
    if (why === undefined) onClose();
    else setRefused(why);
  };
  const positions = Array.from({ length: sublayers.length + 1 }, (_, at) => at);
  const positionText = (at: number) =>
    sublayers.length === 0
      ? "Its only sublayer"
      : at === 0
        ? `Strongest: before @${sublayers[0]!.asset}@`
        : at === sublayers.length
          ? `Weakest: after @${sublayers[at - 1]!.asset}@`
          : `After @${sublayers[at - 1]!.asset}@, before @${sublayers[at]!.asset}@`;
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>Add a sublayer to {name}</DialogTitle>
        <DialogDescription>
          Authored in {name}'s <code>subLayers</code> as one undoable edit, whatever layer other edits go to; the stage then reads it. A sublayer's opinions are weaker than {name}'s own,
          and stronger than those of the sublayers after it.
        </DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void add();
        }}
        className="flex flex-col gap-2"
      >
        <DialogSubmit />
        <span className="text-xs">Layer</span>
        {source?.layers === undefined ? (
          <p className="text-[11px] text-muted-foreground">This source cannot list its layers: type the asset path.</p>
        ) : (
          <>
            <Input placeholder="Find a layer" aria-label="Find a layer" value={filter} onChange={(event) => setFilter(event.target.value)} className="h-7 text-xs" />
            <ListBox
              aria-label="Layers the source holds"
              selectionMode="single"
              selectedKeys={chosen === null ? [] : [chosen]}
              disabledKeys={candidates.filter((candidate) => candidate.refusal !== null).map((candidate) => candidate.identifier)}
              onSelectionChange={(keys) => {
                const [key] = keys === "all" ? [] : [...keys];
                setChosen(key === undefined ? null : String(key));
                setTyped("");
                setRefused(null);
              }}
              renderEmptyState={() => (
                <p className="flex items-center gap-2 px-2 py-1 text-xs text-muted-foreground">
                  {listed === null ? (
                    <>
                      <Spinner className="size-3" /> Listing the source's layers…
                    </>
                  ) : (
                    "No layer of the source is named like that."
                  )}
                </p>
              )}
              className="max-h-48 overflow-y-auto rounded-md border p-1 outline-none"
            >
              {candidates.map((candidate) => (
                <ListBoxItem key={candidate.identifier} id={candidate.identifier} textValue={candidate.identifier} className={itemClass}>
                  <span className="min-w-0 flex-1 truncate font-mono">{candidate.identifier}</span>
                  <span className="shrink-0 text-[10px] text-muted-foreground">{candidate.refusal ?? (held.has(candidate.identifier) ? "the stage holds it" : `as @${candidate.asset}@`)}</span>
                </ListBoxItem>
              ))}
            </ListBox>
          </>
        )}
        <label className="flex flex-col gap-1 text-xs">
          {source?.layers === undefined ? "Asset path" : "Or an asset path"}
          <Input
            value={typed}
            placeholder="./shot.usda"
            onChange={(event) => {
              setTyped(event.target.value);
              setRefused(null);
            }}
            className="h-7 font-mono text-xs"
          />
          <span className="text-[11px] text-muted-foreground">Relative to {name}. A path the source does not have is authored all the same; the Health tab then says it is missing.</span>
        </label>
        <Select aria-label="Position" value={String(index)} onChange={(key) => setIndex(Number(key))}>
          <SelectTrigger size="sm" className="w-full">
            <span className="shrink-0 text-muted-foreground">Position</span>
            <SelectValue className="truncate" />
          </SelectTrigger>
          <SelectContent>
            {positions.map((at) => (
              <SelectItem key={at} id={String(at)} textValue={positionText(at)}>
                {positionText(at)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <OffsetFields initial={IDENTITY} onChange={setOffset} />
        <SublayersUsda sublayers={after} marked={asset === null || offset === null ? null : index} />
        {refused !== null && (
          <span role="alert" className="text-[11px] text-destructive">
            {refused}
          </span>
        )}
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={asset === null || offset === null || busy} onPress={() => void add()}>
          {busy && <Spinner className="size-3.5" data-icon="inline-start" />}
          Add sublayer
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/** The dialog that changes the offset `parent` reads its sublayer at `index` through. */
export function SublayerOffsetDialog({ store, parent, index, onClose }: { store: EditorStore; parent: Layer; index: number; onClose: () => void }) {
  const stage = store.session?.stage;
  const sublayers = useMemo(() => stage?.sublayers(parent) ?? [], [stage, parent]);
  const entry = sublayers[index];
  const [offset, setOffset] = useState<LayerOffset | null>(entry?.layerOffset ?? IDENTITY);
  const [busy, setBusy] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  if (entry === undefined) return null;
  const name = layerName(parent.identifier);
  const after = offset === null ? sublayers : sublayers.map((each, at) => (at === index ? { ...each, layerOffset: offset } : each));
  const save = async () => {
    if (offset === null) return;
    setBusy(true);
    const why = await store.setSublayerOffset(parent, index, offset);
    setBusy(false);
    if (why === undefined) onClose();
    else setRefused(why);
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>
          Offset of <span className="font-mono">@{entry.asset}@</span>
        </DialogTitle>
        <DialogDescription>
          Authored in {name}'s <code>subLayers</code> as one undoable edit: the times of the sublayer, and of the layers beneath it, are read through it.
        </DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
        className="flex flex-col gap-2"
      >
        <DialogSubmit />
        <OffsetFields initial={entry.layerOffset} onChange={setOffset} />
        <SublayersUsda sublayers={after} marked={index} />
        {refused !== null && (
          <span role="alert" className="text-[11px] text-destructive">
            {refused}
          </span>
        )}
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={offset === null || busy} onPress={() => void save()}>
          {busy && <Spinner className="size-3.5" data-icon="inline-start" />}
          Set offset
        </Button>
      </DialogFooter>
    </Dialog>
  );
}
