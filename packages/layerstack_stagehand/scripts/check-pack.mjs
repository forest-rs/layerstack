// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Refuses to pack without the built module, its license material and the
// compiled wrapper, as packages/layerstack's check-pack.mjs does.
import { statSync } from "node:fs";

const groups = [
  {
    command: "npm run build:wasm",
    files: [
      "wasm/layerstack_stagehand.js",
      "wasm/layerstack_stagehand.d.ts",
      "wasm/layerstack_stagehand_bg.wasm",
      ...[
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "LICENSE-UNICODE",
        "LICENSE-ZLIB",
        "LICENSE-compiler-builtins",
        "LICENSE-libm",
        "THIRD-PARTY-NOTICES",
      ].map((file) => `wasm/licenses/${file}`),
    ],
  },
  {
    command: "npm run build",
    files: ["dist/index.js", "dist/index.d.ts"],
  },
];

for (const { command, files } of groups) {
  const missing = files.filter((path) => {
    const file = statSync(new URL(`../${path}`, import.meta.url), { throwIfNoEntry: false });
    return !file?.isFile() || file.size === 0;
  });
  if (missing.length > 0) {
    console.error(`Cannot pack @forest-rs/layerstack_stagehand. Run ${command} to produce:`);
    for (const path of missing) console.error(`  ${path}`);
    process.exitCode = 1;
  }
}
