// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Choosing a prim (or one of its properties) from those the stage
// composes, rather than typing its path: a dialog with a Find field over
// the hierarchy (`model/primSearch.ts`, as the prim tree finds), narrowed
// to a schema when the caller wants one (`Material`), listing what
// matches with its type. Enter or a double-click chooses the prim; with
// `properties`, the chosen prim's properties are listed to choose from
// too. Anything that targets prims (relationship targets, bindings,
// connections) picks with it.
import { useMemo, useState } from "react";
import { ListBox, ListBoxItem } from "react-aria-components";
import type { Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { searchPrims } from "../model/primSearch.ts";
import { primIcon } from "./icons.ts";

/** The most prims the picker lists. */
const LISTED = 300;

const NOTHING: readonly string[] = [];

/**
 * A dialog that chooses a prim of `stage`, or with `properties` one of its
 * properties, and hands its path to `onPick` (then closes). `type`
 * narrows the prims to a schema (`Prim.isA`, or `Prim.hasAPI` for an API
 * schema); `exclude` leaves out paths already chosen; `prefer` lists the
 * prims it holds for first (class prims, for an inherit).
 */
export function PrimPicker({
  stage,
  title,
  description,
  type,
  properties = false,
  exclude = NOTHING,
  prefer,
  onPick,
  onClose,
}: {
  stage: Stage;
  title: string;
  description?: React.ReactNode;
  type?: string;
  properties?: boolean;
  exclude?: readonly string[];
  prefer?: (path: string) => boolean;
  onPick: (path: string) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState("");
  const [chosen, setChosen] = useState<string | null>(null);
  // Keyed by the paths left out, not the array the caller built this render.
  const excluded = exclude.join("\n");
  const matches = useMemo(() => {
    const found = searchPrims(stage, { text, limit: LISTED + exclude.length, ...(type === undefined ? {} : { type }) }).filter((path) => !exclude.includes(path)).slice(0, LISTED);
    return prefer === undefined ? found : [...found.filter(prefer), ...found.filter((path) => !prefer(path))];
  }, [stage, text, type, excluded, prefer]);
  const prim = chosen === null ? null : stage.prim(chosen);
  const propertyNames = useMemo(() => (prim === null ? [] : [...prim.attributeNames(), ...prim.relationshipNames()].sort()), [prim]);
  const pick = (path: string) => {
    onPick(path);
    onClose();
  };
  const kind = type ?? "prim";
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        {description !== undefined && <DialogDescription>{description}</DialogDescription>}
      </DialogHeader>
      <Input autoFocus placeholder={`Find a ${kind} by name`} aria-label={`Find a ${kind} by name`} value={text} onChange={(event) => setText(event.target.value)} className="h-7 text-xs" />
      <ListBox
        aria-label={type === undefined ? "Prims" : `${type} prims`}
        selectionMode="single"
        selectionBehavior="replace"
        selectedKeys={chosen === null ? [] : [chosen]}
        onSelectionChange={(keys) => {
          const [key] = keys === "all" ? [] : [...keys];
          setChosen(key === undefined ? null : String(key));
        }}
        onAction={(key) => pick(String(key))}
        renderEmptyState={() => (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            {text.trim() === "" ? `The stage has no ${kind} to choose${exclude.length > 0 ? " that is not chosen already" : ""}.` : `No ${kind} is named like that.`}
          </p>
        )}
        className="max-h-64 min-h-16 overflow-y-auto rounded-md border p-1 outline-none"
      >
        {matches.map((path) => {
          const found = stage.prim(path);
          return (
            <ListBoxItem key={path} id={path} textValue={path} className={itemClass}>
              <HugeiconsIcon icon={primIcon(found?.typeName ?? null)} className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="flex min-w-0 flex-col">
                <span className="truncate">{path.slice(path.lastIndexOf("/") + 1)}</span>
                <span className="truncate font-mono text-[10px] text-muted-foreground">{path}</span>
              </span>
              <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
                {found?.specifier === "class" ? "class " : ""}
                {found?.typeName ?? ""}
              </span>
            </ListBoxItem>
          );
        })}
      </ListBox>
      {matches.length === LISTED && <p className="text-[11px] text-muted-foreground">The first {LISTED}: type more of the name to narrow them.</p>}
      {properties && prim !== null && (
        <div className="flex flex-col gap-1">
          <span className="text-[11px] text-muted-foreground">Or one of its properties:</span>
          {propertyNames.length === 0 ? (
            <p className="text-[11px] text-muted-foreground">{prim.path} has no properties.</p>
          ) : (
            <ListBox aria-label={`Properties of ${prim.path}`} onAction={(key) => pick(String(key))} className="max-h-32 overflow-y-auto rounded-md border p-1 outline-none">
              {propertyNames
                .map((name) => `${prim.path}.${name}`)
                .filter((path) => !exclude.includes(path))
                .map((path) => (
                  <ListBoxItem key={path} id={path} textValue={path} className={itemClass}>
                    <span className="truncate font-mono text-[11px]">{path.slice(path.lastIndexOf(".") + 1)}</span>
                  </ListBoxItem>
                ))}
            </ListBox>
          )}
        </div>
      )}
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={chosen === null} onPress={() => chosen !== null && pick(chosen)}>
          {chosen === null ? `Choose a ${kind}` : `Choose ${chosen.slice(chosen.lastIndexOf("/") + 1)}`}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

const itemClass = ({ isSelected, isFocused }: { isSelected: boolean; isFocused: boolean }) =>
  cn("flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none", isFocused && "bg-muted", isSelected && "bg-accent text-accent-foreground");
