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

## Follow-On: Generic Sparse Composition Kernel

### Endpoint

- `Stage` no longer knows about arrays as a special sparse-resolution case.
- Sparse-family detection and strong-over-weak folding live in an internal
  resolver module.
- Default-valued and time-sampled sparse resolution flow through the same
  internal family pipeline.
- Arrays remain the only shipped sparse family in this branch, but the internal
  seam is ready for another family without reopening `Stage`.

### Phases

1. Extract
   - Move sparse array resolution out of `stage.rs` into
     `layerstack/src/value_resolution.rs`.
2. Centralize
   - Make `Stage` call the resolver module rather than array-specific helpers.
3. Generalize internally
   - Introduce an internal sparse-family discriminator and family-owned
     composition API.
4. Unify sampled/default folding
   - Route default and time-sampled sparse resolution through a shared internal
     fold model.
5. Add invariant tests
   - Assert dense termination, sparse-over-sparse associativity, fallback
     seeding, and blocking behavior.
6. Stop there
   - Do not force a second sparse family into this branch unless one is
     genuinely ready.

## Spec Identity + defaultPrim Execution Plan

### Fence

`Path` owns concrete prim namespace paths. It explicitly does not own
variant-qualified opinion provenance or implicit arc targets; those belong to a
separate spec-identity layer and to reference/payload target resolution.

### Goals

- Add a first-class `SpecPath` identity model for composed opinion sources.
- Separate provenance identity from the concrete prim path used to look up
  authored `PrimSpec` data.
- Preserve variant-qualified source identities through `Stage` inspection APIs
  and conformance tests.
- Model references/payloads with an explicit authored target kind so omitted
  prim targets remain distinguishable from explicit `</Foo>` or `/`.
- Add `defaultPrim` layer metadata to USDA/USDC ingestion and compose omitted
  reference/payload targets through it.

### Non-goals

- Generalize `Path` into a universal spec/property/variant path syntax type.
- Implement relocates or value clips in this branch.
- Add a plugin-style provenance system.

### Migration Note

- Public API changes are expected in `layerstack`:
  - `OpinionKey::spec_path` and `Stage::prim_stack()` will expose `SpecPathId`
    instead of `PathId`.
  - `Stage` will expose composed `SpecPath` inspection helpers.
  - `Reference`/payload arc targets will move from always-concrete `PathId` to
    an explicit authored target enum that can represent implicit `defaultPrim`.
  - `Layer` will preserve `default_prim` metadata.

### Phases

1. Spec identity foundation
   - Add `spec_path.rs` with `SpecPath`, `SpecPathId`, and `SpecPathInterner`.
   - Thread a concrete lookup prim path separately from spec provenance through
     `OpinionKey`.
2. Composition integration
   - Intern spec identities while composing local, variant, inherit, reference,
     payload, and specializes opinions.
   - Update `Stage` inspection APIs and conformance harnesses to use the new
     spec identities directly.
3. Arc target modeling
   - Add an explicit reference target enum for authored prim-path vs implicit
     `defaultPrim`.
   - Preserve `defaultPrim` metadata on layers.
4. USDA + USDC
   - Parse/emit layer `defaultPrim` metadata in USDA.
   - Decode USDC pseudo-root `defaultPrim` and omitted arc targets into the new
     model.
5. Validation
   - Unignore the variant-qualified source identity conformance cases.
   - Add focused `defaultPrim` tests and reduce ignored composition cases to
     narrower remaining gaps.

### Risks

- Variant provenance is currently intertwined with actual prim lookup paths in
  `OpinionKey`; this refactor touches most composition entry points.
- `defaultPrim` behavior must preserve the authored distinction between an
  omitted target and an explicit prim path.
- USDC pseudo-root metadata is currently only partially assembled, so
  `defaultPrim` support has to fit that code path cleanly.

## Follow-On: Propagated Variant Provenance Through Arcs

### Endpoint

- Variant-qualified opinion provenance retains the host path for every variant
  selection, including nested selections and repeated variant-set names.
- `SpecPath` can represent multi-host variant qualification along one concrete
  prim path instead of only one host-path insertion point.
- Composition forwarding sites build provenance through one shared helper
  rather than ad hoc `variant_spec_path(...)` calls.
- The remaining ignored PCP composition tests that only require propagated
  variant-qualified provenance are unignored and passing.

### Goals

- Preserve host-aware outer selection context in `VariantSpec`.
- Centralize spec-path construction/remapping for local, variant, reference,
  payload, inherit, and specializes forwarding.
- Unblock the current ignored PCP cases around nested variants and
  variant-qualified prim-stack/source provenance.

### Non-goals

- Implement fallback variant selection supplied externally by the AOUSD PCP
  test harness.
- Implement unrelated missing features like relocates or value clips in the
  same branch.

### Migration Note

- Public API changes are expected in `layerstack`:
  - Variant provenance metadata in the document model will carry host-aware
    selection sites instead of plain `(set, variant)` pairs.
  - Internal spec-path construction helpers will move from single-host builders
    to a more general provenance context.

### Phases

1. Provenance model
   - Add a host-aware variant selection site type.
   - Store outer/required variant context using that type in the document
     model.
2. Spec-path construction
   - Extend `SpecPath` with multi-host construction from a concrete prim path
     plus ordered selection sites.
   - Add focused tests for nested hosts and repeated set names.
3. USDA emission
   - Record nested variant context with host paths when emitting `VariantSpec`s,
     including repeated set names on different hosts.
4. Composition integration
   - Replace ad hoc variant provenance builders in `compose.rs` with shared
     helpers that preserve host context across arc forwarding.
5. Validation
   - Unignore the remaining provenance-related PCP cases.
   - Add targeted regression tests for nested references, payloads, inherits,
     specializes, and remapped descendants carrying variant-qualified sources.

### Risks

- Existing `VariantSpec::merge` behavior currently collapses nested contexts
  that share the same set/variant names; the host path must become part of the
  identity without making merge order unstable.
- Provenance propagation appears in many code paths; partial conversion could
  easily produce mixed single-host and multi-host behavior.

## Follow-On: Remapped Ancestor Selection Roots

### Endpoint

- Forwarded arc composition resolves variant selections against the correct
  selection root even when the source provenance path and the strongest
  selection live on different remapped ancestors.
- Prim-stack provenance for forwarded inherits/specializes/reference/payload
  opinions retains the variant-qualified source path expected by the remaining
  PCP fixtures.
- The remaining provenance-related ignored PCP cases are reduced to the known
  external-fallback and payload-through-subroot gaps.

### Goals

- Replace ad hoc `dest_path` vs `source_path` selection lookups with one
  explicit selection-root model.
- Distinguish three paths during forwarding:
  destination path, source authored path, and selection-root path.
- Unblock the remaining PCP cases around ancestral selections, weaker remapped
  selections, and forwarded prim-stack provenance under inherits/specializes.

### Non-goals

- Implement the AOUSD PCP harness's external fallback selections.
- Implement nested payload-through-subroot or self-payload semantics.

### Phases

1. Failure triage
   - Reproduce the six remaining provenance failures and group them by shared
     selection-root behavior.
2. Selection-root model
   - Add a small helper/context type for forwarded opinions that carries the
     destination path, source path, and selection-root path separately.
3. Forwarding integration
   - Route inherits, specializes, references, and payloads through the shared
     selection-root helper.
4. Validation
   - Unignore the newly passing PCP cases and keep the known non-goals ignored.

### Risks

- Some remaining cases may depend on ancestor remapping that differs between
  value resolution and prim-stack/source provenance.
- Payload forwarding may still expose the older subroot gap; selection-root
  cleanup should not accidentally blur that boundary.

## Path / Spec Identity Cleanup

### Fence

`path.rs` owns concrete scene-namespace identities such as prim and property
paths. It explicitly does not own variant-qualified provenance; that remains in
`spec_path.rs`.

### Endpoint

- `layerstack` has a first-class `PropertyPath` for concrete authored property
  identity.
- Property-path parsing/formatting lives in one core implementation instead of
  ad hoc string splitting in USDA/USDC code.
- `Stage` can answer common queries directly from a parsed property path.
- `SpecPath` stays provenance-oriented, but gains calmer integration points
  with concrete property identity.

### Non-goals

- Introduce a universal path algebra that merges prim paths, property paths,
  target paths, and variant-qualified provenance into one type.
- Change list-op target storage away from `PathId` in this branch.
- Implement relocates or target/connection-path syntax in the same slice.

### Migration Note

- Public API changes are expected in `layerstack`:
  - `PropertyPath` will be added and re-exported from the crate root.
  - `InMemoryStore` and `Stage` will gain property-path-based helpers in
    addition to the existing prim-plus-field APIs.
  - Internal USDA/USDC property-path parsing will move to the shared core
    parser.

### Phases

1. Core path model
   - Add `PropertyPath` parsing, formatting, and construction in `path.rs`.
   - Keep `Path` as the concrete prim-path type.
2. Stage/query integration
   - Add `Stage` helpers that resolve/explain values via `PropertyPath`.
   - Add `InMemoryStore` convenience parsing for property paths.
3. File-format integration
   - Replace local property-path splitters in USDA/USDC assembly with the
     shared core type where the path is concrete rather than provenance-only.
4. Validation
   - Add focused path parsing/display tests and query tests.
   - Run workspace verification and note remaining identity gaps for later
     slices (target paths, richer spec identities, relocates).
5. Concrete target identity
   - Add a first-class `TargetPath` for relationship/connection targets.
   - Replace fake dotted `PathId` targets with structured prim-or-property
     identity across doc/storage, USDA/USDC parsing, and composition remapping.
6. Public API follow-through
   - Add `PropertyPath`-based query helpers for typed property resolution.
   - Add `Layer` editing helpers that author property fields via
     `PropertyPath` instead of separate prim/token inputs.

### Risks

- USDC currently mixes concrete property paths and variant-qualified spec paths
  in a few helper functions; the branch must keep those responsibilities
  separate.
- It is easy to over-generalize into a too-clever path abstraction; this slice
  should stay concrete and boring.
