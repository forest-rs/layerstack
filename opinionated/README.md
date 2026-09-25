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
- sparse array edit programs over any element type, with a host-supplied
  fill policy
- recursive dictionary combination over host values, with shallow overlay as
  a separate named policy
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
requested position.

## Dictionaries

`opinionated` owns dictionary combination. `combine_dictionary_chain` follows
AOUSD Core §6.6.2.1: keys from every opinion are kept, a stronger value wins a
key collision, and when both colliding values are dictionaries they combine
recursively. Output is ordered by key at every nesting level, whether a level
was merged or contributed by a single opinion, as OpenUSD's `VtDictionary`
(a `std::map`) is. Already-ordered levels are detected with one linear scan and
cloned as-is; only unordered levels are rebuilt.

The crate has no value type of its own, so a host exposes nesting through a
small `DictionaryAdapter`: whether a value is a dictionary, its entries, and
how to wrap combined entries back into a value. There is no universal value
enum, domain dependency, or serialization model. `layerstack` implements the
adapter for its `Value` and delegates USD dictionary resolution to this
kernel, keeping only the USD-specific choice of which opinions participate.

A chain folds strongest-first. Recursive combination is not associative when a
key holds a dictionary in one opinion and a non-dictionary in another:
strongest-first over `{s: {a: 1}}`, `{s: 0}`, `{s: {b: 2}}` gives
`{s: {a: 1, b: 2}}`, while weakest-first gives `{s: {a: 1}}`. The spec is
silent on chain order, so OpenUSD governs (AOUSD Core §4.2), and OpenUSD folds
strongest-first. The weakest-first `OpinionFamily` kernel is therefore not
used for recursive dictionaries.

`combine_dictionary_chain_report` combines the same chain and records, for
each dictionary, the key paths it supplied, the nested dictionaries it merged
with stronger ones, and the entries a stronger value overrode. Hosts attach
their own provenance to each dictionary, so a diagnostic can say which
opinion a combined entry came from. The lean `combine_dictionary_chain` shares
the fold and records nothing.

`ShallowOverlay` is an explicitly distinct policy: values are opaque and the
stronger value wins a collision outright, even between nested dictionaries.
The `OpinionOp::Dictionary` enum API uses it because its `V` exposes no
structure; hosts with nested values call `combine_dictionary_chain` with their
own adapter.

## Sparse Array Edits

`ArrayEdit<T>` is a program of instructions that rewrites a dense `Vec<T>` in
order: write, insert and erase elements from literals or by copying from an
index, and bound or set the length. Each instruction reads the array as edited
so far, and an instruction whose index does not resolve is skipped. The
instruction set and index rules follow OpenUSD's `VtArrayEdit`
(`pxr/base/vt/arrayEditOps.h`): negative indices count from the end, and only
an insertion can target `ArrayIndex::End`.

The element type is the host's, and so is the element that fills growth when
an instruction carries no fill of its own. The host supplies it through
`ArrayFill`: an `Option<T>` is a fixed, possibly missing fill, and `FillWith`
computes the element only when growth needs it. `T: Default` is never
required, and a missing fill never invents an element: the growth is skipped.

`ArrayEdit::compose_over` concatenates a weaker program before a stronger one,
so applying the composed edit equals applying the weaker edit and then the
stronger one. That lets a host fold edits through an `OpinionFamily`, keeping
them sparse until a dense value or block ends the chain. `layerstack` uses
`ArrayEdit<Value>` for USD sparse array edits and derives the fill from the
property's type.
