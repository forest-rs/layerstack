// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the editor shows once a Wasm module has trapped (the store's `lost`
// status): why, and the way back. The module's instance is dead for the rest
// of the page, so nothing here calls into it; a reload starts a new one.
import { Button } from "@/components/ui/button";
import type { EditorStatus } from "../store.ts";

/** The address limit of a 32-bit Wasm module's memory. */
const WASM32_LIMIT = "4 GB";

export function ModuleLost({ status }: { status: Extract<EditorStatus, { kind: "lost" }> }) {
  const reload = (keepScene: boolean) => {
    const url = new URL(location.href);
    if (!keepScene) url.searchParams.delete("scene");
    location.assign(url);
  };
  const scene = status.label === null ? "The scene" : status.label;
  return (
    <div className="flex h-screen items-center justify-center bg-background p-4 text-foreground">
      <div className="flex max-w-lg flex-col gap-3 rounded-lg border bg-card p-6 text-sm">
        <h1 className="text-base font-semibold">{status.outOfMemory ? `${scene} needs more memory than this page has` : "The editor's module stopped"}</h1>
        {status.outOfMemory ? (
          <p className="text-muted-foreground">
            A page's WebAssembly module can address at most {WASM32_LIMIT}, and a browser may give it less. Reading the scene's layers and
            building its geometry ran past that, so the module stopped.
          </p>
        ) : (
          <p className="text-muted-foreground">The module hit a bug and stopped. Reloading starts it again.</p>
        )}
        <p className="font-mono text-xs break-words text-muted-foreground">{status.message}</p>
        <p className="text-muted-foreground">A stopped module can't be used again in this page, so the editor needs a reload.</p>
        <div className="flex gap-2 pt-1">
          <Button onPress={() => reload(false)}>Reload with the sample scene</Button>
          {!status.outOfMemory && (
            <Button variant="outline" onPress={() => reload(true)}>
              Reload this scene
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
