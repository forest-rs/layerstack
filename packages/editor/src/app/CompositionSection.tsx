// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Composition section: the arcs a prim composes from,
// strongest first, each with the layer it reaches and the spec there
// (live, as anywhere), and which layer authors it (`model/composition.ts`
// reads that from the layers' specs). Where edits go is said first: the
// spec of this prim an edit would write (`/World/Robot → /Robot in
// assets/robot.usda` through a reference). Each arc offers to edit
// through it; one the edit target's layer authors can be removed there,
// and one authored elsewhere names that layer and offers to edit there.
// "Add" authors a reference, payload, inherit, specialize or variant set
// where edits go. Arcs authored here that compose nothing (an asset that
// is missing, or not fetched) are listed with why. After an edit, its
// layer's text is a click away, the change marked there.
import { useState } from "react";
import type { CompositionArc, EditTarget, Layer, Prim, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import {
  type ArcSource,
  arcItemText,
  arcNoun,
  arcUsda,
  authoredHere,
  authoredItemText,
  deletedHere,
  findAuthoring,
  kindOf,
  mapping,
  type NewArcKind,
  selectionTarget,
  targetRoute,
  targetSpec,
  uncomposedHere,
  uncomposedWhy,
  variantOfSite,
} from "../model/composition.ts";
import type { AuthoredArc } from "../model/layerSpecs.ts";
import type { EditorStore } from "../store.ts";
import { AddArcDialog } from "./AddArcDialog.tsx";
import { useRevision } from "./context.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { Section } from "./Section.tsx";
import { VariantDialog } from "./VariantsSection.tsx";

/** What a dialog of the section is doing. */
type Open = { readonly kind: "arc"; readonly arc: NewArcKind } | { readonly kind: "variantSet" } | null;

/** Where edits go now: the target, or the stage's edit layer. */
export function currentTarget(store: EditorStore): Layer | EditTarget | null {
  const session = store.session;
  return session === null ? null : (session.target ?? session.stage.editLayer);
}

/** Makes `target` where edits go: a layer of the stage's own stack as that layer, as the toolbar's picker lists it. */
export function editThrough(store: EditorStore, target: EditTarget): void {
  store.setTarget(target.kind === "layer" ? target.layer : target);
}

/** Why the prim cannot take a composition edit where edits go; `null` when it can. */
export function compositionBlock(store: EditorStore, prim: Prim): string | null {
  if (prim.instanceProxy) return "An instance proxy is read-only: edit its prototype's source, or the instance.";
  const target = currentTarget(store);
  if (target === null) return "The stage has no edit layer.";
  const where = mapping(target, prim.path);
  return where.specPath === null ? `${where.text}.` : null;
}

/** The prim's composition, strongest first, and the composition edits that go where edits go. */
export function CompositionSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  useRevision(store, "target", "history");
  const [open, setOpen] = useState<Open>(null);
  const [refusals, setRefusals] = useState<ReadonlyMap<string, string>>(new Map());
  const context = presentationContext(store);
  const target = currentTarget(store);
  if (context === null || target === null) return null;
  const stage = context.stage;
  const arcs = prim.compositionArcs();
  const specsOf = store.specsOf;
  const where = mapping(target, prim.path);
  const blocked = compositionBlock(store, prim);
  const uncomposed = uncomposedHere(arcs, target, prim.path, specsOf);
  const deleted = deletedHere(target, prim.path, specsOf);
  const refuse = (key: string, why: string | undefined) => setRefusals((before) => new Map(before).set(key, why ?? ""));
  const remove = (key: string, item: AuthoredArc) => refuse(key, store.removeArc(prim.path, item));
  return (
    <Section title="Composition" aside={<AddMenu blocked={blocked} onChoose={setOpen} />}>
      <TargetLine store={store} target={target} text={where.text} mapped={where.specPath !== null} />
      <ol className="flex flex-col gap-1.5" aria-label={`The arcs of ${prim.path}, strongest first`}>
        {arcs.map((arc, index) => (
          <ArcRow
            key={index}
            store={store}
            stage={stage}
            prim={prim}
            arcs={arcs}
            index={index}
            source={findAuthoring(arcs, index, specsOf)}
            target={target}
            refusal={refusals.get(`arc:${index}`) ?? ""}
            onRemove={(item) => remove(`arc:${index}`, item)}
          />
        ))}
      </ol>
      {uncomposed.length > 0 && (
        <Uncomposed store={store} stage={stage} prim={prim} items={uncomposed} target={target} refusals={refusals} onRemove={remove} />
      )}
      {deleted.length > 0 && (
        <div className="mt-2 flex flex-col gap-0.5 text-[11px] text-muted-foreground">
          <span>Deleted here, so no weaker layer adds them:</span>
          {deleted.map((item) => (
            <code key={item.index} className="truncate font-mono text-[10px]">
              {arcUsda(item.op, item.field, authoredItemText(item))}
            </code>
          ))}
        </div>
      )}
      <CompositionEditNote store={store} prim={prim} variants={false} />
      {open?.kind === "arc" && <AddArcDialog store={store} prim={prim} kind={open.arc} onClose={() => setOpen(null)} />}
      {open?.kind === "variantSet" && <VariantDialog store={store} prim={prim} set={null} onClose={() => setOpen(null)} />}
    </Section>
  );
}

/** "Add" in the section's header: an arc of each kind, or a variant set; disabled with why when the prim takes none here. */
function AddMenu({ blocked, onChoose }: { blocked: string | null; onChoose: (open: Open) => void }) {
  const kinds: readonly [NewArcKind, string][] = [
    ["reference", "Reference…"],
    ["payload", "Payload…"],
    ["inherit", "Inherit…"],
    ["specialize", "Specialize…"],
  ];
  return (
    <DropdownMenuTrigger>
      <Button variant="ghost" size="xs" aria-label="Add an arc or a variant set" isDisabled={blocked !== null}>
        <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
        Add
      </Button>
      <DropdownMenu placement="bottom end" className="min-w-40">
        {kinds.map(([kind, title]) => (
          <DropdownMenuItem key={kind} onAction={() => onChoose({ kind: "arc", arc: kind })}>
            {title}
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onAction={() => onChoose({ kind: "variantSet" })}>Variant set…</DropdownMenuItem>
      </DropdownMenu>
    </DropdownMenuTrigger>
  );
}

/** Where edits of the prim land now, and the way back to the edit layer when they go elsewhere. */
function TargetLine({ store, target, text, mapped }: { store: EditorStore; target: Layer | EditTarget; text: string; mapped: boolean }) {
  const elsewhere = store.session?.target !== undefined;
  const editLayer = store.session?.stage.editLayer ?? null;
  return (
    <div
      className={
        mapped
          ? "mb-2 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-[11px] text-muted-foreground"
          : "mb-2 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 rounded-md bg-amber-500/10 px-2 py-1 text-[11px] text-amber-800 dark:text-amber-300"
      }
    >
      <span className="min-w-0">
        Edits go to <span className="font-mono text-foreground">{text}</span>
        {"kind" in target && mapped && targetRoute(target) !== null && <span>, {targetRoute(target)}</span>}
      </span>
      {elsewhere && editLayer !== null && (
        <Button variant="outline" size="xs" onPress={() => store.setTarget(undefined)}>
          Back to {editLayer.identifier ?? "the session layer"}
        </Button>
      )}
    </div>
  );
}

/** The arc's offset across it, when it is not the identity: `offset 10, scale 2`. */
function offsetText(arc: CompositionArc): string | null {
  const { offset, scale } = arc.layerOffset;
  if (offset === 0 && scale === 1) return null;
  return [offset !== 0 ? `offset ${offset}` : null, scale !== 1 ? `scale ${scale}` : null].filter((part) => part !== null).join(", ");
}

/** The target that edits through the arc, or `null` when none does (a relocate's source). */
function throughTarget(arc: CompositionArc): EditTarget | null {
  if (arc.arc === "relocates") return null;
  try {
    return arc.editTarget();
  } catch {
    return null;
  }
}

/** Whether `a` and `b` write the same place: the same layer, or the same target. */
function sameTarget(a: Layer | EditTarget, b: Layer | EditTarget): boolean {
  const layerOf = (target: Layer | EditTarget) => ("kind" in target ? (target.kind === "layer" ? target.layer : null) : target);
  const left = layerOf(a);
  return left !== null ? left === layerOf(b) : a === b;
}

function ArcRow({
  store,
  stage,
  prim,
  arcs,
  index,
  source,
  target,
  refusal,
  onRemove,
}: {
  store: EditorStore;
  stage: Stage;
  prim: Prim;
  arcs: readonly CompositionArc[];
  index: number;
  source: ArcSource | null;
  target: Layer | EditTarget;
  refusal: string;
  onRemove: (item: AuthoredArc) => void;
}) {
  const context = presentationContext(store);
  const arc = arcs[index]!;
  if (context === null) return null;
  const through = arc.arc === "local" ? null : throughTarget(arc);
  const isTarget = through !== null && sameTarget(through, target);
  const here = source !== null && authoredHere(source, target, prim.path);
  const variant = arc.arc === "variants" ? variantOfSite(arc.site) : null;
  // Where the arc is authored, to edit there: the arc it is beneath, in the layer that authors it.
  let there: EditTarget | null = null;
  if (source !== null && !here) {
    try {
      there = arcs[source.parent]!.editTarget(source.layer);
    } catch {
      there = null;
    }
  }
  const offset = offsetText(arc);
  // The site is a path of the stage when the arc reads the stage's own layer stack (a class, an internal arc).
  const onStage = arc.layerStack === stage.root || stage.hasLocalLayer(arc.layerStack);
  const depth = nesting(arcs, index);
  return (
    <li className="flex flex-col gap-0.5 text-[11px]" style={{ paddingLeft: `${depth * 10}px` }}>
      <div className="flex min-w-0 items-center gap-1.5">
        <span className="shrink-0 font-medium">{variant === null ? arcNoun(arc.arc) : `variant ${variant.set}=${variant.variant}`}</span>
        {arc.implied && <span className="shrink-0 text-muted-foreground">(implied)</span>}
        <span className="flex min-w-0 items-center gap-1">
          <Present object={{ kind: "layer", layer: arc.layerStack }} context={context} />
          {arc.arc !== "local" && arc.arc !== "variants" &&
            (onStage ? (
              <Present object={{ kind: "primPath", path: arc.site }} context={context} />
            ) : (
              <span className="truncate font-mono text-muted-foreground" title={`${arc.site} in ${arc.layerStack.identifier ?? "the session layer"}: what ${prim.path} composes from it`}>
                {arc.site}
              </span>
            ))}
        </span>
        {isTarget && <span className="shrink-0 rounded-sm bg-sky-500/15 px-1 text-[10px] text-sky-700 dark:text-sky-300">edits go here</span>}
        <ArcMenu store={store} arc={arc} through={through} isTarget={isTarget} source={source} here={here} there={there} onRemove={onRemove} />
      </div>
      <div className="flex flex-wrap items-center gap-x-1 text-[10px] text-muted-foreground">
        <Authoring store={store} arc={arc} source={source} here={here} />
        {offset !== null && <span>· {offset}</span>}
      </div>
      {refusal !== "" && (
        <span role="alert" className="text-[10px] text-destructive">
          {refusal}
        </span>
      )}
    </li>
  );
}

/** Where an arc is authored, in words: here (removable), in another layer (live), implied, or not known. */
function Authoring({ store, arc, source, here }: { store: EditorStore; arc: CompositionArc; source: ArcSource | null; here: boolean }) {
  const context = presentationContext(store);
  if (context === null) return null;
  if (arc.arc === "local") return <span>its own opinions, in its layer stack</span>;
  if (arc.arc === "variants") return <span>the variant selected</span>;
  if (arc.arc === "relocates") return <span>relocated here; its source's specs do not contribute</span>;
  if (arc.implied) return <span>implied: a class arc authored inside what it references</span>;
  if (source === null) return <span>no spec of the layers it is read beneath authors it</span>;
  const usda = arcUsda(source.item.op, source.item.field, authoredItemText(source.item));
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-x-1">
      {here ? <span className="text-foreground">authored where edits go:</span> : source.ancestor === null ? <span>authored in</span> : <span>authored on its ancestor {source.ancestor} in</span>}
      {!here && <Present object={{ kind: "layer", layer: source.layer }} context={context} />}
      {!here && variantOfSite(source.specPath) !== null && <span>inside {source.specPath.slice(source.specPath.lastIndexOf("{"))}</span>}
      <code className="truncate font-mono" title={usda}>
        {usda}
      </code>
    </span>
  );
}

/** An arc's "⋯": edit through it, remove it where edits go, edit where it is authored, show that layer's text. */
function ArcMenu({
  store,
  arc,
  through,
  isTarget,
  source,
  here,
  there,
  onRemove,
}: {
  store: EditorStore;
  arc: CompositionArc;
  through: EditTarget | null;
  isTarget: boolean;
  source: ArcSource | null;
  here: boolean;
  there: EditTarget | null;
  onRemove: (item: AuthoredArc) => void;
}) {
  if (through === null && source === null) return null;
  const authoring = source?.layer.identifier ?? "the session layer";
  const noun = arcNoun(arc.arc);
  return (
    <DropdownMenuTrigger>
      <Button variant="ghost" size="icon-xs" aria-label={`Actions for the ${noun} to ${arc.site}`} className="ml-auto">
        <HugeiconsIcon icon={MoreHorizontalIcon} />
      </Button>
      <DropdownMenu placement="bottom end" className="min-w-56">
        {through !== null && (
          <DropdownMenuItem isDisabled={isTarget} onAction={() => editThrough(store, through)}>
            {isTarget ? "Edits go through this arc" : arc.arc === "variants" ? "Edit inside this variant" : `Edit through this ${noun} (in ${through.layer.identifier ?? "the session layer"})`}
          </DropdownMenuItem>
        )}
        {source !== null && here && (
          <DropdownMenuItem variant="destructive" onAction={() => onRemove(source.item)}>
            Remove the {noun} (from {authoring})
          </DropdownMenuItem>
        )}
        {source !== null && !here && there !== null && source.ancestor === null && (
          <DropdownMenuItem onAction={() => editThrough(store, there)}>Edit where it is authored, to remove it ({authoring})</DropdownMenuItem>
        )}
        {source !== null && (
          <DropdownMenuItem onAction={() => store.showLayerText(source.layer)}>Show it in {authoring}'s text</DropdownMenuItem>
        )}
      </DropdownMenu>
    </DropdownMenuTrigger>
  );
}

/** How many arcs `index`'s arc is beneath. */
function nesting(arcs: readonly { readonly parent: number | null }[], index: number): number {
  let count = 0;
  for (let at = arcs[index]?.parent ?? null; at !== null; at = arcs[at]?.parent ?? null) count++;
  return count;
}

/**
 * Arcs the spec edits go to authors that compose nothing, with why
 * (`uncomposedWhy`): a cycle they make, the composition error that names
 * their asset (a missing one), an asset not fetched yet, or a stronger
 * opinion that deletes them.
 */
function Uncomposed({
  store,
  stage,
  prim,
  items,
  target,
  refusals,
  onRemove,
}: {
  store: EditorStore;
  stage: Stage;
  prim: Prim;
  items: readonly AuthoredArc[];
  target: Layer | EditTarget;
  refusals: ReadonlyMap<string, string>;
  onRemove: (key: string, item: AuthoredArc) => void;
}) {
  // Read only when there is something to explain: the status walks the stage.
  const issues = stage.status().compositionErrors.filter((issue) => issue.prim === prim.path);
  const waiting = stage.unavailable();
  const layer = "kind" in target ? target.layer : target;
  return (
    <div className="mt-2 flex flex-col gap-1.5">
      <span className="text-[11px] text-muted-foreground">Authored where edits go, composing nothing:</span>
      {items.map((item) => {
        const text = arcItemText(item.asset, item.primPath);
        const pending = item.asset !== null && waiting.some((asset) => asset.assetPath === item.asset && asset.anchor === layer);
        const why = uncomposedWhy(item, layer.identifier, issues, pending, targetSpec(target, prim.path)?.specPath ?? null).text;
        const key = `uncomposed:${item.index}`;
        return (
          <div key={key} role="group" aria-label={`${kindOf(item.field)} ${text}`} className="flex flex-col gap-0.5 rounded-md bg-amber-500/10 px-2 py-1 text-[11px] text-amber-800 dark:text-amber-300">
            <div className="flex min-w-0 items-center gap-1.5">
              <span className="shrink-0 font-medium">{kindOf(item.field)}</span>
              <code className="min-w-0 truncate font-mono" title={arcUsda(item.op, item.field, authoredItemText(item))}>
                {arcUsda(item.op, item.field, authoredItemText(item))}
              </code>
              <Button variant="outline" size="xs" className="ml-auto" onPress={() => onRemove(key, item)}>
                Remove
              </Button>
            </div>
            <span className="text-[10px]">{why}</span>
            {(refusals.get(key) ?? "") !== "" && (
              <span role="alert" className="text-[10px] text-destructive">
                {refusals.get(key)}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

/**
 * The last composition edit of the prim, while it is the newest step:
 * what it was, and its layer's text a click away (the Text tab marks what
 * it changed); for a variant added, editing inside it. `variants` picks
 * the edits of variant sets (the Variants section's) or of arcs.
 */
export function CompositionEditNote({ store, prim, variants }: { store: EditorStore; prim: Prim; variants: boolean }) {
  const edit = store.compositionEdit;
  if (edit === null || edit.prim !== prim.path || isVariantEdit(edit.label) !== variants) return null;
  const layer = edit.layer.identifier ?? "the session layer";
  const outranked = edit.variant?.outranked;
  if (edit.variant !== undefined && outranked !== undefined) {
    const stronger = outranked.layer.identifier ?? "the session layer";
    const { set, variant } = edit.variant;
    return (
      <div role="status" className="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1 rounded-md bg-muted px-2 py-1 text-[11px]">
        <span className="min-w-0">
          Done: <span className="font-mono">{edit.label}</span>, in {layer}, its selection authored there; but {stronger}, stronger, selects{" "}
          <span className="font-mono">{outranked.variant}</span>, so {variant} is not what the stage selects.
        </span>
        <Button variant="outline" size="xs" onPress={() => void store.selectVariant(edit.prim, set, variant, selectionTarget(prim, outranked) ?? undefined)}>
          Select {variant} in {stronger}
        </Button>
        <Button variant="outline" size="xs" onPress={() => store.showLayerText(edit.layer)}>
          Show in {layer}'s text
        </Button>
      </div>
    );
  }
  return (
    <div role="status" className="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1 rounded-md bg-muted px-2 py-1 text-[11px]">
      <span className="min-w-0">
        Done: <span className="font-mono">{edit.label}</span>, in {layer}.
      </span>
      {edit.variant !== undefined && (
        <Button variant="outline" size="xs" onPress={() => store.editInsideVariant(edit.prim, edit.variant!.set, edit.variant!.variant)}>
          Edit inside {edit.variant.variant}
        </Button>
      )}
      <Button variant="outline" size="xs" onPress={() => store.showLayerText(edit.layer)}>
        Show in {layer}'s text
      </Button>
    </div>
  );
}

/** Whether a composition edit's label is a variant set's. */
function isVariantEdit(label: string): boolean {
  return label.startsWith("add variant") || label.startsWith("remove variant");
}
