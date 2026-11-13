// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One attribute's editor, chosen by what the stage knows of it: its value
// presented in the `accept` view, by its declared type and the schema's
// `allowedTokens` for a token (`presentations/values.tsx`), and the fields
// those editors are made of. What has no editor (most arrays,
// dictionaries) shows its value. Every commit authors through the store,
// which reports what the stage refused.
import { useEffect, useId, useRef, useState } from "react";
import type { Attribute } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import { NOT_A_NUMBER, parseNumber } from "../model/refusal.ts";
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

/** How long a field shows that its input was refused, in milliseconds. */
const REFUSED_MS = 2500;

/**
 * A text field that commits on Enter or blur, and goes back to `value` on
 * Escape (authoring nothing). What is typed is shown only until it is
 * committed: then the field shows `value` again, which is what the stage
 * reads after the edit, so an edit a stronger opinion hides does not look
 * as if it took. `onCommit` returns why the input was refused, if it was:
 * the field then marks itself invalid for a moment, with the reason as its
 * title and for assistive technology.
 */
export function CommitField({
  value,
  onCommit,
  className,
  label,
  numeric = false,
  disabled = false,
}: {
  value: string;
  onCommit: (text: string) => string | void;
  className?: string;
  label: string;
  numeric?: boolean;
  disabled?: boolean;
}) {
  // What is typed and not yet committed; `null` shows `value`.
  const [draft, setDraft] = useState<string | null>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const cancelled = useRef(false);
  const reason = useId();
  const text = draft ?? value;
  useEffect(() => {
    if (refused === null) return;
    const timer = setTimeout(() => setRefused(null), REFUSED_MS);
    return () => clearTimeout(timer);
  }, [refused]);
  return (
    <>
      <input
        aria-label={label}
        value={text}
        disabled={disabled}
        inputMode={numeric ? "decimal" : undefined}
        aria-invalid={refused !== null ? true : undefined}
        aria-describedby={refused !== null ? reason : undefined}
        title={refused ?? undefined}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") event.currentTarget.blur();
          if (event.key === "Escape") {
            // The blur below runs before the reset renders: say it is a cancel.
            cancelled.current = true;
            setDraft(null);
            setRefused(null);
            event.currentTarget.blur();
          }
        }}
        onBlur={() => {
          if (cancelled.current) {
            cancelled.current = false;
            return;
          }
          setDraft(null);
          if (text === value) return;
          const why = onCommit(text);
          if (typeof why === "string") setRefused(why);
        }}
        className={cn(
          "h-6 min-w-0 rounded-md border border-input bg-input/20 px-1.5 font-mono text-[11px] outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30 dark:bg-input/30",
          numeric && "text-right tabular-nums",
          disabled && "opacity-60",
          refused !== null && "border-destructive focus-visible:border-destructive focus-visible:ring-destructive/30",
          className,
        )}
      />
      {refused !== null && (
        <span id={reason} role="alert" className="sr-only">
          {refused}
        </span>
      )}
    </>
  );
}

/** A number field of a vector that commits a finite number on Enter or blur; `onCommit` may return why it was refused. */
export function CommitNumber({
  label,
  value,
  onCommit,
  disabled = false,
}: {
  label: string;
  value: string;
  onCommit: (value: number) => string | void;
  disabled?: boolean;
}) {
  return (
    <CommitField
      label={label}
      numeric
      value={value}
      disabled={disabled}
      className="w-0 flex-1"
      onCommit={(text) => {
        const number = parseNumber(text);
        return number === null ? NOT_A_NUMBER : onCommit(number);
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
