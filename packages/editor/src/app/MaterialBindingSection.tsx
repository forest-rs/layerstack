// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Material section: the material the prim is bound to
// (`ComputeBoundMaterial`), live, with the binding that decides it (its
// own or an ancestor's, direct or through a collection) and the surface
// shader's inputs; "Bind…" picks a material to bind and "Unbind" blocks
// it, each where edits go and saying what it authors.
import type { Prim, Stage } from "@forest-rs/layerstack";
import { Button } from "@/components/ui/button";
import { materialSummary } from "../model/materials.ts";
import { primOfProperty } from "./presentations/objects.tsx";
import { Present, presentationContext } from "./presentations/Present.tsx";
import type { EditorStore } from "../store.ts";
import { askPrim } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** Whether a prim of `prim`'s kind gets a Material section: anything but the shading network itself, unless something binds it. */
export function showsMaterialSection(prim: Prim): boolean {
  return !(prim.isA("Material") || prim.isA("Shader") || prim.isA("NodeGraph")) || prim.boundMaterial() !== null;
}

/** The prim's bound material, its binding and inputs, with Bind… and Unbind. */
export function MaterialBindingSection({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  const [refusal, attempt] = useRefusal();
  const context = presentationContext(store);
  const binding = prim.boundMaterial();
  const summary = binding === null ? null : materialSummary(stage, prim.path);
  const readOnly = prim.instanceProxy;
  const where = editsGoTo(store);
  // A binding on an ancestor or through a collection is inherited: unbinding here blocks it for this prim and beneath.
  const own = binding !== null && binding.kind === "direct" && primOfProperty(binding.relationship) === prim.path;
  return (
    <Section
      title="Material"
      aside={
        <span className="flex items-center gap-1">
          <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => askPrim({ kind: "bind", path: prim.path })}>
            Bind…
          </Button>
          {binding !== null && (
            <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => attempt(() => store.unbindMaterial(prim.path))}>
              {own ? "Unbind" : "Block"}
            </Button>
          )}
        </span>
      }
    >
      {binding === null || context === null ? (
        <p className="text-[11px] text-muted-foreground">Nothing binds a material to it or above it.</p>
      ) : (
        <>
          <div className="flex min-w-0 items-center gap-1 text-[11px]">
            {binding.material === null ? (
              <span className="font-mono">
                {binding.target} <span className="text-muted-foreground">(no material there)</span>
              </span>
            ) : (
              <Present object={{ kind: "primPath", path: binding.material }} context={context} />
            )}
          </div>
          <div className="mb-1.5 flex min-w-0 flex-wrap items-center gap-x-1 text-[11px] text-muted-foreground">
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
        </>
      )}
      <RefusalNote refusal={refusal} className="mt-1 text-[11px] text-destructive" />
      {!readOnly && (
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Bind authors <span className="font-mono">material:binding</span> and applies <span className="font-mono">MaterialBindingAPI</span> in {where};{" "}
          {binding === null ? "descendants inherit it." : `${own ? "Unbind" : "Block"} authors an empty binding there, which also blocks what ancestors bind.`}
        </p>
      )}
    </Section>
  );
}
