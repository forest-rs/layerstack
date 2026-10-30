# Layerstack scene editor (demo)

A private demo of [`@forest-rs/layerstack`](../layerstack): a scene editor
in plain DOM with no UI framework, so what it shows is composition, and a
3D viewport drawn only from the stage's reads. It opens the layered sample
in `public/scene/`: a root layer;
a sublayer with a class the rock inherits and a lamp whose `aim` names the
robot and whose `flicker` is sampled, then blocked at time 24; a robot
referenced from its own file, which binds it a material with a preview
surface shader reading its finish's `displayColor`, with a variant set whose `chrome` variant
references an asset fetched only when it is selected; two instances of
a beacon, which share one prototype; a ground mesh; a point instancer
of five stones; a top spun by time samples on its `xformOp:rotateY`; a
camera; and lights: the lamp (a sphere light, in the sublayer), a sun
(distant), a sky (dome), a spot (a disk light with `ShapingAPI`) and a
panel (rect); a sign bound to a checker texture (`textures/checker.png`,
turned and scaled by a `UsdTransform2d`, its coordinates' primvar and its
tint material interface inputs) and a strip bound to a UDIM set
(`textures/tile.<UDIM>.png`: tiles 1001 and 1002).

The viewport (`src/viewport.ts`, drawn by three.js in `src/viewport3d.ts`)
takes everything from layerstack, with no USD loader, through a
`DrawList` of `@forest-rs/layerstack_stagehand` (`../layerstack_stagehand`):
instanced draws of the composed tree (a gprim drawn once, each gprim of the
beacon prototype drawn at both instances, the stones' prototype at each
visible instance), left out when invisible or of a purpose not drawn
(default and proxy are, as in usdview), placed by the stage's world
transforms, over buffers stagehand built from typed arrays read through
layerstack (a mesh's topology, points, primvars and `materialBind`
subsets, or a shape's size attributes): indexed buffers with normals,
per-vertex `displayColor` where it varies, concave faces ear-clipped (the
path), and a three.js group per subset range (the ground). A draw binding a
material is drawn with it: the draw list's `UsdPreviewSurface` record
becomes a `MeshStandardMaterial` or `MeshPhysicalMaterial`
(`src/surfaces.ts` documents the mapping), its textures loaded through
`TextureLoader` from the bytes the asset loop's host fetches, a UDIM set's
tiles (found through `textures/listing.json`, as a static host cannot list
a directory) laid into one atlas. Other draws are colored by the
`displayColor` primvar (inherited from an ancestor or a class too);
`stage.worldBounds(["/"])`
frames the free camera. The view looks through the stage's camera
(`/World/Camera`: its view and projection matrices as `GfFrustum` computes
them, fitted to the canvas by `CameraUtilConformWindow`'s `fit` policy,
with its `ComputeLinearExposureScale` as the tone mapping exposure) until
"Free camera" is checked. The draw list's lights become three.js lights
in physical units (`PointLight`, `SpotLight`, `RectAreaLight`,
`DirectionalLight`, a `HemisphereLight` for the dome); `src/viewport3d.ts`
documents the conversion from `UsdLux` luminance.

After every action the draw list hears the editor's change records and
answers with a diff, which the view applies in place: an `InstancedMesh`
per group whose matrices are rewritten, geometries refilled or rebuilt
only when their buffers were, objects added, removed, shown or hidden as
listed. A new stage (opened, reopened) is read whole. Selecting a prim
highlights it. Drag to orbit, scroll to zoom. Under it, the same read
summarized (the prims drawn, the instances, the triangles, the framed
bounds) and what the last update did: draws added, removed, moved, shown
or hidden, buffers refilled or rebuilt, meshes triangulated and shapes
tessellated, draws read and left untouched (after a time change, the
draws nothing animated reaches, which it does not read), local transforms
recomputed, lights and cameras read and changed.
`scripts/acceptance.ts` checks both after every step without rendering a
pixel.

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

The script runs eighteen steps, one button each:

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
    and 12. The viewport reads at time 30 the top alone, the one animated
    draw, and moves it; no other draw, light or camera is read.
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
    stone 3, and frames the smaller bounds: its update moves four draws and
    hides one, and triangulates, tessellates and refills nothing; of the
    lights it reads the north beacon's glow alone, a light inside the
    instance, listed once per instance. The transform cache recomputes
    one local transform, the beacon's, and the world transforms below it,
    and reuses the rest: the rock's first `visibility` opinion is a new
    property spec, which resyncs only `/World/Rock.visibility`, as
    OpenUSD's `ObjectsChanged` does, and no transform reads it.
15. Brighten the lamp: its `inputs:intensity` in the edit layer, over the
    sublayer's. The draw list reads that light alone (no draw, no camera,
    no other light), and the viewport replaces its three.js light alone.
16. Warm the sign's checker: the checker material's `tint`, an interface
    input its texture's `scale` reads. The draw list reads that material
    alone (no draw, light or camera), and the viewport makes the sign's
    three.js material again, reusing the loaded image.
17. Document the stage: the root layer's `documentation`, stage metadata
    that composes nothing. The change record lists `/` info-only, and the
    draw list reads nothing: no draw, light, camera or material.
18. Read the checker through the sign's card set: the checker material's
    `coordinates` input names `card`, the sign's second texture coordinate
    set, instead of `st`. The draw list reads that material again and
    plans the sign's mesh again with `card` as a buffer channel of its own
    (rebuilt); the viewport gives it to three.js as `uv1` and samples the
    checker from it, and its checks turn square.

`scripts/acceptance.ts` runs the same steps headless in Node, reading the
sample from disk, and checks the values and explanations after each one,
then unmutes the robot asset as the layers panel's toggle does; CI runs it.

```sh
# Build the packages first (see ../layerstack/README.md):
(cd ../layerstack && npm ci && npm run build:wasm && npm run build)
(cd ../layerstack_stagehand && npm ci && npm run build:wasm && npm run build)
npm ci
npm run dev          # the editor at http://localhost:5173
npm run build        # type-check, then vite build into dist/
npm run acceptance   # the steps, headless
```
