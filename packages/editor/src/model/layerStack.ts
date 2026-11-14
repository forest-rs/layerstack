// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's own layer stack as a tree, strongest first: the session
// layer (when the stage has one) and its sublayers, then the root and its
// sublayers, each layer's sublayers beneath it in the order it lists them
// (`subLayers`, strongest first). An opinion in a layer higher in that
// order wins over one in a layer below it (AOUSD Core §10.3.1, layer
// stacks; §7.6.1, `subLayers`). Each sublayer carries the offset its
// parent reads it through (§12.3.2.1, layer offsets), which composes down
// the tree. The other layers the stage holds are reached through arcs
// (references, payloads): they are not in the stack, and their opinions
// come in through the prims that name them.
//
// No DOM and no stage: the tree is built from what the caller reads
// (`Stage.layers`, `Stage.sublayers`, `Stage.isMuted`), so it is tested
// in Node. The pure helpers here also say how an asset path anchors to
// the layer that authors it, and how to write a path to a file from
// another layer.

/** A layer offset: stage time = layer time × `scale` + `offset`. */
export interface LayerOffset {
  readonly offset: number;
  readonly scale: number;
}

/** The identity offset: the layer's times are the stage's. */
export const IDENTITY: LayerOffset = Object.freeze({ offset: 0, scale: 1 });

/** Whether `offset` changes no time. */
export function isIdentity(offset: LayerOffset): boolean {
  return offset.offset === 0 && offset.scale === 1;
}

/**
 * The offset a layer is read at through `outer` and then `inner` (its
 * parent's offset, then its own): `outer(inner(t))`, as OpenUSD's
 * `SdfLayerOffset::operator*` composes them.
 */
export function composeOffsets(outer: LayerOffset, inner: LayerOffset): LayerOffset {
  return { offset: outer.offset + outer.scale * inner.offset, scale: outer.scale * inner.scale };
}

/** An offset in words (`offset 10`, `scale 2`, `offset 10, scale 0.5`); `null` for the identity. */
export function offsetText(offset: LayerOffset): string | null {
  if (isIdentity(offset)) return null;
  const parts = [offset.offset !== 0 ? `offset ${offset.offset}` : null, offset.scale !== 1 ? `scale ${offset.scale}` : null];
  return parts.filter((part) => part !== null).join(", ");
}

/** An offset as USDA writes it after a sublayer's asset path (`(offset = 10; scale = 2)`); empty for the identity. */
export function offsetUsda(offset: LayerOffset): string {
  if (isIdentity(offset)) return "";
  const parts = [offset.offset !== 0 ? `offset = ${offset.offset}` : null, offset.scale !== 1 ? `scale = ${offset.scale}` : null];
  return ` (${parts.filter((part) => part !== null).join("; ")})`;
}

/** The path segments of `path`, `.` dropped and `..` taking one off (kept at the front when there is nothing to take). */
function normalized(path: string): string[] {
  const out: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === ".." && out.length > 0 && out.at(-1) !== "..") out.pop();
    else out.push(part);
  }
  return out;
}

/** The directory part of `path`, with its trailing slash (`assets/` for `assets/robot.usda`); empty at the top. */
function directoryOf(path: string): string {
  return path.slice(0, path.lastIndexOf("/") + 1);
}

/** Whether `asset` is read as written: from the root (`/`), or a URL. */
function isAbsolute(asset: string): boolean {
  return asset.startsWith("/") || asset.includes("://");
}

/**
 * `identifier` without `.` segments, each `..` taking off the segment
 * before it: never the scheme and host of a URL nor the leading `/` of a
 * path from the root, and kept at the front of a relative one.
 */
function normalizedIdentifier(identifier: string): string {
  const scheme = identifier.indexOf("://");
  const slash = scheme < 0 ? -1 : identifier.indexOf("/", scheme + 3);
  const cut = scheme < 0 ? 0 : slash < 0 ? identifier.length : slash;
  const path = identifier.slice(cut);
  const absolute = path.startsWith("/");
  const out: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === ".." && out.length > 0 && out.at(-1) !== "..") out.pop();
    else if (part !== ".." || !absolute) out.push(part);
  }
  return `${identifier.slice(0, cut)}${absolute ? "/" : ""}${out.join("/")}`;
}

/**
 * The identifier `asset`, authored in the layer `anchor` (its
 * identifier), names, as the stage's host storage makes it: a relative
 * path (`./b.usda`, `../b.usda`, and `b.usda` alike: the host has no
 * search paths to look along) beside the layer; a path from the root or
 * a URL as written; `.` and `..` segments resolved. Inside a USDZ package
 * (`set.usdz[geo/rock.usda]`), beside the member, in the package. An
 * anonymous anchor (`null`, the session layer) anchors at the source's
 * top. The stage reads a sublayer from that identifier and no other: one
 * it holds no layer for is not read, whatever else it holds by a like
 * name.
 */
export function anchoredIdentifier(anchor: string | null, asset: string): string {
  if (isAbsolute(asset)) return normalizedIdentifier(asset);
  const open = anchor?.indexOf("[") ?? -1;
  if (anchor !== null && open >= 0 && anchor.endsWith("]")) {
    const member = anchor.slice(open + 1, -1);
    return `${anchor.slice(0, open)}[${normalized(`${directoryOf(member)}${asset}`).join("/")}]`;
  }
  return normalizedIdentifier(`${anchor === null ? "" : directoryOf(anchor)}${asset}`);
}

/**
 * How the layer `from` writes a path to the file `to` (both identifiers
 * under the source's top): `./lib.usda` beside it, `../shared/lib.usda`
 * elsewhere, as OpenUSD writes an anchored asset path.
 */
export function relativeAsset(from: string | null, to: string): string {
  const base = normalized(directoryOf(from ?? ""));
  const target = normalized(to);
  let common = 0;
  while (common < base.length && common < target.length - 1 && base[common] === target[common]) common++;
  const ups = base.length - common;
  const rest = target.slice(common).join("/");
  return ups === 0 ? `./${rest}` : `${"../".repeat(ups)}${rest}`;
}

/** A sublayer as a layer lists it ({@link StackInput.sublayers}). */
export interface SublayerEntry {
  /** The asset path as authored. */
  readonly asset: string;
  readonly layerOffset: LayerOffset;
}

/** What {@link layerStack} reads of the stage: the layers it holds, the root, the session layer, and each layer's sublayers. */
export interface StackInput<L extends { readonly identifier: string | null }> {
  readonly root: L;
  /** The session layer in front of the root; `null` when the stage has none. */
  readonly session: L | null;
  /** Every layer the stage holds (`Stage.layers`). */
  readonly held: readonly L[];
  /** The sublayers `layer` lists, strongest first (`Stage.sublayers`). */
  sublayers(layer: L): readonly SublayerEntry[];
  /** Whether the stage mutes `layer`, a layer it holds or an identifier (`Stage.isMuted`). */
  muted(layer: L | string): boolean;
}

/** What became of a layer of the stack. */
export type StackRowState =
  /** The stage read it, and composes it. */
  | "read"
  /** Muted: the stage composes neither it nor its sublayers. */
  | "muted"
  /** The stage holds no layer for it: its asset is missing, or not supplied yet. */
  | "unread"
  /** It is its own ancestor: a sublayer cycle, which the stage reads once. */
  | "cycle";

/** One layer of the root layer stack, as the tree shows it. */
export interface StackRow<L> {
  /** Unique in the tree: the indices down to it (`root/0/1`). */
  readonly key: string;
  /** What it is in the stack. */
  readonly role: "session" | "root" | "sublayer";
  /** The layer the stage holds; `null` when it holds none ({@link StackRowState} `unread`). */
  readonly layer: L | null;
  /** The identifier its asset anchors to (`null` for an anonymous session layer). */
  readonly identifier: string | null;
  /** The asset path as its parent authors it; `null` for the root and the session layer. */
  readonly asset: string | null;
  /** The layer that lists it; `null` for the root and the session layer. */
  readonly parent: L | null;
  /** Its place in its parent's sublayers (0 the strongest); -1 for the root and the session layer. */
  readonly index: number;
  /** How many sublayers its parent lists. */
  readonly siblings: number;
  /** How deep it is: 0 for the root and the session layer. */
  readonly depth: number;
  /** The offset its parent authors for it. */
  readonly offset: LayerOffset;
  /** The offset the stage reads it at: its ancestors' offsets composed with its own. */
  readonly composed: LayerOffset;
  /** Its place in the stack's strength order, 1 the strongest; 0 for one that does not compose (muted, unread, a cycle). */
  readonly strength: number;
  readonly state: StackRowState;
}

/** The root layer stack and the other layers the stage holds. */
export interface LayerStackTree<L> {
  /** The stack, strongest first, each layer's sublayers after it (depth first). */
  readonly rows: readonly StackRow<L>[];
  /** The layers the stage holds that are not in the stack: reached through references and payloads. */
  readonly reached: readonly L[];
}

/**
 * The stage's root layer stack in strength order, and the layers it holds
 * outside it. A sublayer the stage holds no layer for is a row of its own
 * (`unread`); a muted one is shown without its sublayers, which do not
 * compose; a layer met again beneath itself is a `cycle` and not followed.
 */
export function layerStack<L extends { readonly identifier: string | null }>(input: StackInput<L>): LayerStackTree<L> {
  const byIdentifier = new Map<string, L>();
  for (const layer of input.held) if (layer.identifier !== null) byIdentifier.set(layer.identifier, layer);
  const rows: StackRow<L>[] = [];
  const inStack = new Set<L>();
  let strength = 0;
  const visit = (
    layer: L | null,
    identifier: string | null,
    place: Pick<StackRow<L>, "key" | "role" | "asset" | "parent" | "index" | "siblings" | "depth" | "offset" | "composed">,
    ancestors: ReadonlySet<L>,
  ) => {
    const state: StackRowState =
      layer === null ? (identifier !== null && input.muted(identifier) ? "muted" : "unread") : ancestors.has(layer) ? "cycle" : input.muted(layer) ? "muted" : "read";
    rows.push({ ...place, layer, identifier, state, strength: state === "read" ? ++strength : 0 });
    if (layer === null || state !== "read") return;
    inStack.add(layer);
    const below = new Set(ancestors).add(layer);
    const sublayers = input.sublayers(layer);
    sublayers.forEach((entry, index) => {
      const identifier = anchoredIdentifier(layer.identifier, entry.asset);
      visit(
        byIdentifier.get(identifier) ?? null,
        identifier,
        {
          key: `${place.key}/${index}`,
          role: "sublayer",
          asset: entry.asset,
          parent: layer,
          index,
          siblings: sublayers.length,
          depth: place.depth + 1,
          offset: entry.layerOffset,
          composed: composeOffsets(place.composed, entry.layerOffset),
        },
        below,
      );
    });
  };
  const top = { asset: null, parent: null, index: -1, siblings: 0, depth: 0, offset: IDENTITY, composed: IDENTITY };
  if (input.session !== null) visit(input.session, input.session.identifier, { ...top, key: "session", role: "session" }, new Set());
  visit(input.root, input.root.identifier, { ...top, key: "root", role: "root" }, new Set());
  const reached = input.held.filter((layer) => !inStack.has(layer) && !rows.some((row) => row.layer === layer));
  return { rows, reached };
}

/**
 * Where the sublayer a row named is in `sublayers` now. A row names its
 * sublayer by the asset path its parent authored and the index it had
 * when the tree was built, and the edits address `subLayers` by index
 * (`SdfLayer::RemoveSubLayerPath(int)`, `SetSubLayerOffset`), so an
 * action that ran in between (an insert whose fetch was still running,
 * an undo) leaves the index naming another sublayer. The index when the
 * asset is still there; else where the asset is, when one entry authors
 * it; else -1: the sublayer is gone, or cannot be told from another.
 */
export function sublayerIndex(sublayers: readonly SublayerEntry[], index: number, asset: string): number {
  if (sublayers[index]?.asset === asset) return index;
  const first = sublayers.findIndex((entry) => entry.asset === asset);
  return first >= 0 && sublayers.findIndex((entry, at) => at > first && entry.asset === asset) < 0 ? first : -1;
}

/**
 * A layer's sublayers with the one at `index` moved by `by` places
 * (negative: stronger, toward the front), offsets kept: what
 * `setSublayers` authors to reorder them. `null` when it would leave the
 * list.
 */
export function movedSublayers(sublayers: readonly SublayerEntry[], index: number, by: number): SublayerEntry[] | null {
  const to = index + by;
  if (index < 0 || index >= sublayers.length || to < 0 || to >= sublayers.length || by === 0) return null;
  const list = [...sublayers];
  const [moved] = list.splice(index, 1);
  list.splice(to, 0, moved!);
  return list;
}

/** A file the source holds, as the Add sublayer picker offers it for one layer. */
export interface SublayerCandidate {
  /** Its identifier under the source's top. */
  readonly identifier: string;
  /** How the layer would author it ({@link relativeAsset}). */
  readonly asset: string;
  /** Why it cannot be added there; `null` when it can. */
  readonly refusal: string | null;
}

/**
 * The layers `listed` by the source, as sublayers `parent` (its
 * identifier) could add: each with the asset path it would author, and
 * why it cannot be added when it cannot (the layer itself, one it lists
 * already, or one above it in the stack, which would make a cycle).
 */
export function sublayerCandidates(
  listed: readonly string[],
  parent: string | null,
  present: readonly string[],
  above: readonly string[],
): SublayerCandidate[] {
  const listedNow = new Set(present);
  const ancestors = new Set(above);
  return listed.map((identifier) => ({
    identifier,
    asset: relativeAsset(parent, identifier),
    refusal:
      identifier === parent
        ? "the layer itself"
        : listedNow.has(identifier)
          ? "listed already"
          : ancestors.has(identifier)
            ? "above it in the stack: a cycle"
            : null,
  }));
}

/** The identifiers of the layers above `row` in its branch of the tree: its parent, its parent's parent, up to the root. */
export function ancestorsOf<L>(rows: readonly StackRow<L>[], row: StackRow<L>): (string | null)[] {
  const out: (string | null)[] = [];
  let key = row.key;
  for (;;) {
    const slash = key.lastIndexOf("/");
    if (slash < 0) break;
    key = key.slice(0, slash);
    const parent = rows.find((each) => each.key === key);
    if (parent === undefined) break;
    out.push(parent.identifier);
  }
  return out;
}

/** What a reload did, as {@link reloadText} says it: the layers by identifier. */
export interface ReloadSummary {
  readonly reloaded: readonly { readonly identifier: string | null }[];
  readonly skipped: readonly unknown[];
  /** The asset paths of the layers whose new content could not be read. */
  readonly failed: readonly string[];
  /** The identifiers of the layers the source no longer has. */
  readonly gone: readonly string[];
  /** The undo and redo steps forgotten. */
  readonly forgotten: number;
}

/**
 * What a reload did, in a sentence or a few: the layers read again, or
 * that nothing changed only when nothing did; a layer whose new content
 * could not be read or that the source no longer has is said, not passed
 * over as unchanged; then the layers left for their unsaved edits and the
 * undo history forgotten.
 */
export function reloadText(report: ReloadSummary, name: (identifier: string | null) => string): string {
  const troubled = report.failed.length > 0 || report.gone.length > 0;
  const parts = [
    report.reloaded.length > 0 ? `Read again: ${report.reloaded.map((layer) => name(layer.identifier)).join(", ")}` : troubled ? null : "Nothing changed in the source",
    report.failed.length > 0 ? `could not be read, kept as before: ${report.failed.join(", ")}` : null,
    report.gone.length > 0 ? `no longer in the source, kept as read: ${report.gone.map(name).join(", ")}` : null,
    report.skipped.length > 0 ? `${report.skipped.length} with unsaved edits left as they are` : null,
    report.forgotten > 0 ? `the undo history was forgotten (${report.forgotten} ${report.forgotten === 1 ? "step" : "steps"})` : null,
  ].filter((part) => part !== null);
  const text = parts.join("; ");
  return `${text.charAt(0).toUpperCase()}${text.slice(1)}.`;
}
