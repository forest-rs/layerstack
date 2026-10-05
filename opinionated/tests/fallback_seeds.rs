// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fallback-seeded resolution coverage.
//!
//! A fallback participates as the weakest dense seed for the resolved
//! family without being modeled as an extra layer: it carries no
//! provenance, never resolves on its own, and is hidden by a strongest
//! block.

use opinionated::{
    ChainOpinion, ListOp, OpinionKind, OpinionOp, Resolution, ResolutionEvent, ResolvedValue,
    SparseComposer, resolve_ordered_chain_report, resolve_ordered_chain_with_fallback,
    resolve_ordered_chain_with_fallback_report,
};

#[test]
fn composer_explanation_matches_seeded_resolution() {
    let mut composer: SparseComposer<&str, &str, &str, &str, &str, &str, &str> =
        SparseComposer::try_new(["user"]).unwrap();
    composer
        .set_opinion(
            "user",
            "editor",
            "plugins",
            OpinionOp::List(ListOp::appended(vec!["lint"])),
            "user.toml",
        )
        .unwrap();
    let fallback = ResolvedValue::List(vec!["core"]);
    let resolution = composer.resolve_with_fallback("editor", "plugins", &fallback);
    let report = composer.explain_with_fallback("editor", "plugins", &fallback);
    assert_eq!(
        report.resolution, resolution,
        "explaining a seeded resolution must retain its fallback contribution"
    );
    assert_eq!(
        report.events,
        [ResolutionEvent::Contributed {
            provenance: "user.toml",
            kind: OpinionKind::List,
        }],
        "the fallback must not masquerade as an authored opinion"
    );
    let missing = composer.explain_with_fallback("editor", "missing", &fallback);
    assert!(missing.resolution.is_absent());
    assert!(missing.events.is_empty());
}

// Exercise both public report paths with mixed families, explicit lists,
// blocks at every strength, and empty or shape-mismatched fallback seeds.
#[test]
fn seeded_reports_match_resolution_across_short_chains() {
    let ops: [OpinionOp<&str, &str, &str>; 8] = [
        OpinionOp::Set("dark"),
        OpinionOp::Block,
        OpinionOp::List(ListOp::appended(vec!["lint"])),
        OpinionOp::List(ListOp::deleted(vec!["core"])),
        OpinionOp::List(ListOp::explicit(vec!["standalone"])),
        OpinionOp::List(ListOp::explicit(vec![])),
        OpinionOp::Dictionary(vec![("theme", "dark")]),
        OpinionOp::Dictionary(vec![("layout", "wide")]),
    ];
    let fallbacks = [
        ResolvedValue::Scalar("light"),
        ResolvedValue::List(vec!["core"]),
        ResolvedValue::List(vec![]),
        ResolvedValue::Dictionary(vec![("theme", "light"), ("font", "mono")]),
    ];
    for count in 0..=3 {
        for mut code in 0..ops.len().pow(count) {
            let mut authored = Vec::new();
            for _ in 0..count {
                authored.push(&ops[code % ops.len()]);
                code /= ops.len();
            }
            let layers: Vec<_> = (0..authored.len()).collect();
            let chain = || {
                authored
                    .iter()
                    .zip(&layers)
                    .map(|(op, provenance)| ChainOpinion { op, provenance })
            };
            let plain_report = resolve_ordered_chain_report(chain());
            let mut composer = SparseComposer::try_new(layers.iter().copied()).unwrap();
            for (&op, &layer) in authored.iter().zip(&layers) {
                composer
                    .set_opinion(layer, "editor", "field", op.clone(), layer)
                    .unwrap();
            }
            for fallback in &fallbacks {
                let resolution = resolve_ordered_chain_with_fallback(chain(), fallback);
                let report = resolve_ordered_chain_with_fallback_report(chain(), fallback);
                assert_eq!(
                    report.resolution, resolution,
                    "chain {authored:?}, seed {fallback:?}"
                );
                assert_eq!(
                    report.events, plain_report.events,
                    "seeds add no events or cutoffs"
                );
                assert_eq!(report.events.len(), authored.len());
                assert_eq!(
                    composer.resolve_with_fallback("editor", "field", fallback),
                    resolution
                );
                assert_eq!(
                    composer.explain_with_fallback("editor", "field", fallback),
                    report
                );
            }
        }
    }
}

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
