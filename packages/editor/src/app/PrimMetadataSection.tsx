// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Metadata section: the prim's `documentation`, edited
// where edits go, and the keys of its `customData` and `assetInfo`
// dictionaries with their values. The dictionaries are read, not edited:
// a dictionary is authored whole in one layer and composes key by key
// with weaker layers', so an edit of one key here would need the
// edit target's own dictionary, which the stage does not expose.
import { useState } from "react";
import type { Prim, Value } from "@forest-rs/layerstack";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { holdsField } from "../model/primAuthoring.ts";
import { formatValue, releaseValue } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { useRevision } from "./context.ts";
import { editsGoTo, RefusalNote, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** The dictionary fields shown, with what each is for. */
const DICTIONARIES = [
  { key: "customData", about: "Data for tools and pipelines, which USD keeps and does not interpret." },
  { key: "assetInfo", about: "What asset this is: its identifier, name and version, as asset tools author them." },
] as const;

/** The prim's documentation (editable) and its customData and assetInfo (read). */
export function PrimMetadataSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  const documentation = scalarText(prim.metadata("documentation"));
  const dictionaries = DICTIONARIES.map(({ key, about }) => ({ key, about, entries: entries(prim.metadata(key)) }));
  return (
    <Section title="Metadata">
      <Documentation key={`${prim.path} ${documentation ?? ""}`} store={store} prim={prim} documentation={documentation} />
      {dictionaries.map(({ key, about, entries }) => (
        <div key={key} className="mt-2 text-[11px]">
          <div className="text-muted-foreground" title={about}>
            <span className="font-mono">{key}</span>
            {entries.length === 0 && " — none authored"}
          </div>
          {entries.length > 0 && (
            <div className="mt-0.5 grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-x-2 gap-y-0.5">
              {entries.map(([name, text]) => (
                <div key={name} className="contents">
                  <span className="truncate font-mono text-muted-foreground" title={name}>
                    {name}
                  </span>
                  <span className="truncate font-mono" title={text}>
                    {text}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
      {dictionaries.some(({ entries }) => entries.length > 0) && (
        <p className="mt-1.5 text-[11px] text-muted-foreground">Dictionaries are shown as composed, key by key across layers; they are not edited here.</p>
      )}
    </Section>
  );
}

/** The documentation field: a text area, saved where edits go, or cleared there. */
function Documentation({ store, prim, documentation }: { store: EditorStore; prim: Prim; documentation: string | null }) {
  // Clear is offered for what the spec edits go to holds: read again when they go elsewhere.
  useRevision(store, "target");
  const [text, setText] = useState(documentation ?? "");
  const [refusal, attempt] = useRefusal();
  const changed = text !== (documentation ?? "");
  const readOnly = prim.instanceProxy;
  const save = () => changed && attempt(() => store.setDocumentation(prim.path, text));
  return (
    <div className="flex flex-col gap-1">
      <Textarea
        aria-label="Documentation"
        placeholder="What this prim is for"
        value={text}
        disabled={readOnly}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            save();
          }
          if (event.key === "Escape" && changed) {
            event.stopPropagation();
            setText(documentation ?? "");
          }
        }}
        className="min-h-12 text-[11px]"
      />
      <div className="flex items-center gap-1">
        <span className="mr-auto text-[11px] text-muted-foreground">{changed ? `⌘↩ saves to ${editsGoTo(store)}` : "documentation"}</span>
        {changed && (
          <Button variant="ghost" size="xs" onPress={() => setText(documentation ?? "")}>
            Revert
          </Button>
        )}
        {changed && (
          <Button variant="outline" size="xs" onPress={save}>
            Save
          </Button>
        )}
        {!changed && holdsField(store.targetSpec(prim.path), "documentation") && !readOnly && (
          <Button variant="ghost" size="xs" aria-label={`Clear ${editsGoTo(store)}'s documentation`} onPress={() => attempt(() => store.setDocumentation(prim.path, ""))}>
            Clear
          </Button>
        )}
      </div>
      <RefusalNote refusal={refusal} className="text-[11px] text-destructive" />
    </div>
  );
}

/** A scalar metadata value as text; `null` for none. Releases the value. */
function scalarText(value: Value | null): string | null {
  const text = value?.kind === "scalar" ? String(value.value) : null;
  releaseValue(value);
  return text;
}

/** A dictionary's entries as `[key, value text]`, sorted, nested keys flattened as `a:b`; none for another value. Releases the value. */
function entries(value: Value | null): [string, string][] {
  const out: [string, string][] = [];
  const walk = (dictionary: ReadonlyMap<string, Value | null>, prefix: string) => {
    for (const [key, entry] of dictionary) {
      if (entry?.kind === "dictionary") walk(entry.value, `${prefix}${key}:`);
      else out.push([`${prefix}${key}`, formatValue(entry)]);
    }
  };
  if (value?.kind === "dictionary") walk(value.value, "");
  releaseValue(value);
  return out.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}
