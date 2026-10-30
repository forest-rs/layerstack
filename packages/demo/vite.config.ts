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
  // The package loads its module with `new URL(..., import.meta.url)`;
  // pre-bundling it would move it away from its module file.
  optimizeDeps: { exclude: ["@forest-rs/layerstack"] },
  // The package is linked from ../layerstack.
  server: { fs: { allow: [".."] } },
});
