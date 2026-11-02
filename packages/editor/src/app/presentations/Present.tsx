// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Presenting objects in React (`../../presentations/registry.ts` chooses
// the view): `Present` shows an object in a one-of view (`inline`,
// `tooltip`, or `accept` when given a callback that authors), `Live`
// makes what it wraps a live presentation of an object (a click runs its
// first command, a context menu offers them all), and `usePrimRevision`
// is what a panel keys its reads by, so it reads its prim again only when
// a change reaches it.
import { useRef, useState } from "react";
import { Menu, MenuItem, Popover } from "react-aria-components";
import type { ValueInput } from "@forest-rs/layerstack";
import { cn } from "@/lib/utils";
import type { Session } from "../../model/session.ts";
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
  const offered = open ? commands.forObject(object, context) : [];
  return (
    <>
      <button
        ref={ref}
        type="button"
        title={title}
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
      <Popover
        triggerRef={ref}
        isOpen={open && offered.length > 0}
        onOpenChange={setOpen}
        placement="bottom start"
        className="z-50 min-w-40 rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10 outline-none"
      >
        <Menu
          aria-label="Commands"
          className="outline-hidden"
          onAction={(key) => {
            offered.find((command) => command.id === key)?.run(object, context);
            setOpen(false);
          }}
        >
          {offered.map((command) => (
            <MenuItem key={command.id} id={command.id} className="cursor-default rounded-md px-2 py-1 text-xs outline-hidden data-focused:bg-foreground/10">
              {command.title}
            </MenuItem>
          ))}
        </Menu>
      </Popover>
    </>
  );
}

/** Whether `changes`' paths reach `path`: it, a prim above it resynced, or a prim or property beneath it changed. */
function reaches(path: string, paths: readonly string[], below: boolean): boolean {
  for (const changed of paths) {
    if (changed === path || changed === "/") return true;
    if (below && (changed.startsWith(`${path}/`) || changed.startsWith(`${path}.`))) return true;
    if (path.startsWith(changed) && (path[changed.length] === "/" || path[changed.length] === ".")) return true;
  }
  return false;
}

/** What a session's change log has been read to, and the revision of each prim asked about. */
interface Revisions {
  read: number;
  readonly prims: Map<string, number>;
}

const revisionsBySession = new WeakMap<Session, Revisions>();

/**
 * A number that changes when, and only when, a recomposition reaches the
 * prim at `path` (it or its subtree resynced, created or removed; it or
 * a prim or property beneath it changed): what a panel keys its reads of
 * the prim by (`useMemo`), so an edit elsewhere reads nothing again. The
 * change log is read once per session for all prims asked about.
 */
export function usePrimRevision(store: EditorStore, path: string): number {
  const session = store.session;
  if (session === null) return 0;
  let revisions = revisionsBySession.get(session);
  if (revisions === undefined) {
    revisions = { read: session.changeLog.length, prims: new Map() };
    revisionsBySession.set(session, revisions);
  }
  const log = session.changeLog;
  for (let at = revisions.read; at < log.length; at++) {
    const { changes } = log[at]!;
    for (const [prim, revision] of revisions.prims) {
      const touched =
        reaches(prim, changes.resynced, true) ||
        reaches(prim, changes.created, true) ||
        reaches(prim, changes.removed, true) ||
        reaches(prim, changes.changedInfoOnly, true) ||
        reaches(prim, changes.resyncedProperties, true);
      if (touched) revisions.prims.set(prim, revision + 1);
    }
  }
  revisions.read = log.length;
  if (!revisions.prims.has(path)) revisions.prims.set(path, 0);
  return revisions.prims.get(path)!;
}
