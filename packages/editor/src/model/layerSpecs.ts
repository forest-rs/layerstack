// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a layer authors about composition, read from its USDA text (what
// `Stage.exportLayer(layer, "usda")` writes, the Text tab's text): each
// prim and variant spec's arcs as list ops (`prepend references =
// @./robot.usda@</Robot>`), the variant sets it holds and their
// variants, the selections and `variantSetNames` it authors, and the
// layer's `defaultPrim` and root prims. The stage composes these but does
// not say which layer authored an arc, or whether a layer holds a variant
// set's spec; the inspector needs both to offer removing an arc where
// edits go, and to say which layer to edit otherwise. AOUSD Core §7.6.2
// (prim spec fields: `references`, `payload`, `inherits`, `specializes`,
// `variantSetNames`, `variantSelection`), §7.3.6 (variant set specs),
// §12.4 (list ops).
//
// The reader scans tokens rather than parsing the grammar whole: it keeps
// the prim, variant set and variant blocks and the metadata of each, and
// skips everything else (properties, values, dictionaries) as balanced
// runs, so a layer's large arrays cost one pass and no allocation.
import type { Layerstack } from "@forest-rs/layerstack";

/** The prim spec fields that author arcs. */
export type ArcField = "references" | "payload" | "inherits" | "specializes";

/** How an arc list op entry is authored: `prepend`, `append`, `add`, `delete`, `reorder`, or `explicit` (no keyword). */
export type ListOpKind = "explicit" | "prepend" | "append" | "add" | "delete" | "reorder";

/** One arc a spec authors: an item of one of its arc list ops. */
export interface AuthoredArc {
  readonly field: ArcField;
  readonly op: ListOpKind;
  /** The asset path as authored, without its `@`s; `null` for an internal reference or payload, and for class arcs. */
  readonly asset: string | null;
  /** The prim path as authored, without its `<>`; `null` for a reference or payload to the layer's default prim. */
  readonly primPath: string | null;
  /** The layer offset across a reference or payload; identity when none is authored. */
  readonly offset: number;
  readonly scale: number;
  /** The line of the text (0-based) the item is on. */
  readonly line: number;
}

/** A variant set's names as one `variantSetNames` list op entry authors them. */
export interface AuthoredNames {
  readonly op: ListOpKind;
  readonly names: readonly string[];
}

/** What one prim or variant spec authors about composition. */
export interface SpecSummary {
  /** Its path in the layer (`/World/Robot`, `/World/Robot{finish=chrome}`). */
  readonly path: string;
  /** The line (0-based) its block starts on. */
  readonly line: number;
  /** `def`, `over` or `class` for a prim spec; `null` for a variant. */
  readonly specifier: "def" | "over" | "class" | null;
  /** Its type name as authored; `null` without one, and for a variant. */
  readonly typeName: string | null;
  /** Its arcs, in the order authored. */
  readonly arcs: readonly AuthoredArc[];
  /** The variant sets it holds a spec of (`variantSet "finish" = {...}`), each with its variants in order. */
  readonly variantSets: ReadonlyMap<string, readonly string[]>;
  /** The line of each variant set block (0-based). */
  readonly variantSetLines: ReadonlyMap<string, number>;
  /** Its variant selections (`variants = { string finish = "chrome" }`). */
  readonly selections: ReadonlyMap<string, string>;
  /** Its `variantSetNames` list op entries. */
  readonly variantSetNames: readonly AuthoredNames[];
}

/** A root prim of a layer: its name, specifier and type. */
export interface RootPrim {
  readonly name: string;
  readonly specifier: "def" | "over" | "class";
  readonly typeName: string | null;
}

/** What a layer authors about composition. */
export interface LayerSpecs {
  /** The layer's `defaultPrim`; `null` when it authors none. */
  readonly defaultPrim: string | null;
  /** Its root prims, in order. */
  readonly roots: readonly RootPrim[];
  /** Every prim and variant spec, by path. */
  readonly specs: ReadonlyMap<string, SpecSummary>;
}

type TokenKind = "word" | "string" | "asset" | "path" | "punct";

interface Token {
  readonly kind: TokenKind;
  readonly text: string;
  /** The line it starts on, and the line it ends on (a triple-quoted string spans lines). */
  readonly line: number;
  readonly end: number;
}

const PUNCT = new Set(["(", ")", "[", "]", "{", "}", "=", ",", ";"]);
const OPEN: Readonly<Record<string, string>> = { "(": ")", "[": "]", "{": "}" };
const LIST_OPS = new Set(["prepend", "append", "add", "delete", "reorder"]);
const ARC_FIELDS = new Set<string>(["references", "payload", "inherits", "specializes"]);
const SPECIFIERS = new Set(["def", "over", "class"]);

/** The tokens of USDA `text`: strings, asset paths, paths, punctuation and words; comments dropped. */
function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  let line = 0;
  let at = 0;
  const length = text.length;
  const push = (kind: TokenKind, value: string, start: number) => tokens.push({ kind, text: value, line: start, end: line });
  while (at < length) {
    const char = text[at]!;
    if (char === "\n") {
      line++;
      at++;
      continue;
    }
    if (char === " " || char === "\t" || char === "\r") {
      at++;
      continue;
    }
    if (char === "#") {
      while (at < length && text[at] !== "\n") at++;
      continue;
    }
    const start = line;
    if (char === '"' || char === "'") {
      const triple = text.startsWith(char.repeat(3), at);
      if (triple) {
        const close = text.indexOf(char.repeat(3), at + 3);
        const stop = close < 0 ? length : close;
        const body = text.slice(at + 3, stop);
        for (const each of body) if (each === "\n") line++;
        push("string", body, start);
        at = stop + 3;
        continue;
      }
      let value = "";
      at++;
      while (at < length && text[at] !== char && text[at] !== "\n") {
        if (text[at] === "\\" && at + 1 < length) {
          const next = text[at + 1]!;
          value += next === "n" ? "\n" : next === "t" ? "\t" : next;
          at += 2;
        } else {
          value += text[at];
          at++;
        }
      }
      at++;
      push("string", value, start);
      continue;
    }
    if (char === "@") {
      const triple = text.startsWith("@@@", at);
      const mark = triple ? "@@@" : "@";
      const close = text.indexOf(mark, at + mark.length);
      const stop = close < 0 ? length : close;
      push("asset", text.slice(at + mark.length, stop), start);
      at = stop + mark.length;
      continue;
    }
    if (char === "<") {
      const close = text.indexOf(">", at + 1);
      const stop = close < 0 ? length : close;
      push("path", text.slice(at + 1, stop), start);
      at = stop + 1;
      continue;
    }
    if (PUNCT.has(char)) {
      push("punct", char, start);
      at++;
      continue;
    }
    let stop = at;
    while (stop < length && !/[\s()[\]{}=,;"'@<#]/.test(text[stop]!)) stop++;
    // A lone character the scan would not take (`>`): one token of its own, so the scan moves on.
    if (stop === at) stop = at + 1;
    push("word", text.slice(at, stop), start);
    at = stop;
  }
  return tokens;
}

/** A spec being read. */
interface Building {
  readonly path: string;
  readonly line: number;
  readonly specifier: "def" | "over" | "class" | null;
  readonly typeName: string | null;
  readonly arcs: AuthoredArc[];
  readonly variantSets: Map<string, string[]>;
  readonly variantSetLines: Map<string, number>;
  readonly selections: Map<string, string>;
  readonly variantSetNames: AuthoredNames[];
}

/** Reads the specs of USDA text. Never throws: what it cannot read it skips. */
class Reader {
  readonly #tokens: Token[];
  #at = 0;
  defaultPrim: string | null = null;
  readonly roots: RootPrim[] = [];
  readonly specs = new Map<string, SpecSummary>();

  constructor(text: string) {
    this.#tokens = tokenize(text);
  }

  read(): void {
    if (this.#is("punct", "(")) this.#metadata(null);
    this.#body("/", null);
  }

  #peek(offset = 0): Token | undefined {
    return this.#tokens[this.#at + offset];
  }

  #is(kind: TokenKind, text?: string, offset = 0): boolean {
    const token = this.#peek(offset);
    return token !== undefined && token.kind === kind && (text === undefined || token.text === text);
  }

  #take(): Token | undefined {
    return this.#tokens[this.#at++];
  }

  /** Skips one balanced group (`(...)`, `[...]`, `{...}`) or one token; returns the last token skipped. */
  #skipUnit(): Token | undefined {
    const first = this.#take();
    if (first === undefined || first.kind !== "punct" || OPEN[first.text] === undefined) return first;
    let depth = 1;
    let last: Token | undefined = first;
    while (depth > 0) {
      const token = this.#take();
      if (token === undefined) return last;
      last = token;
      if (token.kind !== "punct") continue;
      if (OPEN[token.text] !== undefined) depth++;
      else if (token.text === ")" || token.text === "]" || token.text === "}") depth--;
    }
    return last;
  }

  /** Skips a statement of a block (a property, a `reorder`): units until the next token is on a later line, or closes the block. */
  #skipStatement(): void {
    let last = this.#skipUnit();
    for (;;) {
      const next = this.#peek();
      if (next === undefined || last === undefined || next.line > last.end) return;
      if (next.kind === "punct" && next.text === "}") return;
      last = this.#skipUnit();
    }
  }

  /** A block's statements until its `}` (taken) or the end. */
  #body(parent: string, spec: Building | null): void {
    for (;;) {
      const token = this.#peek();
      if (token === undefined) return;
      if (token.kind === "punct" && token.text === "}") {
        this.#at++;
        return;
      }
      if (token.kind === "word" && SPECIFIERS.has(token.text)) this.#prim(parent);
      else if (token.kind === "word" && token.text === "variantSet" && this.#is("string", undefined, 1) && spec !== null) this.#variantSet(spec);
      else this.#skipStatement();
    }
  }

  #prim(parent: string): void {
    const head = this.#take()!;
    const words: string[] = [];
    while (this.#is("word")) words.push(this.#take()!.text);
    if (!this.#is("string")) return;
    const name = this.#take()!.text;
    const path = parent === "/" ? `/${name}` : parent.endsWith("}") ? `${parent}${name}` : `${parent}/${name}`;
    const specifier = head.text as "def" | "over" | "class";
    const typeName = words.length === 0 ? null : words.join(" ");
    const spec = building(path, head.line, specifier, typeName);
    if (parent === "/") this.roots.push({ name, specifier, typeName });
    if (this.#is("punct", "(")) this.#metadata(spec);
    this.specs.set(path, spec);
    if (!this.#is("punct", "{")) return;
    this.#at++;
    this.#body(path, spec);
  }

  #variantSet(owner: Building): void {
    const head = this.#take()!;
    const set = this.#take()!.text;
    const variants = owner.variantSets.get(set) ?? [];
    owner.variantSets.set(set, variants);
    owner.variantSetLines.set(set, head.line);
    if (this.#is("punct", "=")) this.#at++;
    if (!this.#is("punct", "{")) return;
    this.#at++;
    for (;;) {
      const token = this.#peek();
      if (token === undefined) return;
      if (token.kind === "punct" && token.text === "}") {
        this.#at++;
        return;
      }
      if (token.kind !== "string") {
        this.#skipUnit();
        continue;
      }
      this.#at++;
      variants.push(token.text);
      const path = `${owner.path}{${set}=${token.text}}`;
      const spec = building(path, token.line, null, null);
      if (this.#is("punct", "(")) this.#metadata(spec);
      this.specs.set(path, spec);
      if (!this.#is("punct", "{")) continue;
      this.#at++;
      this.#body(path, spec);
    }
  }

  /** A metadata block `( ... )`: the layer's (`spec` `null`) or a spec's. */
  #metadata(spec: Building | null): void {
    this.#at++;
    for (;;) {
      const token = this.#peek();
      if (token === undefined) return;
      if (token.kind === "punct" && token.text === ")") {
        this.#at++;
        return;
      }
      if (token.kind === "string" || (token.kind === "punct" && token.text === ";")) {
        this.#at++;
        continue;
      }
      if (token.kind !== "word") {
        this.#skipUnit();
        continue;
      }
      let op: ListOpKind = "explicit";
      if (LIST_OPS.has(token.text) && this.#is("word", undefined, 1)) {
        op = token.text as ListOpKind;
        this.#at++;
      }
      const key = this.#take()!.text;
      if (!this.#is("punct", "=")) continue;
      this.#at++;
      if (spec !== null && ARC_FIELDS.has(key)) spec.arcs.push(...this.#arcs(key as ArcField, op));
      else if (spec !== null && key === "variants") this.#selections(spec);
      else if (spec !== null && key === "variantSets") spec.variantSetNames.push({ op, names: this.#names() });
      else if (spec === null && key === "defaultPrim" && this.#is("string")) this.defaultPrim = this.#take()!.text;
      else this.#skipValue();
    }
  }

  /** A metadata value: units on the line it starts on. */
  #skipValue(): void {
    let last = this.#skipUnit();
    for (;;) {
      const next = this.#peek();
      if (next === undefined || last === undefined || next.line > last.end) return;
      if (next.kind === "punct" && (next.text === ")" || next.text === ";")) return;
      last = this.#skipUnit();
    }
  }

  /** An arc list op's value: `None`, one item, or a list of them. */
  #arcs(field: ArcField, op: ListOpKind): AuthoredArc[] {
    if (this.#is("word", "None")) {
      this.#at++;
      return [];
    }
    if (!this.#is("punct", "[")) {
      const one = this.#arc(field, op);
      return one === null ? [] : [one];
    }
    this.#at++;
    const items: AuthoredArc[] = [];
    for (;;) {
      const token = this.#peek();
      if (token === undefined) return items;
      if (token.kind === "punct" && token.text === "]") {
        this.#at++;
        return items;
      }
      if (token.kind === "punct" && token.text === ",") {
        this.#at++;
        continue;
      }
      const item = this.#arc(field, op);
      if (item === null) this.#skipUnit();
      else items.push(item);
    }
  }

  /** One arc: `@asset@`, `</path>`, or both, with an optional `(offset = 10; scale = 2)`. */
  #arc(field: ArcField, op: ListOpKind): AuthoredArc | null {
    const first = this.#peek();
    if (first === undefined) return null;
    let asset: string | null = null;
    let primPath: string | null = null;
    if (this.#is("asset")) asset = this.#take()!.text;
    if (this.#is("path")) primPath = this.#take()!.text;
    if (asset === null && primPath === null) return null;
    let offset = 0;
    let scale = 1;
    if (this.#is("punct", "(")) {
      this.#at++;
      for (;;) {
        const token = this.#peek();
        if (token === undefined) break;
        if (token.kind === "punct" && token.text === ")") {
          this.#at++;
          break;
        }
        if (token.kind === "word" && (token.text === "offset" || token.text === "scale") && this.#is("punct", "=", 1) && this.#is("word", undefined, 2)) {
          const value = Number(this.#peek(2)!.text);
          if (Number.isFinite(value)) {
            if (token.text === "offset") offset = value;
            else scale = value;
          }
          this.#at += 3;
          continue;
        }
        this.#skipUnit();
      }
    }
    return { field, op, asset, primPath, offset, scale, line: first.line };
  }

  /** `variants = { string finish = "chrome" ... }`. */
  #selections(spec: Building): void {
    if (!this.#is("punct", "{")) {
      this.#skipValue();
      return;
    }
    this.#at++;
    for (;;) {
      const token = this.#peek();
      if (token === undefined) return;
      if (token.kind === "punct" && token.text === "}") {
        this.#at++;
        return;
      }
      if (token.kind === "word" && this.#is("word", undefined, 1) && this.#is("punct", "=", 2) && this.#is("string", undefined, 3)) {
        spec.selections.set(this.#peek(1)!.text, this.#peek(3)!.text);
        this.#at += 4;
        continue;
      }
      this.#skipUnit();
    }
  }

  /** `"finish"` or `["finish", "look"]`. */
  #names(): string[] {
    if (this.#is("string")) return [this.#take()!.text];
    if (!this.#is("punct", "[")) {
      this.#skipValue();
      return [];
    }
    this.#at++;
    const names: string[] = [];
    for (;;) {
      const token = this.#take();
      if (token === undefined || (token.kind === "punct" && token.text === "]")) return names;
      if (token.kind === "string") names.push(token.text);
    }
  }
}

function building(path: string, line: number, specifier: Building["specifier"], typeName: string | null): Building {
  return { path, line, specifier, typeName, arcs: [], variantSets: new Map(), variantSetLines: new Map(), selections: new Map(), variantSetNames: [] };
}

/** What USDA `text` authors about composition ({@link LayerSpecs}). */
export function readLayerSpecs(text: string): LayerSpecs {
  const reader = new Reader(text);
  reader.read();
  return { defaultPrim: reader.defaultPrim, roots: reader.roots, specs: reader.specs };
}

/** A layer's default prim and root prims, as the dialog that adds an arc offers them; or why they were not read. */
export type LayerSummary =
  | { readonly kind: "read"; readonly defaultPrim: string | null; readonly roots: readonly RootPrim[] }
  | { readonly kind: "unread"; readonly reason: string };

/** The summary of a layer read ({@link LayerSpecs}). */
export function summaryOf(specs: LayerSpecs): LayerSummary {
  return { kind: "read", defaultPrim: specs.defaultPrim, roots: specs.roots };
}

/**
 * The default prim and root prims of a layer the stage has not read, from
 * its bytes: USDA text is scanned ({@link readLayerSpecs}); a crate or a
 * package is opened on a stage of its own, read and freed, nothing it
 * names fetched.
 */
export function summarizeBytes(layerstack: Pick<Layerstack, "open">, identifier: string, bytes: Uint8Array): LayerSummary {
  if (new TextDecoder().decode(bytes.subarray(0, 5)) === "#usda") return summaryOf(readLayerSpecs(new TextDecoder().decode(bytes)));
  const stage = layerstack.open({ identifier, bytes });
  try {
    const value = stage.metadata("defaultPrim");
    const defaultPrim = value?.kind === "scalar" && typeof value.value === "string" && value.value !== "" ? value.value : null;
    const roots = stage.children("/").map((path): RootPrim => {
      const prim = stage.prim(path);
      return { name: path.slice(1), specifier: prim?.specifier ?? "def", typeName: prim?.typeName ?? null };
    });
    return { kind: "read", defaultPrim, roots };
  } finally {
    stage.free();
  }
}
