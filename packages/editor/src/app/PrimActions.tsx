// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring prims: add a child of any type the stage registers, rename,
// delete. Each is one undoable action through the store, written where
// edits go; the stage refuses what it must (a name that is not a valid
// identifier, a path already taken) and the notice says why.
import { useMemo, useState } from "react";
import { ListBox, ListBoxItem, ListBoxSection, Header } from "react-aria-components";
import type { SchemaInfo, Stage } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { MoreHorizontalIcon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { EditorStore } from "../store.ts";
import { primIcon } from "./icons.ts";

/** What a dialog is doing, and to which prim. */
type Open = { readonly kind: "add"; readonly parent: string } | { readonly kind: "rename"; readonly path: string } | null;

/** A prim name as USD takes it: an identifier. */
const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** The concrete typed schemas, grouped by what they are, by their bases. */
function typeGroups(schemas: readonly SchemaInfo[]): [string, SchemaInfo[]][] {
  const groups = new Map<string, SchemaInfo[]>();
  for (const schema of schemas) {
    if (schema.kind !== "concreteTyped") continue;
    const group = schema.bases.includes("Gprim") || schema.name === "PointInstancer"
      ? "Geometry"
      : schema.bases.some((base) => base.endsWith("LightBase")) || schema.name.endsWith("Light")
        ? "Lights"
        : schema.name === "Camera"
          ? "Cameras"
          : ["Material", "Shader", "NodeGraph"].includes(schema.name)
            ? "Shading"
            : schema.name.startsWith("Physics")
              ? "Physics"
              : schema.name === "Xform" || schema.name === "Scope" || schema.bases.includes("Xformable")
                ? "Transforms and groups"
                : "Other";
    groups.set(group, [...(groups.get(group) ?? []), schema]);
  }
  const order = ["Transforms and groups", "Geometry", "Lights", "Cameras", "Shading", "Physics", "Other"];
  return order.filter((group) => groups.has(group)).map((group) => [group, groups.get(group)!]);
}

/** A name not yet taken among `parent`'s children: `base`, else `base1`, `base2`, ... */
function freeName(stage: Stage, parent: string, base: string): string {
  const taken = new Set(stage.children(parent).map((path) => path.slice(path.lastIndexOf("/") + 1)));
  if (!taken.has(base)) return base;
  for (let index = 1; ; index++) if (!taken.has(`${base}${index}`)) return `${base}${index}`;
}

/** The "⋯" menu of a prim's actions, and the dialogs it opens. */
export function PrimActions({ store, path, className }: { store: EditorStore; path: string; className?: string }) {
  const [open, setOpen] = useState<Open>(null);
  const stage = store.session?.stage;
  if (stage === undefined) return null;
  const prim = stage.prim(path);
  const readOnly = prim?.instanceProxy === true;
  // Loading works on any prim (its payloads and those beneath it, as
  // usdview offers); a prototype's proxies are loaded through their instance.
  const loadable = prim !== null && !readOnly && (prim.hasAuthoredPayloads || stage.findLoadable(path).length > 0 || stage.loadRules().length > 0);
  return (
    <>
      <DropdownMenuTrigger>
        <Button variant="ghost" size="icon-xs" aria-label={`Actions for ${path}`} className={className}>
          <HugeiconsIcon icon={MoreHorizontalIcon} />
        </Button>
        <DropdownMenu placement="bottom end" className="min-w-44">
          <DropdownMenuItem onAction={() => setOpen({ kind: "add", parent: path })} isDisabled={readOnly}>
            Add a child prim…
          </DropdownMenuItem>
          {loadable && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onAction={() => void store.load(path)} isDisabled={!prim?.active}>
                Load payloads
              </DropdownMenuItem>
              <DropdownMenuItem onAction={() => void store.load(path, "withoutDescendants")} isDisabled={!prim?.active}>
                Load payloads, not beneath
              </DropdownMenuItem>
              <DropdownMenuItem onAction={() => void store.unload(path)}>Unload payloads</DropdownMenuItem>
            </>
          )}
          {path !== "/" && (
            <>
              <DropdownMenuItem onAction={() => setOpen({ kind: "rename", path })} isDisabled={readOnly}>
                Rename…
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onAction={() => store.delete(path)} isDisabled={readOnly}>
                Delete
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenu>
      </DropdownMenuTrigger>
      {open?.kind === "add" && <AddPrimDialog store={store} stage={stage} parent={open.parent} onClose={() => setOpen(null)} />}
      {open?.kind === "rename" && <RenameDialog store={store} stage={stage} path={open.path} onClose={() => setOpen(null)} />}
    </>
  );
}

function AddPrimDialog({ store, stage, parent, onClose }: { store: EditorStore; stage: Stage; parent: string; onClose: () => void }) {
  const groups = useMemo(() => typeGroups(stage.schemas()), [stage]);
  const [typeName, setTypeName] = useState<string | null>("Xform");
  const [filter, setFilter] = useState("");
  const [name, setName] = useState(() => freeName(stage, parent, "Xform"));
  const [named, setNamed] = useState(false);
  const shown = groups
    .map(([group, schemas]) => [group, schemas.filter((schema) => schema.name.toLowerCase().includes(filter.toLowerCase()))] as const)
    .filter(([, schemas]) => schemas.length > 0);
  const taken = stage.children(parent).some((child) => child.slice(child.lastIndexOf("/") + 1) === name);
  const problem = !NAME.test(name) ? "A prim name is a letter or _ then letters, digits or _." : taken ? `${parent} already has a child ${name}.` : null;
  const choose = (next: string | null) => {
    setTypeName(next);
    if (!named) setName(freeName(stage, parent, next ?? "Prim"));
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Add a child of {parent}</DialogTitle>
        <DialogDescription>Any concrete type the stage registers, {groups.reduce((count, [, schemas]) => count + schemas.length, 0)} of them.</DialogDescription>
      </DialogHeader>
      <Input autoFocus placeholder="Find a type" value={filter} onChange={(event) => setFilter(event.target.value)} className="h-7 text-xs" />
      <ListBox
        aria-label="Type"
        selectionMode="single"
        disallowEmptySelection
        selectedKeys={[typeName ?? ""]}
        onSelectionChange={(keys) => {
          const [key] = keys === "all" ? [] : [...keys];
          if (key !== undefined) choose(key === "" ? null : String(key));
        }}
        className="max-h-64 overflow-y-auto rounded-md border p-1 outline-none"
      >
        <ListBoxItem id="" textValue="typeless" className={itemClass}>
          <HugeiconsIcon icon={primIcon(null)} className="size-3.5 text-muted-foreground" />
          No type <span className="text-muted-foreground">(typeless)</span>
        </ListBoxItem>
        {shown.map(([group, schemas]) => (
          <ListBoxSection key={group}>
            <Header className="px-2 pt-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">{group}</Header>
            {schemas.map((schema) => (
              <ListBoxItem key={schema.name} id={schema.name} textValue={schema.name} className={itemClass}>
                <HugeiconsIcon icon={primIcon(schema.name)} className="size-3.5 text-muted-foreground" />
                {schema.name}
                <span className="ml-auto truncate text-[10px] text-muted-foreground">{schema.bases[0] ?? ""}</span>
              </ListBoxItem>
            ))}
          </ListBoxSection>
        ))}
      </ListBox>
      <label className="flex flex-col gap-1 text-xs">
        Name
        <Input
          value={name}
          onChange={(event) => {
            setName(event.target.value);
            setNamed(true);
          }}
          className="h-7 font-mono text-xs"
        />
        {problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
      </label>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button
          isDisabled={problem !== null}
          onPress={() => {
            store.definePrim(parent, name, typeName);
            onClose();
          }}
        >
          Add {typeName ?? "prim"}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

const itemClass = ({ isSelected, isFocused }: { isSelected: boolean; isFocused: boolean }) =>
  cn(
    "flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none",
    isFocused && "bg-muted",
    isSelected && "bg-accent text-accent-foreground",
  );

function RenameDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const parent = path.slice(0, path.lastIndexOf("/")) || "/";
  const old = path.slice(path.lastIndexOf("/") + 1);
  const [name, setName] = useState(old);
  const taken = name !== old && stage.children(parent).some((child) => child.slice(child.lastIndexOf("/") + 1) === name);
  const problem = !NAME.test(name) ? "A prim name is a letter or _ then letters, digits or _." : taken ? `${parent} already has a child ${name}.` : null;
  const commit = () => {
    if (problem !== null || name === old) return;
    store.move(path, parent === "/" ? `/${name}` : `${parent}/${name}`);
    onClose();
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()}>
      <DialogHeader>
        <DialogTitle>Rename {old}</DialogTitle>
        <DialogDescription>Every opinion of the edit target's layer stack moves, and the paths that name it are repaired.</DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          commit();
        }}
        className="flex flex-col gap-1 text-xs"
      >
        <Input autoFocus value={name} onChange={(event) => setName(event.target.value)} className="h-7 font-mono text-xs" />
        {problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={problem !== null || name === old} onPress={commit}>
          Rename
        </Button>
      </DialogFooter>
    </Dialog>
  );
}
