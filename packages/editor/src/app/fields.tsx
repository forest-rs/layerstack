// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// One attribute's editor, chosen by what the stage knows of it: its
// declared type, and the schema's `allowedTokens` for a token. A token
// with allowed values is a picker of them, a `bool` a switch, a number or
// a tuple of numbers fields that commit on Enter or blur, a color a
// swatch beside its channels. What has no editor yet (arrays,
// dictionaries) shows its value. Every commit authors through the store,
// which reports what the stage refused.
import { useEffect, useState } from "react";
import type { Attribute, Value } from "@forest-rs/layerstack";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { formatNumber, formatValue, releaseValue } from "../model/values.ts";
import type { EditorStore } from "../store.ts";

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

/** Whether a value type is a color (`color3f`, `color4d`). */
const isColor = (type: string) => /^color[34][fdh]$/.test(type);

/** A linear channel as an sRGB hex pair, for a swatch. */
function hexChannel(linear: number): string {
  const clamped = Math.min(1, Math.max(0, linear));
  const srgb = clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * clamped ** (1 / 2.4) - 0.055;
  return Math.round(srgb * 255)
    .toString(16)
    .padStart(2, "0");
}

/** An sRGB hex pair as a linear channel. */
function linearChannel(hex: string): number {
  const srgb = Number.parseInt(hex, 16) / 255;
  return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
}

/** A text field that commits on Enter or blur, and goes back to `value` on Escape or when `value` changes. */
function CommitField({
  value,
  onCommit,
  className,
  label,
  numeric = false,
}: {
  value: string;
  onCommit: (text: string) => void;
  className?: string;
  label: string;
  numeric?: boolean;
}) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  return (
    <input
      aria-label={label}
      value={text}
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
        className,
      )}
    />
  );
}

/**
 * A value as the inspector holds it across a render: a scalar or tuple
 * as it is, anything else (an array on the module's memory, a
 * dictionary) only as its text, read and released before rendering.
 */
export interface Shown {
  readonly value: (Value & { readonly kind: "scalar" | "tuple" }) | null;
  readonly text: string;
}

/** `value` read for the inspector, and released. */
export function shown(value: Value | null): Shown {
  const text = formatValue(value);
  const kept = value?.kind === "scalar" || value?.kind === "tuple" ? value : null;
  releaseValue(value);
  return { value: kept, text };
}

/** The editor of `attribute`'s value now. */
export function ValueField({ store, attribute, shown: { value, text } }: { store: EditorStore; attribute: Attribute; shown: Shown }) {
  const type = attribute.typeName ?? value?.type ?? "";
  const label = attribute.name;
  const set = (input: Parameters<EditorStore["setValue"]>[1]) => store.setValue(attribute.path, input);
  if (type === "token") {
    const allowed = allowedTokens(attribute);
    const current = value?.kind === "scalar" ? String(value.value) : "";
    if (allowed !== null && allowed.length > 0) {
      const options = allowed.includes(current) || current === "" ? allowed : [current, ...allowed];
      return (
        <Select aria-label={label} value={current} onChange={(key) => key !== null && key !== current && set({ type, value: String(key) })} className="w-full">
          <SelectTrigger size="sm" className="font-mono text-[11px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {options.map((token) => (
              <SelectItem key={token} id={token} textValue={token} className="font-mono text-[11px]">
                {token}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      );
    }
  }
  if (type === "bool" && (value === null || value.kind === "scalar")) {
    return <Switch size="sm" aria-label={label} isSelected={value?.value === true} onChange={(on) => set({ type, value: on })} />;
  }
  if (value?.kind === "scalar" && (typeof value.value === "number" || typeof value.value === "bigint")) {
    return (
      <CommitField
        label={label}
        numeric
        value={formatNumber(value.value)}
        className="w-full"
        onCommit={(text) => {
          const number = Number(text);
          if (text.trim() !== "" && Number.isFinite(number)) set({ type, value: number });
        }}
      />
    );
  }
  if (value?.kind === "scalar" && typeof value.value === "string") {
    return <CommitField label={label} value={value.value} className="w-full" onCommit={(text) => set({ type, value: text })} />;
  }
  if (value?.kind === "tuple" && value.value.length <= 4) {
    const channels = value.value;
    const color = isColor(type) ? `#${channels.slice(0, 3).map(hexChannel).join("")}` : null;
    return (
      <div className="flex w-full items-center gap-1">
        {color !== null && (
          <input
            type="color"
            aria-label={`${label} color`}
            value={color}
            onChange={(event) => {
              const hex = event.target.value.slice(1);
              const rgb = [0, 2, 4].map((at) => linearChannel(hex.slice(at, at + 2)));
              set({ type, value: [...rgb, ...channels.slice(3)] });
            }}
            className="h-6 w-7 shrink-0 cursor-pointer rounded border border-input bg-transparent p-0.5"
          />
        )}
        {channels.map((channel, index) => (
          <CommitField
            key={index}
            label={`${label} ${index}`}
            numeric
            value={formatNumber(channel)}
            className="w-0 flex-1"
            onCommit={(text) => {
              const number = Number(text);
              if (text.trim() === "" || !Number.isFinite(number)) return;
              set({ type, value: channels.map((old, at) => (at === index ? number : old)) });
            }}
          />
        ))}
      </div>
    );
  }
  return (
    <span className="truncate font-mono text-[11px] text-muted-foreground" title={text}>
      {text}
    </span>
  );
}
