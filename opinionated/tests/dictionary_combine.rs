// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recursive dictionary combination through a host value adapter.
//!
//! The host value type here is a toy tree, deliberately not
//! `opinionated::OpinionOp`, standing in for a domain value such as USD's.

use core::cell::Cell;

use opinionated::{
    DictionaryAdapter, ShallowOverlay, combine_dictionaries, combine_dictionary_chain,
};

/// A host value: an opaque integer or a nested dictionary.
#[derive(Clone, Debug, PartialEq)]
enum Host {
    Int(i64),
    Dict(Vec<(&'static str, Self)>),
}

type Entries = Vec<(&'static str, Host)>;

/// The host's adapter: exposes nested dictionaries to the kernel.
struct HostDictionaries;

impl DictionaryAdapter<&'static str, Host> for HostDictionaries {
    fn entries<'v>(&self, value: &'v Host) -> Option<&'v [(&'static str, Host)]> {
        match value {
            Host::Dict(entries) => Some(entries),
            Host::Int(_) => None,
        }
    }

    fn dictionary(&self, entries: Entries) -> Host {
        Host::Dict(entries)
    }
}

fn dict(entries: &[(&'static str, Host)]) -> Host {
    Host::Dict(entries.to_vec())
}

fn int(value: i64) -> Host {
    Host::Int(value)
}

#[test]
fn nested_dictionaries_combine_recursively() {
    let strong = vec![("sub", dict(&[("x", int(10))])), ("only_strong", int(1))];
    let weak = vec![
        ("sub", dict(&[("x", int(99)), ("y", int(20))])),
        ("only_weak", int(2)),
    ];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&strong, &weak]),
        vec![
            ("only_strong", int(1)),
            ("only_weak", int(2)),
            ("sub", dict(&[("x", int(10)), ("y", int(20))])),
        ]
    );
}

/// The three-layer dictionary/scalar conflict, pinned to OpenUSD.
///
/// `usdcat --flatten` over sublayers carrying `customData` of
/// `{settings: {a: 1}}`, `{settings: 0}`, `{settings: {b: 2}}` (strongest
/// first) yields `{settings: {a: 1, b: 2}}`: OpenUSD folds strongest-first.
#[test]
fn dictionary_scalar_conflict_across_three_layers_folds_strongest_first() {
    let strong = vec![("settings", dict(&[("a", int(1))]))];
    let middle = vec![("settings", int(0))];
    let weak = vec![("settings", dict(&[("b", int(2))]))];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&strong, &middle, &weak]),
        vec![("settings", dict(&[("a", int(1)), ("b", int(2))]))],
    );

    // The operation is not associative: folding weakest-first would drop `b`.
    let weakest_first = combine_dictionaries(
        &HostDictionaries,
        &strong,
        &combine_dictionaries(&HostDictionaries, &middle, &weak),
    );
    assert_eq!(weakest_first, vec![("settings", dict(&[("a", int(1))]))]);
}

#[test]
fn stronger_scalar_replaces_weaker_dictionary_and_vice_versa() {
    let strong = vec![("s", int(0)), ("d", dict(&[("x", int(1))]))];
    let weak = vec![("s", dict(&[("x", int(1))])), ("d", int(5))];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&strong, &weak]),
        vec![("d", dict(&[("x", int(1))])), ("s", int(0))],
    );
}

#[test]
fn every_level_is_key_ordered_whether_merged_or_single_sided() {
    let strong = vec![
        ("zeta", int(1)),
        ("merged", dict(&[("b", int(1))])),
        // Only the stronger side contributes `alpha`.
        (
            "alpha",
            dict(&[("z", int(1)), ("y", dict(&[("q", int(1)), ("p", int(2))]))]),
        ),
    ];
    let weak = vec![
        ("merged", dict(&[("a", int(2))])),
        // Only the weaker side contributes `beta`.
        ("beta", dict(&[("n", int(3)), ("m", int(4))])),
    ];
    let expected = vec![
        (
            "alpha",
            dict(&[("y", dict(&[("p", int(2)), ("q", int(1))])), ("z", int(1))]),
        ),
        ("beta", dict(&[("m", int(4)), ("n", int(3))])),
        ("merged", dict(&[("a", int(2)), ("b", int(1))])),
        ("zeta", int(1)),
    ];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&strong, &weak]),
        expected
    );
    assert_eq!(
        combine_dictionaries(&HostDictionaries, &strong, &weak),
        expected,
        "the pairwise and chain entry points order output identically"
    );
}

#[test]
fn single_opinion_chain_is_key_ordered_at_every_level() {
    let only = vec![
        (
            "b",
            dict(&[("d", int(1)), ("c", dict(&[("f", int(2)), ("e", int(3))]))]),
        ),
        ("a", int(0)),
        ("b", int(9)),
    ];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&only]),
        vec![
            ("a", int(0)),
            (
                "b",
                dict(&[("c", dict(&[("e", int(3)), ("f", int(2))])), ("d", int(1))])
            ),
        ]
    );
}

/// Counts how often the kernel rebuilds a host dictionary.
struct CountingDictionaries(Cell<usize>);

impl DictionaryAdapter<&'static str, Host> for CountingDictionaries {
    fn entries<'v>(&self, value: &'v Host) -> Option<&'v [(&'static str, Host)]> {
        HostDictionaries.entries(value)
    }

    fn dictionary(&self, entries: Entries) -> Host {
        self.0.set(self.0.get() + 1);
        Host::Dict(entries)
    }
}

#[test]
fn already_ordered_nested_input_is_not_rebuilt() {
    let strong = vec![("a", dict(&[("x", dict(&[("m", int(1)), ("n", int(2))]))]))];
    let weak = vec![("b", dict(&[("y", int(3)), ("z", int(4))]))];

    let adapter = CountingDictionaries(Cell::new(0));
    let combined = combine_dictionary_chain(&adapter, [&strong, &weak]);
    assert_eq!(combined, vec![strong[0].clone(), weak[0].clone()]);
    assert_eq!(adapter.0.get(), 0, "ordered levels are cloned, not rebuilt");
}

#[test]
fn first_occurrence_of_a_duplicate_key_wins_within_one_dictionary() {
    let strong = vec![("k", int(1)), ("k", int(2))];
    let weak = vec![("w", dict(&[("a", int(1))])), ("w", dict(&[("b", int(2))]))];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&strong, &weak]),
        vec![("k", int(1)), ("w", dict(&[("a", int(1))]))],
    );
}

#[test]
fn fallback_seed_is_the_weakest_dictionary() {
    let authored = vec![("settings", dict(&[("a", int(1))]))];
    let fallback = vec![
        ("settings", dict(&[("a", int(0)), ("c", int(3))])),
        ("version", int(1)),
    ];

    assert_eq!(
        combine_dictionary_chain(&HostDictionaries, [&authored, &fallback]),
        vec![
            ("settings", dict(&[("a", int(1)), ("c", int(3))])),
            ("version", int(1)),
        ]
    );
}

#[test]
fn empty_chain_is_an_empty_dictionary() {
    let chain: [Entries; 0] = [];
    assert!(combine_dictionary_chain(&HostDictionaries, chain).is_empty());
}

#[test]
fn shallow_overlay_is_a_distinct_policy() {
    let strong = vec![("sub", dict(&[("x", int(10))]))];
    let weak = vec![("sub", dict(&[("y", int(20))])), ("other", int(1))];

    assert_eq!(
        combine_dictionary_chain(&ShallowOverlay, [&strong, &weak]),
        vec![("other", int(1)), ("sub", dict(&[("x", int(10))]))],
        "shallow overlay keeps the stronger nested dictionary wholesale"
    );
}
