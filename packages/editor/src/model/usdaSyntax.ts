// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// USDA's tokens, a line at a time, for colouring a layer's text: comments,
// strings, asset paths (`@./robot.usda@`), scene paths (`</World/Robot>`),
// the grammar's keywords (specifiers, list-op verbs, arcs, variability),
// value type names and numbers (AOUSD Core §6, the text file format). It
// colours; it does not parse: a string that spans lines is coloured on its
// first line only.

/** What a run of a line is. */
export type UsdaTokenKind = "comment" | "string" | "asset" | "path" | "keyword" | "type" | "number" | "plain";

/** A run of a line, and what it is. */
export interface UsdaToken {
  readonly text: string;
  readonly kind: UsdaTokenKind;
}

/** Specifiers, list-op verbs, arcs and the other words of the grammar. */
const KEYWORDS = new Set([
  "def",
  "over",
  "class",
  "uniform",
  "custom",
  "config",
  "rel",
  "prepend",
  "append",
  "add",
  "delete",
  "reorder",
  "references",
  "payload",
  "inherits",
  "specializes",
  "variantSet",
  "variantSets",
  "variants",
  "subLayers",
  "timeSamples",
  "spline",
  "connect",
  "None",
  "true",
  "false",
]);

/** Value type names, without `[]`. */
const TYPES = new Set([
  "bool",
  "uchar",
  "int",
  "uint",
  "int64",
  "uint64",
  "half",
  "float",
  "double",
  "timecode",
  "string",
  "token",
  "asset",
  "opaque",
  "pathExpression",
  "dictionary",
  "matrix2d",
  "matrix3d",
  "matrix4d",
  "quath",
  "quatf",
  "quatd",
  "frame4d",
  "int2",
  "int3",
  "int4",
  "half2",
  "half3",
  "half4",
  "float2",
  "float3",
  "float4",
  "double2",
  "double3",
  "double4",
  "point3h",
  "point3f",
  "point3d",
  "vector3h",
  "vector3f",
  "vector3d",
  "normal3h",
  "normal3f",
  "normal3d",
  "color3h",
  "color3f",
  "color3d",
  "color4h",
  "color4f",
  "color4d",
  "texCoord2h",
  "texCoord2f",
  "texCoord2d",
  "texCoord3h",
  "texCoord3f",
  "texCoord3d",
]);

/** One token at the start of what is left of a line: its pattern and kind, tried in order. */
const RULES: readonly (readonly [RegExp, UsdaTokenKind | "word"])[] = [
  [/^#.*/, "comment"],
  [/^"""[\s\S]*?(?:"""|$)/, "string"],
  [/^'''[\s\S]*?(?:'''|$)/, "string"],
  [/^"(?:[^"\\]|\\.)*"?/, "string"],
  [/^'(?:[^'\\]|\\.)*'?/, "string"],
  [/^@@@[\s\S]*?(?:@@@|$)/, "asset"],
  [/^@[^@]*@?/, "asset"],
  [/^<[^>]*>?/, "path"],
  [/^-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?(?![A-Za-z0-9_])/, "number"],
  [/^-?(?:inf|nan)(?![A-Za-z0-9_])/, "number"],
  [/^[A-Za-z_][A-Za-z0-9_]*(?:\[\])?/, "word"],
  [/^\s+/, "plain"],
  [/^[^\sA-Za-z0-9_#"'@<-]+|^-/, "plain"],
];

/** The tokens of one line of USDA, in order; their texts joined are the line. */
export function usdaTokens(line: string): UsdaToken[] {
  const tokens: UsdaToken[] = [];
  let rest = line;
  while (rest.length > 0) {
    let matched = false;
    for (const [pattern, kind] of RULES) {
      const found = pattern.exec(rest)?.[0];
      if (found === undefined || found.length === 0) continue;
      const word = kind === "word" ? found.replace(/\[\]$/, "") : null;
      const resolved: UsdaTokenKind = word === null ? (kind as UsdaTokenKind) : KEYWORDS.has(word) ? "keyword" : TYPES.has(word) ? "type" : "plain";
      push(tokens, found, resolved);
      rest = rest.slice(found.length);
      matched = true;
      break;
    }
    if (!matched) {
      push(tokens, rest[0]!, "plain");
      rest = rest.slice(1);
    }
  }
  return tokens;
}

/** Appends a token, merged with the last one when both are plain. */
function push(tokens: UsdaToken[], text: string, kind: UsdaTokenKind): void {
  const last = tokens.at(-1);
  if (last !== undefined && last.kind === "plain" && kind === "plain") tokens[tokens.length - 1] = { text: last.text + text, kind };
  else tokens.push({ text, kind });
}
