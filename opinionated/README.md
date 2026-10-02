# opinionated

`opinionated` is a small `no_std` crate for resolving layered sparse opinions
over typed addresses.

The crate intentionally does not model any particular domain. Addresses can be
settings scopes, document sections, game items, or any other stable typed key.
The resolver only knows layer strength, address/field identity, authored
operations, and provenance.

[API documentation](https://docs.rs/opinionated) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/opinionated)

## Installation

```toml
[dependencies]
opinionated = "0.1"
```

Requires Rust **1.89** or later. The crate uses `no_std` with `alloc`, has no
dependencies, and has no feature flags. An allocator is required; there is no
allocation-free mode.

## Getting started

Supply layers strongest-to-weakest, then author opinions at address/field keys.
Removing a stronger opinion reveals the weaker one again:

```rust
use opinionated::{OpinionOp, SparseComposer};

// Layer, address, field, scalar value, list item, dictionary key, provenance.
type Settings = SparseComposer<
    &'static str, &'static str, &'static str, &'static str,
    &'static str, &'static str, &'static str,
>;

let mut settings = Settings::try_new(["project", "defaults"]).unwrap();
settings.set_opinion(
    "defaults", "workspace", "theme", OpinionOp::Set("light"), "defaults file",
).unwrap();
settings.set_opinion(
    "project", "workspace", "theme", OpinionOp::Set("dark"), "project file",
).unwrap();

let result = settings.resolve("workspace", "theme").resolved().unwrap();
assert_eq!(result.value.as_scalar(), Some(&"dark"));
assert_eq!(result.provenance, "project file");

settings.remove_opinion("project", "workspace", "theme").unwrap();
let result = settings.resolve("workspace", "theme").resolved().unwrap();
assert_eq!(result.value.as_scalar(), Some(&"light"));
```

Keys and provenance can be your own types. The crate does not require strings
or prescribe what a provenance value identifies.

## Choosing an API

| Need | Entry point |
| --- | --- |
| Store sparse opinions and edit them by layer | `SparseComposer` |
| Resolve an already ordered stack of scalar, list, or shallow dictionary opinions | `resolve_ordered_chain` |
| Resolve borrowed list opinions with a fallback seed | `resolve_list_chain` |
| Explain that resolution | `SparseComposer::explain` or `resolve_ordered_chain_report` |
| Fold a host-defined value/edit family | `OpinionFamily` and `resolve_family_chain` |
| Merge nested dictionaries represented by host values | `DictionaryAdapter` and `combine_dictionary_chain` |
| Execute or compose sparse array edit programs | `ArrayEdit<T>` |

`OpinionFamily` classifies operations as dense values, sparse edits, blocks,
or foreign operations. The caller selects the family; the kernel accumulates
edits until a dense value, block, or the end of the chain, then applies them
weakest-first. `ScalarFamily`, `ListFamily`, and `DictionaryFamily` adapt the
built-in operations. `DictionaryFamily` uses shallow overlay; recursive
dictionary composition has its own strongest-first fold, described below.

## Scope

`opinionated` owns:

- explicit layer strength ordering
- sparse `(address, field)` opinion storage with set/remove/clear editing
- storage-agnostic ordered-chain resolution
- scalar set/block resolution
- ordered unique-list edits
- sparse array edit programs over any element type, with a host-supplied
  fill policy
- temporal sparse-composition recipes over host time keys and values
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

## Resolution semantics

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

Sparse-family adapters choose ownership through `OpinionFamily::Edit<'op>`:
it can be a reference into the classified operation or an owned, synthesized
edit. The shared fold keeps sparse edits until it reaches a dense value or
block, then applies them weakest-first. Dense results remain owned so edits
can modify them in place. Borrowing changes neither lazy cutoff nor the
provenance reported by the diagnostic entry point.

## Temporal composition

`TemporalPlanner` accepts already ordered sources, one bracket at a time. It
requests the time at which to sample the next source and stops when weaker
sources are hidden. `TemporalSelection` recipes identify the source samples to
fold with `resolve_family_chain`; interpolation happens after that fold.

The planner reads no values. Hosts retain source brackets and own time mapping,
sample discovery, time-equivalence policy, fallback seeds, and interpolation.
Time keys can be integer animation ticks or floating-point times. `TemporalMode`
selects held or two-bracket planning without imposing a numeric value type.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
