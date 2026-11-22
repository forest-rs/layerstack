// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { statSync } from "node:fs";

const groups = [
  {
    command: "npm run build:wasm",
    files: [
      "wasm/layerstack.js",
      "wasm/layerstack.d.ts",
      "wasm/layerstack_bg.wasm",
      ...[
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "LICENSE-TOST-1.0",
        "LICENSE-UNICODE",
        "LICENSE-ZLIB",
        "LICENSE-compiler-builtins",
        "LICENSE-libm",
        "NOTICE",
        "THIRD-PARTY-NOTICES",
      ].map((file) => `wasm/licenses/${file}`),
    ],
  },
  {
    command: "npm run build",
    files: ["dist/index.js", "dist/index.d.ts", "dist/native.js", "dist/native.d.ts"],
  },
];

for (const { command, files } of groups) {
  const missing = files.filter((path) => {
    const file = statSync(new URL(`../${path}`, import.meta.url), { throwIfNoEntry: false });
    return !file?.isFile() || file.size === 0;
  });
  if (missing.length > 0) {
    console.error(`Cannot pack @forest-rs/layerstack. Run ${command} to produce:`);
    for (const path of missing) console.error(`  ${path}`);
    process.exitCode = 1;
  }
}
