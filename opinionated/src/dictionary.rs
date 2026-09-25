// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recursive dictionary combination over host value types.
//!
//! Dictionaries combine key by key: a key present on one side only is kept,
//! and when both sides hold a value for the same key the stronger value wins,
//! unless both values are themselves dictionaries, in which case they combine
//! recursively. This is AOUSD Core §6.6.2.1 (combining) and OpenUSD's
//! `VtDictionaryOverRecursive`.
//!
//! `opinionated` does not own a value type, so a host exposes the nested
//! structure of its own values through a small [`DictionaryAdapter`]: "is this
//! value a dictionary, and what are its entries?" plus a way to wrap combined
//! entries back into a value. No universal value enum or serialization model
//! is involved.
//!
//! # Fold direction
//!
//! Recursive combination is **not associative** once a key holds a dictionary
//! in one opinion and a non-dictionary in another. With opinions, strongest
//! first, `{s: {a: 1}}`, `{s: 0}`, `{s: {b: 2}}`:
//!
//! - folding strongest-first, `(S ∪ M) ∪ W`, keeps `{s: {a: 1, b: 2}}`;
//! - folding weakest-first, `S ∪ (M ∪ W)`, keeps only `{s: {a: 1}}`.
//!
//! AOUSD Core §6.6.2.1 calls combining associative, which holds only while
//! colliding values agree on being dictionaries. The spec states the pairwise
//! operation but not how a chain is folded, so
//! OpenUSD's behavior governs (AOUSD Core §4.2). OpenUSD folds
//! strongest-first: `MetadataValueComposer::ConsumeAuthored` in
//! `pxr/usd/usd/stage.cpp` composes the accumulated stronger partial over each
//! weaker opinion in turn, and `usdcat --flatten` on the three-layer case above
//! yields `{s: {a: 1, b: 2}}`. [`combine_dictionary_chain`] therefore folds
//! strongest-first; it is deliberately not expressed through the weakest-first
//! [`resolve_family_chain`](crate::resolve_family_chain) kernel.
//!
//! # Output order
//!
//! The result is ordered by key at every nesting level, whether a level was
//! merged or contributed by a single opinion, matching OpenUSD's
//! `VtDictionary` (a `std::map`). Within one dictionary, the first occurrence
//! of a duplicate key wins.
//!
//! Cost: every contributed dictionary level is checked with one linear scan
//! for strictly increasing keys. Levels that are already ordered are cloned
//! as-is; only unordered (or duplicate-keyed) levels are rebuilt through a
//! `BTreeMap`, `O(n log n)` in that level's size. Merging accumulates into a
//! `BTreeMap`, `O(log n)` per inserted key.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

/// Structural access to nested dictionaries inside a host value type `V`.
///
/// Implement this once per host value type. The kernel consults it for every
/// contributed value, to merge colliding dictionaries and to key-order nested
/// dictionaries; opaque values are cloned through untouched.
pub trait DictionaryAdapter<K, V> {
    /// Returns the entries of `value` when it is itself a dictionary, or
    /// `None` when it is an opaque (non-dictionary) value.
    fn entries<'v>(&self, value: &'v V) -> Option<&'v [(K, V)]>;

    /// Wraps combined or key-ordered entries back into a host dictionary
    /// value.
    ///
    /// Called only for values [`DictionaryAdapter::entries`] reported as
    /// dictionaries.
    fn dictionary(&self, entries: Vec<(K, V)>) -> V;
}

/// The shallow overlay policy: values are opaque and never merged.
///
/// Entries combine by key only and the stronger value wins outright, even when
/// both values are nested dictionaries. Use it where a domain intends overlay
/// semantics, or where values are genuinely opaque, as in the
/// [`OpinionOp`](crate::OpinionOp) enum API whose `V` exposes no structure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShallowOverlay;

impl<K, V> DictionaryAdapter<K, V> for ShallowOverlay {
    fn entries<'v>(&self, _value: &'v V) -> Option<&'v [(K, V)]> {
        None
    }

    fn dictionary(&self, _entries: Vec<(K, V)>) -> V {
        unreachable!("ShallowOverlay never reports a nested dictionary")
    }
}

/// Combines a `stronger` dictionary over a `weaker` one (AOUSD Core §6.6.2.1).
///
/// Keys from either side are kept; for a key on both sides the stronger value
/// wins unless `adapter` reports both values as dictionaries, which then
/// combine recursively. The result is ordered by key at every nesting level.
#[must_use]
pub fn combine_dictionaries<K, V, A>(
    adapter: &A,
    stronger: &[(K, V)],
    weaker: &[(K, V)],
) -> Vec<(K, V)>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
{
    let mut combined = collect_first_occurrence(adapter, stronger, &mut NoTrace);
    combine_into(adapter, &mut combined, weaker, &mut NoTrace);
    combined.into_iter().collect()
}

/// Combines a chain of dictionaries supplied strongest-to-weakest.
///
/// The chain folds strongest-first, `((d0 ∪ d1) ∪ d2) ∪ …`, which is the
/// order OpenUSD uses and differs from a weakest-first fold when a key holds a
/// dictionary in one opinion and a non-dictionary in another (see the module
/// docs). A caller-supplied fallback seed is simply the last, weakest element
/// of the chain. An empty chain yields an empty dictionary. The result is
/// ordered by key at every nesting level, including for a single-opinion
/// chain.
#[must_use]
pub fn combine_dictionary_chain<K, V, A>(
    adapter: &A,
    dicts_strong_to_weak: impl IntoIterator<Item = impl AsRef<[(K, V)]>>,
) -> Vec<(K, V)>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
{
    fold_chain(
        adapter,
        dicts_strong_to_weak.into_iter().map(|dict| (dict, ())),
        &mut NoTrace,
    )
}

/// How one dictionary of a chain took part in the combined result.
///
/// Events name a `key_path`: the keys from the combined dictionary's root
/// down to the entry, one per nesting level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DictionaryEvent<K, P> {
    /// The dictionary supplied the entry at `key_path`, which no stronger
    /// dictionary holds. A supplied nested dictionary is supplied whole.
    Supplied {
        /// Provenance for the supplying dictionary.
        provenance: P,
        /// Keys from the root to the supplied entry.
        key_path: Vec<K>,
    },
    /// The dictionary's nested dictionary at `key_path` combined with a
    /// stronger one. Its entries follow as events of their own.
    Merged {
        /// Provenance for the merged dictionary.
        provenance: P,
        /// Keys from the root to the merged dictionary.
        key_path: Vec<K>,
    },
    /// A stronger value at `key_path` won over this dictionary's value.
    Overridden {
        /// Provenance for the overridden dictionary.
        provenance: P,
        /// Keys from the root to the overridden entry.
        key_path: Vec<K>,
    },
}

/// A combined dictionary paired with the events that explain it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictionaryReport<K, V, P> {
    /// The combined dictionary, exactly as [`combine_dictionary_chain`]
    /// returns it.
    pub value: Vec<(K, V)>,
    /// Events for every dictionary of the chain, strongest dictionary first
    /// and each dictionary's entries in their authored order.
    pub events: Vec<DictionaryEvent<K, P>>,
}

/// Combines a chain of dictionaries and records how each one took part.
///
/// Behaves exactly like [`combine_dictionary_chain`], and additionally
/// records, per dictionary, which entries it supplied, which of its nested
/// dictionaries merged with stronger ones, and which of its entries a
/// stronger value overrode. Within one dictionary, a repeated key is ignored
/// without an event, as the combination ignores it.
#[must_use]
pub fn combine_dictionary_chain_report<K, V, A, P>(
    adapter: &A,
    dicts_strong_to_weak: impl IntoIterator<Item = (impl AsRef<[(K, V)]>, P)>,
) -> DictionaryReport<K, V, P>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
    P: Clone,
{
    let mut recorder = Recorder {
        provenance: None,
        path: Vec::new(),
        events: Vec::new(),
    };
    let value = fold_chain(adapter, dicts_strong_to_weak, &mut recorder);
    DictionaryReport {
        value,
        events: recorder.events,
    }
}

/// Observes a strongest-first fold, entry by entry.
///
/// The lean entry points pass [`NoTrace`], whose methods compile away.
trait Trace<K> {
    /// Provenance that names each dictionary of the chain.
    type Provenance;
    /// Starts the events of the next, weaker dictionary.
    fn begin(&mut self, provenance: Self::Provenance);
    /// The current dictionary supplied `key`.
    fn supplied(&mut self, key: &K);
    /// The current dictionary's nested dictionary at `key` merges; the
    /// events that follow are nested under `key` until [`Trace::leave`].
    fn enter(&mut self, key: &K);
    /// Ends the nested events of the last [`Trace::enter`].
    fn leave(&mut self);
    /// A stronger value at `key` overrode the current dictionary's.
    fn overridden(&mut self, key: &K);
}

/// The trace of the lean entry points: records nothing.
struct NoTrace;

impl<K> Trace<K> for NoTrace {
    type Provenance = ();
    #[inline(always)]
    fn begin(&mut self, (): ()) {}
    #[inline(always)]
    fn supplied(&mut self, _: &K) {}
    #[inline(always)]
    fn enter(&mut self, _: &K) {}
    #[inline(always)]
    fn leave(&mut self) {}
    #[inline(always)]
    fn overridden(&mut self, _: &K) {}
}

/// The trace of [`combine_dictionary_chain_report`].
struct Recorder<K, P> {
    provenance: Option<P>,
    path: Vec<K>,
    events: Vec<DictionaryEvent<K, P>>,
}

impl<K: Clone, P: Clone> Recorder<K, P> {
    fn record(&mut self, key: &K, event: fn(P, Vec<K>) -> DictionaryEvent<K, P>) {
        let Some(provenance) = self.provenance.clone() else {
            // Every event follows the `begin` of its dictionary.
            return;
        };
        let mut key_path = self.path.clone();
        key_path.push(key.clone());
        self.events.push(event(provenance, key_path));
    }
}

impl<K: Clone, P: Clone> Trace<K> for Recorder<K, P> {
    type Provenance = P;

    fn begin(&mut self, provenance: P) {
        self.provenance = Some(provenance);
    }

    fn supplied(&mut self, key: &K) {
        self.record(key, |provenance, key_path| DictionaryEvent::Supplied {
            provenance,
            key_path,
        });
    }

    fn enter(&mut self, key: &K) {
        self.record(key, |provenance, key_path| DictionaryEvent::Merged {
            provenance,
            key_path,
        });
        self.path.push(key.clone());
    }

    fn leave(&mut self) {
        self.path.pop();
    }

    fn overridden(&mut self, key: &K) {
        self.record(key, |provenance, key_path| DictionaryEvent::Overridden {
            provenance,
            key_path,
        });
    }
}

/// The strongest-first fold shared by the lean and reporting chain entry
/// points.
fn fold_chain<K, V, A, P, T>(
    adapter: &A,
    dicts_strong_to_weak: impl IntoIterator<Item = (impl AsRef<[(K, V)]>, P)>,
    trace: &mut T,
) -> Vec<(K, V)>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
    T: Trace<K, Provenance = P>,
{
    let mut dicts = dicts_strong_to_weak.into_iter();
    let Some((strongest, provenance)) = dicts.next() else {
        return Vec::new();
    };
    trace.begin(provenance);
    let mut combined = collect_first_occurrence(adapter, strongest.as_ref(), trace);
    for (weaker, provenance) in dicts {
        trace.begin(provenance);
        combine_into(adapter, &mut combined, weaker.as_ref(), trace);
    }
    combined.into_iter().collect()
}

/// Collects entries into a key-ordered map, keeping the first occurrence of a
/// duplicate key and key-ordering nested dictionaries.
///
/// `trace` observes every entry kept.
fn collect_first_occurrence<K, V, A, T>(
    adapter: &A,
    entries: &[(K, V)],
    trace: &mut T,
) -> BTreeMap<K, V>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
    T: Trace<K>,
{
    let mut out = BTreeMap::new();
    for (key, value) in entries {
        out.entry(key.clone()).or_insert_with(|| {
            trace.supplied(key);
            key_ordered(adapter, value)
        });
    }
    out
}

/// Returns `value` with every nested dictionary level key-ordered.
///
/// Opaque values and already-ordered dictionaries are cloned as-is.
fn key_ordered<K, V, A>(adapter: &A, value: &V) -> V
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
{
    match adapter
        .entries(value)
        .and_then(|entries| reorder_entries(adapter, entries))
    {
        Some(entries) => adapter.dictionary(entries),
        None => value.clone(),
    }
}

/// Key-orders one dictionary level and its nested dictionaries.
///
/// Returns `None` when the level and everything below it are already ordered
/// with unique keys, so callers can clone the original without rebuilding it.
fn reorder_entries<K, V, A>(adapter: &A, entries: &[(K, V)]) -> Option<Vec<(K, V)>>
where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
{
    let ordered = entries.windows(2).all(|pair| pair[0].0 < pair[1].0);
    let nested: Vec<Option<Vec<(K, V)>>> = entries
        .iter()
        .map(|(_, value)| {
            adapter
                .entries(value)
                .and_then(|inner| reorder_entries(adapter, inner))
        })
        .collect();
    if ordered && nested.iter().all(Option::is_none) {
        return None;
    }

    let rebuilt = entries.iter().zip(nested).map(|((key, value), inner)| {
        let value = match inner {
            Some(inner) => adapter.dictionary(inner),
            None => value.clone(),
        };
        (key.clone(), value)
    });
    if ordered {
        return Some(rebuilt.collect());
    }
    let mut out = BTreeMap::new();
    for (key, value) in rebuilt {
        out.entry(key).or_insert(value);
    }
    Some(out.into_iter().collect())
}

/// Combines the accumulated stronger dictionary over one weaker dictionary.
///
/// `trace` observes how every entry of `weaker` took part.
fn combine_into<K, V, A, T>(
    adapter: &A,
    stronger: &mut BTreeMap<K, V>,
    weaker: &[(K, V)],
    trace: &mut T,
) where
    K: Clone + Ord,
    V: Clone,
    A: DictionaryAdapter<K, V> + ?Sized,
    T: Trace<K>,
{
    let mut seen = BTreeSet::new();
    for (key, weak_value) in weaker {
        // Within one dictionary the first occurrence of a key wins.
        if !seen.insert(key) {
            continue;
        }
        match stronger.get_mut(key) {
            None => {
                trace.supplied(key);
                stronger.insert(key.clone(), key_ordered(adapter, weak_value));
            }
            Some(strong_value) => {
                if let (Some(strong_entries), Some(weak_entries)) =
                    (adapter.entries(strong_value), adapter.entries(weak_value))
                {
                    trace.enter(key);
                    // The stronger entries were observed when they were
                    // kept; only the weaker ones are observed here.
                    let mut merged =
                        collect_first_occurrence(adapter, strong_entries, &mut NoTrace);
                    combine_into(adapter, &mut merged, weak_entries, trace);
                    trace.leave();
                    *strong_value = adapter.dictionary(merged.into_iter().collect());
                } else {
                    // Otherwise the stronger value wins as-is.
                    trace.overridden(key);
                }
            }
        }
    }
}
