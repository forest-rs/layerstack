// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Runs the demo's steps headless against the built package, reading
// the sample from `public/scene/` as the page fetches it, and checks the
// values and explanations after each step. Run after building
// `@forest-rs/layerstack` (`npm run build:wasm && npm run build` there).
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { LayerstackError, loadLayerstack } from "@forest-rs/layerstack";
import type { EditTarget, Explanation, Stage, Value } from "@forest-rs/layerstack";
import type { LayerFile } from "../src/editor.ts";
import { Session, sample, steps } from "../src/script.ts";
import type { Host } from "../src/script.ts";
import { formatValue, releaseValue } from "../src/values.ts";

const scene = fileURLToPath(new URL("../public/scene/", import.meta.url));
const downloads = await mkdtemp(join(tmpdir(), "layerstack-demo-"));
/** What the last download wrote and read back. */
let downloaded: readonly LayerFile[] = [];

const host: Host = {
  layerstack: await loadLayerstack(),
  async fetch(identifier) {
    try {
      return new Uint8Array(await readFile(join(scene, identifier)));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    }
  },
  // The browser downloads the saved layers; here they go to a directory and
  // are read back from it, so the reopen reads what was written.
  async download(files) {
    const back: LayerFile[] = [];
    for (const { identifier, bytes } of files) {
      const path = join(downloads, identifier);
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, bytes);
      back.push({ identifier, bytes: new Uint8Array(await readFile(path)) });
    }
    downloaded = back;
    return back;
  },
};

/** One opinion of an explanation as `layer specPath arc role`, the explanation panel's columns. */
function opinions(explanation: Explanation | null): string[] {
  assert.ok(explanation !== null, "the attribute exists");
  return explanation.opinions.map(
    (opinion) =>
      `${opinion.layer.identifier} ${opinion.specPath} ${opinion.arc} ${opinion.role}${opinion.cause === null ? "" : ` (${opinion.cause})`}`,
  );
}

/** The attribute's value as the inspector shows it at `time` (the default by default), and its explanation. */
function read(stage: Stage, path: string, time?: number): { value: string; opinions: string[]; explanation: Explanation } {
  const attribute = stage.attribute(path);
  assert.ok(attribute !== null, `${path} exists`);
  const value: Value | null = attribute.get(time);
  const explanation = attribute.explain(time);
  assert.ok(explanation !== null);
  const shown = formatValue(value);
  assert.equal(formatValue(explanation.value), shown, `${path}: the explanation explains the value read`);
  releaseValue(value);
  releaseValue(explanation.value);
  return { value: shown, opinions: opinions(explanation), explanation };
}

function selection(stage: Stage, robot: string = sample.robot): string | null {
  return stage.prim(robot)?.variantSets().find((set) => set.name === sample.set)?.selection ?? null;
}

/** The instance proxy's radius explanation in the live stage, before the save, for the reopen to match. */
let proxyBeforeSave: string[] = [];
/** The edit target of the robot's reference, for the rename to make stale. */
let throughReference: EditTarget | null = null;

/** The code of the `LayerstackError` `f` throws. */
function code(f: () => unknown): string {
  try {
    f();
  } catch (error) {
    assert.ok(error instanceof LayerstackError, `expected a LayerstackError, got ${String(error)}`);
    return error.code;
  }
  assert.fail("expected a LayerstackError");
}

const session = new Session();
const checks: ((session: Session) => void)[] = [
  // 1. Open: the sublayer and the reference are wanted and fetched in one round.
  ({ current }) => {
    const status = current.stage.status();
    assert.equal(status.complete, true, "every asset resolved");
    assert.deepEqual(status.compositionErrors, []);
    const wanted = current.loopLog.filter((entry) => entry.phase === "want").map((entry) => entry.identifier);
    assert.deepEqual([...new Set(wanted)].sort(), ["assets/beacon.usda", "assets/robot.usda", "library.usda"]);
    const loaded = current.loopLog.filter((entry) => entry.phase === "load").map((entry) => entry.identifier);
    assert.deepEqual(loaded.sort(), ["assets/beacon.usda", "assets/robot.usda", "library.usda"], "the beacon is read once for both instances");
    assert.ok(!current.loopLog.some((entry) => entry.identifier === sample.variantAsset), "the chrome finish is not fetched yet");
    assert.deepEqual(current.stage.children("/"), ["/Shape", "/World"]);
    assert.deepEqual(current.stage.children("/World"), ["/World/Lamp", "/World/Rock", "/World/Robot", "/World/Beacons"]);
    assert.equal(current.stage.prim("/Shape")?.abstract, true);
    assert.equal(current.stage.editLayer?.identifier, sample.root);
    assert.equal(current.changeLog.length, 1, "one recomposition for the one round");
  },
  // 2. The rock's radius is inherited from the class in the sublayer.
  ({ current, prim, attribute }) => {
    assert.equal(prim, sample.rock);
    assert.equal(attribute, sample.radius);
    const radius = read(current.stage, sample.radius);
    assert.equal(radius.value, "2");
    assert.equal(radius.explanation.source, "default");
    assert.deepEqual(radius.opinions, ["library.usda /Shape.radius inherits contributed"]);
  },
  // 3. The override in the edit layer shadows the inherited opinion.
  ({ current }) => {
    const radius = read(current.stage, sample.radius);
    assert.equal(radius.value, String(sample.override));
    assert.deepEqual(radius.opinions, [
      "scene.usda /World/Rock.radius local contributed",
      "library.usda /Shape.radius inherits ignored (shadowed)",
    ]);
    assert.deepEqual(current.undoStack.map((entry) => entry.label), [`set ${sample.radius}`]);
    assert.deepEqual(current.changeLog.at(-1)?.changes.resynced, [sample.rock]);
    assert.equal(current.stage.isDirty(current.stage.root), true);
  },
  // 4. The variant wants an unfetched asset: the loop runs step by step.
  ({ current }) => {
    const phases = current.loopLog
      .filter((entry) => entry.cause === `${sample.set}=${sample.variant}`)
      .map((entry) => `${entry.phase} ${entry.identifier}`);
    assert.deepEqual(phases, [
      `want ${sample.variantAsset}`,
      `fetch ${sample.variantAsset}`,
      `supply ${sample.variantAsset}`,
      `load ${sample.variantAsset}`,
    ]);
    const receipt = current.undoStack.at(-1)?.receipt;
    assert.deepEqual(
      receipt?.unavailable.map((asset) => `${asset.identifier} ${asset.usedBy} ${asset.prims.join()}`),
      [`${sample.variantAsset} reference ${sample.robot}`],
      "the receipt says what the variant wants",
    );
    assert.equal(selection(current.stage), sample.variant);
    const color = read(current.stage, sample.color);
    assert.equal(color.value, "(0.9, 0.9, 0.95)");
    assert.deepEqual(color.opinions, [
      "assets/chrome.usda /Chrome.color references contributed",
      "assets/robot.usda /Robot.color references ignored (shadowed)",
    ]);
    assert.equal(current.stage.status().complete, true);
  },
  // 5. Both undone, the override redone.
  ({ current }) => {
    assert.equal(selection(current.stage), "painted");
    const color = read(current.stage, sample.color);
    assert.equal(color.value, "(0.8, 0.2, 0.1)");
    assert.deepEqual(color.opinions, [
      "scene.usda /World/Robot{finish=painted}.color variants contributed",
      "assets/robot.usda /Robot.color references ignored (shadowed)",
    ]);
    const radius = read(current.stage, sample.radius);
    assert.equal(radius.value, String(sample.override));
    assert.equal(radius.opinions[0], "scene.usda /World/Rock.radius local contributed");
    assert.deepEqual(current.undoStack.map((entry) => entry.label), [`set ${sample.radius}`]);
    assert.deepEqual(current.redoStack.map((entry) => entry.label), [`select ${sample.set}=${sample.variant} on ${sample.robot}`]);
    const causes = current.changeLog.map((entry) => entry.cause);
    assert.deepEqual(causes.slice(-3), [
      `undo select ${sample.set}=${sample.variant} on ${sample.robot}`,
      `undo set ${sample.radius}`,
      `redo set ${sample.radius}`,
    ]);
  },
  // 6. A typed prim and its schema attribute, defined and set in one edit.
  ({ current, prim, attribute, time }) => {
    assert.equal(prim, sample.pebble);
    assert.equal(attribute, `${sample.pebble}.radius`);
    assert.equal(time, null);
    assert.equal(current.stage.prim(sample.pebble)?.typeName, "Sphere");
    const radius = read(current.stage, `${sample.pebble}.radius`);
    assert.equal(radius.value, "0.5");
    assert.equal(current.stage.attribute(`${sample.pebble}.radius`)?.typeName, "double", "the Sphere schema types it");
    assert.deepEqual(radius.opinions, [`scene.usda ${sample.pebble}.radius local contributed`]);
    assert.deepEqual(current.undoStack.map((entry) => entry.label), [
      `set ${sample.radius}`,
      `define ${sample.pebble} as a Sphere with radius 0.5`,
    ]);
    assert.deepEqual(current.changeLog.at(-1)?.changes.created, [sample.pebble]);
  },
  // 7. Retyped, with the new type's attribute set in the same edit.
  ({ current, attribute }) => {
    assert.equal(attribute, `${sample.pebble}.size`);
    assert.equal(current.stage.prim(sample.pebble)?.typeName, "Cube");
    const size = read(current.stage, `${sample.pebble}.size`);
    assert.equal(size.value, "0.8");
    assert.deepEqual(size.opinions, [`scene.usda ${sample.pebble}.size local contributed`]);
    assert.equal(read(current.stage, `${sample.pebble}.radius`).value, "0.5", "the radius it authored stays");
    assert.equal(current.undoStack.at(-1)?.label, `retype ${sample.pebble} as a Cube with size 0.8`);
    assert.deepEqual(current.changeLog.at(-1)?.changes.resynced, [sample.pebble]);
  },
  // 8. The robot's height edited where it lives: in its asset, through the reference.
  ({ current, prim, attribute, target }) => {
    assert.equal(prim, sample.robot);
    assert.equal(attribute, sample.height);
    assert.ok(target !== null, "the edit field writes through the opinion's target");
    assert.equal(target.kind, "arc");
    assert.equal(target.arc, "references");
    assert.equal(target.layer.identifier, sample.robotAsset);
    assert.equal(target.mapPath(sample.height), "/Robot.height");
    const height = read(current.stage, sample.height);
    assert.equal(height.value, String(sample.assetHeight));
    assert.deepEqual(height.opinions, [`${sample.robotAsset} /Robot.height references contributed`]);
    assert.equal(height.explanation.opinions[0]?.editTarget, target, "the same target, interned");
    const receipt = current.undoStack.at(-1)?.receipt;
    assert.equal(receipt?.target, target);
    assert.deepEqual(receipt?.layers.map((layer) => layer.identifier), [sample.robotAsset]);
    assert.equal(current.undoStack.at(-1)?.label, `set ${sample.height} in ${sample.robotAsset} at /Robot`);
    assert.equal(current.stage.isDirty(target.layer), true, "the asset is dirty");
    assert.match(
      new TextDecoder().decode(current.stage.exportLayer(target.layer, "usda")),
      /double height = 1\.5/,
      "written in the asset",
    );
    assert.equal(code(() => current.stage.move(sample.robot, sample.rover, target)), "namespace.unmappable");
    throughReference = target;
  },
  // 9. The robot renamed: its variant set, its variant specs and the sublayer's aim move with it.
  ({ current, prim, attribute }) => {
    assert.equal(prim, sample.rover);
    assert.equal(attribute, sample.roverColor);
    assert.equal(current.stage.prim(sample.robot), null);
    assert.equal(selection(current.stage, sample.rover), "painted");
    const color = read(current.stage, sample.roverColor);
    assert.equal(color.value, "(0.8, 0.2, 0.1)");
    assert.deepEqual(color.opinions, [
      `scene.usda ${sample.rover}{finish=painted}.color variants contributed`,
      "assets/robot.usda /Robot.color references ignored (shadowed)",
    ]);
    assert.ok(current.stage.prim(sample.lamp)?.relationshipNames().includes("aim"), "the inspector lists the aim");
    assert.deepEqual(current.stage.relationship(sample.aim)?.targets(), [sample.rover]);
    assert.deepEqual(
      current.dirtyLayers().map((layer) => layer.identifier).sort(),
      [sample.robotAsset, "library.usda", sample.root],
      "one rename edited two layers; the asset was edited before",
    );
    assert.deepEqual(
      current.undoStack.at(-1)?.receipt.layers.map((layer) => layer.identifier).sort(),
      ["library.usda", sample.root],
      "the receipt names both",
    );
    assert.equal(current.undoStack.at(-1)?.label, `move ${sample.robot} to ${sample.rover}`);
    assert.deepEqual(
      current.undoStack.at(-1)?.receipt.unrepaired.map((left) => `${left.layer.identifier} ${left.spec} ${left.field} ${left.path}`),
      [`${sample.root} ${sample.rover}{finish=chrome}.mirrors targets ${sample.robot}`],
      "the receipt names the path the unselected chrome finish keeps",
    );
    const stale = throughReference;
    assert.ok(stale !== null);
    assert.equal(code(() => stale.mapPath(sample.height)), "target.stale", "no arc of the renamed prim maps it");
    assert.equal(stale.composed(), false);
    assert.equal(read(current.stage, `${sample.rover}.height`).value, String(sample.assetHeight));
    const changes = current.changeLog.at(-1)?.changes;
    assert.ok(changes?.removed.includes(sample.robot) && changes.created.includes(sample.rover));
  },
  // 10. A held blocked sample: the time samples are the source, with no value.
  ({ current, attribute, time }) => {
    assert.equal(attribute, sample.flicker);
    assert.equal(time, sample.blockedTime);
    const blocked = read(current.stage, sample.flicker, sample.blockedTime);
    assert.equal(blocked.value, "none");
    assert.equal(blocked.explanation.source, "timeSamples");
    assert.deepEqual(blocked.explanation.samples, { lower: 24, upper: 24 });
    assert.deepEqual(blocked.opinions, [`library.usda ${sample.flicker} local block`]);
    const between = read(current.stage, sample.flicker, 6);
    assert.equal(between.value, "0.5");
    assert.deepEqual(between.explanation.samples, { lower: 0, upper: 12 });
    assert.deepEqual(between.opinions, [`library.usda ${sample.flicker} local contributed`]);
  },
  // 11. An instance proxy explains its own composition.
  ({ current, prim, attribute }) => {
    assert.equal(prim, sample.proxy);
    assert.equal(attribute, sample.proxyRadius);
    assert.equal(current.stage.prim(sample.instance)?.instance, true);
    assert.equal(current.stage.prim("/World/Beacons/South")?.instance, true);
    assert.equal(current.stage.prim(sample.proxy)?.instanceProxy, true);
    const radius = read(current.stage, sample.proxyRadius);
    assert.equal(radius.value, "0.25");
    assert.deepEqual(radius.opinions, ["assets/beacon.usda /Beacon/Bulb.radius references contributed"]);
    proxyBeforeSave = radius.opinions;
  },
  // 12. Saved, downloaded and reopened: every edit survives, read from the downloaded bytes.
  ({ current }) => {
    assert.deepEqual(
      downloaded.map((file) => file.identifier).sort(),
      [sample.robotAsset, "library.usda", sample.root],
      "the edit layer, the sublayer the rename repaired and the asset edited through its reference were dirty",
    );
    const text = (identifier: string) =>
      new TextDecoder().decode(downloaded.find((file) => file.identifier === identifier)?.bytes);
    assert.match(text(sample.root), /double radius = 3/);
    assert.match(text(sample.root), /def Cube "Pebble"/);
    assert.match(text(sample.root), /def Xform "Rover"/);
    assert.match(text("library.usda"), /rel aim = <\/World\/Rover>/);
    assert.match(text(sample.robotAsset), /double height = 1\.5/);
    const root = current.loopLog.find((entry) => entry.phase === "supply" && entry.identifier === sample.root);
    assert.match(root?.detail ?? "", /from the saved layers/, "the root is read from the download");
    const fetches = current.loopLog.filter((entry) => entry.phase === "fetch");
    assert.deepEqual(
      fetches.filter((entry) => entry.detail.endsWith("from the saved layers")).map((entry) => entry.identifier).sort(),
      [sample.robotAsset, "library.usda"],
      "the sublayer and the robot asset are read from the download too",
    );
    assert.deepEqual(
      fetches.filter((entry) => !entry.detail.endsWith("from the saved layers")).map((entry) => entry.identifier).sort(),
      ["assets/beacon.usda"],
      "the rest is fetched; chrome is not selected",
    );
    assert.equal(current.stage.status().complete, true);
    assert.deepEqual(current.dirtyLayers(), [], "nothing is dirty after the reopen");
    const radius = read(current.stage, sample.radius);
    assert.equal(radius.value, String(sample.override));
    assert.deepEqual(radius.opinions, [
      "scene.usda /World/Rock.radius local contributed",
      "library.usda /Shape.radius inherits ignored (shadowed)",
    ]);
    assert.equal(current.stage.prim(sample.robot), null);
    assert.equal(selection(current.stage, sample.rover), "painted");
    assert.equal(read(current.stage, sample.roverColor).value, "(0.8, 0.2, 0.1)");
    assert.deepEqual(current.stage.relationship(sample.aim)?.targets(), [sample.rover]);
    assert.deepEqual(read(current.stage, `${sample.rover}.height`).opinions, [
      `${sample.robotAsset} /Robot.height references contributed`,
    ]);
    assert.equal(read(current.stage, `${sample.rover}.height`).value, String(sample.assetHeight), "the asset's edit reopened");
    assert.equal(current.stage.prim(sample.pebble)?.typeName, "Cube");
    assert.equal(read(current.stage, `${sample.pebble}.size`).value, "0.8");
    assert.deepEqual(
      read(current.stage, sample.proxyRadius).opinions,
      proxyBeforeSave,
      "a fresh open explains the instance proxy as the edited live stage did",
    );
  },
];

assert.equal(checks.length, steps.length);
try {
  for (const [index, step] of steps.entries()) {
    const before = host.layerstack.transfer();
    await step.run(session, host);
    checks[index]?.(session);
    const after = host.layerstack.transfer();
    assert.ok(after.bytesIn >= before.bytesIn && after.bytesOut >= before.bytesOut, "the counters only grow");
    console.log(
      `ok ${index + 1} ${step.title}: in ${after.bytesIn - before.bytesIn} B, out ${after.bytesOut - before.bytesOut} B, views ${after.views - before.views}`,
    );
  }
  const { bytesIn, bytesOut, memoryBytes } = host.layerstack.transfer();
  console.log(`transfer: in ${bytesIn} B, out ${bytesOut} B, memory ${memoryBytes} B`);
} finally {
  session.editor?.free();
  await rm(downloads, { recursive: true, force: true });
}
