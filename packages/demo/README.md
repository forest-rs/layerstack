# Layerstack scene editor (demo)

A private demo of [`@forest-rs/layerstack`](../layerstack): a scene editor
in plain DOM with no UI framework, so what it shows is composition, and a
3D viewport drawn only from the stage's reads. It opens the layered sample
in `public/scene/`: a root layer;
a sublayer with a class the rock inherits and a lamp whose `aim` names the
robot and whose `flicker` is sampled, then blocked at time 24; a robot
referenced from its own file, which binds it a material with a preview
surface shader, with a variant set whose `chrome` variant
references an asset fetched only when it is selected; two instances of
a beacon, which share one prototype; a ground mesh; and a point instancer
of five stones.

The viewport (`src/viewport.ts`, drawn by three.js in `src/viewport3d.ts`)
takes everything from layerstack, with no USD loader: the drawable prims
(meshes, spheres, cubes and point instancers) from the composed tree, one
batched `stage.visibilities` and `stage.purposeInfos` read to leave out
what is invisible or of a purpose not drawn (default and proxy are, as in
usdview), one batched `stage.worldTransforms` read for every placement, a
mesh's `points` and face indices (fanned into triangles) or a sphere's
radius and a cube's size, the `displayColor` primvar (inherited from an
ancestor or a class too) or else the bound material's surface
`diffuseColor`, `stage.pointInstances` for the stones, each matrix times the
instancer's world transform, and `stage.worldBounds(["/"])` to frame the
camera. It is read again after every action, so the edits, undos, the
variant switch, the rename and the muted asset show in it; selecting a prim
highlights it. Drag to orbit, scroll to zoom. Under it, the same read
summarized: the prims drawn, the instances, the triangles and the framed
bounds, which `scripts/acceptance.ts` checks after every step without
rendering a pixel.

Panels: the prim tree (instances, instance proxies and composition errors
badged), the
attribute inspector (the variant dropdowns, the time to read at, the
relationships and their targets, the bound material with the binding that
decides it and its surface shader's inputs, an edit field, and rename and
delete), the
explanation (every opinion with its layer, arc and role, the value's source,
and an "Edit here" button that points the edit field at where the opinion
lives, through the arc that reaches it, and the prim's composition errors),
the layers (dirty, edit layer, a mute toggle, save and reopen), the undo
stack, the asset loop (want, fetch, supply, load), the change log (with the
layers each change muted or unmuted), and the transfer counters in the
header. The layers panel shows which layers are muted from the changes the
stage reports (`Changes.mutedIdentifiers` and `unmutedIdentifiers`), not by
asking the stage again.

The script runs fourteen steps, one button each:

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
8. Edit the robot asset through its reference: the robot's `height` comes
   from `assets/robot.usda`, and that opinion's `editTarget` writes
   `/Robot.height` in the asset (`stage.apply(edits, opinion.editTarget)`),
   with the stage path `/World/Robot.height`. The asset is dirty, not
   `scene.usda`, and every prim referencing it reads 1.5.
9. Rename the robot to `Rover` (`Stage.move`): the variant set and the
   specs inside its variants move with it, so the painted color now comes
   from `/World/Rover{finish=painted}`, and the lamp's `aim` in the
   sublayer is repaired, so one rename edits two layers. It is one entry
   on the undo stack, which shows the path the rename leaves naming
   `/World/Robot` (`EditReceipt.unrepaired`): the unselected chrome
   finish's `mirrors`, which OpenUSD does not repair either. The
   reference's edit target from step 8 is stale now (`target.stale`): no
   arc of `/World/Robot` maps it.
10. Read the lamp's `flicker` at time 30: the explanation names the time
    samples as the source, held from 24, where a block is authored, so
    there is no value. At time 6 it reads 0.5, between the samples at 0
    and 12.
11. Explain `/World/Beacons/North/Bulb`, an instance proxy: its radius
    comes from `assets/beacon.usda` through North's reference.
12. Save: the three dirty layers are downloaded, and the root is opened again
    from the downloaded bytes, the rest fetched; every edit is there (the
    asset's height included), and
    the reopened stage explains the instance proxy as the edited live
    stage did.
13. Mute the robot asset (`Stage.muteLayer`): the rover's color keeps only
    the painted variant's opinion, its height and its `Body` and `Head`
    are gone, and the rover reports its reference to the muted asset
    (`composition.muted_asset`), in the explanation and as a tree badge.
    The stage still holds the layer. Its Unmute in the layers panel
    brings every opinion back in one change and fetches nothing. Muting
    is not an edit: the undo stack is empty.
14. Move a beacon, hide the rock and a stone, in one edit: the north
    beacon's `xformOp:translate`, the rock's `visibility` and the stones'
    `invisibleIds`. The viewport moves the beacon, leaves out the rock and
    stone 3, and frames the smaller bounds. The transform cache recomputes
    two local transforms, the beacon's and the rock's (whose first
    `visibility` opinion is a new spec, which resyncs it), and the world
    transforms below them, and reuses the rest.

`scripts/acceptance.ts` runs the same steps headless in Node, reading the
sample from disk, and checks the values and explanations after each one,
then unmutes the robot asset as the layers panel's toggle does; CI runs it.

```sh
# Build the package first (see ../layerstack/README.md):
(cd ../layerstack && npm ci && npm run build:wasm && npm run build)
npm ci
npm run dev          # the editor at http://localhost:5173
npm run build        # type-check, then vite build into dist/
npm run acceptance   # the steps, headless
```
