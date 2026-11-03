// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Presenting objects in React (`../../presentations/registry.ts` chooses
// the view): `Present` shows an object in a one-of view (`inline`,
// `tooltip`, or `accept` when given a callback that authors), `Live`
// makes what it wraps a live presentation of an object (a click runs its
// first command, a context menu offers them all, a hover shows its
// `tooltip` presentation when it has one), and `usePrimRevision`
// is what a panel keys its reads by, so it reads its prim again only when
// a change reaches it.
import { useRef, useState } from "react";
import { Menu, MenuItem, Popover } from "react-aria-components";
import type { ValueInput } from "@forest-rs/layerstack";
import { Tooltip, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { primRevision } from "../../model/revisions.ts";
import { commands, type PresentationContext, type Presented, type PresentedKind, presentations, type PresentationView } from "../../presentations/registry.ts";
import type { EditorStore } from "../../store.ts";

/** The context presentations are given, for `store` as it is now; `null` with no stage. */
export function presentationContext(store: EditorStore): PresentationContext | null {
  const stage = store.session?.stage;
  return stage === undefined ? null : { store, stage, time: store.time };
}

/**
 * `object` in `view`: the registration of its most specific type that has
 * the view. With `accept`, its `accept` view when one applies (an editor
 * authoring through `accept`), else its `inline` one.
 */
export function Present<K extends PresentedKind>({
  object,
  context,
  view = "inline",
  accept,
}: {
  object: Presented<K>;
  context: PresentationContext;
  view?: Exclude<PresentationView, "panel" | "accept">;
  accept?: (value: ValueInput) => void;
}) {
  if (accept !== undefined) {
    const editor = presentations.find(object, "accept", context);
    if (editor?.accept !== undefined) {
      const Accept = editor.accept;
      return <Accept object={object} context={context} accept={accept} />;
    }
  }
  const found = presentations.find(object, view, context);
  const View = found?.[view];
  if (View === undefined) return <span className="truncate font-mono text-[11px] text-muted-foreground">{fallbackText(object as Presented)}</span>;
  return <View object={object} context={context} />;
}

/** What an object with no presentation shows. */
function fallbackText(object: Presented): string {
  switch (object.kind) {
    case "prim":
      return object.prim.path;
    case "value":
      return object.shown.text;
    case "primPath":
    case "propertyPath":
      return object.path;
    case "layer":
      return object.layer.identifier ?? "anonymous layer";
    case "instance":
      return `${object.instancer} #${object.index}`;
  }
}

/**
 * `children` as a live presentation of `object`: a click (or Enter) runs
 * the first command that applies to it, a context menu (or Shift+F10)
 * lists them all. Commands are read when asked for, not as it renders.
 */
export function Live<K extends PresentedKind>({
  object,
  context,
  children,
  className,
  title,
}: {
  object: Presented<K>;
  context: PresentationContext;
  children: React.ReactNode;
  className?: string;
  title?: string;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  // A tooltip presentation, when its type has one, in place of the title: read only while shown.
  const Tip = presentations.find(object, "tooltip", context)?.tooltip;
  const button = (
    <button
      ref={ref}
      type="button"
      title={Tip === undefined ? title : undefined}
      className={cn("min-w-0 truncate text-left font-mono text-[11px] text-sky-600 hover:underline dark:text-sky-400", className)}
      onClick={() => commands.forObject(object, context)[0]?.run(object, context)}
      onContextMenu={(event) => {
        event.preventDefault();
        setOpen(true);
      }}
      onKeyDown={(event) => {
        if (event.key === "F10" && event.shiftKey) {
          event.preventDefault();
          setOpen(true);
        }
      }}
    >
      {children}
    </button>
  );
  return (
    <>
      {Tip === undefined ? (
        button
      ) : (
        <TooltipTrigger delay={500}>
          {button}
          <Tooltip placement="left" className="max-w-xs">
            <Tip object={object} context={context} />
          </Tooltip>
        </TooltipTrigger>
      )}
      {open && <CommandMenu object={object} context={context} triggerRef={ref} onClose={() => setOpen(false)} />}
    </>
  );
}

/**
 * The commands that apply to `object`, as a menu at `triggerRef` (what a
 * context menu on a presented object opens): choosing one runs it, and
 * either closes the menu (`onClose`). Read when it opens.
 */
export function CommandMenu<K extends PresentedKind>({
  object,
  context,
  triggerRef,
  onClose,
}: {
  object: Presented<K>;
  context: PresentationContext;
  triggerRef: React.RefObject<Element | null>;
  onClose: () => void;
}) {
  const [offered] = useState(() => commands.forObject(object, context));
  if (offered.length === 0) return null;
  return (
    <Popover
      triggerRef={triggerRef}
      isOpen
      onOpenChange={(open) => !open && onClose()}
      placement="bottom start"
      className="z-50 min-w-40 rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10 outline-none"
    >
      <Menu
        aria-label="Commands"
        autoFocus="first"
        className="outline-hidden"
        onAction={(key) => {
          offered.find((command) => command.id === key)?.run(object, context);
          onClose();
        }}
      >
        {offered.map((command) => (
          <MenuItem key={command.id} id={command.id} className="cursor-default rounded-md px-2 py-1 text-xs outline-hidden data-focused:bg-foreground/10">
            {command.title}
          </MenuItem>
        ))}
      </Menu>
    </Popover>
  );
}

/**
 * A number that changes when, and only when, a recomposition reaches the
 * prim at `path` ({@link primRevision}): what a panel keys its reads of
 * the prim by (`useMemo`), so an edit elsewhere reads nothing of it again.
 */
export function usePrimRevision(store: EditorStore, path: string): number {
  return store.session === null ? 0 : primRevision(store.session, path);
}
