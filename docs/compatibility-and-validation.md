# Compatibility and validation

Support is a claim about a specific operation, representation and tested scope.
The [workspace status](../README.md#status-and-conformance) and individual crate
guides describe the current subset. This note defines how to extend that evidence
without turning fixture coverage into a blanket conformance claim.

## Authorities and versions

Keep these authorities identifiable:

- **AOUSD Core requirements:** cite the relevant section close to semantics-changing
  code. The conformance harness records its specification and supplemental inputs
  in [its guide](../layerstack_conformance/README.md).
- **Versioned OpenUSD behavior:** record the release and oracle inputs. Generated
  schemas expose `layerstack_schemas::OPENUSD_VERSION`; several differential tests
  assert that version. Updating a wheel must not silently redefine expected results.
- **Extensions and proposals:** identify their contract and tested interpretation.
  Publication of a proposal does not establish implementation or compatibility.
- **Destination profiles:** validate the actual emitted representation, including
  restrictions on instancing, root format, assets and supported value types.

When an implementation oracle disagrees with the chosen specification contract,
keep a named, explained divergence and a minimal case. Do not turn the difference
into an unconditional success or change the baseline without review. The
[temporal sparse tests](../layerstack_conformance/tests/conformance/temporal_sparse.rs) and
[sparse composition note](generic-sparse-composition.md) show this distinction.

Schema identifiers and unknown authored metadata should survive supported
read/write paths without being silently upgraded. Preserving a declaration is a
different claim from composing its schema family/version or evaluating its domain
semantics. Connection storage, domain validation and graph execution also require
separate evidence; discovering a shader source does not execute it.

## State the operation being tested

| Claim | Evidence needed |
| --- | --- |
| Read/preserve | Authored types, slots, ordering and relevant unknown fields survive the supported path |
| Compose | Ordered occurrences, namespace/time mappings and resulting opinions match the contract |
| Evaluate | Defaults, samples, interpolation, blocks, fallbacks and errors agree for the query context |
| Write | The destination represents the input under its explicit format/profile limits |
| Interoperate | An independent implementation reads and interprets the result as intended |

A same-library round trip can preserve a shared reader/writer mistake. Add an
independent reader or oracle for new representation semantics. For incrementality,
compare the updated stage with a fresh composition of the same final authored
inputs. Compare source evidence and diagnostics where relevant, as well as values.

Order and multiplicity matter: sorting source stacks or deduplicating repeated
occurrences can hide composition bugs. Check authored absence separately from an
explicit value, and compare float bits when representation fidelity is the
contract. Tolerance is appropriate only when the operation explicitly permits it.
Explanations must return the same value or failure as their corresponding resolve
query; [explanation tests](../layerstack_conformance/tests/conformance/explain_consistency.rs)
exercise that invariant.

## Failure, recovery and bounded work

An import result can contain a usable partial scene and errors. Preserve structured
causes, source/member identity and locations across bare files, packages and nested
packages. Applications select recovery policy using typed diagnostics, rather
than matching display strings. See [USDZ import guidance](../layerstack_usdz/README.md)
and [I/O policy](../layerstack_io/README.md).

Successful directory inventory does not prove every field can decode or every
dependency can compose. Retained numeric reads can fail on first demand. Validate
both inspected and demanded paths, including error propagation from save/export.
Resource limits should reject work before the corresponding allocation or decode;
test oversized counts, shared field references and failed compressed candidates.
Decoder work units, encoded bytes and live native allocations are different metrics.
The [USDC robustness tests](../layerstack_conformance/tests/usdc_robustness.rs)
include malformed-input and allocation checks.

For package workflows, check dependency anchors, nested-member identity, moved
package reopen and missing resources. Validate the root after installing the
resolved layers; a successful ZIP read alone is insufficient. Intentional external
dependencies must remain visible in the preparation report and acceptance policy.

## Reproducing the evidence

Run from the repository root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

For optional native checks, select a Python with the pinned OpenUSD bindings:

```sh
LAYERSTACK_USD_PYTHON=/path/to/python cargo test -p layerstack_conformance --all-features
```

Inspect skips and oracle versions. A green run without the external tools does
not establish that optional interoperability checks ran. Exporter validators and
optional rendered checks are described in the
[conformance guide](../layerstack_conformance/README.md#exporter-interoperability-optional).
Schema regeneration/check inputs are described in
[the generator guide](../layerstack_schemagen/README.md).

Preserve regressions as small deterministic fixtures or tests beside the owning
behavior. Review reports and scratch outputs are useful investigation evidence;
they are not a maintained support matrix. Performance claims additionally follow
the [measurement protocol](../wind_tunnel/MEASUREMENT.md).
