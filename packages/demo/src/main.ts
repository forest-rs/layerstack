// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The page: plain DOM over the editor model. Every panel is rendered again
// from the stage after each action; the sample is small, and reading the
// stage is what the demo shows.
import { LayerstackError, loadLayerstack } from "@forest-rs/layerstack";
import type { Attribute, Layer, Prim, Stage } from "@forest-rs/layerstack";
import type { Editor, LayerFile } from "./editor.ts";
import { Session, steps } from "./script.ts";
import type { Host } from "./script.ts";
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
  renderTree(stage, waiting);
  renderInspector(editor);
  renderExplanation(stage, waiting);
  renderLayers(editor);
  renderUndo(editor);
  renderLoop(editor);
  renderChanges(editor);
  // Last, so the counters include what rendering the panels read.
  renderCounters(editor);
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

function renderTree(stage: Stage, waiting: ReadonlyMap<string, readonly string[]>): void {
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
              session.select(child, session.prim === child ? session.attribute : null);
              render();
            },
            title: `${prim.specifier}${prim.typeName === null ? "" : ` ${prim.typeName}`} ${child}`,
          },
          h("span", { class: "name" }, prim.name),
          prim.typeName !== null && h("span", { class: "type" }, prim.typeName),
          prim.specifier !== "def" && h("span", { class: "badge" }, prim.specifier),
          wants !== undefined && h("span", { class: "badge waiting", title: `Composed without ${wants.join(", ")}` }, "waiting"),
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
  const rows = prim.attributeNames().map((name) => {
    const attribute = prim.attribute(name) as Attribute;
    const value = attribute.get();
    const shown = formatValue(value);
    releaseValue(value);
    return h(
      "tr",
      {
        class: `${attribute.path === session.attribute ? "selected" : ""}${attribute.authored ? " authored" : ""}`,
        onclick: () => {
          session.select(prim.path, attribute.path);
          render();
        },
      },
      h("td", { class: "name" }, name),
      h("td", { class: "type" }, attribute.typeName ?? ""),
      h("td", { class: "value" }, shown),
    );
  });
  fill(target, 
    h("div", { class: "heading" }, h("code", {}, prim.path), " ", prim.typeName ?? "typeless", prim.abstract && " (class)"),
    variants.length > 0 && h("div", { class: "variants" }, ...variants),
    h("table", { class: "attributes" }, h("tbody", {}, ...rows)),
    h("p", { class: "hint" }, "Bold attributes have authored opinions. Select one to explain and edit it."),
    session.attribute !== null && editForm(editor, session.attribute),
  );
}

function editForm(editor: Editor, path: string): HTMLElement | null {
  const attribute = editor.stage.attribute(path);
  if (attribute === null || attribute.typeName === null) return null;
  const current = attribute.get();
  const editable = current === null || current.kind === "scalar" || current.kind === "tuple";
  const input = h("input");
  input.value = current === null ? "" : formatValue(current);
  input.disabled = busy || !editable;
  const typeName = attribute.typeName;
  const form = h(
    "form",
    { class: "edit" },
    h("label", {}, `${attribute.name} (${typeName})`, input),
    h(
      "button",
      { disabled: busy || !editable },
      `Set in ${editor.stage.editLayer === null ? "(no edit layer)" : layerName(editor.stage.editLayer)}`,
    ),
  );
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const now = attribute.get();
    const value = parseValue(typeName, input.value, now);
    releaseValue(now);
    void act(() => editor.setValue(path, value));
  });
  releaseValue(current);
  return form;
}

function renderExplanation(stage: Stage, waiting: ReadonlyMap<string, readonly string[]>): void {
  const target = panel("explanation-body");
  const attribute = session.attribute === null ? null : stage.attribute(session.attribute);
  const explanation = attribute?.explain() ?? null;
  if (attribute === null || explanation === null) {
    fill(target, h("p", { class: "hint" }, "Select an attribute."));
    return;
  }
  const value = formatValue(explanation.value);
  releaseValue(explanation.value);
  const wants = waiting.get(attribute.path.slice(0, attribute.path.lastIndexOf(".")));
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
    ),
  );
  fill(target, 
    h("div", { class: "heading" }, h("code", {}, attribute.path), " = ", h("strong", {}, value)),
    h(
      "p",
      {},
      `Source: ${explanation.source}`,
      explanation.samples !== null && `, between samples ${explanation.samples.lower} and ${explanation.samples.upper}`,
      explanation.opinions.length === 0 && explanation.source === "fallback" && " (the schema's fallback; no layer authors it)",
    ),
    wants !== undefined &&
      h("p", { class: "warning" }, `The prim is composed without ${wants.join(", ")}: stronger opinions may arrive.`),
    explanation.opinions.length > 0 &&
      h(
        "table",
        { class: "opinions" },
        h(
          "thead",
          {},
          h("tr", {}, h("th", {}, "#"), h("th", {}, "layer"), h("th", {}, "spec"), h("th", {}, "arc"), h("th", {}, "role"), h("th", {}, "")),
        ),
        h("tbody", {}, ...rows),
      ),
    h("p", { class: "hint" }, "Opinions are listed strongest first."),
  );
}

function renderLayers(editor: Editor): void {
  const stage = editor.stage;
  const edit = stage.editLayer;
  const rows = stage.layers().map((layer) => {
    const dirty = stage.isDirty(layer);
    return h(
      "tr",
      { class: dirty ? "dirty" : "" },
      h("td", {}, layerName(layer)),
      h("td", {}, layer.format),
      h("td", {}, [dirty && "dirty", layer.readOnly && "read-only", layer === edit && "edit layer"].filter(Boolean).join(", ")),
    );
  });
  const dirty = editor.dirtyLayers().length;
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
          title: "Save the dirty layers and download them",
        },
        `Save and download ${dirty} dirty layer${dirty === 1 ? "" : "s"}`,
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

function renderUndo(editor: Editor): void {
  const entries = (stack: typeof editor.undoStack) =>
    stack.length === 0
      ? h("p", { class: "hint" }, "empty")
      : h(
          "ol",
          { class: "stack" },
          ...[...stack].reverse().map((entry) =>
            h("li", {}, entry.label, h("span", { class: "detail" }, ` in ${layerName(entry.receipt.target)}`)),
          ),
        );
  fill(panel("undo-body"), 
    h(
      "div",
      { class: "controls" },
      h("button", { disabled: busy || editor.undoStack.length === 0, onclick: () => void act(() => editor.undo()) }, "Undo"),
      h("button", { disabled: busy || editor.redoStack.length === 0, onclick: () => void act(() => editor.redo()) }, "Redo"),
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
      list("resynced", entry.changes.resynced),
      list("info", entry.changes.changedInfoOnly),
    ),
  );
  const body = panel("changes-body");
  fill(body, h("ol", { class: "log" }, ...rows));
  body.scrollTop = body.scrollHeight;
}

render();
void act(() => runStep(0));
