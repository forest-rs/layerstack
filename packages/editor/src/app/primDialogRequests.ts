// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which prim dialog is open, asked for from anywhere: a "⋯" menu, the
// inspector, or a command on a prim path wherever one is shown (the tree's
// context menu, a relationship's target, a viewport pick's path). Commands
// run outside React, so they ask here, and the one `PrimDialogHost`
// (`PrimDialogs.tsx`, mounted by the app) shows what is asked. One dialog
// at a time: asking for another replaces it.
import { useSyncExternalStore } from "react";

/** A dialog about a prim, and the prim. */
export type PrimDialogRequest =
  | { readonly kind: "add"; readonly parent: string }
  | { readonly kind: "rename"; readonly path: string }
  | { readonly kind: "bind"; readonly path: string }
  | { readonly kind: "moveTo"; readonly path: string }
  | { readonly kind: "retype"; readonly path: string }
  | { readonly kind: "applySchema"; readonly path: string }
  /** A duplicate the edit target had no spec for: where the prim's opinions are, and "Edit there". */
  | { readonly kind: "copyRefused"; readonly path: string; readonly refusal: string };

let open: PrimDialogRequest | null = null;
const listeners = new Set<() => void>();

/** Opens `request`'s dialog (closing any other), or with `null` closes it. */
export function askPrim(request: PrimDialogRequest | null): void {
  open = request;
  for (const listener of listeners) listener();
}

/** The dialog asked for, for the host; renders again when it changes. */
export function usePrimDialog(): PrimDialogRequest | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => open,
  );
}
