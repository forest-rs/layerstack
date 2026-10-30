// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The demo's script over the sample scene. The page runs it one
// step at a time; `scripts/acceptance.ts` runs it headless and checks the
// stage after each step.
import type { EditTarget, Layerstack } from "@forest-rs/layerstack";
import { Editor } from "./editor.ts";
import type { Fetcher, LayerFile } from "./editor.ts";

/** The sample scene's names the script uses (`public/scene/`). */
export const sample = {
  /** The root layer; it sublayers `library.usda`. */
  root: "scene.usda",
  /** A sphere that inherits its radius from the class `/Shape` in the sublayer. */
  rock: "/World/Rock",
  radius: "/World/Rock.radius",
  /** The radius the script authors over the inherited one. */
  override: 3,
  /** A robot referenced from `assets/robot.usda`, with the variant set `finish`. */
  robot: "/World/Robot",
  color: "/World/Robot.color",
  /** The robot's height, authored only in its asset, and the height the script writes there through the reference. */
  height: "/World/Robot.height",
  assetHeight: 1.5,
  /** The robot's own file, which the reference reads. */
  robotAsset: "assets/robot.usda",
  set: "finish",
  /** The variant whose reference (`assets/chrome.usda`) is not fetched until it is selected. */
  variant: "chrome",
  /** The asset the variant needs. */
  variantAsset: "assets/chrome.usda",
  /** A prim the script defines as a `Sphere` and sets the schema's radius of, in one edit. */
  pebble: "/World/Pebble",
  /** Where the robot is renamed to, with its variant set, and the relationship in the sublayer that names it. */
  rover: "/World/Rover",
  roverColor: "/World/Rover.color",
  aim: "/World/Lamp.aim",
  /** A lamp attribute sampled in the sublayer, blocked from time 24 on. */
  lamp: "/World/Lamp",
  flicker: "/World/Lamp.flicker",
  /** A time after the block: the blocked sample is held. */
  blockedTime: 30,
  /** One of two instances of `assets/beacon.usda`, and a prim inside it: an instance proxy. */
  instance: "/World/Beacons/North",
  proxy: "/World/Beacons/North/Bulb",
  proxyRadius: "/World/Beacons/North/Bulb.radius",
  /** Five stones a point instancer places, and the id the script hides. */
  stones: "/World/Stones",
  hiddenStone: 3n,
  /** Where the script moves the north beacon. */
  beaconMoved: [-5, 0, -1],
  /** The lamp's intensity, a `UsdLux` input, and what the script brightens it to. */
  lampIntensity: "/World/Lamp.inputs:intensity",
  brighter: 1500,
  /** A cube spun by time samples on its `xformOp:rotateY`: the sample's one animated draw. */
  top: "/World/Top",
  /** The camera the viewport looks through, until "Free camera" is checked. */
  camera: "/World/Camera",
  /** A sign drawn with a checker texture, its material, and the material's tint: an interface input that scales the texture. */
  sign: "/World/Sign",
  checker: "/World/Looks/Checker",
  tint: "/World/Looks/Checker.inputs:tint",
  /** The tint the script sets. */
  warmer: [1, 0.85, 0.6, 1],
  /** A strip drawn with UDIM tiles (`tile.<UDIM>.png`: 1001 and 1002), and its material. */
  strip: "/World/Strip",
  tiles: "/World/Looks/Tiles",
  /** The stage's `documentation`, which the last step sets. */
  documentation: "A rock, a robot, two beacons and their lights",
} as const;

/** What the script needs of its host: the instance, the fetch, and a way to hand saved layers to the user. */
export interface Host {
  readonly layerstack: Layerstack;
  readonly fetch: Fetcher;
  /**
   * Hands the saved layers to the user (a download in a browser, files on
   * disk in Node) and returns them as they come back, for the reopen.
   */
  download(files: readonly LayerFile[]): Promise<readonly LayerFile[]>;
  /** Called after each step of the asset loop; the page renders. */
  readonly progress?: () => void;
}

/** The editor the script works on and what is selected in it. */
export class Session {
  /** The current editor; `null` before the first step. */
  editor: Editor | null = null;
  /** The selected prim's path. */
  prim: string | null = null;
  /** The selected attribute's property path. */
  attribute: string | null = null;
  /** The time code the inspector and the explanation read at; `null` reads the defaults. */
  time: number | null = null;
  /**
   * Where the edit field writes: an opinion's edit target the user picked
   * in the explanation, or `null` for the stage's edit layer.
   */
  target: EditTarget | null = null;

  /** The current editor; throws before the first step. */
  get current(): Editor {
    if (this.editor === null) throw new Error("no stage is open: run the first step");
    return this.editor;
  }

  /** Makes `editor` current, freeing the one before. */
  replace(editor: Editor): void {
    this.editor?.free();
    this.editor = editor;
  }

  /**
   * Selects a prim, and one of its attributes or none, read at `time` (the
   * defaults by default). Selecting another attribute sends edits to the
   * edit layer again.
   */
  select(prim: string | null, attribute: string | null = null, time: number | null = null): void {
    if (attribute !== this.attribute) this.target = null;
    this.prim = prim;
    this.attribute = attribute;
    this.time = time;
  }
}

/** One step of the script. */
export interface Step {
  /** What it does, in a few words. */
  readonly title: string;
  /** What to look at once it ran. */
  readonly look: string;
  run(session: Session, host: Host): Promise<void>;
}

/** The steps, in order. */
export const steps: readonly Step[] = [
  {
    title: "Open the layered sample",
    look: "The asset loop: the root wants its sublayer, the robot's reference and the beacon both instances reference; all are fetched in one round.",
    async run(session, host) {
      session.replace(await Editor.open(host.layerstack, sample.root, host.fetch, { progress: host.progress }));
      session.select(null);
    },
  },
  {
    title: "Select the rock: an inherited radius",
    look: "The explanation: the radius comes from the class /Shape in library.usda, through an inherits arc.",
    async run(session) {
      session.select(sample.rock, sample.radius);
    },
  },
  {
    title: "Override the radius in the edit layer",
    look: "The explanation: the local opinion in scene.usda contributes; the inherited one is shadowed.",
    async run(session) {
      session.current.setValue(sample.radius, { type: "double", value: sample.override });
      session.select(sample.rock, sample.radius);
    },
  },
  {
    title: "Switch the robot to chrome",
    look: "The asset loop: the variant references assets/chrome.usda, which is wanted, fetched, supplied and read; the color now comes from it.",
    async run(session) {
      session.select(sample.robot, sample.color);
      await session.current.selectVariant(sample.robot, sample.set, sample.variant);
    },
  },
  {
    title: "Undo both, then redo the override",
    look: "The undo stack: the variant is painted again and the radius inherited, then the override is back for the save.",
    async run(session) {
      const editor = session.current;
      await editor.undo();
      await editor.undo();
      await editor.redo();
      session.select(sample.rock, sample.radius);
    },
  },
  {
    title: "Define a sphere and set its radius in one edit",
    look: "The undo stack: one action defines /World/Pebble as a Sphere and sets radius, which the Sphere schema types as a double, though the prim did not exist before the edit.",
    async run(session) {
      session.current.apply(`define ${sample.pebble} as a Sphere with radius 0.5`, [
        { type: "definePrim", path: sample.pebble, typeName: "Sphere" },
        { type: "setValue", path: `${sample.pebble}.radius`, value: { type: "double", value: 0.5 } },
      ]);
      session.select(sample.pebble, `${sample.pebble}.radius`);
    },
  },
  {
    title: "Retype the pebble as a cube and set its size",
    look: "The inspector: the pebble is a Cube now, and size, a Cube schema attribute, was set in the same edit that retyped it. The radius it authored stays, outside the new schema.",
    async run(session) {
      session.current.apply(`retype ${sample.pebble} as a Cube with size 0.8`, [
        { type: "definePrim", path: sample.pebble, typeName: "Cube" },
        { type: "setValue", path: `${sample.pebble}.size`, value: { type: "double", value: 0.8 } },
      ]);
      session.select(sample.pebble, `${sample.pebble}.size`);
    },
  },
  {
    title: "Edit the robot asset through its reference",
    look: "The explanation: the height comes from assets/robot.usda through the reference, and that opinion's edit target writes /Robot.height in that file. The height is 1.5 now, written in the asset, not in scene.usda: assets/robot.usda is dirty, and any other prim referencing the asset reads it too.",
    async run(session) {
      const editor = session.current;
      session.select(sample.robot, sample.height);
      const opinion = editor.stage.attribute(sample.height)?.explain()?.opinions[0];
      if (opinion?.editTarget == null) throw new Error(`${sample.height} has no opinion to edit through`);
      session.target = opinion.editTarget;
      editor.setValueThrough(opinion.editTarget, sample.height, { type: "double", value: sample.assetHeight });
    },
  },
  {
    title: "Rename the robot to Rover",
    look: "The explanation: the painted color now comes from /World/Rover{finish=painted}; the variant set moved with the prim. The lamp's aim, in library.usda, targets /World/Rover: one rename edited two layers. The edit target of the robot's reference is stale now: no arc of /World/Robot maps it, and a rename through it is refused. The undo list: the rename leaves one path naming /World/Robot, the chrome finish's mirrors, which no prim selects, as OpenUSD leaves it; selecting chrome would target a robot that is gone.",
    async run(session) {
      session.current.move(sample.robot, sample.rover);
      session.select(sample.rover, sample.roverColor);
    },
  },
  {
    title: "Read the lamp's flicker after its block",
    look: "The explanation at time 30: the source is the time samples, held from 24, where a block is authored, so there is no value. Set the time to 6 to read 0.5, between the samples at 0 and 12.",
    async run(session) {
      session.select(sample.lamp, sample.flicker, sample.blockedTime);
    },
  },
  {
    title: "Explain a prim inside an instance",
    look: "The tree: North and South are instances of one beacon, and Bulb is an instance proxy. Its radius comes from assets/beacon.usda through North's reference, and the reopened stage explains it the same way.",
    async run(session) {
      session.select(sample.proxy, sample.proxyRadius);
    },
  },
  {
    title: "Save, download, reopen",
    look: "The layers: scene.usda, library.usda and assets/robot.usda were dirty. The reopened stage reads them from the downloaded bytes and fetches the rest; the override, the pebble, the asset's height and the rename are there.",
    async run(session, host) {
      const editor = session.current;
      editor.save();
      const downloaded = await host.download(editor.saved);
      session.replace(await editor.reopen(downloaded));
      session.select(sample.rock, sample.radius);
    },
  },
  {
    title: "Mute the robot asset",
    look: "The explanation: the rover's color has one opinion left, the painted variant's; the asset's is gone, and the rover reports its reference to the muted asset (composition.muted_asset). The tree: Body and Head are gone, and the rover has no height. The layers: assets/robot.usda is muted, still held. Press its Unmute: one change brings every opinion back, and nothing is fetched.",
    async run(session) {
      const editor = session.current;
      session.select(sample.rover, sample.roverColor);
      await editor.setMuted(editor.layerNamed(sample.robotAsset), true);
    },
  },
  {
    title: "Move a beacon, hide the rock and a stone",
    look: "The viewport: one edit moves the north beacon (its xformOp:translate), makes the rock invisible and hides stone 3 (the instancer's invisibleIds). The transform cache recomputes one local transform, the beacon's, and reuses the rest: the rock's first visibility opinion resyncs only that property, which no transform reads; the framed bounds shrink without the rock. The viewport's stats line: the draw list moves four draws (the beacon's post and bulb, the rock, the stones) and hides one, triangulating, tessellating and refilling nothing; it reads one light, the north beacon's glow, a light inside the instance listed once per instance, and not the south's. Undo brings them back.",
    async run(session) {
      session.current.apply("move the north beacon, hide the rock and stone 3", [
        { type: "setValue", path: `${sample.instance}.xformOp:translate`, value: { type: "double3", value: [...sample.beaconMoved] } },
        { type: "setValue", path: `${sample.rock}.visibility`, value: { type: "token", value: "invisible" } },
        { type: "setValue", path: `${sample.stones}.invisibleIds`, value: { type: "int64[]", value: [sample.hiddenStone] } },
      ]);
      session.select(sample.instance);
    },
  },
  {
    title: "Brighten the lamp",
    look: "The viewport: the lamp, a UsdLux sphere light drawn as a three.js point light, brightens. The stats line: the edit reads that light alone, and no draw, camera or other light; its three.js light alone is replaced. The edit layer holds the new intensity over the sublayer's.",
    async run(session) {
      session.current.setValue(sample.lampIntensity, { type: "float", value: sample.brighter });
      session.select(sample.lamp, sample.lampIntensity);
    },
  },
  {
    title: "Warm the sign's checker",
    look: "The viewport: the sign's checker texture warms. Its material's tint, a material interface input, connects to the texture's scale, which three.js applies as the map's color. The stats line: the edit reads that material alone, and no draw, light or camera; the sign's three.js material alone is made again, and the checker image is not loaded again.",
    async run(session) {
      session.current.setValue(sample.tint, { type: "float4", value: [...sample.warmer] });
      session.select(sample.checker, sample.tint);
    },
  },
  {
    title: "Document the stage",
    look: "The change log: the root layer's documentation, stage metadata that composes nothing, changes / info-only. The stats line: the draw list reads nothing, no draw, light, camera or material.",
    async run(session) {
      session.current.apply("document the stage", [
        { type: "setLayerMetadata", key: "documentation", value: { type: "string", value: sample.documentation } },
      ]);
    },
  },
];
