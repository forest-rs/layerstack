---
id: lay-on89
status: open
deps: []
links: []
created: 2026-03-13T18:07:46Z
type: feature
priority: 1
assignee: Bruce Mitchener
parent: lay-p1nr
tags: [file-formats, spec-alignment, blocking]
---
# Production USDA parser

Implement a production-quality USDA (text format) parser per §16.2. Currently a minimal parser exists in the conformance crate that handles only sublayers, basic prim defs, and simple attributes. A full parser must handle: all prim specifiers (def/over/class), all composition arc metadata (references, payloads, inherits, specializes, variantSets, relocates), variant set blocks, attribute types and values (including arrays, timeSamples, connections), relationship declarations and targets, all metadata fields, nested dictionaries (customData), string/token escaping, comments, and the full PEG grammar from §16.2. This is the highest-priority format task as it unblocks all ~120 remaining conformance fixtures.

## Design

The grammar is fully specified as PEG in §16.2. Consider using a PEG parser generator (pest, pom) or hand-written recursive descent. The parser should produce Layer/PrimSpec structures compatible with the existing doc.rs model. Keep it in a separate crate (e.g., layerstack_usda) to avoid adding parser dependencies to the no_std core. Must handle UTF-8 identifiers per §7.3.3 (XID_Start/XID_Continue).

## Acceptance Criteria

Can parse all conformance fixture USDA files. Produces correct Layer structures that compose identically to the current hand-built test fixtures. Handles all §16.2 grammar productions.


## Notes

**2026-03-16T18:22:11Z**

Design decisions locked in:
- Separate crate: layerstack_usda (no_std default, optional std feature for file I/O)
- Hand-written recursive descent parser
- Lossless CST (preserves whitespace, comments, all formatting)
- Own AST types (not Layer/PrimSpec — separate lowering step)
- Error recovery (partial trees + diagnostics on malformed input)
- Lexer foundation landed with 39 tests (spans, token kinds, lossless roundtrip)
- Full PEG grammar extracted from AOUSD Core §16.2 for reference

Crate structure:
  lexer.rs  — tokenizer with spans (done)
  span.rs   — Span + TextPosition (done)
  cst.rs    — lossless syntax tree (next)
  ast.rs    — typed abstract tree
  lower.rs  — CST → AST
  bridge.rs — AST → Layer/PrimSpec
  error.rs  — diagnostics
