// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the prim-authoring sections share: where edits go, in words; a
// refusal held beside the control that caused it; and the specifier
// picker the State section and the Add dialog both offer.
import { useState } from "react";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { type PrimSpecifier, SPECIFIER_MEANINGS } from "../model/primAuthoring.ts";
import type { EditorStore } from "../store.ts";
import { targetText } from "./TargetPicker.tsx";

/** Where `store`'s edits go now, in words (`scene.usda`, `robot.usda at /Robot`). */
export function editsGoTo(store: EditorStore): string {
  const session = store.session;
  if (session === null) return "nowhere";
  const destination = session.target ?? session.stage.editLayer;
  return destination === null ? "the edit layer" : targetText(destination);
}

/**
 * A refusal to show beside a control, and the function that runs an
 * action and keeps what it returns: the store's refusal, or nothing when
 * it was taken (or waits its turn, when the refusal is a notice alone).
 */
export function useRefusal(): [string | null, (action: () => string | undefined) => void] {
  const [refusal, setRefusal] = useState<string | null>(null);
  return [refusal, (action) => setRefusal(action() ?? null)];
}

/** A refusal said under the control that caused it; nothing without one. */
export function RefusalNote({ refusal, className }: { refusal: string | null; className?: string }) {
  if (refusal === null) return null;
  return (
    <p role="alert" className={className ?? "col-span-full text-[11px] text-destructive"}>
      {refusal}
    </p>
  );
}

const SPECIFIERS: readonly PrimSpecifier[] = ["def", "over", "class"];

/** A picker of `def`, `over` and `class`, each with what it means. */
export function SpecifierPicker({
  value,
  onChange,
  isDisabled = false,
  label = "Specifier",
}: {
  value: PrimSpecifier;
  onChange: (specifier: PrimSpecifier) => void;
  isDisabled?: boolean;
  label?: string;
}) {
  return (
    <Select aria-label={label} value={value} isDisabled={isDisabled} onChange={(key) => key !== null && key !== value && onChange(key as PrimSpecifier)} className="w-full">
      <SelectTrigger size="sm">
        <SelectValue>{({ selectedText }) => selectedText}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {SPECIFIERS.map((specifier) => (
          <SelectItem key={specifier} id={specifier} textValue={specifier}>
            <span className="flex flex-col">
              <span className="font-mono">{specifier}</span>
              <span className="text-[10px] whitespace-normal text-muted-foreground">{SPECIFIER_MEANINGS[specifier]}</span>
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
