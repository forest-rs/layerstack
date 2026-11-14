// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Variants section: each variant set of the prim with its
// selection, chosen from its variants (a selection fetches what the
// variant needs). Each set's "⋯" makes edits go inside one of its
// variants (`Prim.variantEditTarget`: what is authored there shows only
// while it is selected, which the section says), adds a variant (then
// selects it, and offers to edit inside it), or removes the set from the
// layer edits go to when that layer holds it, else names the layer that
// does and offers to edit there. "+ Variant set" adds a set with its
// first variant. Each edit is one undoable step where edits go.
// AOUSD Core §7.3.6 (variant sets), §7.6.2.3.5 (`variantSetNames`).
import { useState } from "react";
import type { EditTarget, Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogSubmit, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { describeTarget, mapping, variantNameProblem, variantOfSite, variantSetNameProblem, variantSetSource } from "../model/composition.ts";
import type { EditorStore } from "../store.ts";
import { CompositionEditNote, compositionBlock, currentTarget, editThrough } from "./CompositionSection.tsx";
import { useRevision } from "./context.ts";
import { Section } from "./Section.tsx";

/** The most variants a set's menu offers to edit inside, by name. */
const MENU_VARIANTS = 12;

/** What a dialog of the section is doing. */
type Open = { readonly kind: "variant"; readonly set: string | null } | { readonly kind: "remove"; readonly set: string } | null;

/** The variant edits go inside now, when it is one of `prim`'s: its set and variant. */
function insideOf(store: EditorStore, prim: Prim): { readonly set: string; readonly variant: string; readonly target: EditTarget } | null {
  const target = store.session?.target;
  if (target === undefined || !("kind" in target) || target.kind === "layer" || target.prim !== prim.path) return null;
  const variant = variantOfSite(target.site);
  return variant === null ? null : { ...variant, target };
}

export function VariantsSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  useRevision(store, "target", "history");
  const [open, setOpen] = useState<Open>(null);
  const sets = prim.variantSets();
  const blocked = compositionBlock(store, prim);
  const inside = insideOf(store, prim);
  return (
    <Section
      title="Variants"
      aside={
        <Button variant="ghost" size="xs" isDisabled={blocked !== null} onPress={() => setOpen({ kind: "variant", set: null })} aria-label="Add a variant set">
          <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
          Variant set
        </Button>
      }
    >
      {sets.length === 0 && (
        <p className="text-[11px] text-muted-foreground">
          No variant sets. A set holds alternative versions of the prim, its variants, one selected at a time; add one to author them.
        </p>
      )}
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto] items-center gap-x-2 gap-y-1.5 text-xs">
        {sets.map((set) => (
          <VariantRow
            key={set.name}
            store={store}
            prim={prim}
            name={set.name}
            selection={set.selection}
            variants={set.variants}
            blocked={blocked}
            onAdd={() => setOpen({ kind: "variant", set: set.name })}
            onRemove={() => setOpen({ kind: "remove", set: set.name })}
          />
        ))}
      </div>
      {inside !== null && <InsideNote store={store} prim={prim} set={inside.set} variant={inside.variant} target={inside.target} />}
      <CompositionEditNote store={store} prim={prim} variants />
      {open?.kind === "variant" && <VariantDialog store={store} prim={prim} set={open.set} onClose={() => setOpen(null)} />}
      {open?.kind === "remove" && <RemoveSetDialog store={store} prim={prim} set={open.set} onClose={() => setOpen(null)} />}
    </Section>
  );
}

function VariantRow({
  store,
  prim,
  name,
  selection,
  variants,
  blocked,
  onAdd,
  onRemove,
}: {
  store: EditorStore;
  prim: Prim;
  name: string;
  selection: string | null;
  variants: readonly string[];
  blocked: string | null;
  onAdd: () => void;
  onRemove: () => void;
}) {
  // The selected variant first: the one whose edits show.
  const offered = [...(selection === null ? [] : [selection]), ...variants.filter((variant) => variant !== selection)].slice(0, MENU_VARIANTS);
  const inside = insideOf(store, prim);
  return (
    <>
      <span className="truncate">{name}</span>
      {variants.length === 0 ? (
        <span className="truncate text-[11px] text-muted-foreground" title="Its name stays in the prim's variantSetNames; no layer holds a variant of it">
          no variants
        </span>
      ) : (
        <Select
          aria-label={name}
          value={selection}
          placeholder="none selected"
          onChange={(key) => key !== null && key !== selection && void store.selectVariant(prim.path, name, String(key))}
          className="w-full"
        >
          <SelectTrigger size="sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {variants.map((variant) => (
              <SelectItem key={variant} id={variant} textValue={variant}>
                {variant}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
      <DropdownMenuTrigger>
        <Button variant="ghost" size="icon-xs" aria-label={`Actions for the variant set ${name}`}>
          <HugeiconsIcon icon={MoreHorizontalIcon} />
        </Button>
        <DropdownMenu placement="bottom end" className="min-w-52">
          {offered.map((variant) => (
            <DropdownMenuItem
              key={variant}
              isDisabled={blocked !== null || (inside?.set === name && inside.variant === variant)}
              onAction={() => store.editInsideVariant(prim.path, name, variant)}
            >
              Edit inside {variant}
              {variant === selection ? " (selected)" : ""}
            </DropdownMenuItem>
          ))}
          {offered.length > 0 && <DropdownMenuSeparator />}
          <DropdownMenuItem isDisabled={blocked !== null} onAction={onAdd}>
            Add a variant…
          </DropdownMenuItem>
          <DropdownMenuItem variant="destructive" isDisabled={blocked !== null} onAction={onRemove}>
            Remove the variant set…
          </DropdownMenuItem>
        </DropdownMenu>
      </DropdownMenuTrigger>
    </>
  );
}

/** Where edits go while they go inside one of the prim's variants: whether it shows, and the way back. */
function InsideNote({ store, prim, set, variant, target }: { store: EditorStore; prim: Prim; set: string; variant: string; target: EditTarget }) {
  const selected = prim.variantSets().find((each) => each.name === set)?.selection === variant;
  const editLayer = store.session?.stage.editLayer ?? null;
  return (
    <div role="status" className="mt-2 flex flex-col gap-1 rounded-md bg-sky-500/10 px-2 py-1 text-[11px] text-sky-800 dark:text-sky-300">
      <span>
        Edits go inside {set}={variant}: at <span className="font-mono">{mapping(target, prim.path).specPath ?? prim.path}</span> in {target.layer.identifier ?? "the session layer"}.
        {selected ? " It is selected, so they show." : " It is not selected: what is authored there shows only once it is."}
      </span>
      <span className="flex flex-wrap gap-1">
        {!selected && (
          <Button variant="outline" size="xs" onPress={() => void store.selectVariant(prim.path, set, variant)}>
            Select {variant}
          </Button>
        )}
        {editLayer !== null && (
          <Button variant="outline" size="xs" onPress={() => store.setTarget(undefined)}>
            Back to {editLayer.identifier ?? "the session layer"}
          </Button>
        )}
      </span>
    </div>
  );
}

/**
 * Adds a variant to the set `set` of `prim`, or, with `set` `null`, a new
 * set with its first variant; the variant is selected. Says what it
 * authors, in USDA, and where.
 */
export function VariantDialog({ store, prim, set, onClose }: { store: EditorStore; prim: Prim; set: string | null; onClose: () => void }) {
  const sets = prim.variantSets();
  const [setName, setSetName] = useState("");
  const [variant, setVariant] = useState("");
  const [refused, setRefused] = useState<string | null>(null);
  const target = currentTarget(store);
  const newSet = set === null;
  const name = set ?? setName;
  const taken = sets.find((each) => each.name === name)?.variants ?? [];
  // A set whose name stays listed with no variants (removed before) can be added again.
  const named = sets.filter((each) => each.variants.length > 0).map((each) => each.name);
  const problem = (newSet ? variantSetNameProblem(setName, named) : null) ?? variantNameProblem(variant, taken);
  const where = target === null ? "the edit layer" : describeTarget(target);
  const add = () => {
    if (problem !== null) return;
    const why = store.addVariant(prim.path, name, variant, newSet);
    if (why === undefined) onClose();
    else setRefused(why);
  };
  const usda = `${newSet ? `prepend variantSets = "${name || "name"}"\n` : ""}variants = {\n    string ${name || "name"} = "${variant || "variant"}"\n}\nvariantSet "${name || "name"}" = {\n    "${variant || "variant"}" {\n    }\n}`;
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{newSet ? `Add a variant set to ${prim.name || "/"}` : `Add a variant to ${set}`}</DialogTitle>
        <DialogDescription>Authored in {where}, where edits go, and selected, as one undoable edit. Then edit inside it to author what it changes.</DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
        className="flex flex-col gap-2"
      >
        <DialogSubmit />
        {newSet && (
          <label className="flex flex-col gap-1 text-xs">
            Set name
            <Input
              autoFocus
              value={setName}
              placeholder="finish"
              onChange={(event) => {
                setSetName(event.target.value);
                setRefused(null);
              }}
              className="h-7 font-mono text-xs"
            />
          </label>
        )}
        <label className="flex flex-col gap-1 text-xs">
          {newSet ? "First variant" : "Variant name"}
          <Input
            autoFocus={!newSet}
            value={variant}
            placeholder="matte"
            onChange={(event) => {
              setVariant(event.target.value);
              setRefused(null);
            }}
            className="h-7 font-mono text-xs"
          />
        </label>
        {taken.length > 0 && <span className="text-[11px] text-muted-foreground">Its variants: {taken.join(", ")}.</span>}
        {(setName !== "" || variant !== "") && problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
        <pre className="overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-[11px] text-muted-foreground" aria-label="What is authored, in USDA">
          {usda}
        </pre>
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
          {newSet ? "Add variant set" : "Add variant"}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/**
 * Removing a variant set where edits go, when that layer holds its spec:
 * what goes (its variants and what they author, the layer's selection)
 * and what stays (its name in `variantSetNames`, as OpenUSD keeps it);
 * otherwise the layer that holds it, to edit there.
 */
function RemoveSetDialog({ store, prim, set, onClose }: { store: EditorStore; prim: Prim; set: string; onClose: () => void }) {
  const [refused, setRefused] = useState<string | null>(null);
  const target = currentTarget(store);
  const arcs = prim.compositionArcs();
  const held = variantSetSource(arcs, set, store.specsOf);
  const spec = target === null ? null : mapping(target, prim.path).specPath;
  const targetLayer = target === null ? null : "kind" in target ? target.layer : target;
  const here = held !== null && targetLayer === held.layer && spec === held.specPath;
  const variants = held === null ? [] : (store.authoredSpec(held.layer, held.specPath)?.spec.variantSets.find((each) => each.name === set)?.variants ?? []);
  const layerName = (held?.layer.identifier ?? targetLayer?.identifier) ?? "the session layer";
  let there: EditTarget | null = null;
  if (held !== null && !here) {
    try {
      there = arcs[held.arc]!.editTarget(held.layer);
    } catch {
      there = null;
    }
  }
  const remove = () => {
    const why = store.removeVariantSet(prim.path, set);
    if (why === undefined) onClose();
    else setRefused(why);
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Remove the variant set {set}</DialogTitle>
        <DialogDescription>
          {here
            ? `From ${layerName}, where edits go, as one undoable edit.`
            : held === null
              ? "No layer this prim reads holds this set's spec where it can be read here."
              : `${layerName} holds it${held.specPath === prim.path ? "" : ` at ${held.specPath}`}, not where edits go.`}
        </DialogDescription>
      </DialogHeader>
      {here && (
        <div className="flex flex-col gap-1.5 text-xs">
          <p>
            Removes {layerName}'s spec of the set: its variant{variants.length === 1 ? "" : "s"} {variants.join(", ")} and everything authored inside them, and
            the selection {layerName} authors for it.
          </p>
          <p className="text-[11px] text-muted-foreground">
            The set's name stays in the prim's <span className="font-mono">variantSetNames</span>, as OpenUSD leaves it (the bindings do not edit that list), so
            the set is still listed, with no variants, until that line is removed from the layer's text. A selection or variants other layers author stay too.
          </p>
        </div>
      )}
      {refused !== null && (
        <span role="alert" className="text-[11px] text-destructive">
          {refused}
        </span>
      )}
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        {here ? (
          <Button variant="destructive" onPress={remove}>
            Remove {set}
          </Button>
        ) : (
          there !== null && (
            <Button
              onPress={() => {
                editThrough(store, there!);
                onClose();
              }}
            >
              Edit in {layerName}
            </Button>
          )
        )}
      </DialogFooter>
    </Dialog>
  );
}
