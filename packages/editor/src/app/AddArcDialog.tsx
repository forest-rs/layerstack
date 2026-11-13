// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Adding an arc to a prim, where edits go. A reference or payload names
// a layer, chosen from those the scene's source lists and the stage holds
// (each offered as the asset path that names it from the layer edits go
// to), this layer itself (an internal arc, to a prim of the stage), or a
// typed path; then a prim in it: its default prim, said when it has none,
// one of its root prims (read from the stage, or from the source without
// composing it), or a typed path. An inherit or specialize names a prim
// of the stage (class prims first). The list position (prepend, as
// OpenUSD's default, or append) and, across a reference or payload, a
// layer offset. The dialog shows the USDA line it authors and where; the
// action is one undoable step that fetches what the arc names, the
// overlay showing the asset loop, and a refusal is said in the dialog.
import { useEffect, useMemo, useState } from "react";
import { ListBox, ListBoxItem, ToggleButton, ToggleButtonGroup } from "react-aria-components";
import type { Layer, ListPosition, Prim, Stage } from "@forest-rs/layerstack";
import { Button, buttonVariants } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import {
  anchorAsset,
  describeTarget,
  layerOffsetProblem,
  mapping,
  type NewArc,
  type NewArcKind,
  newArcUsda,
  primPathProblem,
  relativeAsset,
} from "../model/composition.ts";
import type { LayerSummary } from "../model/layerSpecs.ts";
import type { EditorStore } from "../store.ts";
import { currentTarget } from "./CompositionSection.tsx";
import { PrimPicker } from "./PrimPicker.tsx";

/** Which layer a reference or payload names. */
type LayerChoice = { readonly kind: "internal" } | { readonly kind: "listed"; readonly identifier: string } | { readonly kind: "typed" };

/** Which prim in that layer: its default prim, a root prim, or a typed path. */
type PrimChoice = { readonly kind: "default" } | { readonly kind: "root"; readonly name: string } | { readonly kind: "typed" };

const KINDS: readonly [NewArcKind, string][] = [
  ["reference", "Reference"],
  ["payload", "Payload"],
  ["inherit", "Inherit"],
  ["specialize", "Specialize"],
];

/** What each kind of arc does, in a sentence. */
const MEANING: Readonly<Record<NewArcKind, string>> = {
  reference: "Composes a prim of a layer (or of this one) beneath this prim, its opinions weaker than this prim's own and its inherits' and variants'.",
  payload: "A reference the stage can leave unloaded: what it names is read only while the prim is loaded.",
  inherit: "Composes a prim of the stage (usually a class) into this one, stronger than its variants and references: an edit to the class reaches every prim that inherits it.",
  specialize: "Composes a prim of the stage as this one's base, weaker than everything else: its opinions are fallbacks the prim refines.",
};

/** Class prims first in the picker, for a class arc. */
function classFirst(stage: Stage): (path: string) => boolean {
  return (path) => stage.prim(path)?.specifier === "class";
}

/** The dialog that adds an arc of `kind` (switchable) to `prim`, opened from the Composition section. */
export function AddArcDialog({ store, prim, kind: initial, onClose }: { store: EditorStore; prim: Prim; kind: NewArcKind; onClose: () => void }) {
  const stage = prim.stage;
  const target = currentTarget(store);
  const targetLayer: Layer | null = target === null ? null : "kind" in target ? target.layer : target;
  const from = targetLayer?.identifier ?? null;
  const [kind, setKind] = useState<NewArcKind>(initial);
  const [layers, setLayers] = useState<readonly string[] | null>(null);
  const [layerChoice, setLayerChoice] = useState<LayerChoice | null>(null);
  const [typedAsset, setTypedAsset] = useState("");
  const [summary, setSummary] = useState<LayerSummary | null>(null);
  const [primChoice, setPrimChoice] = useState<PrimChoice>({ kind: "default" });
  const [typedPrim, setTypedPrim] = useState("");
  const [stagePrim, setStagePrim] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const [position, setPosition] = useState<ListPosition>("backOfPrepend");
  const [offsetText, setOffsetText] = useState("0");
  const [scaleText, setScaleText] = useState("1");
  const [refused, setRefused] = useState<string | null>(null);
  const prefer = useMemo(() => classFirst(stage), [stage]);
  const external = kind === "reference" || kind === "payload";

  // The layers an arc can name: listed once, when the dialog opens.
  useEffect(() => {
    let current = true;
    void store.arcLayers().then((found) => current && setLayers(found.filter((identifier) => identifier !== from)));
    return () => {
      current = false;
    };
  }, [store, from]);

  // The chosen layer's default and root prims: read when it is chosen.
  const identifier = layerChoice?.kind === "listed" ? layerChoice.identifier : layerChoice?.kind === "typed" && typedAsset.trim() !== "" ? anchorAsset(from, typedAsset.trim()) : null;
  useEffect(() => {
    setSummary(null);
    if (identifier === null) return;
    let current = true;
    const timer = setTimeout(() => void store.layerSummary(identifier).then((read) => current && setSummary(read)), layerChoice?.kind === "typed" ? 300 : 0);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [store, identifier, layerChoice?.kind]);

  // A layer without a default prim is pointed at one of its root prims.
  useEffect(() => {
    if (summary?.kind !== "read") return;
    if (summary.defaultPrim !== null) setPrimChoice({ kind: "default" });
    else {
      const first = summary.roots.find((root) => root.specifier === "def") ?? summary.roots[0];
      setPrimChoice(first === undefined ? { kind: "typed" } : { kind: "root", name: first.name });
    }
  }, [summary]);

  const asset = !external || layerChoice === null || layerChoice.kind === "internal" ? null : layerChoice.kind === "listed" ? relativeAsset(from, layerChoice.identifier) : typedAsset.trim();
  const internal = external && layerChoice?.kind === "internal";
  const primPath = !external || internal ? stagePrim : primChoice.kind === "default" ? null : primChoice.kind === "root" ? `/${primChoice.name}` : typedPrim.trim();
  const offset = Number(offsetText);
  const scale = Number(scaleText);
  const problem =
    external && layerChoice === null
      ? "Choose the layer it names."
      : external && layerChoice?.kind === "typed" && typedAsset.trim() === ""
        ? "Type the asset path of a layer."
        : (!external || internal) && stagePrim === null
          ? `Choose the prim it ${kind === "reference" ? "references" : kind === "payload" ? "loads" : `${kind}s`}.`
          : external && !internal && primChoice.kind === "typed"
            ? primPathProblem(typedPrim.trim())
            : external
              ? layerOffsetProblem(offset, scale)
              : null;
  const arc: NewArc = { kind, asset, primPath, position, offset: external ? offset : 0, scale: external ? scale : 1 };
  const where = target === null ? null : mapping(target, prim.path);
  const add = () => {
    if (problem !== null) return;
    const { refusal } = store.addArc(prim.path, arc);
    if (refusal === undefined) onClose();
    else setRefused(refusal);
  };
  const option = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "sm" });
  const smallOption = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "xs" });
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>Add an arc to {prim.name || "/"}</DialogTitle>
        <DialogDescription>
          Authored in {target === null ? "the edit layer" : describeTarget(target)}, where edits go, as one undoable edit.
        </DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
        className="flex flex-col gap-2.5"
      >
        <ToggleButtonGroup
          aria-label="Kind of arc"
          selectionMode="single"
          disallowEmptySelection
          selectedKeys={[kind]}
          onSelectionChange={(keys) => {
            const next = [...keys][0];
            const found = KINDS.find(([each]) => each === next);
            if (found !== undefined) setKind(found[0]);
            setRefused(null);
          }}
          className="flex gap-0.5"
        >
          {KINDS.map(([each, title]) => (
            <ToggleButton key={each} id={each} className={option}>
              {title}
            </ToggleButton>
          ))}
        </ToggleButtonGroup>
        <p className="text-[11px] text-muted-foreground">{MEANING[kind]}</p>

        {external && (
          <div className="flex flex-col gap-1 text-xs">
            Layer
            <ListBox
              aria-label="The layer it names"
              selectionMode="single"
              selectedKeys={layerChoice === null ? [] : [layerChoice.kind === "listed" ? `listed:${layerChoice.identifier}` : layerChoice.kind]}
              onSelectionChange={(keys) => {
                const [key] = keys === "all" ? [] : [...keys].map(String);
                setRefused(null);
                if (key === undefined) return;
                setLayerChoice(key === "internal" ? { kind: "internal" } : key === "typed" ? { kind: "typed" } : { kind: "listed", identifier: key.slice("listed:".length) });
              }}
              className="max-h-44 overflow-y-auto rounded-md border p-1 outline-none"
            >
              <ListBoxItem id="internal" textValue="This layer" className={itemClass}>
                <span>This layer</span>
                <span className="ml-auto text-[10px] text-muted-foreground">an internal {kind}: a prim of the stage</span>
              </ListBoxItem>
              {(layers ?? []).map((each) => (
                <ListBoxItem key={each} id={`listed:${each}`} textValue={each} className={itemClass}>
                  <span className="truncate font-mono">{relativeAsset(from, each)}</span>
                  <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">{stage.layers().some((layer) => layer.identifier === each) ? "read" : "not read yet"}</span>
                </ListBoxItem>
              ))}
              <ListBoxItem id="typed" textValue="Another path" className={itemClass}>
                <span>Another path…</span>
              </ListBoxItem>
            </ListBox>
            {layers === null && <span className="text-[11px] text-muted-foreground">Listing the scene's layers…</span>}
            {layers !== null && layers.length === 0 && <span className="text-[11px] text-muted-foreground">The scene's source lists no other layers: type a path.</span>}
            {layerChoice?.kind === "typed" && (
              <Input
                autoFocus
                aria-label="Asset path"
                placeholder="./assets/robot.usda"
                value={typedAsset}
                onChange={(event) => {
                  setTypedAsset(event.target.value);
                  setRefused(null);
                }}
                className="h-7 font-mono text-xs"
              />
            )}
          </div>
        )}

        {external && layerChoice !== null && !internal && (
          <PrimInLayer
            identifier={identifier}
            summary={summary}
            choice={primChoice}
            typed={typedPrim}
            onChoose={(next) => {
              setPrimChoice(next);
              setRefused(null);
            }}
            onType={(next) => {
              setTypedPrim(next);
              setRefused(null);
            }}
          />
        )}

        {(!external || internal) && (
          <div className="flex items-center gap-2 text-xs">
            <span>Prim</span>
            {stagePrim === null ? <span className="text-[11px] text-muted-foreground">none chosen</span> : <span className="truncate font-mono text-[11px]">{stagePrim}</span>}
            <Button variant="outline" size="sm" className="ml-auto" onPress={() => setPicking(true)}>
              {stagePrim === null ? "Choose a prim…" : "Choose another…"}
            </Button>
          </div>
        )}

        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1.5 text-xs">
          <span>Position</span>
          <span className="flex items-center gap-2">
            <ToggleButtonGroup
              aria-label="List position"
              selectionMode="single"
              disallowEmptySelection
              selectedKeys={[position]}
              onSelectionChange={(keys) => {
                const [next] = [...keys];
                if (next === "backOfPrepend" || next === "backOfAppend") setPosition(next);
              }}
              className="flex gap-0.5"
            >
              <ToggleButton id="backOfPrepend" className={smallOption}>
                prepend
              </ToggleButton>
              <ToggleButton id="backOfAppend" className={smallOption}>
                append
              </ToggleButton>
            </ToggleButtonGroup>
            <span className="text-[11px] text-muted-foreground">
              {position === "backOfPrepend" ? "OpenUSD's default: stronger than what weaker layers add" : "weaker than what weaker layers prepend"}
            </span>
          </span>
          {external && (
            <>
              <span>Layer offset</span>
              <span className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                offset
                <Input aria-label="Offset" inputMode="decimal" value={offsetText} onChange={(event) => setOffsetText(event.target.value)} className="h-6 w-16 font-mono text-xs" />
                scale
                <Input aria-label="Scale" inputMode="decimal" value={scaleText} onChange={(event) => setScaleText(event.target.value)} className="h-6 w-16 font-mono text-xs" />
                <span>stage time = offset + scale × its time</span>
              </span>
            </>
          )}
        </div>

        <div className="flex flex-col gap-1">
          <pre className="overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-[11px] text-muted-foreground" aria-label="What is authored, in USDA">
            {newArcUsda({ ...arc, asset: external && !internal && asset === null ? "…" : arc.asset, primPath: arc.primPath ?? (!external || internal ? "/…" : null) })}
          </pre>
          {where !== null && (
            <span className="text-[11px] text-muted-foreground">
              On the spec <span className="font-mono text-foreground">{where.specPath ?? prim.path}</span> in {targetLayer?.identifier ?? "the session layer"}
              {external && asset !== null && identifier !== null && !stage.layers().some((layer) => layer.identifier === identifier) ? `; ${identifier} is then fetched.` : "."}
            </span>
          )}
        </div>
        {problem !== null && <span className="text-[11px] text-muted-foreground">{problem}</span>}
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
        <Button isDisabled={problem !== null} onPress={add}>
          Add {kind}
        </Button>
      </DialogFooter>
      {picking && (
        <PrimPicker
          stage={stage}
          title={external ? `The prim this ${kind} names` : `The prim ${prim.name} ${kind}s`}
          description={external ? "A prim of the stage: an internal arc." : "Class prims first: an inherit or specialize usually names one."}
          exclude={[prim.path]}
          prefer={prefer}
          onPick={(picked) => {
            setStagePrim(picked);
            setRefused(null);
          }}
          onClose={() => setPicking(false)}
        />
      )}
    </Dialog>
  );
}

/** The prim of the chosen layer: its default prim, a root prim, or a typed path. */
function PrimInLayer({
  identifier,
  summary,
  choice,
  typed,
  onChoose,
  onType,
}: {
  identifier: string | null;
  summary: LayerSummary | null;
  choice: PrimChoice;
  typed: string;
  onChoose: (choice: PrimChoice) => void;
  onType: (typed: string) => void;
}) {
  const key = choice.kind === "root" ? `root:${choice.name}` : choice.kind;
  const read = summary?.kind === "read" ? summary : null;
  return (
    <div className="flex flex-col gap-1 text-xs">
      Prim in it
      {identifier !== null && summary === null && <span className="text-[11px] text-muted-foreground">Reading {identifier}'s prims…</span>}
      {summary?.kind === "unread" && <span className="text-[11px] text-muted-foreground">{summary.reason} Type the prim's path, or leave the default prim.</span>}
      {read !== null && read.defaultPrim === null && (
        <span className="text-[11px] text-amber-700 dark:text-amber-300">{identifier} has no defaultPrim: an arc to it names one of its prims.</span>
      )}
      <ListBox
        aria-label="The prim it names"
        selectionMode="single"
        disallowEmptySelection
        selectedKeys={[key]}
        onSelectionChange={(keys) => {
          const [next] = keys === "all" ? [] : [...keys].map(String);
          if (next === undefined) return;
          onChoose(next === "default" ? { kind: "default" } : next === "typed" ? { kind: "typed" } : { kind: "root", name: next.slice("root:".length) });
        }}
        className="max-h-36 overflow-y-auto rounded-md border p-1 outline-none"
      >
        {(read === null || read.defaultPrim !== null) && (
          <ListBoxItem id="default" textValue="Its default prim" className={itemClass}>
            <span>Its default prim</span>
            <span className="ml-auto font-mono text-[10px] text-muted-foreground">{read?.defaultPrim === undefined ? "" : read.defaultPrim === null ? "" : `/${read.defaultPrim}`}</span>
          </ListBoxItem>
        )}
        {(read?.roots ?? [])
          .filter((root) => root.name !== read?.defaultPrim)
          .map((root) => (
          <ListBoxItem key={root.name} id={`root:${root.name}`} textValue={root.name} className={itemClass}>
            <span className="font-mono">/{root.name}</span>
            <span className="ml-auto text-[10px] text-muted-foreground">
              {root.specifier === "def" ? "" : `${root.specifier} `}
              {root.typeName ?? ""}
            </span>
          </ListBoxItem>
        ))}
        <ListBoxItem id="typed" textValue="Another path" className={itemClass}>
          Another path…
        </ListBoxItem>
      </ListBox>
      {choice.kind === "typed" && (
        <Input aria-label="Prim path in the layer" placeholder="/Robot" value={typed} onChange={(event) => onType(event.target.value)} className="h-7 font-mono text-xs" />
      )}
    </div>
  );
}

const itemClass = ({ isSelected, isFocused }: { isSelected: boolean; isFocused: boolean }) =>
  cn("flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none", isFocused && "bg-muted", isSelected && "bg-accent text-accent-foreground");
