# Sparse Array Edits Execution Plan

## Goals

- Add a core sparse array edit value family to `layerstack`.
- Preserve enough property declaration metadata to resolve typed array edits.
- Refactor value resolution so sparse array opinions compose over weaker opinions, including time samples.
- Extend `layerstack_usda` to parse and emit authored `edit(...)` values.
- Extend `layerstack_usdc` behind an explicit cargo feature for experimental sparse array edit decoding/assembly.
- Keep the work as a sequence of clean, coherent commits.

## Non-goals

- Generalize the new resolver to every possible future sparse value family in the first slice.
- Add new production dependencies.
- Implement stable public serialization for experimental USDC sparse array edits without a feature gate.

## Migration Note

- Public API changes are expected in `layerstack`:
  - `Value` will gain a sparse array edit variant.
  - Property declaration metadata will be preserved in the core document model.
  - Stage resolution semantics for array-valued fields will broaden from strongest-wins to composed sparse resolution.

## Phases

1. Data model foundation
   - Add property declaration metadata to the core document model and composition index.
   - Add sparse array edit kernel types and instruction semantics.
2. Resolution
   - Move composed-array resolution into a dedicated resolver module.
   - Integrate fallback seeding and time-sampled sparse composition.
3. USDA
   - Parse `edit(...)` as an authored value and emit it into the core model.
4. USDC
   - Add experimental sparse array edit decoding/assembly behind a cargo feature.
5. Validation
   - Add focused unit/integration tests and run fmt/clippy/test slices.

## Risks

- Property type information is currently discarded; sparse array edits require it for `minsize` / `resize`.
- Schema fallback is currently bolted on after authored resolution; sparse composition needs fallback as part of the weakest opinion chain.
- USDC support is inherently provisional until the proposal settles on stable wire encoding details.
