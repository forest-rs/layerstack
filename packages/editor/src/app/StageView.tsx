// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The Stage tab: the stage's metadata (`model/stageMetadata.ts`), each
// field with its value, the layer the stage reads it from (the session
// layer over the root, or the field's fallback), an editor offering what
// the stage knows (the up axes, the units, the root prims) and a clear of
// the opinion. An edit is one undoable action in the root (or the session
// layer, when edits go there): the stage reads its metadata from no other
// layer, and the tab says so when edits go elsewhere. The viewport turns
// to a new up axis and the timeline follows the range and the rate.
import { useEffect, useState } from "react";
import { ToggleButton, ToggleButtonGroup } from "react-aria-components";
import type { Layer, Stage, Value, ValueInput } from "@forest-rs/layerstack";
import { Button, buttonVariants } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { NOT_A_NUMBER, parseNumber } from "../model/refusal.ts";
import { defaultPrimPath, type FieldSource, inheritedFrom, rootOnly, STAGE_FIELDS, type StageField, UNIT_PRESETS, unitPreset } from "../model/stageMetadata.ts";
import { formatNumber } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { CommitField } from "./fields.tsx";
import { layerName } from "./format.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";
import { countRender } from "./renders.ts";

/** A metadata value as the tab edits it: a number, a token or string, or nothing. */
function scalarOf(value: Value | null): number | string | null {
  if (value?.kind !== "scalar") return null;
  return typeof value.value === "number" || typeof value.value === "string" ? value.value : null;
}

/** The stage's metadata, editable. */
export function StageView() {
  countRender("StageView");
  const store = useEditorStore();
  // The metadata is the stage's (`composition`); where edits go decides where an edit is authored (`target`).
  useRevision(store, "composition", "target", "history");
  const [refused, setRefused] = useState<{ readonly key: string; readonly message: string } | null>(null);
  const session = store.session;
  if (session === null) return <p className="p-3 text-xs text-muted-foreground">Nothing is open.</p>;
  const stage = session.stage;
  const placement = store.metadataPlacement();
  const sources = store.stageMetadataSources();
  const context = presentationContext(store);
  const commit = (key: string, value: ValueInput): string | undefined => {
    const why = store.setStageMetadata(key, value);
    setRefused(why === undefined ? null : { key, message: why });
    return why;
  };
  const clear = (key: string, layer: Layer) => {
    const why = store.clearStageMetadata(key, layer);
    setRefused(why === undefined ? null : { key, message: why });
  };
  const sourceLayer = (source: FieldSource): Layer | null => (source === "session" ? session.sessionLayer : source === "root" ? stage.root : null);
  return (
    <div className="flex flex-col pb-3 text-xs">
      <div className="border-b px-3 py-2 text-[11px] text-muted-foreground">
        {placement === null ? null : placement.note === null ? (
          <>
            Edits are authored in{" "}
            {context === null ? layerName(placement.layer.identifier) : <Present object={{ kind: "layer", layer: placement.layer }} context={context} />}
            {placement.layer === stage.root ? ", the root layer" : ", the session layer, over the root"}, as one undoable edit each. The stage reads its metadata from the session layer over the
            root, and from no sublayer.
          </>
        ) : (
          placement.note
        )}
      </div>
      <dl className="flex flex-col">
        {STAGE_FIELDS.map((field) => {
          const source = sources.get(field.key) ?? "fallback";
          const holder = sourceLayer(source);
          const inherited = inheritedFrom(field.key, sources);
          const lender = inherited === null ? null : sourceLayer(inherited.source);
          return (
            <div key={field.key} className="flex flex-col gap-1 border-b px-3 py-2">
              <div className="flex items-baseline gap-2">
                <dt>
                  <TooltipTrigger delay={300}>
                    <Button variant="ghost" size="xs" className="-ml-1.5 h-auto px-1.5 py-0 font-medium">
                      {field.label}
                    </Button>
                    <Tooltip className="max-w-64">{field.about}</Tooltip>
                  </TooltipTrigger>
                </dt>
                <span className="min-w-0 truncate font-mono text-[10px] text-muted-foreground">{field.key}</span>
              </div>
              <dd className="flex flex-col gap-1">
                <FieldEditor
                  store={store}
                  field={field}
                  // The stage's value is the session layer's over the root's; a field read from the root alone is the root's.
                  value={rootOnly(field.key) ? stage.layerSpec(stage.root, "/").defaultPrim : scalarOf(stage.metadata(field.key))}
                  commit={(value) => commit(field.key, value)}
                />
                <span className="flex min-w-0 items-center gap-1 text-[10px] text-muted-foreground">
                  {holder === null && inherited !== null && lender !== null ? (
                    <>
                      <span className="shrink-0">No layer authors it: it is the {inherited.key} of</span>
                      {context === null ? layerName(lender.identifier) : <Present object={{ kind: "layer", layer: lender }} context={context} />}
                    </>
                  ) : holder === null ? (
                    field.key === "defaultPrim" || field.key === "documentation" ? (
                      "No layer authors it."
                    ) : (
                      "No layer authors it: the field's fallback."
                    )
                  ) : (
                    <>
                      <span className="shrink-0">The opinion is in</span>
                      {context === null ? layerName(holder.identifier) : <Present object={{ kind: "layer", layer: holder }} context={context} />}
                      <Button variant="ghost" size="xs" className="h-5 px-1.5 text-[10px]" onPress={() => clear(field.key, holder)} aria-label={`Clear ${field.key} in ${layerName(holder.identifier)}`}>
                        Clear
                      </Button>
                    </>
                  )}
                </span>
              </dd>
              {refused?.key === field.key && (
                <p role="alert" className="text-[11px] text-destructive">
                  {refused.message}
                </p>
              )}
            </div>
          );
        })}
      </dl>
    </div>
  );
}

/** The editor of one field, by what it is. */
function FieldEditor({ store, field, value, commit }: { store: EditorStore; field: StageField; value: number | string | null; commit: (value: ValueInput) => string | undefined }) {
  switch (field.key) {
    case "upAxis":
      return <UpAxisPicker value={value === "Z" ? "Z" : "Y"} commit={commit} />;
    case "metersPerUnit":
      return <UnitPicker value={typeof value === "number" ? value : 0.01} commit={commit} />;
    case "defaultPrim":
      return <DefaultPrimPicker store={store} value={typeof value === "string" ? value : null} commit={commit} />;
    case "documentation":
      return <DocumentationField value={typeof value === "string" ? value : ""} commit={commit} />;
    default:
      return (
        <CommitField
          label={field.label}
          numeric
          className="w-28"
          value={typeof value === "number" ? formatNumber(value) : ""}
          onCommit={(text) => {
            const number = parseNumber(text);
            if (number === null) return NOT_A_NUMBER;
            if (field.key.endsWith("PerSecond") && number <= 0) return "A rate is above 0";
            return commit({ type: "double", value: number });
          }}
        />
      );
  }
}

const option = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "xs" });

/** Y or Z. */
function UpAxisPicker({ value, commit }: { value: "Y" | "Z"; commit: (value: ValueInput) => string | undefined }) {
  return (
    <ToggleButtonGroup
      aria-label="Up axis"
      selectionMode="single"
      disallowEmptySelection
      selectedKeys={[value]}
      onSelectionChange={(keys) => {
        const [key] = [...keys];
        if ((key === "Y" || key === "Z") && key !== value) commit({ type: "token", value: key });
      }}
      className="flex gap-0.5"
    >
      <ToggleButton id="Y" className={option}>
        Y up
      </ToggleButton>
      <ToggleButton id="Z" className={option}>
        Z up
      </ToggleButton>
    </ToggleButtonGroup>
  );
}

/** A unit by name, or any length in meters. */
function UnitPicker({ value, commit }: { value: number; commit: (value: ValueInput) => string | undefined }) {
  const preset = unitPreset(value);
  return (
    <div className="flex items-center gap-2">
      <Select
        aria-label="Unit"
        value={preset === null ? "other" : preset.short}
        onChange={(key) => {
          const chosen = UNIT_PRESETS.find((each) => each.short === key);
          if (chosen !== undefined && chosen !== preset) commit({ type: "double", value: chosen.metersPerUnit });
        }}
        className="w-40"
      >
        <SelectTrigger size="sm">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {UNIT_PRESETS.map((each) => (
            <SelectItem key={each.short} id={each.short} textValue={each.name}>
              {each.name} <span className="text-muted-foreground">{formatNumber(each.metersPerUnit)} m</span>
            </SelectItem>
          ))}
          {preset === null && (
            <SelectItem id="other" textValue="Another length">
              another length
            </SelectItem>
          )}
        </SelectContent>
      </Select>
      <CommitField
        label="Meters per unit"
        numeric
        className="w-28"
        value={formatNumber(value)}
        onCommit={(text) => {
          const number = parseNumber(text);
          if (number === null) return NOT_A_NUMBER;
          if (number <= 0) return "A length is above 0";
          return commit({ type: "double", value: number });
        }}
      />
      <span className="text-[10px] text-muted-foreground">m</span>
    </div>
  );
}

/** Whether `stage` composes a prim at `path`; not for a path it refuses. */
function hasPrim(stage: Stage, path: string): boolean {
  try {
    return stage.prim(path) !== null;
  } catch {
    return false;
  }
}

/** A root prim, by name: what the stage has at its top. */
function DefaultPrimPicker({ store, value, commit }: { store: EditorStore; value: string | null; commit: (value: ValueInput) => string | undefined }) {
  const stage = store.session?.stage;
  const roots = (stage?.children("/") ?? []).map((path) => path.slice(1));
  // A root prim's name, or a prim's path (`World/Robot`): what a reference without a prim path brings in.
  const path = value === null ? null : defaultPrimPath(value);
  const missing = value !== null && (path === null || stage === undefined || !hasPrim(stage, path));
  const listed = value !== null && roots.includes(value);
  return (
    <div className="flex flex-col gap-1">
      <Select
        aria-label="Default prim"
        value={value ?? "none"}
        onChange={(key) => {
          if (typeof key === "string" && key !== "none" && key !== value && roots.includes(key)) commit({ type: "token", value: key });
        }}
        className="w-56"
      >
        <SelectTrigger size="sm">
          <SelectValue className="font-mono" />
        </SelectTrigger>
        <SelectContent>
          {value === null && (
            <SelectItem id="none" textValue="None">
              <span className="text-muted-foreground">none</span>
            </SelectItem>
          )}
          {value !== null && !listed && (
            <SelectItem id={value} textValue={value}>
              <span className="font-mono">{value}</span> {missing && <span className="text-muted-foreground">no prim</span>}
            </SelectItem>
          )}
          {roots.map((name) => (
            <SelectItem key={name} id={name} textValue={name}>
              <span className="font-mono">{name}</span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {missing && <span className="text-[11px] text-destructive">No prim is at {path ?? value}: a reference to this layer without a prim path brings nothing.</span>}
      {roots.length === 0 && <span className="text-[11px] text-muted-foreground">The stage has no root prims to choose from.</span>}
    </div>
  );
}

/** The documentation, committed on blur (or ⌘↩), Escape going back. */
function DocumentationField({ value, commit }: { value: string; commit: (value: ValueInput) => string | undefined }) {
  const [draft, setDraft] = useState<string | null>(null);
  useEffect(() => setDraft(null), [value]);
  const text = draft ?? value;
  const save = () => {
    if (draft !== null && draft !== value) commit({ type: "string", value: draft });
    setDraft(null);
  };
  return (
    <Textarea
      aria-label="Documentation"
      value={text}
      placeholder="What this stage is, in words"
      onChange={(event) => setDraft(event.target.value)}
      onBlur={save}
      onKeyDown={(event) => {
        if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) event.currentTarget.blur();
        if (event.key === "Escape") setDraft(null);
      }}
      className={cn("min-h-12 text-xs")}
    />
  );
}
