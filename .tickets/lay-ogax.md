---
id: lay-ogax
status: closed
deps: []
links: []
created: 2026-03-13T18:08:56Z
type: feature
priority: 2
assignee: Bruce Mitchener
tags: [file-formats, composition, spec-alignment]
---
# Asset resolution interface

Implement the resource/asset resolution interface per §9. Asset identifiers (§9.2) are URIs that locate layers, textures, and other resources. Resolution involves: protocol handling (§9.3), relative identifier resolution (§9.4), search path resolution (§9.5), extension resolution (§9.6), and packaged resource resolution (§9.7, for USDZ). This is needed for any multi-file USD workflow — references and payloads specify asset paths that must be resolved to actual layer data.

## Design

Define an AssetResolver trait that maps asset paths to layer data. The default resolver handles file:// and relative paths. USDZ resolution wraps the package reader. The resolver is consulted during composition when references/payloads/sublayers specify asset paths. Variable substitution in asset paths should also be supported.

## Acceptance Criteria

Relative asset paths resolve correctly relative to the referring layer. USDZ-internal paths resolve within the package. Search path resolution works. Extension resolution (.usd → .usda/.usdc) works.

