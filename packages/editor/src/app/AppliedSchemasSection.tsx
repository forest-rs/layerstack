// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The inspector's Applied schemas section: the API schemas the prim has
// (`Prim.appliedSchemas`), those its type builds in told from those layers
// author in `apiSchemas`; one the spec edits go to applies is removed
// there, one only a weaker layer applies is deleted there (which says so),
// and one a stronger layer applies names that layer; and "Apply…", the
// schemas that can apply to it. After an apply, the properties the schema
// brought are named here and shown under Properties.
import type { Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { Cancel01Icon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { API_SCHEMA_NOTES, appliedSchemaRows, authoredApiSchemas, type SchemaRemoval, schemaRemoval, schemaSites } from "../model/primAuthoring.ts";
import { targetSite } from "../model/propertyOps.ts";
import type { EditorStore } from "../store.ts";
import { useRevision, useStoreSelector } from "./context.ts";
import { askPrim } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, useRefusal } from "./primAuthoringParts.tsx";
import { Section } from "./Section.tsx";

/** The most of a schema's new properties named after it is applied. */
const NAMED = 4;

/** A layer as the section names it. */
function layerName(layer: { readonly identifier: string | null } | null): string {
  return layer === null ? "a weaker layer" : (layer.identifier ?? "the session layer");
}

/** The prim's applied API schemas, each with what taking it away where edits go would do, and Apply… for more. */
export function AppliedSchemasSection({ store, prim }: { store: EditorStore; prim: Prim }) {
  const [refusal, attempt] = useRefusal();
  const revealed = useStoreSelector(store, ["history"], (at) => at.revealed);
  // What removing a schema does depends on where edits go: read again when they go elsewhere.
  useRevision(store, "target");
  const rows = appliedSchemaRows(prim.appliedSchemas(), authoredApiSchemas(prim));
  const readOnly = prim.instanceProxy;
  const where = editsGoTo(store);
  // The specs `apiSchemas` compose from, each read once per change of the stage: which of them applies each schema.
  const session = store.session;
  const site = session === null ? null : targetSite(session.stage, session.target, prim.path);
  const read = rows.some((row) => row.authored) ? schemaSites(prim.compositionArcs(), site, (layer, path) => store.authoredSpec(layer, path)?.spec ?? null) : null;
  const removals = new Map(rows.map((row) => [row.applied, schemaRemoval(row, read?.sites ?? [], read?.target ?? -1)]));
  return (
    <Section
      title="Applied schemas"
      aside={
        <Button variant="ghost" size="xs" isDisabled={readOnly} onPress={() => askPrim({ kind: "applySchema", path: prim.path })} aria-label="Apply an API schema, where edits go">
          <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
          Apply…
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
              <SchemaRemove
                removal={removals.get(row.applied)!}
                applied={row.applied}
                where={where}
                readOnly={readOnly}
                onRemove={() => attempt(() => store.removeSchema(prim.path, row.schema, row.instance))}
              />
            </li>
          ))}
        </ul>
      )}
      {revealed !== null && revealed.prim === prim.path && rows.some((row) => row.applied === revealed.schema) && (
        <p role="status" className="mt-1.5 text-[11px] text-muted-foreground">
          {revealed.names.length === 0
            ? `${revealed.schema} brought no properties of its own.`
            : `${revealed.schema} brought ${revealed.names.length} ${revealed.names.length === 1 ? "property" : "properties"}, at their fallbacks under Properties: ${revealed.names.slice(0, NAMED).join(", ")}${revealed.names.length > NAMED ? ` and ${revealed.names.length - NAMED} more` : ""}.`}
        </p>
      )}
      <RefusalNote refusal={refusal} className="mt-1 text-[11px] text-destructive" />
      {[...removals.values()].some((removal) => removal.kind === "remove" || removal.kind === "delete") && !readOnly && (
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Remove takes a schema from apiSchemas in {where} and deletes what weaker layers apply; Delete here authors only the delete.
        </p>
      )}
    </Section>
  );
}

/** What a row offers to take its schema away where edits go ({@link SchemaRemoval}). */
function SchemaRemove({ removal, applied, where, readOnly, onRemove }: { removal: SchemaRemoval; applied: string; where: string; readOnly: boolean; onRemove: () => void }) {
  if (removal.kind === "builtIn") {
    return (
      <Badge variant="outline" title="Its type applies it (a built-in or auto-applied schema): no layer can remove it">
        from its type
      </Badge>
    );
  }
  if (removal.kind === "stronger") {
    const layer = layerName(removal.layer);
    return (
      <Badge variant="outline" className="ml-auto" title={`${layer} applies it, stronger than ${where}: a delete there would not take it away. Edit in ${layer} to remove it.`}>
        in {layer}
      </Badge>
    );
  }
  if (readOnly) return null;
  if (removal.kind === "delete") {
    const label = `Delete ${applied} here, which removes what ${layerName(removal.layer)} applies`;
    return (
      <Button variant="ghost" size="xs" className="ml-auto opacity-0 group-hover:opacity-100 focus-visible:opacity-100" aria-label={label} onPress={onRemove}>
        Delete here
      </Button>
    );
  }
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      className="ml-auto opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
      aria-label={`Remove ${applied} in ${where}`}
      onPress={onRemove}
    >
      <HugeiconsIcon icon={Cancel01Icon} />
    </Button>
  );
}
