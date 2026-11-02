// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One attribute's editor, chosen by what the stage knows of it: its value
// presented in the `accept` view, by its declared type and the schema's
// `allowedTokens` for a token (`presentations/values.tsx`), and the fields
// those editors are made of. What has no editor (most arrays,
// dictionaries) shows its value. Every commit authors through the store,
// which reports what the stage refused.
import { useEffect, useState } from "react";
import type { Attribute } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import { releaseValue, type Shown } from "../model/values.ts";

export { shown, type Shown } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";

/** The tokens the schema allows `attribute`'s value to be, or `null` when it names none. */
export function allowedTokens(attribute: Attribute): string[] | null {
  const value = attribute.metadata("allowedTokens");
  if (value === null) return null;
  try {
    if (value.kind === "array") return Array.from(value.array.copy() as ArrayLike<string>, String);
    if (value.kind === "list") return value.value.flatMap((item) => (item?.kind === "scalar" ? [String(item.value)] : []));
    return null;
  } finally {
    releaseValue(value);
  }
}

/** A text field that commits on Enter or blur, and goes back to `value` on Escape or when `value` changes. */
export function CommitField({
  value,
  onCommit,
  className,
  label,
  numeric = false,
  disabled = false,
}: {
  value: string;
  onCommit: (text: string) => void;
  className?: string;
  label: string;
  numeric?: boolean;
  disabled?: boolean;
}) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  return (
    <input
      aria-label={label}
      value={text}
      disabled={disabled}
      inputMode={numeric ? "decimal" : undefined}
      onChange={(event) => setText(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") event.currentTarget.blur();
        if (event.key === "Escape") {
          setText(value);
          event.currentTarget.blur();
        }
      }}
      onBlur={() => {
        if (text !== value) onCommit(text);
      }}
      className={cn(
        "h-6 min-w-0 rounded-md border border-input bg-input/20 px-1.5 font-mono text-[11px] outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30 dark:bg-input/30",
        numeric && "text-right tabular-nums",
        disabled && "opacity-60",
        className,
      )}
    />
  );
}

/** A number field of a vector that commits a finite number on Enter or blur. */
export function CommitNumber({ label, value, onCommit, disabled = false }: { label: string; value: string; onCommit: (value: number) => void; disabled?: boolean }) {
  return (
    <CommitField
      label={label}
      numeric
      value={value}
      disabled={disabled}
      className="w-0 flex-1"
      onCommit={(text) => {
        const number = Number(text);
        if (text.trim() !== "" && Number.isFinite(number)) onCommit(number);
      }}
    />
  );
}

/**
 * The editor of `attribute`'s value: its value presented in the `accept`
 * view (`presentations/values.tsx` registers the built-ins, by the value's
 * type), authoring the default, or with `at` (an animated attribute, at
 * the current time) a sample there.
 */
export function ValueField({ store, attribute, shown, at }: { store: EditorStore; attribute: Attribute; shown: Shown; at: number | null }) {
  const context = presentationContext(store);
  if (context === null) return null;
  const typeName = attribute.typeName ?? shown.value?.type ?? "";
  return <Present object={{ kind: "value", attribute, typeName, shown, at }} context={context} accept={(value) => store.setValue(attribute.path, value, at)} />;
}
