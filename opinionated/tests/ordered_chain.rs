// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Storage-agnostic ordered-chain resolution coverage.

use opinionated::{
    ChainOpinion, IgnoreReason, ListOp, OpinionKind, OpinionOp, Resolution, ResolutionEvent,
    resolve_ordered_chain, resolve_ordered_chain_report,
};

#[test]
fn ordered_chain_resolves_strongest_scalar() {
    let strong = OpinionOp::<&str>::Set("dark");
    let weak = OpinionOp::Set("light");
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"project",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];

    let resolved = resolve_ordered_chain(opinions).resolved().unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&"dark"));
    assert_eq!(resolved.provenance, "project");
}

#[test]
fn ordered_chain_block_suppresses_weaker_scalar() {
    let block = OpinionOp::<&str>::Block;
    let weak = OpinionOp::Set("light");
    let opinions = [
        ChainOpinion {
            op: &block,
            provenance: &"project",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];

    assert_eq!(
        resolve_ordered_chain(opinions),
        Resolution::Blocked {
            provenance: "project"
        }
    );
}

#[test]
fn ordered_chain_without_opinions_is_absent() {
    let opinions: [ChainOpinion<'_, &str, &str, &str, &str>; 0] = [];

    assert!(resolve_ordered_chain(opinions).is_absent());
}

#[test]
fn ordered_chain_composes_lists_until_block() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp {
        append: vec!["theme"],
        ..ListOp::default()
    });
    let block = OpinionOp::Block;
    let weak = OpinionOp::List(ListOp {
        explicit: Some(vec!["search"]),
        ..ListOp::default()
    });
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

    let resolved = resolve_ordered_chain(opinions).resolved().unwrap();
    assert_eq!(resolved.value.as_list(), Some(["theme"].as_slice()));
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn ordered_chain_combines_dictionaries_until_block() {
    let strong = OpinionOp::<u32, u32, &str>::Dictionary(vec![("font_size", 14)]);
    let weak = OpinionOp::Dictionary(vec![("font_size", 12), ("max_tabs", 8)]);
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"project",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];

    let resolved = resolve_ordered_chain(opinions).resolved().unwrap();
    assert_eq!(
        resolved.value.as_dictionary(),
        Some([("font_size", 14), ("max_tabs", 8)].as_slice())
    );
    assert_eq!(resolved.provenance, "project");
}

#[test]
fn ordered_chain_report_marks_mixed_operation_kinds() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp {
        append: vec!["theme"],
        ..ListOp::default()
    });
    let weak = OpinionOp::Set("light");
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"user",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];

    let report = resolve_ordered_chain_report(opinions);
    assert_eq!(
        report.resolution.resolved().unwrap().value.as_list(),
        Some(["theme"].as_slice())
    );
    assert_eq!(
        report.events,
        vec![
            ResolutionEvent::Contributed {
                provenance: "user",
                kind: OpinionKind::List,
            },
            ResolutionEvent::Ignored {
                provenance: "defaults",
                reason: IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::List,
                    ignored: OpinionKind::Set,
                },
            },
        ]
    );
}

#[test]
fn lean_and_report_resolvers_agree() {
    let strong = OpinionOp::<&str, &str, &str>::List(ListOp {
        append: vec!["theme"],
        ..ListOp::default()
    });
    let incompatible = OpinionOp::Set("light");
    let weak = OpinionOp::List(ListOp {
        explicit: Some(vec!["search"]),
        ..ListOp::default()
    });
    let opinions = [
        ChainOpinion {
            op: &strong,
            provenance: &"user",
        },
        ChainOpinion {
            op: &incompatible,
            provenance: &"workspace",
        },
        ChainOpinion {
            op: &weak,
            provenance: &"defaults",
        },
    ];

    assert_eq!(
        resolve_ordered_chain(opinions),
        resolve_ordered_chain_report(opinions).resolution
    );
}
