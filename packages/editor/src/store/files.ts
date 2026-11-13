// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What leaves the page, as the store keeps it: which layer the Text tab
// shows and the texts read of each layer (so the tab can say what the
// last edit changed), and where the next scene's edits go. Saving and
// exporting run on the store's session (`../store.ts`); the bytes are the
// app's to download or write (`../app/FileMenu.tsx`).
import type { Layer } from "@forest-rs/layerstack";
import { LayerTexts } from "../model/layerText.ts";
import type { Session } from "../model/session.ts";
import type { Signals } from "./signals.ts";

/**
 * Where the next scene's edits go: its root layer, or a session layer in
 * front of it (`OpenOptions.editLayer: "session"`), which the root never
 * holds and only an export writes. A root that cannot take edits (a USDZ
 * package) is edited in a session layer whatever this says.
 */
export type EditLayerChoice = "root" | "session";

/** The layer the Text tab shows, and the texts read; bumps `view` as the layer shown changes. */
export class FileState {
  /** The texts read, per stage and layer, for what changed between reads. */
  readonly texts = new LayerTexts();
  /** The layer the Text tab shows, with the session it is of. */
  #shown: { readonly session: Session; readonly layer: Layer } | null = null;
  readonly #signals: Signals;

  constructor(signals: Signals) {
    this.#signals = signals;
  }

  /** The layer the Text tab shows in `session`: the one chosen, while the stage still holds it, else its edit layer. */
  shown(session: Session | null): Layer | null {
    if (session === null) return null;
    const chosen = this.#shown?.session === session ? this.#shown.layer : null;
    if (chosen !== null && session.stage.layers().includes(chosen)) return chosen;
    return session.stage.editLayer ?? session.stage.root;
  }

  /** Shows `layer` of `session` in the Text tab. */
  show(session: Session, layer: Layer): void {
    this.#shown = { session, layer };
    this.#signals.bump("view");
  }
}
