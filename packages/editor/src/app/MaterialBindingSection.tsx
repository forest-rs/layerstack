// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Material section: the material the prim is bound to
// (`ComputeBoundMaterial`), live, with the binding that decides it (its
// own or an ancestor's, direct or through a collection) and the surface
// shader's inputs, and then the binding of each purpose (`preview`,
// `full`) that one of that purpose decides; "Bind…" picks a material to
// bind and "Unbind" empties the prim's own binding, each where edits go
// and saying what it authors.
import type { Prim, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { PaintBoardIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { materialSummary } from "../model/materials.ts";
import { type BindingRow, bindingRows } from "../model/primAuthoring.ts";
import type { PresentationContext } from "../presentations/registry.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import type { EditorStore } from "../store.ts";
import { askPrim } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** Whether a prim of `prim`'s kind gets a Material section: anything but the shading network itself, unless something binds it. */
export function showsMaterialSection(prim: Prim): boolean {
  return !(prim.isA("Material") || prim.isA("Shader") || prim.isA("NodeGraph")) || prim.boundMaterial() !== null;
}

/** The prim's bound material for all purposes and for each purpose bound apart, their bindings and inputs, with Bind… and Unbind. */
export function MaterialBindingSection({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  const [refusal, attempt] = useRefusal();
  const context = presentationContext(store);
  const rows = bindingRows(prim);
  const summary = rows[0]?.purpose === "" ? materialSummary(stage, prim.path) : null;
  const readOnly = prim.instanceProxy;
  const where = editsGoTo(store);
  return (
    <Section
      title="Material"
      aside={
        <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => askPrim({ kind: "bind", path: prim.path })}>
          <HugeiconsIcon icon={PaintBoardIcon} data-icon="inline-start" />
          Bind…
        </Button>
      }
    >
      {rows.length === 0 || context === null ? (
        <p className="text-[11px] text-muted-foreground">Nothing binds a material to it or above it, for any purpose.</p>
      ) : (
        rows.map((row) => (
          <Binding key={row.purpose} row={row} context={context} readOnly={readOnly} onUnbind={() => attempt(() => store.unbindMaterial(prim.path, row.purpose === "" ? undefined : row.purpose))} />
        ))
      )}
      {summary !== null && summary.inputs.length > 0 && (
        <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-x-2 gap-y-0.5 text-[11px]">
          {summary.inputs.map((input) => (
            <div key={input.name} className="contents">
              <span className="truncate text-muted-foreground">{input.name}</span>
              <span className="truncate font-mono" title={input.value}>
                {input.value}
              </span>
            </div>
          ))}
        </div>
      )}
      <RefusalNote refusal={refusal} className="mt-1 text-[11px] text-destructive" />
      {!readOnly && (
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Bind authors <span className="font-mono">material:binding</span> and applies <span className="font-mono">MaterialBindingAPI</span> in {where}; descendants
          inherit it.
          {rows.some((row) => row.own) && " Unbind authors an empty binding there, which takes away what weaker layers bind it to; it is then bound as its ancestors are."}
          {rows.some((row) => !row.own) && " A binding it inherits is changed on the prim that authors it, or by binding this one."}
        </p>
      )}
    </Section>
  );
}

/** One binding: its material, what binds it and for which purpose, and Unbind when it is the prim's own. */
function Binding({ row, context, readOnly, onUnbind }: { row: BindingRow; context: PresentationContext; readOnly: boolean; onUnbind: () => void }) {
  const { binding, own, purpose } = row;
  return (
    <div className="mb-1.5">
      <div className="flex min-w-0 items-center gap-1 text-[11px]">
        {purpose !== "" && <span className="shrink-0 text-muted-foreground">for {purpose}:</span>}
        {binding.material === null ? (
          <span className="font-mono">
            {binding.target} <span className="text-muted-foreground">(no material there)</span>
          </span>
        ) : (
          <Present object={{ kind: "primPath", path: binding.material }} context={context} />
        )}
        {own && (
          <Button variant="ghost" size="xs" className="ml-auto shrink-0" isDisabled={readOnly} aria-label={purpose === "" ? "Unbind" : `Unbind for ${purpose}`} onPress={onUnbind}>
            Unbind
          </Button>
        )}
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-1 text-[11px] text-muted-foreground">
        <span>{own ? "bound here by" : binding.kind === "collection" ? "bound through a collection by" : "inherited from"}</span>
        <Present object={{ kind: "propertyPath", path: binding.relationship }} context={context} />
        {binding.collection !== null && (
          <>
            <span>for the prims of</span>
            <Present object={{ kind: "propertyPath", path: binding.collection }} context={context} />
          </>
        )}
        {binding.strength === "strongerThanDescendants" && <span>, stronger than descendants</span>}
      </div>
    </div>
  );
}
