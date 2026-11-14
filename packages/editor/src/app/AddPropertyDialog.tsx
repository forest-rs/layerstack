// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Adding a property to a prim, where edits go: an attribute (a name in
// namespaces, a value type from those the runtime registers, uniform or
// varying), created with a starting value so it is shown ready to edit,
// or a relationship with the targets chosen from the stage's prims. The
// properties the prim's schemas define and no layer authors yet are
// offered by name, with their schema's type and fallback. The dialog says
// what will be authored, in USDA, and where; a refusal is said in it.
import { useMemo, useState } from "react";
import { Header, ListBox, ListBoxItem, ListBoxSection, ToggleButton, ToggleButtonGroup } from "react-aria-components";
import type { Prim } from "@forest-rs/layerstack";
import { HugeiconsIcon } from "@hugeicons/react";
import { PlusSignIcon } from "@hugeicons/core-free-icons";
import { Button, buttonVariants } from "@/components/ui/button";
import { Dialog, DialogDescription, DialogFooter, DialogHeader, DialogSubmit, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { initialValue, propertyNameProblem, schemaStart, usdaValueText, valueTypeGroups, withoutTarget, withTarget } from "../model/propertyOps.ts";
import type { EditorStore } from "../store.ts";
import { PrimPicker } from "./PrimPicker.tsx";
import { TargetPath } from "./RelationshipTargets.tsx";
import { targetText } from "./TargetPicker.tsx";

type Kind = "attribute" | "relationship";

/** The most schema-defined names offered at once. */
const PICKS = 12;

/** The dialog that adds a property to `prim`, opened from the inspector's Properties section. */
export function AddPropertyDialog({ store, prim, onClose }: { store: EditorStore; prim: Prim; onClose: () => void }) {
  const stage = prim.stage;
  const [kind, setKind] = useState<Kind>("attribute");
  const [name, setName] = useState("");
  const [scalar, setScalar] = useState("float");
  const [array, setArray] = useState(false);
  const [uniform, setUniform] = useState(false);
  const [filter, setFilter] = useState("");
  const [targets, setTargets] = useState<readonly string[]>([]);
  const [picking, setPicking] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  // What the prim has: authored attributes and relationships with an opinion are taken; what only its schemas define is offered.
  const { taken, definedAttributes, definedRelationships } = useMemo(() => {
    const attributes = prim
      .attributeNames()
      .map((each) => prim.attribute(each))
      .filter((each) => each !== null);
    const relationships = prim
      .relationshipNames()
      .map((each) => prim.relationship(each))
      .filter((each) => each !== null);
    const usedRelationship = (each: (typeof relationships)[number]) => each.custom || each.targets().length > 0;
    return {
      taken: {
        attribute: new Set([...attributes.filter((each) => each.authored).map((each) => each.name), ...relationships.map((each) => each.name)]),
        relationship: new Set([...attributes.map((each) => each.name), ...relationships.filter(usedRelationship).map((each) => each.name)]),
      },
      definedAttributes: new Map(attributes.filter((each) => !each.authored).map((each) => [each.name, each])),
      definedRelationships: relationships.filter((each) => !usedRelationship(each)).map((each) => each.name),
    };
  }, [prim]);
  // The runtime's registry, the same for every stage.
  const valueTypes = useMemo(() => valueTypeGroups(store.layerstack.valueTypes()), [store]);
  const scalarType = valueTypes.flatMap(({ types }) => types).find((type) => type.name === scalar);
  const defined = kind === "attribute" ? definedAttributes.get(name) : undefined;
  const typeName = defined?.typeName ?? (array ? (scalarType?.arrayType ?? `${scalar}[]`) : scalar);
  const variability = defined?.variability ?? (uniform ? "uniform" : "varying");
  // A schema's attribute starts at its fallback, read once for the name chosen.
  const start = useMemo(() => (defined === undefined ? null : schemaStart(defined)), [defined]);
  const value = start === null ? initialValue(typeName) : start.value;
  const problem = propertyNameProblem(name, taken[kind]);
  const picks = (kind === "attribute" ? [...definedAttributes.keys()] : definedRelationships).filter((each) => each !== name && each.toLowerCase().includes(name.toLowerCase())).sort();
  const groups = valueTypes
    .map(({ group, types }) => ({ group, types: types.map((type) => type.name).filter((type) => type.toLowerCase().includes(filter.toLowerCase())) }))
    .filter(({ types }) => types.length > 0);
  const session = store.session;
  const destination = session?.target ?? stage.editLayer;
  const where = destination === null ? "the edit layer" : targetText(destination);
  const path = `${prim.path}.${name}`;
  const add = () => {
    if (problem !== null) return;
    const why = kind === "attribute" ? store.createAttribute(path, typeName, variability, value ?? undefined) : store.setTargets(path, targets);
    if (why === undefined) onClose();
    else setRefused(why);
  };
  const usda =
    kind === "attribute"
      ? `${defined === undefined ? "custom " : ""}${variability === "uniform" ? "uniform " : ""}${typeName} ${name || "name"}${value === null ? "" : ` = ${usdaValueText(value)}`}`
      : `${definedRelationships.includes(name) ? "" : "custom "}rel ${name || "name"} = ${targets.length === 0 ? "None" : targets.length === 1 ? `<${targets[0]}>` : `[${targets.map((target) => `<${target}>`).join(", ")}]`}`;
  const option = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "sm" });
  const smallOption = ({ isSelected }: { isSelected: boolean }) => buttonVariants({ variant: isSelected ? "secondary" : "ghost", size: "xs" });
  return (
    <Dialog isOpen onOpenChange={(isOpen) => isOpen || onClose()} className="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>Add a property to {prim.name || "/"}</DialogTitle>
        <DialogDescription>Authored in {where}, where edits go, as one undoable edit.</DialogDescription>
      </DialogHeader>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          add();
        }}
        className="flex flex-col gap-2"
      >
        <DialogSubmit />
        <ToggleButtonGroup
          aria-label="Kind of property"
          selectionMode="single"
          disallowEmptySelection
          selectedKeys={[kind]}
          onSelectionChange={(keys) => {
            setKind([...keys][0] === "relationship" ? "relationship" : "attribute");
            setRefused(null);
          }}
          className="flex gap-0.5"
        >
          <ToggleButton id="attribute" className={option}>
            Attribute
          </ToggleButton>
          <ToggleButton id="relationship" className={option}>
            Relationship
          </ToggleButton>
        </ToggleButtonGroup>
        <label className="flex flex-col gap-1 text-xs">
          Name
          <Input
            autoFocus
            value={name}
            placeholder={kind === "attribute" ? "userProperties:weight" : "lookAt"}
            onChange={(event) => {
              setName(event.target.value);
              setRefused(null);
            }}
            className="h-7 font-mono text-xs"
          />
          {name !== "" && problem !== null && <span className="text-[11px] text-destructive">{problem}</span>}
        </label>
        {picks.length > 0 && (
          <div className="flex flex-col gap-1">
            <span className="text-[11px] text-muted-foreground">Its schemas define{picks.length > PICKS ? ` (${picks.length}; type to narrow)` : ""}, and no layer authors:</span>
            <div className="flex flex-wrap gap-1">
              {picks.slice(0, PICKS).map((each) => (
                <Button key={each} variant="outline" size="xs" className="font-mono" onPress={() => setName(each)}>
                  {each}
                </Button>
              ))}
            </div>
          </div>
        )}
        {kind === "attribute" && defined !== undefined && (
          <p className="text-[11px] text-muted-foreground">
            Its schema defines <span className="font-mono text-foreground">{name}</span> as {variability === "uniform" ? "uniform " : ""}
            <span className="font-mono text-foreground">{typeName}</span>
            {start?.fallback === true
              ? value === null
                ? ": adding it declares it here with no value, so its fallback still shows."
                : ": adding it authors its fallback, to edit."
              : ", with no fallback: adding it authors the value below, to edit."}
          </p>
        )}
        {kind === "attribute" && defined === undefined && (
          <>
            <span className="text-xs">Value type</span>
            <Input placeholder="Find a value type" aria-label="Find a value type" value={filter} onChange={(event) => setFilter(event.target.value)} className="h-7 text-xs" />
            <ListBox
              aria-label="Value type"
              selectionMode="single"
              disallowEmptySelection
              selectedKeys={[scalar]}
              onSelectionChange={(keys) => {
                const [key] = keys === "all" ? [] : [...keys];
                if (key !== undefined) setScalar(String(key));
              }}
              renderEmptyState={() => <p className="px-2 py-1 text-xs text-muted-foreground">No value type is named like that.</p>}
              className="max-h-48 overflow-y-auto rounded-md border p-1 outline-none"
            >
              {groups.map(({ group, types }) => (
                <ListBoxSection key={group}>
                  <Header className="px-2 pt-2 pb-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">{group}</Header>
                  {types.map((type) => (
                    <ListBoxItem key={type} id={type} textValue={type} className={itemClass}>
                      <span className="font-mono">{type}</span>
                    </ListBoxItem>
                  ))}
                </ListBoxSection>
              ))}
            </ListBox>
            <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1.5 text-xs">
              <span>Array</span>
              <span className="flex items-center gap-2">
                <Switch size="sm" aria-label="Array" isSelected={array} onChange={setArray} />
                <span className="text-[11px] text-muted-foreground">{array ? `${typeName}: any number of ${scalar} values` : `one ${scalar}`}</span>
              </span>
              <span>Variability</span>
              <span className="flex items-center gap-2">
                <ToggleButtonGroup
                  aria-label="Variability"
                  selectionMode="single"
                  disallowEmptySelection
                  selectedKeys={[uniform ? "uniform" : "varying"]}
                  onSelectionChange={(keys) => setUniform([...keys][0] === "uniform")}
                  className="flex gap-0.5"
                >
                  <ToggleButton id="varying" className={smallOption}>
                    varying
                  </ToggleButton>
                  <ToggleButton id="uniform" className={smallOption}>
                    uniform
                  </ToggleButton>
                </ToggleButtonGroup>
                <span className="text-[11px] text-muted-foreground">{uniform ? "one value, never time samples" : "may be animated by time samples"}</span>
              </span>
            </div>
          </>
        )}
        {kind === "relationship" && (
          <div className="flex flex-col gap-1 text-xs">
            Targets
            {targets.length === 0 && <span className="text-[11px] text-muted-foreground">None yet: a relationship may have none.</span>}
            {targets.map((target) => (
              <div key={target} className="flex min-w-0 items-center gap-1">
                <span className="flex min-w-0 flex-1">
                  <TargetPath store={store} path={target} />
                </span>
                <Button variant="ghost" size="xs" onPress={() => setTargets(withoutTarget(targets, target))}>
                  Remove
                </Button>
              </div>
            ))}
            <Button variant="outline" size="sm" className="self-start" onPress={() => setPicking(true)}>
              <HugeiconsIcon icon={PlusSignIcon} data-icon="inline-start" />
              Add a target
            </Button>
          </div>
        )}
        <pre className="overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-[11px] text-muted-foreground" aria-label="What is authored, in USDA">
          {usda}
        </pre>
        {refused !== null && (
          <span role="alert" className="text-[11px] text-destructive">
            {refused}
          </span>
        )}
      </form>
      <DialogFooter>
        <Button variant="outline" onPress={onClose}>
          Cancel
        </Button>
        <Button isDisabled={problem !== null} onPress={add}>
          Add {kind}
        </Button>
      </DialogFooter>
      {picking && (
        <PrimPicker
          stage={stage}
          title={`Add a target to ${name || "the relationship"}`}
          description="A prim, or one of its properties."
          properties
          exclude={targets}
          onPick={(picked) => setTargets(withTarget(targets, picked))}
          onClose={() => setPicking(false)}
        />
      )}
    </Dialog>
  );
}

const itemClass = ({ isSelected, isFocused }: { isSelected: boolean; isFocused: boolean }) =>
  cn("flex cursor-default items-center gap-2 rounded px-2 py-1 text-xs outline-none", isFocused && "bg-muted", isSelected && "bg-accent text-accent-foreground");
