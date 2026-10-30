// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { defineConfig } from "vite";

export default defineConfig({
  // Relative asset URLs: the built page works from any directory.
  base: "./",
  // A missing asset must be a 404 the asset loop sees as "not found", not the page.
  appType: "mpa",
  build: {
    // The page awaits the module at the top level.
    target: "es2022",
    // three.js loads in a chunk of its own, after the editor, at 560 kB.
    chunkSizeWarningLimit: 600,
  },
  // The packages load their modules with `new URL(..., import.meta.url)`;
  // pre-bundling them would move them away from their module files.
  optimizeDeps: { exclude: ["@forest-rs/layerstack", "@forest-rs/layerstack_stagehand"] },
  // The packages are linked from ../layerstack and ../layerstack_stagehand.
  server: { fs: { allow: [".."] } },
});
