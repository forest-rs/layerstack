// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the editor shows in a browser that cannot draw with WebGPU: the
// viewport and the material previews draw only with it (`viewport/gpu.ts`),
// so rather than an editor with an empty viewport, why, and where it works.
// Browser support as MDN's compatibility data lists it (late 2026).
import type { WebGpuMissing } from "../viewport/support.ts";

export function NoWebGPU({ missing }: { missing: Exclude<WebGpuMissing, null> }) {
  return (
    <div className="flex h-screen items-center justify-center bg-background p-4 text-foreground">
      <div className="flex max-w-lg flex-col gap-3 rounded-lg border bg-card p-6 text-sm">
        <h1 className="text-base font-semibold">The editor needs WebGPU</h1>
        {missing === "api" ? (
          <p className="text-muted-foreground">
            This browser does not offer WebGPU to this page. The editor draws its viewport and material previews with WebGPU only; there is
            no WebGL fallback.
          </p>
        ) : (
          <p className="text-muted-foreground">
            This browser has WebGPU but offered no graphics adapter: hardware acceleration may be turned off, or this GPU or driver may be
            blocked. The editor draws its viewport and material previews with WebGPU only; there is no WebGL fallback.
          </p>
        )}
        <div className="text-muted-foreground">
          <p>Browsers with WebGPU:</p>
          <ul className="mt-1 ml-4 list-disc space-y-0.5">
            <li>Chrome and Edge 113 or later on Windows, macOS and ChromeOS; 144 or later on Linux (Intel Gen12+ GPUs only); 121 or later on Android</li>
            <li>Safari 26 or later, on macOS, iOS and iPadOS</li>
            <li>Firefox 141 or later on Windows, and 147 or later on Apple silicon Macs (not yet on Linux or Android)</li>
          </ul>
        </div>
        <p className="text-muted-foreground">WebGPU is offered only to secure pages: HTTPS, or localhost.</p>
      </div>
    </div>
  );
}
