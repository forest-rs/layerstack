// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fallback-seeded resolution coverage.
//!
//! A fallback participates as the weakest dense seed for the resolved
//! family without being modeled as an extra layer: it carries no
//! provenance, never resolves on its own, and is hidden by a strongest
//! block.

use opinionated::{
    ChainOpinion, ListOp, OpinionOp, Resolution, ResolvedValue, SparseComposer,
    resolve_ordered_chain_with_fallback,
};

#[test]
fn list_chain_folds_over_fallback_seed() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["profiler"]));
    let weak = OpinionOp::List(ListOp::deleted(vec!["legacy"]));
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"user",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"workspace",
        },
    ];
    let fallback = ResolvedValue::List(vec!["search", "legacy"]);

    let resolved = resolve_ordered_chain_with_fallback(opinions, &fallback)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["search", "profiler"].as_slice()),
        "list edits must fold over the fallback list as the weakest dense seed"
    );
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn dictionary_chain_combines_over_fallback_entries() {
    let strong = OpinionOp::<&str, &str, &str>::Dictionary(vec![("theme", "dark")]);
    let opinions = [ChainOpinion {
        op: &strong,
        provenance: &"user",
    }];
    let fallback = ResolvedValue::Dictionary(vec![("theme", "light"), ("layout", "wide")]);

    let resolved = resolve_ordered_chain_with_fallback(opinions, &fallback)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_dictionary(),
        Some([("layout", "wide"), ("theme", "dark")].as_slice()),
        "authored dictionary entries must win over fallback entries by key"
    );
}

#[test]
fn scalar_chain_never_consults_fallback() {
    let strong = OpinionOp::<&str, &str, &str>::Set("dark");
    let opinions = [ChainOpinion {
        op: &strong,
        provenance: &"user",
    }];
    let fallback = ResolvedValue::Scalar("light");

    let resolved = resolve_ordered_chain_with_fallback(opinions, &fallback)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&"dark"));
}

#[test]
fn empty_chain_with_fallback_is_absent() {
    let opinions: [ChainOpinion<'_, &str, &str, &str, &str>; 0] = [];
    let fallback = ResolvedValue::List(vec!["search"]);

    assert!(
        resolve_ordered_chain_with_fallback(opinions, &fallback).is_absent(),
        "a fallback is a seed, not an opinion: it must not resolve on its own"
    );
}

#[test]
fn strongest_block_hides_fallback() {
    let block = OpinionOp::<&str, &str, &str>::Block;
    let opinions = [ChainOpinion {
        op: &block,
        provenance: &"user",
    }];
    let fallback = ResolvedValue::List(vec!["search"]);

    assert_eq!(
        resolve_ordered_chain_with_fallback(opinions, &fallback),
        Resolution::Blocked { provenance: "user" }
    );
}

#[test]
fn weaker_block_cuts_chain_but_stronger_edits_fold_over_seed() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["profiler"]));
    let block = OpinionOp::Block;
    let weak = OpinionOp::List(ListOp::appended(vec!["legacy"]));
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"user",
        },
        ChainOpinion {
            op: &block,
            provenance: &"workspace",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];
    let fallback = ResolvedValue::List(vec!["search"]);

    let resolved = resolve_ordered_chain_with_fallback(opinions, &fallback)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["search", "profiler"].as_slice()),
        "a weaker block hides weaker opinions, not the seed stronger edits fold over"
    );
}

#[test]
fn mismatched_fallback_shape_is_ignored() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["profiler"]));
    let opinions = [ChainOpinion {
        op: &strong,
        provenance: &"user",
    }];
    let fallback = ResolvedValue::Scalar("light");

    let resolved = resolve_ordered_chain_with_fallback(opinions, &fallback)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["profiler"].as_slice()),
        "a fallback whose shape does not match the resolved family must not contribute"
    );
}

#[test]
fn composer_resolves_list_field_over_fallback() {
    let mut composer: SparseComposer<&str, &str, &str, &str, &str, String, &str> =
        SparseComposer::try_new(["user", "workspace"]).unwrap();
    composer
        .set_opinion(
            "user",
            "editor",
            "panels",
            OpinionOp::List(ListOp::appended(vec!["profiler"])),
            "user-layer",
        )
        .unwrap();

    let fallback = ResolvedValue::List(vec!["search"]);
    let resolved = composer
        .resolve_with_fallback("editor", "panels", &fallback)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["search", "profiler"].as_slice())
    );
    assert_eq!(resolved.provenance, "user-layer");

    assert!(
        composer
            .resolve_with_fallback("editor", "missing", &fallback)
            .is_absent(),
        "an unauthored key must stay absent even when a fallback is supplied"
    );
}
