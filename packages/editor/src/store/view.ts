// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How the editor shows the scene, as the viewer chose: what is drawn and
// under what budget, the camera looked through, the default lights and
// the tone mapping, and which tab of the scene browser and of the bottom
// panel is in front. The budget and the tone mapping are remembered
// for the viewer (`localStorage`, through the helpers that own their
// keys). A setting that changes what is drawn asks for the draws to be
// synced; the others are handed to the viewport.
import { type BudgetChoice, type DrawScopeChoice, rememberBudget, rememberedBudget } from "../model/assetView.ts";
import { DEFAULT_LIGHT_SETTINGS, type LightSettings } from "../viewport/defaultLights.ts";
import type { UpAxis } from "../viewport/framing.ts";
import { rememberToneMap, rememberedToneMap, type ToneMapChoice } from "../viewport/toneMap.ts";
import type { Viewport } from "../viewport/viewport3d.ts";
import type { Signals } from "./signals.ts";

/** The scene browser's tabs. */
export const SCENE_TABS = ["prims", "layers", "cameras"] as const;
/** A tab of the scene browser. */
export type SceneTab = (typeof SCENE_TABS)[number];

/** The bottom panel's tabs. */
export const BOTTOM_TABS = ["explain", "changes", "history", "assets", "shading", "cost", "notices"] as const;
/** A tab of the bottom panel. */
export type BottomTab = (typeof BOTTOM_TABS)[number];

/** What {@link ViewSettings} asks of the store. */
export interface ViewHost {
  /** The three.js view, once attached. */
  readonly viewport: Viewport | null;
  /** Brings the draws up to a setting that changes them (the scope, the budget), bumping `view` with what changed. */
  redraw(): void;
}

/** The viewer's settings of the viewport; bumps `view` as they change. */
export class ViewSettings {
  /**
   * What the viewport draws: the whole scene (as a scene opens), the
   * selection alone (the asset view), or nothing; each under the draw
   * budget ({@link ViewSettings.budgetChoice}).
   */
  drawScope: DrawScopeChoice = "scene";
  /**
   * The draw budget this viewer chose, remembered across visits: what
   * does not fit is drawn as boxes or skipped, the selection admitted
   * first.
   */
  budgetChoice: BudgetChoice = rememberedBudget();
  /** The stage camera the viewport looks through, by path; `null` for the free camera. */
  through: string | null = null;
  /** The stage's up axis (its `upAxis` metadata, `Y` when it has none): what the viewport and the previews orbit about. */
  upAxis: UpAxis = "Y";
  /** The default lights the viewport adds to the stage's, as usdview's View > Lights menu sets them: the camera light on. */
  lightSettings: LightSettings = DEFAULT_LIGHT_SETTINGS;
  /** How the viewport and the previews are shown on the display: usdview's sRGB by default, or AgX; remembered for this viewer. */
  toneMap: ToneMapChoice = rememberedToneMap();
  /** The scene browser's tab in front: an action that points at a layer brings the layers forward. */
  sceneTab: SceneTab = "prims";
  /** The bottom panel's tab in front: explaining a property brings the explanation forward. */
  bottomTab: BottomTab = "explain";
  readonly #signals: Signals;
  readonly #host: ViewHost;

  constructor(signals: Signals, host: ViewHost) {
    this.#signals = signals;
    this.#host = host;
  }

  /** Hands every setting to a viewport newly attached. */
  apply(viewport: Viewport): void {
    viewport.lookThrough(this.through);
    viewport.setLightSettings(this.lightSettings);
    viewport.setToneMap(this.toneMap);
    viewport.setUpAxis(this.upAxis);
  }

  /** Another scene: the whole of it is drawn, through the free camera, about its `up`. Tells no one; the caller does. */
  reset(up: UpAxis): void {
    this.drawScope = "scene";
    this.through = null;
    this.#host.viewport?.lookThrough(null);
    this.setUpAxis(up);
  }

  /** Orients the viewport about `up`, the stage's; tells no one (the stage's change does). */
  setUpAxis(up: UpAxis): void {
    this.upAxis = up;
    this.#host.viewport?.setUpAxis(up);
  }

  /** Draws the whole scene, the selection alone, or nothing, from now on. */
  setDrawScope(choice: DrawScopeChoice): void {
    if (choice === this.drawScope) return;
    this.drawScope = choice;
    this.#host.redraw();
  }

  /** Draws under the budget `choice` from now on, and remembers it for this viewer. */
  setBudgetChoice(choice: BudgetChoice): void {
    if (choice === this.budgetChoice) return;
    this.budgetChoice = choice;
    rememberBudget(choice);
    this.#host.redraw();
  }

  /** Brings `tab` of the scene browser to the front. */
  showSceneTab(tab: SceneTab): void {
    if (tab === this.sceneTab) return;
    this.sceneTab = tab;
    this.#signals.bump("view");
  }

  /** Brings `tab` of the bottom panel to the front. */
  showBottomTab(tab: BottomTab): void {
    if (tab === this.bottomTab) return;
    this.bottomTab = tab;
    this.#signals.bump("view");
  }

  /** Looks through the stage camera at `path`, or the free camera with `null`. */
  lookThrough(path: string | null): void {
    this.through = path;
    this.#host.viewport?.lookThrough(path);
    this.#signals.bump("view");
  }

  /** Switches the viewport's default lights (usdview's View > Lights menu). */
  setLightSettings(settings: LightSettings): void {
    this.lightSettings = settings;
    this.#host.viewport?.setLightSettings(settings);
    this.#signals.bump("view");
  }

  /** Shows the viewport and the previews sRGB-encoded as usdview does, or through AgX, and remembers the choice. */
  setToneMap(choice: ToneMapChoice): void {
    this.toneMap = choice;
    rememberToneMap(choice);
    this.#host.viewport?.setToneMap(choice);
    this.#signals.bump("view");
  }
}
