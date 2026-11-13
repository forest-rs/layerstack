// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Applied schemas section: the API schemas the prim has
// (`Prim.appliedSchemas`), those its type builds in told from those layers
// author in `apiSchemas`, each authored one removable where edits go; and
// "Apply…", the schemas that can apply to it. After an apply, the
// properties the schema brought are named here and shown under
// Properties.
import type { Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { Cancel01Icon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { API_SCHEMA_NOTES, appliedSchemaRows, authoredApiSchemas } from "../model/primAuthoring.ts";
import type { EditorStore } from "../store.ts";
import { useStoreSelector } from "./context.ts";
import { askPrim } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** The most of a schema's new properties named after it is applied. */
const NAMED = 4;

/** The prim's applied API schemas, with Remove on those layers author and Apply… for more. */
export function AppliedSchemasSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  const [refusal, attempt] = useRefusal();
  const revealed = useStoreSelector(store, ["history"], (at) => at.revealed);
  const rows = appliedSchemaRows(prim.appliedSchemas(), authoredApiSchemas(prim));
  const readOnly = prim.instanceProxy;
  const where = editsGoTo(store);
  return (
    <Section
      title="Applied schemas"
      aside={
        <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => askPrim({ kind: "applySchema", path: prim.path })} aria-label="Apply an API schema, where edits go">
          <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
          Apply
        </Button>
      }
    >
      {rows.length === 0 ? (
        <p className="text-[11px] text-muted-foreground">No API schema is applied.</p>
      ) : (
        <ul className="flex flex-col gap-0.5">
          {rows.map((row) => (
            <li key={row.applied} className="group flex min-h-5 items-center gap-1.5 text-[11px]" title={API_SCHEMA_NOTES[row.schema]}>
              <span className="truncate font-mono">
                {row.schema}
                {row.instance !== null && <span className="text-muted-foreground">:{row.instance}</span>}
              </span>
              {!row.authored && (
                <Badge variant="outline" title="Its type applies it (a built-in or auto-applied schema): no layer can remove it">
                  from its type
                </Badge>
              )}
              {row.authored && !readOnly && (
                <Button
                  variant="ghost"
                  size="icon-xs"
                  className="ml-auto opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                  aria-label={`Remove ${row.applied} in ${where}`}
                  onPress={() => attempt(() => store.removeSchema(prim.path, row.schema, row.instance))}
                >
                  <HugeiconsIcon icon={Cancel01Icon} />
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {revealed !== null && revealed.prim === prim.path && rows.some((row) => row.applied === revealed.schema) && (
        <p role="status" className="mt-1.5 text-[11px] text-muted-foreground">
          {revealed.names.length === 0
            ? `${revealed.schema} brought no properties of its own.`
            : `${revealed.schema} brought ${revealed.names.length} propert${revealed.names.length === 1 ? "y" : "ies"}, at their fallbacks under Properties: ${revealed.names.slice(0, NAMED).join(", ")}${revealed.names.length > NAMED ? ` and ${revealed.names.length - NAMED} more` : ""}.`}
        </p>
      )}
      <RefusalNote refusal={refusal} className="mt-1 text-[11px] text-destructive" />
      {rows.some((row) => row.authored) && !readOnly && (
        <p className="mt-1.5 text-[11px] text-muted-foreground">Removing one takes it from apiSchemas in {where}, or deletes it there, which removes what weaker layers apply too.</p>
      )}
    </Section>
  );
}
