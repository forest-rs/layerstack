// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's State section: what the prim is (its type, specifier and
// kind) and whether it is active and instanceable, each changed where
// edits go. The values shown are composed (a stronger layer's opinion may
// be what is read); an edit authors the edit target's spec, and "Clear",
// offered only when that spec holds one, takes its opinion of active or
// instanceable away, so weaker layers' show again.
import type { Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { holdsField, kindChoices } from "../model/primAuthoring.ts";
import type { EditorStore } from "../store.ts";
import { primIcon } from "./icons.ts";
import { askPrim } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, SpecifierPicker, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** The `kind` key of "no kind": choosing it clears the edit target's. */
const NO_KIND = "";

/** The prim's type, specifier, kind, active and instanceable, each authored where edits go. */
export function PrimStateSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  const [refusal, attempt] = useRefusal();
  const readOnly = prim.instanceProxy;
  const where = editsGoTo(store);
  const kindValue = prim.metadata("kind");
  const kind = kindValue?.kind === "scalar" && kindValue.value !== "" ? String(kindValue.value) : null;
  const instanceable = prim.metadata("instanceable");
  const isInstanceable = instanceable?.kind === "scalar" && instanceable.value === true;
  // What the spec edits go to holds itself: all "Clear" can take away there.
  const own = store.targetSpec(prim.path);
  const root = prim.path === "/";
  return (
    <Section title="State">
      <div className="grid grid-cols-[5.5rem_minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1.5 text-xs">
        <span>Type</span>
        <Button
          variant="outline"
          size="sm"
          className="justify-start font-mono"
          isDisabled={readOnly || root}
          aria-label={`Type: ${prim.typeName ?? "none"}. Retype…`}
          onPress={() => askPrim({ kind: "retype", path: prim.path })}
        >
          <HugeiconsIcon icon={primIcon(prim.typeName)} data-icon="inline-start" />
          {prim.typeName ?? "typeless"}
        </Button>
        <span />

        <span title="def defines the prim, over only overrides one defined elsewhere, class defines one others inherit">Specifier</span>
        <SpecifierPicker
          value={prim.specifier}
          isDisabled={readOnly || root}
          onChange={(specifier) => attempt(() => store.setSpecifier(prim.path, specifier))}
        />
        <span />

        <span title="The prim's place in the model hierarchy (kind metadata)">Kind</span>
        <Select
          aria-label="Kind"
          value={kind ?? NO_KIND}
          isDisabled={readOnly || root}
          disabledKeys={holdsField(own, "kind") ? [] : [NO_KIND]}
          onChange={(key) => {
            if (key === null || key === (kind ?? NO_KIND)) return;
            attempt(() => store.setKind(prim.path, key === NO_KIND ? null : String(key)));
          }}
          className="w-full"
        >
          <SelectTrigger size="sm">
            <SelectValue>{({ selectedText }) => selectedText}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem id={NO_KIND} textValue="none">
              <span className="flex flex-col">
                <span>none</span>
                <span className="text-[10px] whitespace-normal text-muted-foreground">
                  {holdsField(own, "kind")
                    ? `Clears ${where}'s kind: not part of the model hierarchy unless a weaker layer says so.`
                    : `${where} authors no kind to clear${kind === null ? "" : `: ${kind} is another layer's`}.`}
                </span>
              </span>
            </SelectItem>
            {kindChoices(kind).map((choice) => (
              <SelectItem key={choice.kind} id={choice.kind} textValue={choice.kind}>
                <span className="flex flex-col">
                  <span className="font-mono">{choice.kind}</span>
                  <span className="text-[10px] whitespace-normal text-muted-foreground">{choice.meaning}</span>
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <span />

        <span>Active</span>
        <Switch
          size="sm"
          aria-label="Active"
          isDisabled={readOnly || root}
          isSelected={prim.active}
          onChange={(on) => attempt(() => store.apply(`${on ? "activate" : "deactivate"} ${prim.path}`, [{ type: "setActive", path: prim.path, active: on }]))}
        />
        <ClearOpinion shown={holdsField(own, "active") && !readOnly} label={`Clear ${where}'s opinion of active`} onPress={() => attempt(() => store.clearPrimOpinion(prim.path, "active"))} />

        <span>Instanceable</span>
        <Switch
          size="sm"
          aria-label="Instanceable"
          isDisabled={readOnly || root}
          isSelected={isInstanceable}
          onChange={(on) => attempt(() => store.apply(`set ${prim.path} instanceable ${on}`, [{ type: "setInstanceable", path: prim.path, instanceable: on }]))}
        />
        <ClearOpinion shown={holdsField(own, "instanceable") && !readOnly} label={`Clear ${where}'s opinion of instanceable`} onPress={() => attempt(() => store.clearPrimOpinion(prim.path, "instanceable"))} />
        <RefusalNote refusal={refusal} />
      </div>
      <p className="mt-1.5 text-[11px] text-muted-foreground">
        {readOnly
          ? "An instance proxy is read-only: edit its prototype's source, or the instance."
          : `Shown as composed; edits go to ${where}, where a stronger layer's opinion may still win.`}
      </p>
    </Section>
  );
}

/** "Clear" beside a switch whose value the spec edits go to holds: takes that opinion away. */
function ClearOpinion({ shown, label, onPress }: { shown: boolean; label: string; onPress: () => void }) {
  if (!shown) return <span />;
  return (
    <TooltipTrigger>
      <Button variant="ghost" size="xs" aria-label={label} onPress={onPress}>
        Clear
      </Button>
      <Tooltip>{label}: weaker layers' opinion shows again</Tooltip>
    </TooltipTrigger>
  );
}
