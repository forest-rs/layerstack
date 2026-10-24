// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! List-op application against a quadratic reference, over generated chains.
//!
//! The reference is the straightforward implementation `ListOp` used to
//! have: a scan of the list per authored item. The generator draws short
//! and long lists over small and large alphabets, so items repeat in the
//! base list and in every authored list, and lookups run on both sides of
//! the scan-or-hash threshold. Every run uses the same fixed seeds.

use opinionated::{ListOp, apply_list_order, resolve_list_chain};

/// A xorshift64* generator: deterministic, dependency-free and good enough to
/// spread chains across list shapes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }

    /// A list of up to `max_len` items drawn from `alphabet` items, empty
    /// a third of the time.
    fn list(&mut self, alphabet: u32, max_len: usize) -> Vec<u32> {
        if self.below(3) == 0 {
            return Vec::new();
        }
        let len = self.below(max_len + 1);
        (0..len)
            .map(|_| u32::try_from(self.below(usize::try_from(alphabet).unwrap())).unwrap())
            .collect()
    }

    fn op(&mut self, alphabet: u32, max_len: usize) -> ListOp<u32> {
        let mut op = ListOp::new()
            .with_deleted(self.list(alphabet, max_len))
            .with_added(self.list(alphabet, max_len))
            .with_prepended(self.list(alphabet, max_len))
            .with_appended(self.list(alphabet, max_len))
            .with_reordered(self.list(alphabet, max_len));
        if self.below(6) == 0 {
            op.explicit = Some(self.list(alphabet, max_len));
        }
        op
    }
}

/// The quadratic implementation, kept as the reference: AOUSD Core §6.6.3;
/// OpenUSD `SdfListOp::ApplyOperations`.
fn reference_apply(op: &ListOp<u32>, out: &mut Vec<u32>) {
    if let Some(explicit) = &op.explicit {
        out.clear();
        for item in explicit {
            if !out.contains(item) {
                out.push(*item);
            }
        }
        return;
    }
    out.retain(|item| !op.delete.contains(item));
    for item in &op.add {
        if !out.contains(item) {
            out.push(*item);
        }
    }
    for item in op.prepend.iter().rev() {
        out.retain(|existing| existing != item);
        out.insert(0, *item);
    }
    for item in &op.append {
        out.retain(|existing| existing != item);
        out.push(*item);
    }
    reference_order(out, &op.reorder);
}

/// Removes the first run of each named item in order, then appends the
/// runs: OpenUSD's `_ReorderKeysHelper`, operationally.
fn reference_order(items: &mut Vec<u32>, order: &[u32]) {
    let mut seen = Vec::new();
    let mut moved = Vec::new();
    for name in order {
        if seen.contains(name) {
            continue;
        }
        seen.push(*name);
        if let Some(start) = items.iter().position(|item| item == name) {
            let end = items[start + 1..]
                .iter()
                .position(|item| order.contains(item))
                .map_or(items.len(), |offset| start + 1 + offset);
            moved.extend(items.drain(start..end));
        }
    }
    items.extend(moved);
}

/// The reference fold of a strongest-first chain over `fallback`.
fn reference_chain(fallback: &[u32], ops: &[ListOp<u32>]) -> Vec<u32> {
    let explicit = ops.iter().position(|op| op.explicit.is_some());
    let edits = &ops[..explicit.unwrap_or(ops.len())];
    let mut out = fallback.to_vec();
    if let Some(index) = explicit {
        reference_apply(&ops[index], &mut out);
    }
    for op in edits.iter().rev() {
        reference_apply(op, &mut out);
    }
    out
}

/// Runs `cases` generated chains over each `(alphabet, max_len)` shape.
fn check_chains(seed: u64, cases: usize, shapes: &[(u32, usize)]) {
    let mut rng = Rng(seed);
    for &(alphabet, max_len) in shapes {
        for case in 0..cases {
            let base = rng.list(alphabet, max_len);
            let ops: Vec<_> = (0..1 + rng.below(5))
                .map(|_| rng.op(alphabet, max_len))
                .collect();
            for op in &ops {
                let mut expected = base.clone();
                reference_apply(op, &mut expected);
                assert_eq!(
                    op.apply_to(&base),
                    expected,
                    "case {case}: {op:?} over {base:?}"
                );
                let mut owned = base.clone();
                op.apply_in_place(&mut owned);
                assert_eq!(owned, expected, "case {case}: {op:?} over {base:?}");
            }
            assert_eq!(
                resolve_list_chain(&base, &ops),
                reference_chain(&base, &ops),
                "case {case}: {ops:?} over {base:?}"
            );
        }
    }
}

#[test]
fn short_lists_match_the_reference() {
    check_chains(0x5eed_0001, 2_000, &[(3, 6), (8, 12), (24, 20)]);
}

#[test]
fn lists_across_the_hash_threshold_match_the_reference() {
    check_chains(0x5eed_0002, 600, &[(12, 40), (40, 60), (200, 80)]);
}

#[test]
fn long_lists_match_the_reference() {
    check_chains(0x5eed_0003, 10, &[(300, 400), (5_000, 1_200)]);
}

#[test]
fn reorders_match_the_reference_on_both_sides_of_the_threshold() {
    let mut rng = Rng(0x5eed_0004);
    for (alphabet, max_len) in [(4, 8), (20, 40), (60, 120), (2_000, 1_000)] {
        for case in 0..300 {
            let base = rng.list(alphabet, max_len);
            let order = rng.list(alphabet, max_len);
            let mut expected = base.clone();
            reference_order(&mut expected, &order);
            let mut ordered = base.clone();
            apply_list_order(&mut ordered, &order);
            assert_eq!(ordered, expected, "case {case}: {order:?} over {base:?}");
        }
    }
}

#[test]
fn long_repeated_authored_items_keep_their_counted_occurrence() {
    // OpenUSD `_AddKeys` keeps first occurrences; `_PrependKeys` inserts in
    // reverse at the front, so the first occurrence wins; `_AppendKeys`
    // inserts in order at the back, so the last occurrence wins.
    let twice: Vec<u32> = (0..40).chain(0..40).rev().collect();
    let first: Vec<u32> = (0..40).rev().collect();
    assert_eq!(ListOp::explicit(twice.clone()).apply_to(&[]), first);
    assert_eq!(ListOp::added(twice.clone()).apply_to(&[5]), {
        let mut added = vec![5];
        added.extend(first.iter().filter(|item| **item != 5));
        added
    });
    assert_eq!(ListOp::prepended(twice).apply_to(&[99, 7]), {
        let mut prepended = first.clone();
        prepended.push(99);
        prepended
    });
    // Each item occurs once in each half, so the second half holds the
    // last occurrences in order.
    let interleaved: Vec<u32> = (0..40).flat_map(|item| [item, 39 - item]).collect();
    let mut appended = vec![99];
    appended.extend_from_slice(&interleaved[40..]);
    assert_eq!(ListOp::appended(interleaved).apply_to(&[7, 99]), appended);
}
