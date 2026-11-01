// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { execFileSync } from "node:child_process";
import { type Dirent, createReadStream, existsSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

const LAYER = /\.(usd|usda|usdc|usdz)$/i;

/**
 * The local scene directory: `LAYERSTACK_SCENES`, else the main checkout's
 * `.local/scenes` (found from any worktree through git's common directory).
 * Scenes there are never committed or built; the dev server serves them.
 */
function scenesDirectory(): string | null {
  const configured = process.env.LAYERSTACK_SCENES;
  if (configured !== undefined) return path.resolve(configured);
  try {
    const common = execFileSync("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { encoding: "utf8" }).trim();
    const directory = path.join(path.dirname(common), ".local", "scenes");
    return existsSync(directory) ? directory : null;
  } catch {
    return null;
  }
}

/**
 * What `entry` of `directory` is, following a symbolic link (a scene kept on
 * another volume and linked in): `null` for a broken link or anything else.
 */
function entryKind(directory: string, entry: Dirent): "file" | "directory" | null {
  if (entry.isFile()) return "file";
  if (entry.isDirectory()) return "directory";
  if (!entry.isSymbolicLink()) return null;
  try {
    const target = statSync(path.join(directory, entry.name));
    return target.isFile() ? "file" : target.isDirectory() ? "directory" : null;
  } catch {
    return null;
  }
}

/** The layers two levels under each of `root`'s directories: the roots a scene there likely opens. */
function sceneRoots(root: string): { path: string }[] {
  const found: { path: string }[] = [];
  const walk = (directory: string, depth: number) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (entry.name.startsWith(".")) continue;
      const full = path.join(directory, entry.name);
      const kind = entryKind(directory, entry);
      if (kind === "file" && LAYER.test(entry.name) && depth < 3) found.push({ path: path.relative(root, full).split(path.sep).join("/") });
      else if (kind === "directory" && depth < 2) walk(full, depth + 1);
    }
  };
  walk(root, 0);
  return found.sort((a, b) => (a.path < b.path ? -1 : 1));
}

/**
 * Serves the local scene directory under `/@scenes/` in development, with
 * `index.json` listing its likely roots and `list.json?dir=` one folder's
 * folders and layers.
 */
function localScenes(): Plugin {
  return {
    name: "layerstack-local-scenes",
    apply: "serve",
    configureServer(server) {
      const root = scenesDirectory();
      server.middlewares.use("/@scenes/", (request, response) => {
        const url = new URL(request.url ?? "/", "http://localhost");
        const relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
        if (root === null) {
          response.statusCode = 404;
          response.end();
          return;
        }
        if (relative === "index.json") {
          response.setHeader("content-type", "application/json");
          response.end(JSON.stringify(sceneRoots(root)));
          return;
        }
        if (relative === "list.json") {
          // One directory: its folders and its layers, for browsing.
          const directory = path.resolve(root, url.searchParams.get("dir") ?? "");
          if ((directory !== root && !directory.startsWith(root + path.sep)) || !existsSync(directory) || !statSync(directory).isDirectory()) {
            response.statusCode = 404;
            response.end();
            return;
          }
          const entries = readdirSync(directory, { withFileTypes: true }).filter((entry) => !entry.name.startsWith("."));
          response.setHeader("content-type", "application/json");
          response.end(
            JSON.stringify({
              folders: entries.filter((entry) => entryKind(directory, entry) === "directory").map((entry) => entry.name).sort(),
              layers: entries.filter((entry) => entryKind(directory, entry) === "file" && LAYER.test(entry.name)).map((entry) => entry.name).sort(),
            }),
          );
          return;
        }
        const file = path.resolve(root, relative);
        if (!file.startsWith(root + path.sep) || !existsSync(file) || !statSync(file).isFile()) {
          response.statusCode = 404;
          response.end();
          return;
        }
        response.setHeader("content-length", statSync(file).size);
        response.setHeader("content-type", "application/octet-stream");
        if (request.method === "HEAD") {
          response.end();
          return;
        }
        createReadStream(file).pipe(response);
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), tailwindcss(), localScenes()],
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
  // Relative asset URLs: the built page works from any directory.
  base: "./",
  // A missing asset must be a 404 the asset loop sees as "not found", not the page.
  appType: "mpa",
  // The page awaits the modules at the top level.
  build: { target: "es2022" },
  // The packages load their modules with `new URL(..., import.meta.url)`;
  // pre-bundling them would move them away from their module files.
  optimizeDeps: { exclude: ["@forest-rs/layerstack", "@forest-rs/layerstack_stagehand"] },
  // The packages are linked from ../layerstack and ../layerstack_stagehand.
  server: { fs: { allow: [".."] } },
});
