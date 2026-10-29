# Layerstack scene editor (demo)

A private demo of [`@forest-rs/layerstack`](../layerstack): a scene editor
in plain DOM, with no 3D and no UI framework, so what it shows is
composition. It opens the layered sample in `public/scene/`: a root layer;
a sublayer with a class the rock inherits and a lamp whose `aim` names the
robot and whose `flicker` is sampled, then blocked at time 24; a robot
referenced from its own file, with a variant set whose `chrome` variant
references an asset fetched only when it is selected; and two instances of
a beacon, which share one prototype.

Panels: the prim tree (instances and instance proxies badged), the
attribute inspector (the variant dropdowns, the time to read at, the
relationships and their targets, an edit field, and rename and delete), the
explanation (every opinion with its layer, arc and role, and the value's
source), the layers (dirty, edit layer, save and reopen), the undo stack,
the asset loop (want, fetch, supply, load), the change log, and the transfer
counters in the header.

The script runs eleven steps, one button each:

1. Open the sample: the stage wants its sublayer, the robot's reference and
   the beacon's (once, for both instances), and `resolve` fetches them.
2. Select the rock: its radius is inherited from `/Shape` in the sublayer.
3. Override the radius in the edit layer (`scene.usda`): the local opinion
   contributes, the inherited one is shadowed.
4. Switch the robot to `chrome`: the variant wants `assets/chrome.usda`,
   which the editor fetches, supplies and loads step by step.
5. Undo both, then redo the override.
6. Define `/World/Pebble` as a `Sphere` and set its `radius` in one edit:
   the attribute is typed by the schema of a prim the same edit defines.
7. Retype the pebble as a `Cube` and set its `size` in one edit: the new
   type's attribute; the radius it authored stays.
8. Rename the robot to `Rover` (`Stage.move`): the variant set and the
   specs inside its variants move with it, so the painted color now comes
   from `/World/Rover{finish=painted}`, and the lamp's `aim` in the
   sublayer is repaired, so one rename dirties two layers. It is one entry
   on the undo stack.
9. Read the lamp's `flicker` at time 30: the explanation names the time
   samples as the source, held from 24, where a block is authored, so there
   is no value. At time 6 it reads 0.5, between the samples at 0 and 12.
10. Explain `/World/Beacons/North/Bulb`, an instance proxy: its radius
    comes from `assets/beacon.usda` through North's reference.
11. Save: the two dirty layers are downloaded, and the root is opened again
    from the downloaded bytes, the rest fetched; every edit is there, and
    the reopened stage explains the instance proxy as the edited live
    stage did.

`scripts/acceptance.ts` runs the same steps headless in Node, reading the
sample from disk, and checks the values and explanations after each one; CI
runs it.

```sh
# Build the package first (see ../layerstack/README.md):
(cd ../layerstack && npm ci && npm run build:wasm && npm run build)
npm ci
npm run dev          # the editor at http://localhost:5173
npm run build        # type-check, then vite build into dist/
npm run acceptance   # the steps, headless
```
