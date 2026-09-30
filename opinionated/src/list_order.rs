// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Run-preserving ordering shared by list edits and host namespaces.

use alloc::{vec, vec::Vec};

/// Moves named items and their following unnamed items into `order`.
///
/// The unnamed prefix stays first. Each named item carries the items after
/// it up to the next named item. Absent names and repeated names in `order`
/// are ignored. If `items` repeats a named item, only its first run is moved;
/// remaining runs keep their relative order ahead of the moved runs.
///
/// Elements are moved without cloning, and the slice's allocation is retained.
/// Uses `O(n + m)` scratch space and `O(n * m)` equality checks for `n` items
/// and `m` names. For wide orders with sortable keys, use
/// [`apply_list_order_by_key`].
///
/// Spec: AOUSD Core §12.4 (list ops). OpenUSD: `SdfApplyListOrdering` and
/// `SdfListOp::_ReorderKeysHelper` in `pxr/usd/sdf/listOp.cpp`.
///
/// ```
/// use opinionated::apply_list_order;
/// let mut items = vec!["prefix", "a", "a-child", "b", "b-child"];
/// apply_list_order(&mut items, &["b", "a"]);
/// assert_eq!(items, ["prefix", "b", "b-child", "a", "a-child"]);
/// ```
pub fn apply_list_order<T: Eq>(items: &mut [T], order: &[T]) {
    order_runs(items, order.len(), |item| {
        order.iter().position(|name| name == item)
    });
}

/// Applies [`apply_list_order`] using host keys instead of element equality.
///
/// `key` is called once per item when an order is nonempty and there are at
/// least two items. Keys identify runs; repeated keys have the same behavior
/// as repeated items in [`apply_list_order`]. Values need not implement
/// `Clone`, `Eq`, or `Ord`. The host keeps ownership of namespace/key mapping.
///
/// Uses `O(n + m)` scratch space and `O((n + m) * log(m + 1))` key comparisons.
/// This avoids scanning every authored name for every item in a wide list.
///
/// ```
/// use opinionated::apply_list_order_by_key;
/// let mut items = vec![(1, "one"), (2, "two"), (3, "three")];
/// apply_list_order_by_key(&mut items, &[3, 1], |item| item.0);
/// assert_eq!(items, [(3, "three"), (1, "one"), (2, "two")]);
/// ```
pub fn apply_list_order_by_key<T, K: Ord>(
    items: &mut [T],
    order: &[K],
    mut key: impl FnMut(&T) -> K,
) {
    if order.is_empty() || items.len() < 2 {
        return;
    }
    // Sorting by (key, authored position) keeps the first occurrence of a
    // repeated name. The compact lookup allocates once instead of building
    // a tree of separately allocated nodes.
    let mut ranks: Vec<_> = order
        .iter()
        .enumerate()
        .map(|(rank, name)| (name, rank))
        .collect();
    ranks.sort_unstable();
    ranks.dedup_by(|a, b| a.0 == b.0);
    order_runs(items, order.len(), |item| {
        let name = key(item);
        ranks
            .binary_search_by(|(key, _)| (*key).cmp(&name))
            .ok()
            .map(|index| ranks[index].1)
    });
}

/// Discovers the first run of each named key, then permutes integer slots.
/// This avoids draining/moving a shrinking value vector once per named run.
fn order_runs<T>(items: &mut [T], names: usize, mut rank: impl FnMut(&T) -> Option<usize>) {
    if names == 0 || items.len() < 2 {
        return;
    }
    let mut runs = vec![None; names];
    let mut current = None;
    for (index, item) in items.iter().enumerate() {
        if let Some(named) = rank(item) {
            if let Some((previous, start)) = current.take() {
                runs[previous] = Some(start..index);
            }
            if runs[named].is_none() {
                current = Some((named, index));
            }
        }
    }
    if let Some((named, start)) = current {
        runs[named] = Some(start..items.len());
    }
    let moved: usize = runs.iter().flatten().map(|run| run.len()).sum();
    if moved == 0 {
        return;
    }

    // Destinations are indexed by original slot. Unselected items precede
    // the ordered runs, including repeated named items left in the input.
    let mut destinations = vec![usize::MAX; items.len()];
    let mut next = items.len() - moved;
    for run in runs.into_iter().flatten() {
        for slot in run {
            destinations[slot] = next;
            next += 1;
        }
    }
    next = 0;
    for destination in &mut destinations {
        if *destination == usize::MAX {
            *destination = next;
            next += 1;
        }
    }
    for slot in 0..items.len() {
        while destinations[slot] != slot {
            let destination = destinations[slot];
            items.swap(slot, destination);
            destinations.swap(slot, destination);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Operational oracle: remove the first named run for each unique name,
    // then append the runs. This deliberately differs from slot permutation.
    fn reference(mut items: Vec<u8>, order: &[u8]) -> Vec<u8> {
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
        items
    }

    fn words(alphabet: u8, max_len: usize) -> Vec<Vec<u8>> {
        let mut words = vec![Vec::new()];
        let mut frontier = vec![Vec::new()];
        for _ in 0..max_len {
            let mut next = Vec::new();
            for prefix in frontier {
                for item in 0..alphabet {
                    let mut word = prefix.clone();
                    word.push(item);
                    words.push(word.clone());
                    next.push(word);
                }
            }
            frontier = next;
        }
        words
    }

    #[test]
    fn both_entry_points_preserve_runs_with_absent_and_repeated_names() {
        let inputs = words(3, 5);
        let orders = words(4, 3);
        for input in inputs {
            for order in &orders {
                let expected = reference(input.clone(), order);
                let mut equal = input.clone();
                apply_list_order(&mut equal, order);
                assert_eq!(equal, expected, "input {input:?}, order {order:?}");
                let mut keyed = input.clone();
                apply_list_order_by_key(&mut keyed, order, |item| *item);
                assert_eq!(keyed, expected, "input {input:?}, order {order:?}");
            }
        }
    }

    #[test]
    fn keyed_order_moves_non_clone_values_and_visits_each_key_once() {
        #[derive(Debug, PartialEq)]
        struct Item(u8);
        let mut items = vec![Item(0), Item(1), Item(2), Item(3)];
        let allocation = items.as_ptr();
        let mut visits = 0;
        apply_list_order_by_key(&mut items, &[3, 1], |item| {
            visits += 1;
            item.0
        });
        assert_eq!(items, [Item(0), Item(3), Item(1), Item(2)]);
        assert_eq!(visits, 4);
        assert_eq!(items.as_ptr(), allocation);
    }

    #[test]
    fn empty_order_does_not_discover_host_keys() {
        apply_list_order_by_key(&mut [1, 2, 3], &[] as &[u8], |_| {
            panic!("no order needs no keys")
        });
    }
}
