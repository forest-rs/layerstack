// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The concrete types the stage registers, grouped by what they are
// (`model/primAuthoring.ts`), with "no type" first, as a filterable list:
// what the Add dialog defines a child as and the Retype dialog makes a
// prim. The key of "no type" is `null`.
import { useMemo } from "react";
import { Header, ListBox, ListBoxItem, ListBoxSection } from "react-aria-components";
import type { Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { cn } from "@/lib/utils";
import { typeGroups } from "../model/primAuthoring.ts";
import { primIcon } from "./icons.ts";

/** The count of concrete types `stage` registers, for a dialog's description. */
export function typeCount(stage: Stage): number {
  return typeGroups(stage.schemas()).reduce((count, [, schemas]) => count + schemas.length, 0);
}

/**
 * The grouped types of `stage` matching `filter`, `typeName` selected
 * (`null`: no type), choosing with `onChoose`; `typelessLabel` says what
 * "no type" does here.
 */
export function TypeList({
  stage,
  filter,
  typeName,
  onChoose,
  typelessLabel = "typeless",
}: {
  stage: Stage;
  filter: string;
  typeName: string | null;
  onChoose: (typeName: string | null) => void;
  typelessLabel?: string;
}) {
  const groups = useMemo(() => typeGroups(stage.schemas()), [stage]);
  const needle = filter.toLowerCase();
  const shown = groups
    .map(([group, schemas]) => [group, schemas.filter((schema) => schema.name.toLowerCase().includes(needle))] as const)
    .filter(([, schemas]) => schemas.length > 0);
  return (
    <ListBox
      aria-label="Type"
      selectionMode="single"
      disallowEmptySelection
      selectedKeys={[typeName ?? ""]}
      onSelectionChange={(keys) => {
        const [key] = keys === "all" ? [] : [...keys];
        if (key !== undefined) onChoose(key === "" ? null : String(key));
      }}
      className="max-h-64 overflow-y-auto rounded-md border p-1 outline-none"
    >
      <ListBoxItem id="" textValue="no type" className={typeItemClass}>
        <HugeiconsIcon icon={primIcon(null)} className="size-3.5 text-muted-foreground" />
        No type <span className="text-muted-foreground">({typelessLabel})</span>
      </ListBoxItem>
      {shown.map(([group, schemas]) => (
        <ListBoxSection key={group}>
          <Header className="px-2 pt-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">{group}</Header>
          {schemas.map((schema) => (
            <ListBoxItem key={schema.name} id={schema.name} textValue={schema.name} className={typeItemClass}>
              <HugeiconsIcon icon={primIcon(schema.name)} className="size-3.5 text-muted-foreground" />
              {schema.name}
              <span className="ml-auto truncate text-[10px] text-muted-foreground">{schema.bases[0] ?? ""}</span>
            </ListBoxItem>
          ))}
        </ListBoxSection>
      ))}
    </ListBox>
  );
}

/** A list item of the editor's dialogs: focused muted, selected accented. */
export const typeItemClass = ({ isSelected, isFocused }: { isSelected: boolean; isFocused: boolean }) =>
  cn("flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none", isFocused && "bg-muted", isSelected && "bg-accent text-accent-foreground");
