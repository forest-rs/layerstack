// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Runs the demo's six steps headless against the built package, reading
// the sample from `public/scene/` as the page fetches it, and checks the
// values and explanations after each step. Run after building
// `@forest-rs/layerstack` (`npm run build:wasm && npm run build` there).
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadLayerstack } from "@forest-rs/layerstack";
import type { Explanation, Stage, Value } from "@forest-rs/layerstack";
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

/** The attribute's value as the inspector shows it, and its explanation. */
function read(stage: Stage, path: string): { value: string; opinions: string[]; explanation: Explanation } {
  const attribute = stage.attribute(path);
  assert.ok(attribute !== null, `${path} exists`);
  const value: Value | null = attribute.get();
  const explanation = attribute.explain();
  assert.ok(explanation !== null);
  const shown = formatValue(value);
  assert.equal(formatValue(explanation.value), shown, `${path}: the explanation explains the value read`);
  releaseValue(value);
  releaseValue(explanation.value);
  return { value: shown, opinions: opinions(explanation), explanation };
}

function selection(stage: Stage): string | null {
  return stage.prim(sample.robot)?.variantSets().find((set) => set.name === sample.set)?.selection ?? null;
}

const session = new Session();
const checks: ((session: Session) => void)[] = [
  // 1. Open: the sublayer and the reference are wanted and fetched in one round.
  ({ current }) => {
    const status = current.stage.status();
    assert.equal(status.complete, true, "every asset resolved");
    assert.deepEqual(status.compositionErrors, []);
    const wanted = current.loopLog.filter((entry) => entry.phase === "want").map((entry) => entry.identifier);
    assert.deepEqual(wanted.sort(), ["assets/robot.usda", "library.usda"]);
    const loaded = current.loopLog.filter((entry) => entry.phase === "load").map((entry) => entry.identifier);
    assert.deepEqual(loaded.sort(), ["assets/robot.usda", "library.usda"]);
    assert.ok(!current.loopLog.some((entry) => entry.identifier === sample.variantAsset), "the chrome finish is not fetched yet");
    assert.deepEqual(current.stage.children("/"), ["/Shape", "/World"]);
    assert.deepEqual(current.stage.children("/World"), ["/World/Lamp", "/World/Rock", "/World/Robot"]);
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
  // 6. Saved, downloaded and reopened: the override survives, read from the downloaded bytes.
  ({ current }) => {
    assert.deepEqual(
      downloaded.map((file) => file.identifier),
      [sample.root],
      "only the edit layer was dirty",
    );
    assert.match(new TextDecoder().decode(downloaded[0]?.bytes), /double radius = 3/);
    const root = current.loopLog.find((entry) => entry.phase === "supply" && entry.identifier === sample.root);
    assert.match(root?.detail ?? "", /from the saved layers/, "the root is read from the download");
    const fetched = current.loopLog.filter((entry) => entry.phase === "fetch").map((entry) => entry.identifier);
    assert.deepEqual(fetched.sort(), ["assets/robot.usda", "library.usda"], "the rest is fetched; chrome is not selected");
    assert.equal(current.stage.status().complete, true);
    assert.deepEqual(current.dirtyLayers(), [], "nothing is dirty after the reopen");
    const radius = read(current.stage, sample.radius);
    assert.equal(radius.value, String(sample.override));
    assert.deepEqual(radius.opinions, [
      "scene.usda /World/Rock.radius local contributed",
      "library.usda /Shape.radius inherits ignored (shadowed)",
    ]);
    assert.equal(selection(current.stage), "painted");
    assert.equal(read(current.stage, sample.color).value, "(0.8, 0.2, 0.1)");
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
