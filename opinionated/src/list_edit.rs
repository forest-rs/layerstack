// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Item lookups and the add, prepend and append edits of a list op.
//!
//! Each edit is one pass over the list plus one over its authored items.
//! A lookup scans its authored items while the scans stay cheap, without
//! allocating, and hashes them beyond that, so no edit is quadratic in the
//! length of a list or of an authored edit. Without the `hash-tables`
//! feature every lookup scans.

use alloc::{vec, vec::Vec};
use core::hash::Hash;
#[cfg(not(feature = "hash-tables"))]
use core::{convert::Infallible, marker::PhantomData};

#[cfg(feature = "hash-tables")]
use hashbrown::{HashMap, hash_map::Entry};

/// The most comparisons a lookup's scans may make in all before it hashes
/// its items instead. Scanning allocates nothing, and the comparisons of
/// small integer ids vectorize, so scans beat hashing them well past this;
/// comparisons of long strings cost more, and scanning them is up to about
/// twice as slow as hashing at this budget.
const SCAN_BUDGET: usize = 1024;

/// Whether `probes` lookups among `items` items should scan them: always,
/// without the `hash-tables` feature.
pub(crate) const fn scans(probes: usize, items: usize) -> bool {
    cfg!(not(feature = "hash-tables")) || probes.saturating_mul(items) <= SCAN_BUDGET
}

/// Which occurrence of a repeated item a [`Positions`] reports.
#[derive(Clone, Copy)]
pub(crate) enum Occurrence {
    /// The first, as explicit, added, prepended and reordered items count.
    First,
    /// The last, as appended items count.
    Last,
}

/// Where each item of an authored list sits, by one occurrence.
pub(crate) struct Positions<'a, T> {
    items: &'a [T],
    occurrence: Occurrence,
    /// Present when the items are hashed rather than scanned.
    table: Option<Table<'a, T>>,
}

/// The hashed form of [`Positions`].
#[cfg(feature = "hash-tables")]
pub(crate) struct Table<'a, T> {
    /// The position of each item's counted occurrence.
    positions: HashMap<&'a T, usize>,
    /// Whether the item at each position is its counted occurrence.
    counted: Vec<bool>,
}

#[cfg(feature = "hash-tables")]
impl<'a, T: Eq + Hash> Table<'a, T> {
    /// The table of `items`, counting each value's `occurrence`.
    fn new(items: &'a [T], occurrence: Occurrence) -> Option<Self> {
        let mut positions = HashMap::with_capacity(items.len());
        let mut counted = vec![false; items.len()];
        for (index, item) in items.iter().enumerate() {
            match positions.entry(item) {
                Entry::Vacant(entry) => {
                    entry.insert(index);
                    counted[index] = true;
                }
                Entry::Occupied(mut entry) => {
                    if let Occurrence::Last = occurrence {
                        counted[entry.insert(index)] = false;
                        counted[index] = true;
                    }
                }
            }
        }
        Some(Self { positions, counted })
    }

    /// The position of `item`'s counted occurrence, if the items hold it.
    pub(crate) fn get(&self, item: &T) -> Option<usize> {
        self.positions.get(item).copied()
    }

    /// Whether the items hold `item`.
    fn contains(&self, item: &T) -> bool {
        self.positions.contains_key(item)
    }

    /// Whether the item at `index` is the counted occurrence of its value.
    fn counted(&self, index: usize) -> bool {
        self.counted[index]
    }
}

/// Without the `hash-tables` feature no table is ever built: every lookup
/// scans.
#[cfg(not(feature = "hash-tables"))]
pub(crate) struct Table<'a, T>(Infallible, PhantomData<&'a T>);

#[cfg(not(feature = "hash-tables"))]
impl<'a, T> Table<'a, T> {
    /// No table: the items are scanned.
    fn new(_items: &'a [T], _occurrence: Occurrence) -> Option<Self> {
        None
    }

    /// Unreachable: no table exists.
    pub(crate) fn get(&self, _item: &T) -> Option<usize> {
        match self.0 {}
    }

    /// Unreachable: no table exists.
    fn contains(&self, _item: &T) -> bool {
        match self.0 {}
    }

    /// Unreachable: no table exists.
    fn counted(&self, _index: usize) -> bool {
        match self.0 {}
    }
}

impl<'a, T: Eq + Hash> Positions<'a, T> {
    /// The positions of `items`, for about `probes` lookups: scanned when
    /// that is cheap ([`scans`]), hashed otherwise.
    pub(crate) fn new(items: &'a [T], occurrence: Occurrence, probes: usize) -> Self {
        let table = if scans(probes, items.len()) {
            None
        } else {
            Table::new(items, occurrence)
        };
        Self {
            items,
            occurrence,
            table,
        }
    }

    /// The position of `item`'s counted occurrence, if the items hold it.
    pub(crate) fn position(&self, item: &T) -> Option<usize> {
        match (&self.table, self.occurrence) {
            (Some(table), _) => table.get(item),
            (None, Occurrence::First) => self.items.iter().position(|named| named == item),
            (None, Occurrence::Last) => self.items.iter().rposition(|named| named == item),
        }
    }

    /// Whether the items hold `item`.
    pub(crate) fn contains(&self, item: &T) -> bool {
        match &self.table {
            Some(table) => table.contains(item),
            None => self.items.contains(item),
        }
    }

    /// Removes every item the items hold from `out`. The lookup is chosen
    /// once, outside the loop, so a scan stays a tight loop.
    pub(crate) fn remove_from(&self, out: &mut Vec<T>) {
        match (&self.table, self.items) {
            (Some(table), _) => out.retain(|item| !table.contains(item)),
            // One item, the most common edit, compares without a loop.
            (None, [only]) => out.retain(|item| item != only),
            (None, items) => out.retain(|item| !items.contains(item)),
        }
    }

    /// The hash table of the counted positions, if the items are hashed.
    pub(crate) fn table(&self) -> Option<&Table<'a, T>> {
        self.table.as_ref()
    }

    /// Whether the item at `index` is the counted occurrence of its value.
    fn counts(&self, index: usize, item: &T) -> bool {
        match (&self.table, self.occurrence) {
            (Some(table), _) => table.counted(index),
            (None, Occurrence::First) => !self.items[..index].contains(item),
            (None, Occurrence::Last) => !self.items[index + 1..].contains(item),
        }
    }

    /// The counted occurrence of each item, in authored order.
    pub(crate) fn unique(&self) -> impl Iterator<Item = &'a T> {
        self.items
            .iter()
            .enumerate()
            .filter(|&(index, item)| self.counts(index, item))
            .map(|(_, item)| item)
    }
}

/// Pushes the first occurrence of each item of `items` that `out` does not
/// hold, in authored order: OpenUSD's `SdfListOp::_AddKeys`, for explicit
/// and added items.
pub(crate) fn push_absent<T: Clone + Eq + Hash>(out: &mut Vec<T>, items: &[T]) {
    if scans(items.len(), out.len() + items.len()) {
        for item in items {
            if !out.contains(item) {
                out.push(item.clone());
            }
        }
        return;
    }
    // Hash the authored items, not the list: an add of a few dozen items
    // to a long list probes the list once and builds a small table.
    let positions = Positions::new(items, Occurrence::First, out.len() + items.len());
    let mut present = vec![false; items.len()];
    for item in out.iter() {
        if let Some(index) = positions.position(item) {
            present[index] = true;
        }
    }
    // `present` is indexed by first occurrence, the one `counts` keeps.
    let fresh = items
        .iter()
        .enumerate()
        .filter(|&(index, item)| !present[index] && positions.counts(index, item))
        .map(|(_, item)| item);
    out.extend(fresh.cloned());
}

/// Moves or inserts `items` at the front of `out`, in authored order,
/// keeping the first occurrence of a repeated item: OpenUSD's
/// `SdfListOp::_PrependKeys`, which inserts the items in reverse at the
/// front.
pub(crate) fn move_to_front<T: Clone + Eq + Hash>(out: &mut Vec<T>, items: &[T]) {
    let added = move_out(out, items, Occurrence::First);
    // One item, the most common prepend, moves with one shift.
    if added == 1
        && let Some(item) = out.pop()
    {
        out.insert(0, item);
    } else {
        out.rotate_right(added);
    }
}

/// Moves or inserts `items` at the back of `out`, in authored order,
/// keeping the last occurrence of a repeated item: OpenUSD's
/// `SdfListOp::_AppendKeys`, which inserts the items in order at the back.
pub(crate) fn move_to_back<T: Clone + Eq + Hash>(out: &mut Vec<T>, items: &[T]) {
    move_out(out, items, Occurrence::Last);
}

/// Removes every copy of `items` from `out` and pushes the counted
/// occurrence of each, returning how many it pushed.
fn move_out<T: Clone + Eq + Hash>(out: &mut Vec<T>, items: &[T], occurrence: Occurrence) -> usize {
    if items.is_empty() {
        return 0;
    }
    // Each list item is looked up, and each authored item to keep one copy.
    let moved = Positions::new(items, occurrence, out.len() + items.len());
    moved.remove_from(out);
    let kept = out.len();
    out.extend(moved.unique().cloned());
    out.len() - kept
}
