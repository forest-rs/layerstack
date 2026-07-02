---
id: opi-s92f
status: closed
deps: []
links: [opi-htjx]
created: 2026-07-02T04:44:23Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Design extensibility seam for opinion families

OpinionOp is a closed enum, so domains cannot add sparse value families (e.g. layerstack's sparse array edits or time-sampled values) without forking the crate. Decide between a family trait, a Custom op variant folded by the caller, or family-generic fold hooks. This is the gating design question for layerstack delegating chain resolution to opinionated.

## Design

OpinionKind already acts as a family discriminator; docs/generic-sparse-composition.md describes the same problem from layerstack's side. This seam is also the gating design for lay-yvpy (layerstack delegating chain value resolution to the kernel).

### Non-goals

- No `OpinionOp::Custom(C)` variant and no new type parameters on `SparseComposer` or `OpinionOp`. The enum layer stays closed and calm.
- No time model in `opinionated`. Sampling maps `(opinion, time)` to an op before classification; the kernel stays time-agnostic.
- No op-over-op composition (flattening) in this slice. The kernel folds ops over values only. If layerstack later needs `compose_over(strong_op, weak_op)` for flattening, that grows on the same trait and must be associative within a family.

### The seam

Every family in the current code — scalar, list, dictionary, and layerstack's arrays — already follows one fold shape. Walk strongest to weakest; each opinion is one of:

- **dense**: self-sufficient; terminates the fold (scalar `Set`, a dense array)
- **sparse**: an edit that composes over weaker opinions (`ListOp`, dictionary entries, `ArrayEdit`)
- **block**: cuts off everything weaker
- **foreign**: belongs to another family; skipped and reported

Accumulate sparse edits until a dense member absorbs them, a block cuts the chain, or the chain ends; then apply the edits weakest-first over the dense value, or the family's seed (on block/chain-end). A family is therefore exactly three operations:

```rust
/// How one authored operation participates in a family's fold.
pub enum FamilyMember<D, S> {
    /// A dense, self-sufficient value; terminates the fold.
    Dense(D),
    /// A sparse edit that composes over weaker opinions.
    Sparse(S),
    /// Blocks all weaker opinions.
    Block,
    /// Another family's operation; skipped, with the reason reported.
    Foreign(IgnoreReason),
}

/// A value family that folds over an ordered opinion chain.
pub trait OpinionFamily<Op> {
    /// The dense resolved value this family produces.
    type Value;
    /// The sparse edit representation this family folds.
    type Edit;

    /// Classifies one authored operation.
    fn classify(&self, op: &Op) -> FamilyMember<Self::Value, Self::Edit>;
    /// Applies one sparse edit over a weaker base value.
    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value;
    /// The weakest base value when no dense opinion terminates the chain.
    fn seed(&self) -> Self::Value;
}
```

`classify` returns owned members because sampled or interpolated values are synthesized at query time, not borrowed from storage. Family selection is the caller's job (the enum layer selects by the strongest op's kind; layerstack selects by inspecting the chain) — the kernel folds exactly one family.

The kernel entry points mirror the existing chain API, generic over any op type, taking `(op, provenance)` pairs:

```rust
pub fn resolve_family_chain<'a, Op, F, P>(
    family: &F,
    opinions_strong_to_weak: impl IntoIterator<Item = (&'a Op, &'a P)>,
) -> FamilyResolution<F::Value, P>;

pub fn resolve_family_chain_report<'a, Op, F, P>(...) -> FamilyReport<F::Value, P>;

pub enum FamilyResolution<T, P> {
    Absent,
    Blocked { provenance: P },
    Resolved { value: T, provenance: P },
}
```

`FamilyReport` pairs a `FamilyResolution` with events. Kernel events are a small family-agnostic enum (`FamilyEvent`): contributed dense, contributed sparse, stopped by block, ignored with an `IgnoreReason`. The enum layer's `ResolutionEvent`/`OpinionKind` reporting stays as-is at its own altitude; the kernel does not know `OpinionKind`.

Provenance of a resolved value is the strongest contributing (non-foreign, non-block) opinion, exactly as today.

### Semantics invariants

1. Edits apply weakest-first; stronger edits have the last word. This matches `resolve_list_chain` and AOUSD Core §12.4 chain ordering.
2. A block cuts the chain; already-accumulated sparse edits still materialize over the seed. (Today: list ops over `[]` when a block cuts — preserved.)
3. A dense member absorbs accumulated edits and terminates. A chain that is blocked before any member contributes resolves to `Blocked`.
4. Foreign ops are skipped, never terminate the fold, and are reported.
5. `seed()` is the hook for opi-htjx (fallback seeds): a follow-up adds a caller-supplied seed override at the composer level; this slice keeps the family-owned seed only.

### Validation plan

- In-crate family adapters for the existing enum: scalar (`Set` is `Dense`, everything else foreign), list (`List` is `Sparse`, seed `[]`), dictionary (`Dictionary` is `Sparse`, seed empty). These are the proof that the trait can express current semantics.
- Parity tests: for every existing chain-resolution test vector, the family adapters over the kernel must produce results identical to `resolve_ordered_chain`.
- An out-of-crate family in `opinionated`'s integration tests (a toy sparse family, e.g. counter deltas over a dense total) proving the acceptance criterion: a second family participates without touching `opinionated`'s enums.
- If, after parity holds, reimplementing `resolve_ordered_chain` internally as kernel dispatch keeps the code as readable as the direct match — do it, so there is one fold algorithm. If the indirection reads worse, keep both and the parity tests guard them.
- lay-yvpy is the real earned proof: layerstack's `ArrayFamily` over `FieldValue`, replacing the array-specific branches in `value_resolution.rs`.

## Acceptance Criteria

A second, out-of-crate value family can participate in resolve/explain without modifying opinionated's enums.


## Notes

**2026-07-02T16:00:15Z**

Design captured in the Design section above: OpinionFamily trait (classify/apply/seed) + FamilyMember + resolve_family_chain kernel below the closed OpinionOp enum. No Custom variant, no new generics on SparseComposer. Implementation delegated; lay-yvpy consumes the seam via an ArrayFamily over FieldValue.
