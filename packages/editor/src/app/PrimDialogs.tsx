// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The dialogs that author a prim, shown by the one host the app mounts
// for whatever asked (`primDialogRequests.ts`): add a child (of a type
// and specifier), rename, bind a material, move under another prim,
// retype, apply an API schema, and, when a duplicate found nothing to
// copy where edits go, where the prim's opinions are. Each says what it
// authors and in which layer, and each edit is one undoable action
// through the store; a refusal is said in the dialog and kept as a
// notice.
import { useEffect, useMemo, useState } from "react";
import { Header, ListBox, ListBoxItem, ListBoxSection } from "react-aria-components";
import type { BindingStrength, EditTarget, Layer, SchemaInfo, Stage } from "@forest-rs/layerstack";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogSubmit, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import {
  API_SCHEMA_NOTES,
  applicableSchemas,
  childPath,
  holdsField,
  instanceNameProblem,
  copySites,
  nameOf,
  parentOf,
  primNameProblem,
  type PrimSpecifier,
  SPECIFIER_MEANINGS,
} from "../model/primAuthoring.ts";
import { arcNoun } from "../model/composition.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { layerName } from "./format.ts";
import { askPrim, usePrimDialog } from "./primDialogRequests.ts";
import { editsGoTo, RefusalNote, SpecifierPicker } from "./primAuthoringParts.tsx";
import { PrimPicker } from "./PrimPicker.tsx";
import { chosenUnderFilter, typeCount, typeItemClass, TypeList } from "./TypeList.tsx";

/** The dialog asked for, over the open stage; nothing when none is, or no stage is open. */
export function PrimDialogHost() {
  const store = useEditorStore();
  // A new stage, or a change that takes the prim away (an undo), closes what is open on it.
  useRevision(store, "status", "composition");
  const request = usePrimDialog();
  const stage = store.session?.stage;
  const path = request === null ? null : request.kind === "add" ? request.parent : request.path;
  const gone = stage === undefined || (path !== null && path !== "/" && stage.prim(path) === null);
  useEffect(() => {
    if (request !== null && gone) askPrim(null);
  }, [request, gone]);
  if (request === null || path === null || stage === undefined || gone) return null;
  const close = () => askPrim(null);
  switch (request.kind) {
    case "add":
      return <AddPrimDialog key={path} store={store} stage={stage} parent={path} onClose={close} />;
    case "rename":
      return <RenameDialog key={path} store={store} stage={stage} path={path} onClose={close} />;
    case "bind":
      return <BindMaterialDialog key={path} store={store} stage={stage} path={path} onClose={close} />;
    case "moveTo":
      return <MoveToDialog key={path} store={store} stage={stage} path={path} onClose={close} />;
    case "retype":
      return <RetypeDialog key={path} store={store} stage={stage} path={path} onClose={close} />;
    case "applySchema":
      return <ApplySchemaDialog key={path} store={store} stage={stage} path={path} onClose={close} />;
    case "copyRefused":
      return <CopyRefusedDialog key={path} store={store} stage={stage} path={path} refusal={request.refusal} onClose={close} />;
  }
}

/**
 * Duplicates the prim at `path`, and when the edit target has no spec of
 * it, says where its opinions are. Any other refusal (a target that does
 * not map the copy, or writes inside a variant) is its notice alone: the
 * dialog is for a spec that is missing.
 */
export function duplicatePrim(store: EditorStore, path: string): void {
  const refusal = store.duplicate(path);
  const notice = store.notices.at(-1);
  if (refusal !== undefined && notice?.message === refusal && notice.code === "edit.rejected.no_such_spec") askPrim({ kind: "copyRefused", path, refusal });
}

/** Defines a child of `parent`: a type (or none), a specifier, a name not taken. */
function AddPrimDialog({ store, stage, parent, onClose }: { store: EditorStore; stage: Stage; parent: string; onClose: () => void }) {
  const [typeName, setTypeName] = useState<string | null>("Xform");
  const [specifier, setSpecifier] = useState<PrimSpecifier>("def");
  const [filter, setFilter] = useState("");
  const taken = useMemo(() => new Set(stage.children(parent).map(nameOf)), [stage, parent]);
  const [name, setName] = useState(() => freeName(taken, "Xform"));
  const [named, setNamed] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const problem = primNameProblem(name, parent, taken);
  const choose = (next: string | null) => {
    setTypeName(next);
    if (!named) setName(freeName(taken, next ?? "Prim"));
  };
  const add = () => {
    if (problem !== null) return;
    const refused = store.definePrim(parent, name, typeName, specifier);
    if (refused === undefined) onClose();
    else setRefusal(refused);
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Add a child of {parent}</DialogTitle>
        <DialogDescription>
          Any concrete type the stage registers, {typeCount(stage)} of them, authored in {editsGoTo(store)}.
        </DialogDescription>
      </DialogHeader>
      {/* A form, so Enter in either field adds, as in the rename dialog. */}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
        className="contents"
      >
        <DialogSubmit />
        <Input
          autoFocus
          placeholder="Find a type"
          aria-label="Find a type"
          value={filter}
          onChange={(event) => {
            setFilter(event.target.value);
            // The type chosen stays one the list shows: Enter adds what the button names.
            const next = chosenUnderFilter(stage, event.target.value, typeName);
            if (next !== typeName) choose(next);
          }}
          className="h-7 text-xs"
        />
        <TypeList stage={stage} filter={filter} typeName={typeName} onChoose={choose} />
        <div className="grid grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-2">
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
          </label>
          <div className="flex flex-col gap-1 text-xs">
            <span>Specifier</span>
            <SpecifierPicker value={specifier} onChange={setSpecifier} />
          </div>
        </div>
        {specifier !== "def" && <p className="text-[11px] text-muted-foreground">{SPECIFIER_MEANINGS[specifier]}</p>}
        {problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
        <RefusalNote refusal={refusal} className="text-[11px] text-destructive" />
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={problem !== null} onPress={add}>
          Add {specifier === "def" ? "" : `${specifier} `}
          {typeName ?? "prim"}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/** `base`, else `base1`, `base2`, ...: the first not in `taken`. */
function freeName(taken: ReadonlySet<string>, base: string): string {
  if (!taken.has(base)) return base;
  for (let index = 1; ; index++) if (!taken.has(`${base}${index}`)) return `${base}${index}`;
}

/** Renames the prim at `path` (`Stage.move`, which repairs the paths that name it). */
function RenameDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const parent = parentOf(path);
  const old = nameOf(path);
  const [name, setName] = useState(old);
  const taken = useMemo(() => new Set(stage.children(parent).map(nameOf).filter((each) => each !== old)), [stage, parent, old]);
  const problem = primNameProblem(name, parent, taken);
  const [refusal, setRefusal] = useState<string | null>(null);
  const commit = () => {
    if (problem !== null || name === old) return;
    const refused = store.move(path, childPath(parent, name));
    if (refused === undefined) onClose();
    else setRefusal(refused);
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
        <DialogSubmit />
        {/* The name is selected, so typing replaces it. */}
        <Input
          autoFocus
          aria-label="Name"
          value={name}
          onFocus={(event) => event.target.select()}
          onChange={(event) => setName(event.target.value)}
          className="h-7 font-mono text-xs"
        />
        {problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
        <RefusalNote refusal={refusal} className="text-[11px] text-destructive" />
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

const ALL_PURPOSES = "all";

/**
 * Binds a material to the prim at `path`: the prim picker narrowed to
 * `Material`, the materials the scene's draws bind first, with the
 * binding's purpose and strength (what `BindMaterialEdit` takes).
 */
function BindMaterialDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const [purpose, setPurpose] = useState(ALL_PURPOSES);
  const [strength, setStrength] = useState<BindingStrength>("weakerThanDescendants");
  // The materials the draws bind: what the scene already uses, at no cost of a search.
  const bound = useMemo(() => [...store.scene.materials.keys()].sort(), [store]);
  return (
    <PrimPicker
      stage={stage}
      title={`Bind a material to ${nameOf(path)}`}
      description={
        <>
          Authors <span className="font-mono">material:binding</span> on {path}, targeting the material, and applies{" "}
          <span className="font-mono">MaterialBindingAPI</span>, in {editsGoTo(store)}. Its descendants inherit it unless they bind their own.
        </>
      }
      type="Material"
      first={bound}
      firstNote="bound in the scene"
      footer={
        <div className="grid grid-cols-2 gap-2 text-xs">
          <div className="flex flex-col gap-1">
            <span>Purpose</span>
            <Select aria-label="Material purpose" value={purpose} onChange={(key) => key !== null && setPurpose(String(key))}>
              <SelectTrigger size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem id={ALL_PURPOSES}>All purposes</SelectItem>
                <SelectItem id="preview">preview</SelectItem>
                <SelectItem id="full">full</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-col gap-1">
            <span>Strength</span>
            <Select aria-label="Binding strength" value={strength} onChange={(key) => key !== null && setStrength(key as BindingStrength)}>
              <SelectTrigger size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem id="weakerThanDescendants" textValue="weaker than descendants">
                  weaker than descendants
                </SelectItem>
                <SelectItem id="strongerThanDescendants" textValue="stronger than descendants">
                  stronger than descendants
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
      }
      onPick={(material) =>
        store.bindMaterial(path, material, {
          ...(purpose === ALL_PURPOSES ? {} : { purpose }),
          ...(strength === "strongerThanDescendants" ? { strength } : {}),
        })
      }
      onClose={onClose}
    />
  );
}

/** Moves the prim at `path` under another (`Stage.move`): any prim but itself, its subtree and its parent; or to the root. */
function MoveToDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const parent = parentOf(path);
  return (
    <PrimPicker
      stage={stage}
      title={`Move ${nameOf(path)} to…`}
      description={
        <span className="flex flex-col gap-1.5">
          <span>
            It keeps its name. Every opinion of {editsGoTo(store)}'s layer stack moves with it, and the paths that name it are repaired.
          </span>
          {parent !== "/" && (
            <Button
              variant="outline"
              size="sm"
              className="self-start"
              onPress={() => {
                // Refused (the root has a prim of its name): the notice says why, and the dialog stays for another choice.
                if (store.moveTo(path, "/") === undefined) onClose();
              }}
            >
              Move it to the root
            </Button>
          )}
        </span>
      }
      exclude={[parent]}
      hide={(each) => each === path || each.startsWith(`${path}/`)}
      onPick={(to) => store.moveTo(path, to)}
      onClose={onClose}
    />
  );
}

/** Retypes the prim at `path` (`UsdPrim::SetTypeName`), from the grouped types, or clears the target's type. */
function RetypeDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const prim = stage.prim(path);
  const [typeName, setTypeName] = useState<string | null>(prim?.typeName ?? null);
  const [filter, setFilter] = useState("");
  const [refusal, setRefusal] = useState<string | null>(null);
  const where = editsGoTo(store);
  // Clearing takes away only the type the spec edits go to holds.
  const clears = holdsField(store.targetSpec(path), "typeName");
  const unchanged = typeName === (prim?.typeName ?? null);
  const nothingToClear = typeName === null && !clears;
  const commit = () => {
    if (unchanged || nothingToClear) return;
    const refused = store.setTypeName(path, typeName);
    if (refused === undefined) onClose();
    else setRefusal(refused);
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Retype {nameOf(path)}</DialogTitle>
        <DialogDescription>
          Authors <span className="font-mono">typeName</span> on its spec in {where}. What layers author stays; what only its old type defined (fallbacks, built-in
          schemas) goes.
        </DialogDescription>
      </DialogHeader>
      <Input
        autoFocus
        placeholder="Find a type"
        aria-label="Find a type"
        value={filter}
        onChange={(event) => {
          setFilter(event.target.value);
          setTypeName(chosenUnderFilter(stage, event.target.value, typeName));
        }}
        className="h-7 text-xs"
      />
      <TypeList
        stage={stage}
        filter={filter}
        typeName={typeName}
        onChoose={setTypeName}
        typelessLabel={clears ? `clears ${where}'s type name` : `${where} authors no type name to clear`}
      />
      <RefusalNote refusal={refusal} className="text-[11px] text-destructive" />
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={unchanged || nothingToClear} onPress={commit}>
          {typeName === null ? `Clear its type in ${where}` : `Make it a ${typeName}`}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/**
 * Applies an API schema to the prim at `path` (`UsdPrim::ApplyAPI`): those
 * that can apply to it, grouped, each with what it is for; a
 * multiple-apply one asks for its instance name.
 */
function ApplySchemaDialog({ store, stage, path, onClose }: { store: EditorStore; stage: Stage; path: string; onClose: () => void }) {
  const prim = stage.prim(path)!;
  const applied = useMemo(() => prim.appliedSchemas(), [prim]);
  const groups = useMemo(() => applicableSchemas(stage.schemas(), prim, applied), [stage, prim, applied]);
  const [filter, setFilter] = useState("");
  const [chosen, setChosen] = useState<SchemaInfo | null>(null);
  const [instance, setInstance] = useState("");
  const [refusal, setRefusal] = useState<string | null>(null);
  const needle = filter.trim().toLowerCase();
  const shown = groups.map(([group, schemas]) => [group, schemas.filter((schema) => schema.name.toLowerCase().includes(needle))] as const).filter(([, schemas]) => schemas.length > 0);
  const multiple = chosen?.kind === "multipleApplyApi";
  const problem = chosen === null ? null : multiple ? instanceNameProblem(chosen, instance, applied) : null;
  const apply = () => {
    if (chosen === null || problem !== null) return;
    const refused = store.applySchema(path, chosen.name, multiple ? instance : null);
    if (refused === undefined) onClose();
    else setRefusal(refused);
  };
  const choose = (name: string) => {
    const schema = groups.flatMap(([, schemas]) => schemas).find((each) => each.name === name) ?? null;
    setChosen(schema);
    setRefusal(null);
    if (schema !== null && schema.allowedInstanceNames.length > 0) setInstance(schema.allowedInstanceNames.find((each) => !applied.includes(`${schema.name}:${each}`)) ?? "");
  };
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Apply a schema to {nameOf(path)}</DialogTitle>
        <DialogDescription>
          Adds it to <span className="font-mono">apiSchemas</span> in {editsGoTo(store)}: the prim then has the schema's properties, at their fallbacks until authored.
          Listed are those that can apply to a {prim.typeName ?? "typeless prim"}.
        </DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          apply();
        }}
        className="contents"
      >
        <DialogSubmit />
        <Input autoFocus placeholder="Find a schema" aria-label="Find a schema" value={filter} onChange={(event) => setFilter(event.target.value)} className="h-7 text-xs" />
        <ListBox
          aria-label="API schemas"
          selectionMode="single"
          selectedKeys={chosen === null ? [] : [chosen.name]}
          onSelectionChange={(keys) => {
            const [key] = keys === "all" ? [] : [...keys];
            if (key !== undefined) choose(String(key));
          }}
          renderEmptyState={() => <p className="px-2 py-1 text-xs text-muted-foreground">{needle === "" ? "No API schema the stage registers can apply here." : "No schema is named like that."}</p>}
          className="max-h-64 min-h-16 overflow-y-auto rounded-md border p-1 outline-none"
        >
          {shown.map(([group, schemas]) => (
            <ListBoxSection key={group}>
              <Header className="px-2 pt-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">{group}</Header>
              {schemas.map((schema) => (
                <ListBoxItem key={schema.name} id={schema.name} textValue={schema.name} className={typeItemClass}>
                  <span className="flex min-w-0 flex-col">
                    <span className="flex items-center gap-1.5">
                      {schema.name}
                      {schema.kind === "multipleApplyApi" && <span className="text-[10px] text-muted-foreground">per instance</span>}
                    </span>
                    {API_SCHEMA_NOTES[schema.name] !== undefined && <span className="text-[10px] whitespace-normal text-muted-foreground">{API_SCHEMA_NOTES[schema.name]}</span>}
                    {schema.canOnlyApplyTo.length > 0 && <span className="text-[10px] text-muted-foreground">Only to {schema.canOnlyApplyTo.join(", ")}.</span>}
                  </span>
                </ListBoxItem>
              ))}
            </ListBoxSection>
          ))}
        </ListBox>
        {multiple && chosen !== null && (
          <label className="flex flex-col gap-1 text-xs">
            Instance name
            <Input value={instance} onChange={(event) => setInstance(event.target.value)} placeholder="parts" className="h-7 font-mono text-xs" />
            {chosen.allowedInstanceNames.length > 0 && (
              <span className="flex flex-wrap gap-1">
                {chosen.allowedInstanceNames.map((each) => (
                  <Button key={each} variant="outline" size="xs" isDisabled={applied.includes(`${chosen.name}:${each}`)} onPress={() => setInstance(each)}>
                    {each}
                  </Button>
                ))}
              </span>
            )}
            {problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
          </label>
        )}
        <RefusalNote refusal={refusal} className="text-[11px] text-destructive" />
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={chosen === null || problem !== null} onPress={apply}>
          {chosen === null ? "Choose a schema" : `Apply ${chosen.name}${multiple && instance !== "" ? `:${instance}` : ""}`}
        </Button>
      </DialogFooter>
    </Dialog>
  );
}

/** A layer or arc target, made where edits go as the toolbar's picker lists it (a layer of the stage's own stack as that layer). */
function targetOf(target: EditTarget): Layer | EditTarget {
  return target.kind === "layer" ? target.layer : target;
}

/**
 * A duplicate that found no spec of the prim where edits go: the refusal,
 * and each layer (or arc) where the prim's opinions are, with "Edit there"
 * and "Duplicate there".
 */
function CopyRefusedDialog({ store, stage, path, refusal, onClose }: { store: EditorStore; stage: Stage; path: string; refusal: string; onClose: () => void }) {
  // Read through the store's spec reader, as the composition and schema sections read: each spec once per change of the stage.
  const sites = useMemo(() => copySites(stage, path, (layer, site) => store.authoredSpec(layer, site) !== null), [store, stage, path]);
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Nothing to copy in {editsGoTo(store)}</DialogTitle>
        <DialogDescription>{refusal}</DialogDescription>
      </DialogHeader>
      {sites.length === 0 ? (
        <p className="text-xs text-muted-foreground">None of the layers with its opinions can take edits here.</p>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {sites.map((site, index) => (
            <li key={index} className="flex items-center gap-2 text-xs">
              <span className="min-w-0 flex-1 truncate font-mono" title={site.layer.identifier ?? undefined}>
                {layerName(site.layer.identifier)}
              </span>
              {site.arc !== null && <Badge variant="outline">through a {arcNoun(site.arc)}</Badge>}
              <Button
                variant="outline"
                size="xs"
                onPress={() => {
                  store.setTarget(targetOf(site.editTarget));
                  onClose();
                }}
              >
                Edit there
              </Button>
              {/* Only where the copy can be written: an arc on the prim itself maps nothing beside it, and no copy is made inside a variant. */}
              {site.copies && (
                <Button
                  size="xs"
                  onPress={() => {
                    store.setTarget(targetOf(site.editTarget));
                    onClose();
                    duplicatePrim(store, path);
                  }}
                >
                  Duplicate there
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      {sites.length > 0 && <p className="text-[11px] text-muted-foreground">Either makes that layer where edits go, until another is chosen.</p>}
      {sites.some((site) => site.arc !== null && site.copies) && (
        <p className="text-[11px] text-muted-foreground">A copy through an arc is made in that asset: every prim that composes it gets the copy.</p>
      )}
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Close
        </Button>
      </DialogFooter>
    </Dialog>
  );
}
