# opinionated

`opinionated` is a small `no_std` crate for resolving layered sparse opinions
over typed addresses.

The crate intentionally does not model any particular domain. Addresses can be
settings scopes, document sections, game items, or any other stable typed key.
The resolver only knows layer strength, address/field identity, authored
operations, and provenance.

## Scope

`opinionated` owns:

- explicit layer strength ordering
- sparse `(address, field)` opinion storage with set/remove/clear editing
- storage-agnostic ordered-chain resolution
- scalar set/block resolution
- ordered unique-list edits
- shallow dictionary composition
- provenance, opinion-stack inspection, and key enumeration
- explanation reports for diagnostics

It explicitly does not own:

- namespaces or tree population
- schemas or domain validation
- references, variants, or asset loading
- table storage or binary encoding
- materialization into a domain artifact

## Current Shape

Layers are supplied strongest-to-weakest through `SparseComposer::try_new` and
are fixed for the lifetime of the composer. Duplicate layers are rejected at
construction time. Each layer holds at most one opinion per typed
`(address, field)` key: `set_opinion` replaces any previous opinion the same
layer authored for that key, `remove_opinion` retracts it, and `clear_layer`
retracts everything a layer authored.

Resolution is a three-way outcome. `Resolution::Absent` means no opinions are
authored for the key, `Resolution::Blocked` means the strongest opinion
suppresses the value (and carries the block's provenance), and
`Resolution::Resolved` carries the composed value plus provenance from the
strongest contributing opinion.

Callers that already own sorted opinion stacks can bypass `SparseComposer` and
use `resolve_ordered_chain` or `resolve_ordered_chain_report` directly. This
keeps the resolver reusable for systems that have their own storage, indexing,
or materialization pipeline. The plain resolver does no diagnostic bookkeeping;
the report variant records how every opinion in the chain was interpreted.

Mixed operation families are intentionally not coerced. The strongest
non-block operation selects the resolved value family; weaker incompatible
operations are ignored and reported. A scalar `Set` is strongest-wins. A
`Block` stops all weaker opinions.

List edits follow the `ListOps` semantics of AOUSD Core §12.4 as implemented
by `layerstack`: an authored explicit list makes the other edits in the same
operation spurious, and re-inserting an existing item moves it to the
requested position. Dictionary composition is shallow: entries combine by key
only, stronger entries win, and output is ordered by key.
