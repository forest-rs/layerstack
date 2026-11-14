// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A relationship's targets as the inspector edits them: each target live
// (a click selects or explains it), moved earlier or later, or removed,
// and a target added from the prim picker, a prim or one of its
// properties. Adding or removing one is that edit where edits go
// (`UsdRelationship::AddTarget`, `RemoveTarget`): what weaker layers give
// stays theirs, and is not copied into the layer written. Moving one
// authors the whole list in its new order as the relationship's explicit
// targets (`SetTargets`), the only way a layer orders them, which stops
// it following weaker layers' targets. Each is one undoable edit; a
// refusal is said beneath the list.
import { useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowDown01Icon, ArrowUp01Icon, Cancel01Icon, PlusSignIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { movedTarget } from "../model/propertyOps.ts";
import type { EditorStore } from "../store.ts";
import { PrimPicker } from "./PrimPicker.tsx";
import { Present, presentationContext } from "./presentations/Present.tsx";

/** A prim or property path presented as the object it names. */
export function TargetPath({ store, path }: { store: EditorStore; path: string }) {
  const context = presentationContext(store);
  if (context === null) return <span className="truncate font-mono text-[11px]">{path}</span>;
  return <Present object={path.includes(".") ? { kind: "propertyPath", path } : { kind: "primPath", path }} context={context} />;
}

/**
 * The targets of the relationship at `path`, editable unless `readOnly`
 * (an instance proxy's, which its prototype gives).
 */
export function RelationshipTargets({ store, path, targets, readOnly }: { store: EditorStore; path: string; targets: readonly string[]; readOnly: boolean }) {
  const [picking, setPicking] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const stage = store.session?.stage;
  const said = (refusal: string | undefined) => setRefused(refusal ?? null);
  const author = (next: readonly string[]) => said(store.setTargets(path, next));
  const name = path.slice(path.lastIndexOf(".") + 1);
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      {targets.length === 0 && <span className="text-[11px] text-muted-foreground">no targets</span>}
      {targets.map((target, index) => (
        <div key={target} className="group/target flex min-w-0 items-center gap-0.5">
          <span className="flex min-w-0 flex-1">
            <TargetPath store={store} path={target} />
          </span>
          {!readOnly && (
            <span className="flex shrink-0 opacity-0 group-focus-within/target:opacity-100 group-hover/target:opacity-100">
              {targets.length > 1 && (
                <>
                  <Button variant="ghost" size="icon-xs" aria-label={`Move ${target} earlier, authoring the targets as an explicit list`} isDisabled={index === 0} onPress={() => author(movedTarget(targets, index, -1))}>
                    <HugeiconsIcon icon={ArrowUp01Icon} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`Move ${target} later, authoring the targets as an explicit list`}
                    isDisabled={index === targets.length - 1}
                    onPress={() => author(movedTarget(targets, index, 1))}
                  >
                    <HugeiconsIcon icon={ArrowDown01Icon} />
                  </Button>
                </>
              )}
              <Button variant="ghost" size="icon-xs" aria-label={`Remove the target ${target}`} onPress={() => said(store.removeTarget(path, target))}>
                <HugeiconsIcon icon={Cancel01Icon} />
              </Button>
            </span>
          )}
        </div>
      ))}
      {!readOnly && stage !== undefined && (
        <Button variant="ghost" size="xs" className="self-start text-muted-foreground" onPress={() => setPicking(true)}>
          <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
          Add a target
        </Button>
      )}
      {refused !== null && (
        <span role="alert" className="text-[11px] text-destructive">
          {refused}
        </span>
      )}
      {picking && stage !== undefined && (
        <PrimPicker
          stage={stage}
          title={`Add a target to ${name}`}
          description="A prim, or one of its properties. It is added where edits go, to that layer's targets: ahead of those weaker layers give, which stay theirs."
          properties
          exclude={targets}
          onPick={(picked) => said(store.addTarget(path, picked))}
          onClose={() => setPicking(false)}
        />
      )}
    </div>
  );
}
