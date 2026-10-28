# Layerstack scene editor (demo)

A private demo of [`@forest-rs/layerstack`](../layerstack): a scene editor
in plain DOM, with no 3D and no UI framework, so what it shows is
composition. It opens the layered sample in `public/scene/` (a root layer, a
sublayer with a class the rock inherits, a robot referenced from its own
file, and a variant set whose `chrome` variant references an asset fetched
only when it is selected).

Panels: the prim tree, the attribute inspector (with the variant dropdowns
and an edit field), the explanation (every opinion with its layer, arc and
role), the layers (dirty, edit layer, save and reopen), the undo stack, the
asset loop (want, fetch, supply, load), the change log, and the transfer
counters in the header.

The script runs six steps, one button each:

1. Open the sample: the stage wants its sublayer and the reference, and
   `resolve` fetches them.
2. Select the rock: its radius is inherited from `/Shape` in the sublayer.
3. Override the radius in the edit layer (`scene.usda`): the local opinion
   contributes, the inherited one is shadowed.
4. Switch the robot to `chrome`: the variant wants `assets/chrome.usda`,
   which the editor fetches, supplies and loads step by step.
5. Undo both, then redo the override.
6. Save: the dirty layer is downloaded, and the root is opened again from
   the downloaded bytes, the rest fetched.

`scripts/acceptance.ts` runs the same steps headless in Node, reading the
sample from disk, and checks the values and explanations after each one; CI
runs it.

```sh
# Build the package first (see ../layerstack/README.md):
(cd ../layerstack && npm ci && npm run build:wasm && npm run build)
npm ci
npm run dev          # the editor at http://localhost:5173
npm run build        # type-check, then vite build into dist/
npm run acceptance   # the six steps, headless
```
