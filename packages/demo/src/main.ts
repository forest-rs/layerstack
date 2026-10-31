// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The page: plain DOM over the editor model. Every panel is rendered again
// from the stage after each action; the sample is small, and reading the
// stage is what the demo shows.
import { LayerstackError, loadLayerstack } from "@forest-rs/layerstack";
import { loadStagehand } from "@forest-rs/layerstack_stagehand";
import type { Attribute, EditTarget, Issue, Layer, Prim, Stage } from "@forest-rs/layerstack";
import type { Editor, LayerFile } from "./editor.ts";
import { Session, steps } from "./script.ts";
import type { Host } from "./script.ts";
import { materialSummary } from "./materials.ts";
import { ViewportModel, updateSummary, viewportSummary } from "./viewport.ts";
import type { ViewportUpdate } from "./viewport.ts";
import type { Viewport } from "./viewport3d.ts";
import { formatNumber, formatValue, parseValue, releaseValue } from "./values.ts";
import "./style.css";

type Child = Node | string | null | undefined | false;
type Props = {
  class?: string;
  title?: string;
  onclick?: () => void;
  disabled?: boolean;
};

/** An element with properties and children; `null` and `false` children are skipped. */
function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  if (props.class !== undefined) element.className = props.class;
  if (props.title !== undefined) element.title = props.title;
  if (props.onclick !== undefined) element.addEventListener("click", props.onclick);
  if (props.disabled === true) element.setAttribute("disabled", "");
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    element.append(child);
  }
  return element;
}

/** Replaces `element`'s children, skipping `null` and `false` ones. */
function fill(element: HTMLElement, ...children: Child[]): void {
  element.replaceChildren(...children.filter((child): child is Node | string => child !== null && child !== undefined && child !== false));
}

function panel(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (element === null) throw new Error(`the page has no #${id}`);
  return element;
}

function bytes(count: number): string {
  if (count < 1024) return `${count} B`;
  if (count < 1024 * 1024) return `${(count / 1024).toFixed(1)} KB`;
  return `${(count / (1024 * 1024)).toFixed(2)} MB`;
}

function layerName(layer: Layer): string {
  return layer.identifier ?? "(anonymous)";
}

const sceneBase = new URL("scene/", document.baseURI);
/** Builds the viewport's triangles: its own module, fed typed arrays read through layerstack. */
const stagehand = await loadStagehand();
/** The viewport's draw list, kept up to date from the editor's change log. */
const model = new ViewportModel(stagehand);
/** The last update that changed something, for the stats line: panels render again without edits. */
let shown: ViewportUpdate | null = null;
/** The download links of the last save, revoked at the next. */
let links: { identifier: string; url: string; size: number }[] = [];

const host: Host = {
  layerstack: await loadLayerstack(),
  async fetch(identifier) {
    const response = await fetch(new URL(identifier, sceneBase));
    if (response.status === 404) return null;
    if (!response.ok) throw new Error(`${identifier}: HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  },
  async download(files) {
    for (const link of links) URL.revokeObjectURL(link.url);
    links = files.map((file) => ({
      identifier: file.identifier,
      url: URL.createObjectURL(new Blob([file.bytes.slice()], { type: "application/octet-stream" })),
      size: file.bytes.byteLength,
    }));
    for (const link of links) {
      const anchor = h("a");
      anchor.href = link.url;
      anchor.download = link.identifier.replaceAll("/", "_");
      anchor.click();
    }
    // A page cannot read its downloads back; it reopens from the very bytes it handed over.
    return files.map((file): LayerFile => ({ identifier: file.identifier, bytes: file.bytes }));
  },
  progress() {
    // Show the loop while its fetches are pending: one render per frame.
    if (frame !== 0) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      render();
    });
  },
};
let frame = 0;

const session = new Session();
let next = 0;
let busy = false;
let failure: unknown = null;

/** Runs an action, then renders; one at a time. */
async function act(action: () => unknown): Promise<void> {
  if (busy) return;
  busy = true;
  failure = null;
  render();
  try {
    await action();
  } catch (error) {
    failure = error;
    console.error(error);
  } finally {
    busy = false;
    render();
  }
}

async function runStep(index: number): Promise<void> {
  const step = steps[index];
  if (step === undefined) return;
  await step.run(session, host);
  next = index + 1;
}

function render(): void {
  const editor = session.editor;
  renderScript();
  renderError();
  if (editor === null) {
    renderCounters(editor);
    return;
  }
  const stage = editor.stage;
  const unavailable = stage.unavailable();
  const waiting = new Map<string, string[]>();
  for (const asset of unavailable) {
    for (const prim of asset.prims) waiting.set(prim, [...(waiting.get(prim) ?? []), asset.identifier]);
  }
  // Composition errors by prim, such as an arc to a muted layer (`composition.muted_asset`).
  const issues = new Map<string, Issue[]>();
  for (const issue of stage.status().compositionErrors) {
    if (issue.prim !== null) issues.set(issue.prim, [...(issues.get(issue.prim) ?? []), issue]);
  }
  renderViewport(editor);
  renderTree(stage, waiting, issues);
  renderInspector(editor);
  renderExplanation(stage, waiting, issues);
  renderLayers(editor);
  renderUndo(editor);
  renderLoop(editor);
  renderChanges(editor);
  // Last, so the counters include what rendering the panels read.
  renderCounters(editor);
}

/** Each texture directory's `listing.json`, fetched once: a static host cannot list a directory. */
const listings = new Map<string, Promise<readonly string[]>>();

/**
 * Whether the scene's storage has the texture `identifier`, for a UDIM
 * set's tiles (`udimTiles`): its directory's listing names it. OpenUSD
 * asks its resolver tile by tile; this host answers from the listing
 * rather than fetching a hundred names that are mostly missing.
 */
async function textureExists(identifier: string): Promise<boolean> {
  const slash = identifier.lastIndexOf("/");
  const directory = identifier.slice(0, slash + 1);
  let listing = listings.get(directory);
  if (listing === undefined) {
    listing = host.fetch(`${directory}listing.json`).then((bytes) =>
      bytes === null ? [] : (JSON.parse(new TextDecoder().decode(bytes)) as string[]),
    );
    listings.set(directory, listing);
  }
  return (await listing).includes(identifier.slice(slash + 1));
}

/**
 * The three.js view: `undefined` while three.js loads (in its own chunk, so
 * the editor works before it arrives), `null` where WebGL is unavailable.
 */
let viewport: Viewport | null | undefined;
import("./viewport3d.ts").then(
  ({ Viewport }) => {
    try {
      viewport = new Viewport(
        panel("viewport-canvas") as HTMLCanvasElement,
        panel("free-camera") as HTMLInputElement,
        (identifier) => host.fetch(identifier),
        textureExists,
      );
    } catch (error) {
      console.error(error);
      viewport = null;
    }
    render();
  },
  (error: unknown) => {
    console.error(error);
    viewport = null;
    render();
  },
);

/**
 * Draws the stage at the inspector's time: the draw list hears the change
 * records since the last render (or reads a new stage whole), and the
 * three.js view applies its diffs. The stats line says what the last
 * update did.
 */
function renderViewport(editor: Editor): void {
  const body = panel("viewport-body");
  let update: ViewportUpdate;
  let scene: ReturnType<ViewportModel["scene"]>;
  try {
    update = model.sync(editor, session.time ?? undefined);
    scene = model.scene(session.time ?? undefined);
  } catch (error) {
    // A read the viewport cannot make (an invalid instancer, say) must not take the panels down.
    fill(body, `The viewport could not read the stage: ${error instanceof Error ? error.message : String(error)}`);
    return;
  }
  const summary = viewportSummary(scene);
  viewport?.update(scene, update, session.prim);
  if (update.reset || update.records > 0 || update.timeChanged || shown === null) shown = update;
  const last = shown;
  const work = updateSummary(last);
  const bounds = summary.bounds === null ? "empty" : `[${summary.bounds.slice(0, 3).map(formatNumber).join(", ")}] to [${summary.bounds.slice(3).map(formatNumber).join(", ")}]`;
  const changed =
    last.reset
      ? `read whole: ${work.added} draws, ${work.triangulations} triangulated, ${work.tessellations} tessellated`
      : `last update (${last.records} change${last.records === 1 ? "" : "s"}${last.timeChanged ? ", new time" : ""}): ` +
        `+${work.added} −${work.removed} draws, ${work.moved} moved, ${work.shown} shown or hidden, ` +
        `${work.refilled} refilled, ${work.rebuilt} rebuilt, ${work.triangulations} triangulated, ${work.tessellations} tessellated; ` +
        `${work.itemsRead} draws read, ${work.itemsUntouched} untouched` +
        (last.timeChanged ? ` (${work.itemsTimeInvariant} not animated)` : "") +
        `, ${work.localTransforms} local transforms; ${work.lightsRead} lights read, ${work.lights} changed; ${work.camerasRead} cameras read; ` +
        `${work.materialsRead} materials read, ${work.materials} changed; ` +
        `curves ${work.curvesRefilled} refilled, ${work.curvesLaidOut} laid out; points ${work.pointsRefilled} refilled, ${work.pointsRebuilt} read anew; ` +
        `light links ${work.linksEvaluated} evaluated, ${work.linksClassified} draws classified, ${work.links} changed`;
  // Each light link that does not link everything: what three.js lights
  // with its per-material masks (shadow links are shown, not drawn).
  const linking = scene.linking.map(
    (line) => `${line.light.slice(line.light.lastIndexOf("/") + 1)} ${line.kind === "light" ? "lights" : "shadows"} ${line.linked.length} of ${line.of} draws`,
  );
  fill(
    body,
    `${scene.items.length} draws: ${summary.drawn.length} drawn (${summary.instances} instances), ${summary.triangles} triangles, framed on ${bounds}; ` +
      `${summary.lights.length} lights, ${summary.materials.length} materials, ${summary.camera === null ? "no stage camera" : `stage camera ${summary.camera}`}`,
    linking.length > 0 ? `; ${linking.join(", ")}` : "",
    summary.hidden.length > 0 ? `; hidden: ${summary.hidden.join(", ")}` : "",
    viewport === null ? " (WebGL is unavailable: the summary is all there is)" : "",
    h("div", { class: "viewport-diff" }, changed),
  );
}

function renderCounters(editor: Editor | null): void {
  const transfer = host.layerstack.transfer();
  const counter = (label: string, value: string, title: string) =>
    h("div", { class: "counter", title }, h("span", { class: "label" }, label), h("span", { class: "value" }, value));
  const items = [
    counter("in", bytes(transfer.bytesIn), "Bytes copied into the module: opened and supplied sources, authored buffers"),
    counter("out", bytes(transfer.bytesOut), "Bytes copied out: array copies, saved and exported layers"),
    counter("views", `${transfer.views} / ${bytes(transfer.bytesViewed)}`, "Views over the module's memory, and the bytes they exposed without a copy"),
    counter("memory", bytes(transfer.memoryBytes), "The module's memory now"),
  ];
  if (editor !== null) {
    const status = editor.stage.status();
    items.push(
      counter("fetched", `${editor.fetched.assets} / ${bytes(editor.fetched.bytes)}`, "Assets this stage fetched over HTTP, and their bytes"),
      counter("prims", String(status.prims), "Prims the default traversal visits"),
      counter(
        "assets",
        status.complete ? "complete" : `${status.unavailable.length} unavailable, ${status.unresolved.length} unresolved`,
        "Whether every asset the stage uses is read",
      ),
    );
  }
  const { crate, revision } = host.layerstack.version;
  items.push(counter("module", `${crate} ${revision.slice(0, 8)}`, "The layerstack_wasm crate and revision the module was built from"));
  fill(panel("counters"), ...items);
}

function renderScript(): void {
  const list = h(
    "ol",
    { class: "steps" },
    ...steps.map((step, index) =>
      h(
        "li",
        { class: index < next ? "done" : index === next ? "next" : "" },
        h(
          "button",
          { onclick: () => void act(() => runStep(index)), disabled: busy, title: "Run this step" },
          `${index + 1}. ${step.title}`,
        ),
      ),
    ),
  );
  const last = steps[next - 1];
  const controls = h(
    "div",
    { class: "controls" },
    h(
      "button",
      { onclick: () => void act(() => runStep(next)), disabled: busy || next >= steps.length },
      next >= steps.length ? "Script done" : `Run step ${next + 1}`,
    ),
    h(
      "button",
      {
        onclick: () =>
          void act(async () => {
            for (let index = next; index < steps.length; index += 1) await runStep(index);
          }),
        disabled: busy || next >= steps.length,
      },
      "Run the rest",
    ),
    h("button", { onclick: () => void act(() => runStep(0)), disabled: busy }, "Start over"),
  );
  fill(
    panel("script-body"),
    controls,
    list,
    last !== undefined && h("p", { class: "look" }, h("strong", {}, `After step ${next}: `), last.look),
  );
}

function renderError(): void {
  const target = panel("error");
  if (failure === null) {
    fill(target);
    target.hidden = true;
    return;
  }
  target.hidden = false;
  const text =
    failure instanceof LayerstackError
      ? `${failure.code}: ${failure.message}${failure.path === undefined ? "" : ` (${failure.path})`}`
      : failure instanceof Error
        ? failure.message
        : String(failure);
  fill(target, h("strong", {}, "Failed: "), text);
}

/** A tree badge's words for a composition error. */
function issueBadge(issue: Issue): string {
  return issue.code === "composition.muted_asset" ? "muted asset" : issue.code.replace(/^composition\./, "");
}

function renderTree(
  stage: Stage,
  waiting: ReadonlyMap<string, readonly string[]>,
  issues: ReadonlyMap<string, readonly Issue[]>,
): void {
  const branch = (path: string): HTMLUListElement =>
    h(
      "ul",
      {},
      ...stage.children(path).map((child) => {
        const prim = stage.prim(child) as Prim;
        const wants = waiting.get(child);
        const row = h(
          "button",
          {
            class: `prim${session.prim === child ? " selected" : ""}${prim.abstract ? " abstract" : ""}${prim.active ? "" : " inactive"}`,
            onclick: () => {
              session.select(child, session.prim === child ? session.attribute : null, session.time);
              render();
            },
            title: `${prim.specifier}${prim.typeName === null ? "" : ` ${prim.typeName}`} ${child}`,
          },
          h("span", { class: "name" }, prim.name),
          prim.typeName !== null && h("span", { class: "type" }, prim.typeName),
          prim.specifier !== "def" && h("span", { class: "badge" }, prim.specifier),
          prim.instance && h("span", { class: "badge instance", title: "An instance: its children compose from a shared prototype" }, "instance"),
          prim.instanceProxy &&
            h("span", { class: "badge proxy", title: "An instance proxy: a prim of the prototype, read through this instance" }, "proxy"),
          wants !== undefined && h("span", { class: "badge waiting", title: `Composed without ${wants.join(", ")}` }, "waiting"),
          ...(issues.get(child) ?? []).map((issue) => h("span", { class: "badge issue", title: `${issue.code}: ${issue.message}` }, issueBadge(issue))),
        );
        return h("li", {}, row, stage.children(child).length > 0 && branch(child));
      }),
    );
  fill(panel("tree-body"), branch("/"));
}

function renderInspector(editor: Editor): void {
  const target = panel("inspector-body");
  const prim = session.prim === null ? null : editor.stage.prim(session.prim);
  if (prim === null) {
    fill(target, h("p", { class: "hint" }, "Select a prim."));
    return;
  }
  const variants = prim.variantSets().map((set) => {
    const select = h("select", { disabled: busy });
    if (set.selection === null) select.append(new Option("(none)", "", true, true));
    for (const variant of set.variants) select.append(new Option(variant, variant, false, variant === set.selection));
    select.addEventListener("change", () => {
      void act(() => editor.selectVariant(prim.path, set.name, select.value));
    });
    return h("label", { class: "variant" }, h("span", {}, set.name), select);
  });
  const time = session.time ?? undefined;
  const rows = prim.attributeNames().map((name) => {
    const attribute = prim.attribute(name) as Attribute;
    const value = attribute.get(time);
    const shown = formatValue(value);
    releaseValue(value);
    return h(
      "tr",
      {
        class: `${attribute.path === session.attribute ? "selected" : ""}${attribute.authored ? " authored" : ""}`,
        onclick: () => {
          session.select(prim.path, attribute.path, session.time);
          render();
        },
      },
      h("td", { class: "name" }, name),
      h("td", { class: "type" }, attribute.typeName ?? ""),
      h("td", { class: "value" }, shown),
    );
  });
  // Relationships with targets: a schema's empty ones (a light's link collections) are left out.
  const relationships = prim.relationshipNames().flatMap((name) => {
    const targets = prim.relationship(name)?.targets() ?? [];
    if (targets.length === 0) return [];
    return [
      h(
        "tr",
        { class: "relationship authored" },
        h("td", { class: "name" }, name),
        h("td", { class: "type" }, "rel"),
        h("td", { class: "value" }, targets.join(", ")),
      ),
    ];
  });
  fill(target,
    h("div", { class: "heading" }, h("code", {}, prim.path), " ", prim.typeName ?? "typeless", prim.abstract && " (class)"),
    variants.length > 0 && h("div", { class: "variants" }, ...variants),
    timeForm(),
    h("table", { class: "attributes" }, h("tbody", {}, ...rows, ...relationships)),
    materialSection(editor.stage, prim.path, time),
    linkSection(prim.path),
    h("p", { class: "hint" }, "Bold attributes have authored opinions. Select one to explain and edit it; rel rows are relationships and their targets."),
    prim.instanceProxy &&
      h("p", { class: "hint" }, "An instance proxy is read through its instance: edit the asset the instance references, or the instance itself."),
    !prim.instanceProxy && session.attribute !== null && editForm(editor, session.attribute),
    !prim.instanceProxy && namespaceForm(editor, prim),
  );
}

/**
 * A light's evaluated links, as the viewport's draw list holds them: the
 * draws it lights and those casting its shadows, each everything unless
 * its collection has a category; nothing for a prim that is not a listed
 * light.
 */
function linkSection(path: string): HTMLElement | false {
  const list = model.list;
  const light = list?.light(path) ?? null;
  if (list === null || light === null) return false;
  const targets = list.items().flatMap((item) => item.linkPaths.map((at) => ({ item: item.id, path: at })));
  const line = (kind: "light" | "shadow", label: string) => {
    const link = kind === "light" ? light.lightLink : light.shadowLink;
    if (link.category === null) return h("p", { class: "hint" }, `${label} every draw (collection:${kind}Link links everything).`);
    const linked = targets.filter((target) => list.isLinked(light.key, kind, target)).map((target) => target.path);
    return h("p", {}, `${label} ${linked.length} of ${targets.length} draws (category ${link.category})${linked.length > 0 ? ": " : ""}`, ...linked.map((at) => h("code", {}, `${at} `)));
  };
  return h("div", { class: "links" }, line("light", "Lights"), line("shadow", "Shadows from"));
}

/** The prim's bound material, and its surface shader's inputs; nothing when no material binds it. */
function materialSection(stage: Stage, path: string, time: number | undefined): HTMLElement | false {
  const summary = materialSummary(stage, path, time);
  if (summary === null) return false;
  const inputs = summary.inputs.map((input) =>
    h(
      "tr",
      {},
      h("td", { class: "name" }, `inputs:${input.name}`),
      h("td", { class: "type" }, input.type),
      h("td", { class: "value" }, input.value),
    ),
  );
  return h(
    "div",
    { class: "material" },
    h("p", {}, "Material ", h("code", {}, summary.material), ` (${summary.binding})`),
    summary.shader !== null && h("p", { class: "hint" }, "Surface shader ", h("code", {}, summary.shader)),
    inputs.length > 0 && h("table", { class: "attributes" }, h("tbody", {}, ...inputs)),
  );
}

/** The time code the inspector and the explanation read at: empty for the defaults. */
function timeForm(): HTMLElement {
  const input = h("input");
  input.type = "number";
  input.placeholder = "default";
  input.value = session.time === null ? "" : String(session.time);
  input.disabled = busy;
  input.addEventListener("change", () => {
    const time = input.value.trim() === "" ? null : Number(input.value);
    session.time = time !== null && Number.isFinite(time) ? time : null;
    render();
  });
  return h(
    "label",
    { class: "time", title: "Read values and explanations at this time code; empty reads the defaults" },
    h("span", {}, "time"),
    input,
  );
}

/** Rename or delete the prim: namespace edits of the edit layer's stack, undoable. */
function namespaceForm(editor: Editor, prim: Prim): HTMLElement {
  const input = h("input");
  input.value = prim.name;
  input.disabled = busy;
  const parent = prim.path.slice(0, prim.path.lastIndexOf("/"));
  const remove = h(
    "button",
    {
      disabled: busy,
      title: "Delete the prim from every layer of the stack, and the paths that name it",
      onclick: () =>
        void act(() => {
          editor.delete(prim.path);
          session.select(parent === "" ? null : parent);
        }),
    },
    "Delete",
  );
  // Only Rename submits the form.
  remove.type = "button";
  const form = h(
    "form",
    { class: "edit namespace" },
    h("label", {}, "name", input),
    h("button", { disabled: busy, title: "Rename the prim in every layer of the stack, fixing the paths that name it" }, "Rename"),
    remove,
  );
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const to = `${parent}/${input.value.trim()}`;
    if (to === prim.path) return;
    void act(() => {
      editor.move(prim.path, to);
      const property = session.attribute?.slice(session.attribute.lastIndexOf("."));
      session.select(to, property === undefined ? null : `${to}${property}`, session.time);
    });
  });
  return form;
}

function editForm(editor: Editor, path: string): HTMLElement | null {
  const attribute = editor.stage.attribute(path);
  if (attribute === null || attribute.typeName === null) return null;
  const time = session.time;
  const current = attribute.get(time ?? undefined);
  const editable = current === null || current.kind === "scalar" || current.kind === "tuple";
  const input = h("input");
  input.value = current === null ? "" : formatValue(current);
  input.disabled = busy || !editable;
  const typeName = attribute.typeName;
  const through = session.target;
  const where =
    through === null
      ? editor.stage.editLayer === null
        ? "(no edit layer)"
        : layerName(editor.stage.editLayer)
      : `${layerName(through.layer)}, at ${mappedPath(through, path)} through the ${through.arc}`;
  const form = h(
    "form",
    { class: "edit" },
    h("label", {}, `${attribute.name} (${typeName})`, input),
    h("button", { disabled: busy || !editable }, `Set ${time === null ? "" : `at time ${time} `}in ${where}`),
    through !== null && editLayerButton(),
  );
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const now = attribute.get(time ?? undefined);
    const value = parseValue(typeName, input.value, now);
    releaseValue(now);
    void act(() => (through === null ? editor.setValue(path, value, time) : editor.setValueThrough(through, path, value, time)));
  });
  releaseValue(current);
  return form;
}

function renderExplanation(
  stage: Stage,
  waiting: ReadonlyMap<string, readonly string[]>,
  issues: ReadonlyMap<string, readonly Issue[]>,
): void {
  const target = panel("explanation-body");
  const attribute = session.attribute === null ? null : stage.attribute(session.attribute);
  const time = session.time;
  const explanation = attribute?.explain(time ?? undefined) ?? null;
  if (attribute === null || explanation === null) {
    fill(target, h("p", { class: "hint" }, "Select an attribute."));
    return;
  }
  const value = formatValue(explanation.value);
  releaseValue(explanation.value);
  const primPath = attribute.path.slice(0, attribute.path.lastIndexOf("."));
  const wants = waiting.get(primPath);
  const rows = explanation.opinions.map((opinion, index) =>
    h(
      "tr",
      { class: opinion.role },
      h("td", {}, String(index + 1)),
      h("td", {}, layerName(opinion.layer)),
      h("td", {}, h("code", {}, opinion.specPath)),
      h("td", {}, opinion.arc ?? ""),
      h("td", { class: "role" }, opinion.role, opinion.cause !== null && ` (${opinion.cause})`),
      h(
        "td",
        {},
        opinion.layerOffset.offset === 0 && opinion.layerOffset.scale === 1
          ? ""
          : `offset ${formatNumber(opinion.layerOffset.offset)}, scale ${formatNumber(opinion.layerOffset.scale)}`,
      ),
      h("td", {}, editHere(opinion.editTarget)),
    ),
  );
  fill(target, 
    h(
      "div",
      { class: "heading" },
      h("code", {}, attribute.path),
      " = ",
      h("strong", {}, value),
      ` ${time === null ? "at the default time" : `at time ${formatNumber(time)}`}`,
    ),
    h(
      "p",
      {},
      `Source: ${explanation.source}`,
      explanation.samples !== null &&
        (explanation.samples.lower === explanation.samples.upper
          ? `, the sample at ${formatNumber(explanation.samples.lower)}`
          : `, between samples ${formatNumber(explanation.samples.lower)} and ${formatNumber(explanation.samples.upper)}`),
      explanation.source === "timeSamples" && explanation.value === null && " (a block: the sample held here authors no value)",
      explanation.opinions.length === 0 && explanation.source === "fallback" && " (the schema's fallback; no layer authors it)",
    ),
    wants !== undefined &&
      h("p", { class: "warning" }, `The prim is composed without ${wants.join(", ")}: stronger opinions may arrive.`),
    ...(issues.get(primPath) ?? []).map((issue) => h("p", { class: "warning" }, h("code", {}, issue.code), `: ${issue.message}`)),
    explanation.opinions.length > 0 &&
      h(
        "table",
        { class: "opinions" },
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "#"),
            h("th", {}, "layer"),
            h("th", {}, "spec"),
            h("th", {}, "arc"),
            h("th", {}, "role"),
            h("th", {}, ""),
            h("th", {}, "edit"),
          ),
        ),
        h("tbody", {}, ...rows),
      ),
    h(
      "p",
      { class: "hint" },
      "Opinions are listed strongest first. Edit here writes where an opinion lives, through the arc that reaches it: in a referenced asset, every prim that references it changes, and an edit of a shadowed opinion changes nothing shown.",
    ),
  );
}

/** The button that sends the edit field back to the edit layer: a plain button, not the form's submit. */
function editLayerButton(): HTMLButtonElement {
  const button = h("button", {
    disabled: busy,
    onclick: () => {
      session.target = null;
      render();
    },
  }, "Edit the edit layer instead");
  button.type = "button";
  return button;
}

/** Where `target` writes `path`, in words: a stale target says so rather than throwing. */
function mappedPath(target: EditTarget, path: string): string {
  try {
    return target.mapPath(path) ?? "(a path it does not map)";
  } catch (error) {
    if (error instanceof LayerstackError && error.code === "target.stale") return "(stale: no arc maps it now)";
    throw error;
  }
}

/** The button that points the edit field at an opinion's edit target, or why it cannot. */
function editHere(target: EditTarget | null): Child {
  if (target === null) return "";
  if (target.layer.readOnly) return h("span", { class: "detail" }, "read-only");
  const current = session.target === target;
  return h(
    "button",
    {
      disabled: busy || current,
      onclick: () => {
        session.target = target;
        render();
      },
    },
    current ? "editing" : "Edit here",
  );
}

function renderLayers(editor: Editor): void {
  const stage = editor.stage;
  const edit = stage.editLayer;
  const rows = stage.layers().map((layer) => {
    const dirty = stage.isDirty(layer);
    // The muted state comes from the changes the stage reported (`Editor.muted`).
    const muted = layer.identifier !== null && editor.muted.has(layer.identifier);
    return h(
      "tr",
      { class: `${dirty ? "dirty" : ""}${muted ? " muted" : ""}` },
      h("td", { class: "name" }, layerName(layer)),
      h("td", {}, layer.format),
      h(
        "td",
        {},
        [muted && "muted", dirty && "dirty", layer.readOnly && "read-only", layer === edit && "edit layer"].filter(Boolean).join(", "),
      ),
      h("td", {}, muteToggle(editor, layer, muted)),
    );
  });
  // `save()` skips a muted layer and leaves it dirty.
  const dirtyLayers = editor.dirtyLayers();
  const skipped = dirtyLayers.filter((layer) => layer.identifier !== null && editor.muted.has(layer.identifier)).length;
  const dirty = dirtyLayers.length - skipped;
  fill(panel("layers-body"), 
    h("table", { class: "layers" }, h("tbody", {}, ...rows)),
    h(
      "div",
      { class: "controls" },
      h(
        "button",
        {
          disabled: busy || dirty === 0,
          onclick: () =>
            void act(async () => {
              editor.save();
              await host.download(editor.saved);
            }),
          title: "Save the dirty layers and download them; a muted layer stays dirty until it is unmuted",
        },
        `Save and download ${dirty} dirty layer${dirty === 1 ? "" : "s"}${skipped === 0 ? "" : ` (${skipped} muted, skipped)`}`,
      ),
      h(
        "button",
        {
          disabled: busy || editor.saved.length === 0,
          onclick: () => void act(async () => session.replace(await editor.reopen(editor.saved))),
          title: "Open the root again, reading the saved layers before fetching the rest",
        },
        "Reopen from the saved bytes",
      ),
    ),
    links.length > 0 &&
      h(
        "ul",
        { class: "downloads" },
        ...links.map((link) => {
          const anchor = h("a", {}, link.identifier);
          anchor.href = link.url;
          anchor.download = link.identifier.replaceAll("/", "_");
          return h("li", {}, anchor, ` ${bytes(link.size)}`);
        }),
      ),
  );
}

/**
 * The button that mutes or unmutes a layer: the stage recomposes without
 * or with its opinions, as one change. The root cannot be muted.
 */
function muteToggle(editor: Editor, layer: Layer, muted: boolean): Child {
  if (layer === editor.stage.root) return h("span", { class: "detail", title: "The root layer cannot be muted" }, "root");
  return h(
    "button",
    {
      class: "mute",
      disabled: busy,
      title: muted
        ? "Compose the layer's opinions again"
        : "Compose the stage without the layer's opinions, wherever it is reached; its content and unsaved edits are kept",
      onclick: () => void act(() => editor.setMuted(layer, !muted)),
    },
    muted ? "Unmute" : "Mute",
  );
}

function renderUndo(editor: Editor): void {
  const entries = (stack: typeof editor.undoStack) =>
    stack.length === 0
      ? h("p", { class: "hint" }, "empty")
      : h(
          "ol",
          { class: "stack" },
          ...[...stack].reverse().map((entry) =>
            h(
              "li",
              {},
              entry.label,
              h("span", { class: "detail" }, ` in ${entry.step.layers.map(layerName).join(", ") || layerName(entry.receipt.target.layer)}`),
              ...entry.receipt.unrepaired.map((left) =>
                h("span", { class: "detail unrepaired" }, `; leaves ${left.path} in ${left.spec} (${left.field}, ${layerName(left.layer)})`),
              ),
            ),
          ),
        );
  fill(panel("undo-body"), 
    h(
      "div",
      { class: "controls" },
      h("button", { disabled: busy || !editor.history.canUndo, onclick: () => void act(() => editor.undo()) }, "Undo"),
      h("button", { disabled: busy || !editor.history.canRedo, onclick: () => void act(() => editor.redo()) }, "Redo"),
    ),
    h("h3", {}, "Undo (newest first)"),
    entries(editor.undoStack),
    h("h3", {}, "Redo"),
    entries(editor.redoStack),
  );
}

function renderLoop(editor: Editor): void {
  const rows: HTMLTableRowElement[] = [];
  let cause: string | null = null;
  for (const entry of editor.loopLog) {
    if (entry.cause !== cause) {
      cause = entry.cause;
      const header = h("th", {}, cause);
      header.colSpan = 3;
      rows.push(h("tr", { class: "group" }, header));
    }
    rows.push(
      h(
        "tr",
        { class: entry.phase },
        h("td", { class: "phase" }, entry.phase),
        h("td", {}, entry.identifier),
        h("td", { class: "detail" }, entry.detail),
      ),
    );
  }
  const body = panel("loop-body");
  fill(body, h("table", { class: "log" }, h("tbody", {}, ...rows)));
  body.scrollTop = body.scrollHeight;
}

function renderChanges(editor: Editor): void {
  const list = (label: string, paths: readonly string[]) =>
    paths.length > 0 && h("span", { class: "paths" }, `${label} ${paths.join(", ")}`);
  const rows = editor.changeLog.map((entry, index) =>
    h(
      "li",
      {},
      h("span", { class: "cause" }, `${index + 1}. ${entry.cause}: `),
      list("created", entry.changes.created),
      list("removed", entry.changes.removed),
      list("resynced", [...entry.changes.resynced, ...entry.changes.resyncedProperties]),
      list("info", entry.changes.changedInfoOnly),
      list("muted", entry.changes.mutedIdentifiers),
      list("unmuted", entry.changes.unmutedIdentifiers),
    ),
  );
  const body = panel("changes-body");
  fill(body, h("ol", { class: "log" }, ...rows));
  body.scrollTop = body.scrollHeight;
}

render();
void act(() => runStep(0));
