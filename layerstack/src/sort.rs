// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Slice sorts that compile to one shared routine on WebAssembly.
//!
//! The standard library's sorts are generic over the element type and the
//! comparison, so every `sort_by` call site with its own closure compiles a
//! fresh copy of the whole sort (several kilobytes each). That is the right
//! trade on native targets and the wrong one for a module downloaded into a
//! browser. These functions keep the standard library's sorts on native
//! targets and, on `target_family = "wasm"`, sort through one non-generic
//! routine instead: a stable sort of the positions `0..len` through a
//! `dyn` comparison, then an in-place permutation of the slice.
//!
//! Results are the standard library's: the stable functions order equal
//! elements as the slice held them, and the unstable ones leave equal
//! elements in an unspecified order on every target, as
//! [`slice::sort_unstable`] does. The shared routine allocates one `usize`
//! per element (the unstable sorts here may allocate on WebAssembly).
//!
//! Collecting into a [`BTreeSet`] or [`BTreeMap`] sorts too: the standard
//! library's `FromIterator` sorts the items with a stable sort compiled for
//! the element type and bulk-builds the tree from them. [`collect_set`] and
//! [`collect_map`] keep that on native targets and, on WebAssembly, insert
//! the items one at a time through `insert`, which every tree the library
//! fills also compiles.
//!
//! Library code in this workspace sorts through these functions. Tests use
//! the standard library's directly, as does an unstable sort whose equal
//! elements can differ, so their order on WebAssembly stays the standard
//! library's.

use alloc::collections::{BTreeMap, BTreeSet};
use core::cmp::Ordering;

/// Sorts `v` stably by [`Ord`], like [`slice::sort`].
pub fn sort<T: Ord>(v: &mut [T]) {
    sort_by(v, T::cmp);
}

/// Sorts `v` stably by `compare`, like [`slice::sort_by`].
pub fn sort_by<T, F>(v: &mut [T], compare: F)
where
    F: FnMut(&T, &T) -> Ordering,
{
    #[cfg(not(target_family = "wasm"))]
    v.sort_by(compare);
    #[cfg(target_family = "wasm")]
    shared::sort_by(v, compare);
}

/// Sorts `v` stably by the key `f` extracts, like [`slice::sort_by_key`].
pub fn sort_by_key<T, K, F>(v: &mut [T], mut f: F)
where
    F: FnMut(&T) -> K,
    K: Ord,
{
    sort_by(v, |a, b| f(a).cmp(&f(b)));
}

/// Sorts `v` by [`Ord`] without preserving the order of equal elements,
/// like [`slice::sort_unstable`].
pub fn sort_unstable<T: Ord>(v: &mut [T]) {
    sort_unstable_by(v, T::cmp);
}

/// Sorts `v` by `compare` without preserving the order of equal elements,
/// like [`slice::sort_unstable_by`].
pub fn sort_unstable_by<T, F>(v: &mut [T], compare: F)
where
    F: FnMut(&T, &T) -> Ordering,
{
    #[cfg(not(target_family = "wasm"))]
    v.sort_unstable_by(compare);
    #[cfg(target_family = "wasm")]
    shared::sort_by(v, compare);
}

/// Sorts `v` by the key `f` extracts without preserving the order of equal
/// elements, like [`slice::sort_unstable_by_key`].
pub fn sort_unstable_by_key<T, K, F>(v: &mut [T], mut f: F)
where
    F: FnMut(&T) -> K,
    K: Ord,
{
    sort_unstable_by(v, |a, b| f(a).cmp(&f(b)));
}

/// Collects `items` into a set, like [`Iterator::collect`].
pub fn collect_set<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    #[cfg(not(target_family = "wasm"))]
    return items.into_iter().collect();
    #[cfg(target_family = "wasm")]
    {
        let mut set = BTreeSet::new();
        for item in items {
            set.insert(item);
        }
        set
    }
}

/// Collects `items` into a map, like [`Iterator::collect`]: of entries with
/// equal keys, the last one's value is kept.
pub fn collect_map<K: Ord, V>(items: impl IntoIterator<Item = (K, V)>) -> BTreeMap<K, V> {
    #[cfg(not(target_family = "wasm"))]
    return items.into_iter().collect();
    #[cfg(target_family = "wasm")]
    {
        let mut map = BTreeMap::new();
        for (key, value) in items {
            map.insert(key, value);
        }
        map
    }
}

/// The one compiled sort behind every element type on WebAssembly.
#[cfg(any(target_family = "wasm", test))]
mod shared {
    use alloc::vec::Vec;
    use core::cmp::Ordering;

    /// Sorts `v` stably by `compare` through [`stable_order`] and [`permute`].
    pub(super) fn sort_by<T, F>(v: &mut [T], mut compare: F)
    where
        F: FnMut(&T, &T) -> Ordering,
    {
        if v.len() < 2 {
            return;
        }
        let order = {
            let v: &[T] = v;
            stable_order(v.len(), &mut |a, b| compare(&v[a], &v[b]))
        };
        permute(order, &mut |a, b| v.swap(a, b));
    }

    /// The positions `0..len` in stable sorted order: `order[i]` is the
    /// position of the element that sorts `i`th.
    #[inline(never)]
    fn stable_order(len: usize, compare: &mut dyn FnMut(usize, usize) -> Ordering) -> Vec<usize> {
        let mut order: Vec<usize> = (0..len).collect();
        order.sort_by(|&a, &b| compare(a, b));
        order
    }

    /// Moves the element at `order[i]` to `i` for every `i`, one cycle of
    /// the permutation at a time, through `swap`.
    #[inline(never)]
    fn permute(mut order: Vec<usize>, swap: &mut dyn FnMut(usize, usize)) {
        for start in 0..order.len() {
            let mut at = start;
            loop {
                let from = order[at];
                // A position that holds its own index is placed.
                order[at] = at;
                if from == start {
                    break;
                }
                swap(at, from);
                at = from;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::sort_by;
        use alloc::vec::Vec;

        /// Every permutation of a small slice with ties sorts as the
        /// standard library's stable sort does.
        #[test]
        fn matches_the_standard_stable_sort() {
            let keys = [3_u8, 1, 3, 0, 2, 1, 3];
            let mut input: Vec<(u8, usize)> = keys.iter().copied().zip(0..).collect();
            for round in 0..200_usize {
                // A deterministic shuffle so ties start in many orders.
                let len = input.len();
                input.swap(round % len, (round * 7 + 3) % len);
                for len in 0..=input.len() {
                    let mut ours = input[..len].to_vec();
                    let mut theirs = ours.clone();
                    sort_by(&mut ours, |a, b| a.0.cmp(&b.0));
                    theirs.sort_by_key(|a| a.0);
                    assert_eq!(ours, theirs);
                }
            }
        }

        /// Longer slices exercise multi-element permutation cycles.
        #[test]
        fn sorts_long_slices() {
            let mut v: Vec<u32> = (0..1000_u32).map(|i| (i * 7919) % 1009).collect();
            let mut expected = v.clone();
            expected.sort();
            sort_by(&mut v, u32::cmp);
            assert_eq!(v, expected);
        }
    }
}
