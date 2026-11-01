// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the editor is doing while a scene opens: the phase (fetching the
// root, reading it, each round of the asset loop fetching and reading, then
// composing what the rounds read),
// what has arrived, how long it has taken, and a way to stop waiting. Each
// phase is shown before its work holds the page (the session waits for it
// to paint), so even a root layer that takes seconds to read says so.
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import type { OpenPhase } from "../model/session.ts";
import { useStore } from "./context.ts";
import { formatBytes } from "./format.ts";

/** The phase in words. */
function describe(phase: OpenPhase): string {
  switch (phase.kind) {
    case "fetchingRoot":
      return `Fetching the root layer ${phase.identifier}`;
    case "readingRoot":
      return `Reading the root layer (${formatBytes(phase.bytes)})`;
    case "fetching":
      return `Round ${phase.round}: fetching ${phase.assets} asset${phase.assets === 1 ? "" : "s"}`;
    case "reading":
      return `Round ${phase.round}: reading ${phase.layers} layer${phase.layers === 1 ? "" : "s"} (${formatBytes(phase.bytes)})`;
    case "composing":
      return `Composing the ${phase.layers} layer${phase.layers === 1 ? "" : "s"} the rounds read`;
  }
}

export function LoadingOverlay() {
  const store = useStore();
  const [, tick] = useState(0);
  const opening = store.status.kind === "opening";
  // The elapsed time moves on its own while the open waits on the network.
  useEffect(() => {
    if (!opening) return;
    const timer = setInterval(() => tick((n) => n + 1), 250);
    return () => clearInterval(timer);
  }, [opening]);
  if (store.status.kind !== "opening") return null;
  const label = store.status.label;
  const at = store.openingPhase;
  const fetched = store.opening?.fetched ?? null;
  const elapsed = at === null ? null : (performance.now() - at.started) / 1000;
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center bg-background/60 backdrop-blur-[2px]">
      <div role="status" aria-live="polite" className="flex w-96 max-w-[90%] flex-col gap-3 rounded-xl border bg-popover p-4 shadow-lg">
        <div className="flex items-center gap-2">
          <Spinner className="size-4 text-muted-foreground" />
          <span className="min-w-0 truncate text-sm font-medium" title={label}>
            Opening {label}
          </span>
        </div>
        <div className="text-xs">{at === null ? "Starting" : describe(at.phase)}…</div>
        <div className="h-1 overflow-hidden rounded-full bg-muted">
          <div className="h-full w-1/3 animate-[loading_1.2s_ease-in-out_infinite] rounded-full bg-primary/70" />
        </div>
        <div className="flex items-center gap-3 font-mono text-[11px] text-muted-foreground tabular-nums">
          {fetched !== null && (
            <span>
              {fetched.assets} assets · {formatBytes(fetched.bytes)}
            </span>
          )}
          {elapsed !== null && <span>{elapsed.toFixed(1)} s</span>}
          <Button variant="ghost" size="xs" className="ml-auto" onPress={() => store.cancelOpen()}>
            Cancel
          </Button>
        </div>
      </div>
    </div>
  );
}
