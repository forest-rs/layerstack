---
id: lay-v1mn
status: closed
deps: [lay-rsdo]
links: []
created: 2026-03-13T18:07:57Z
type: feature
priority: 2
assignee: Bruce Mitchener
parent: lay-p1nr
tags: [file-formats, spec-alignment]
---
# USDC (binary crate) reader

Implement a USDC binary format reader per §16.3. The crate format is a binary encoding of layers with a bootstrap section, table of contents, and six sections: Tokens, Strings, Fields, FieldSets, Paths, and Specs. Values are encoded via value representations (§16.3.9) with support for inlining, compression (LZ4 for sections, custom integer compression for arrays), and lookup tables for floating point arrays. The format has evolved through versions 0.0.1 to 0.12.0, each adding features (path expressions at 0.10.0, relocates at 0.11.0, splines at 0.12.0).

## Design

The reader pipeline: (1) validate bootstrap magic bytes and version (§16.3.2), (2) read TOC to locate sections (§16.3.3), (3) decompress sections with LZ4 (§16.3.4), (4) parse tokens and strings, (5) parse fields and field sets, (6) reconstruct path hierarchy from the paths section (§16.3.7), (7) assemble specs from the specs section using field set indices (§16.3.8), (8) parse value representations on demand (§16.3.9). Keep in a separate crate. Consider memory-mapping for large files.

## Acceptance Criteria

Can read all USDC files in the conformance suite. Produces identical Layer structures to USDA parsing of the same content. Handles all value types in §16.3.10. Correctly decompresses LZ4 and compressed integer arrays.

