// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The demo's six-step script over the sample scene. The page runs it one
// step at a time; `scripts/acceptance.ts` runs it headless and checks the
// stage after each step.
import type { Layerstack } from "@forest-rs/layerstack";
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
  set: "finish",
  /** The variant whose reference (`assets/chrome.usda`) is not fetched until it is selected. */
  variant: "chrome",
  /** The asset the variant needs. */
  variantAsset: "assets/chrome.usda",
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

  /** Selects a prim, and one of its attributes or none. */
  select(prim: string | null, attribute: string | null = null): void {
    this.prim = prim;
    this.attribute = attribute;
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

/** The six steps, in order. */
export const steps: readonly Step[] = [
  {
    title: "Open the layered sample",
    look: "The asset loop: the root wants its sublayer and the robot's reference; both are fetched in one round.",
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
    title: "Save, download, reopen",
    look: "The layers: only scene.usda was dirty. The reopened stage reads it from the downloaded bytes and fetches the rest; the override is there.",
    async run(session, host) {
      const editor = session.current;
      editor.save();
      const downloaded = await host.download(editor.saved);
      session.replace(await editor.reopen(downloaded));
      session.select(sample.rock, sample.radius);
    },
  },
];
