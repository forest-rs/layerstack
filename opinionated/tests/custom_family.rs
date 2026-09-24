// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An out-of-crate family proving a second value family folds over the kernel
//! without touching `opinionated`'s closed enums.
//!
//! The toy family models a running counter: sparse deltas compose over a dense
//! total, a reset blocks weaker opinions, and a label belongs to another family
//! and is skipped. This exercises the kernel's five fold invariants end to end.

use opinionated::{
    FamilyEvent, FamilyMember, FamilyReport, FamilyResolution, IgnoreReason, OpinionFamily,
    OpinionKind, resolve_family_chain, resolve_family_chain_report,
};

/// A counter operation authored at one layer. This is deliberately *not*
/// `opinionated::OpinionOp`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CounterOp {
    /// A dense, self-sufficient running total.
    Total(i64),
    /// A sparse delta applied over a weaker total.
    Delta(i64),
    /// Resets (blocks) weaker opinions.
    Reset,
    /// A value from another family; skipped by the counter fold.
    Label(&'static str),
}

/// The counter family: dense `i64` totals with additive sparse deltas.
struct CounterFamily;

impl OpinionFamily<CounterOp> for CounterFamily {
    type Value = i64;
    type Edit = i64;

    fn classify(&self, op: &CounterOp) -> FamilyMember<Self::Value, Self::Edit> {
        match op {
            CounterOp::Total(total) => FamilyMember::Dense(*total),
            CounterOp::Delta(delta) => FamilyMember::Sparse(*delta),
            CounterOp::Reset => FamilyMember::Block,
            // A foreign op reuses `opinionated`'s shared `IgnoreReason`; the
            // concrete variant only signals the op was skipped.
            CounterOp::Label(_) => FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                resolved: OpinionKind::Set,
                ignored: OpinionKind::Set,
            }),
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        base + edit
    }

    fn seed(&self) -> Self::Value {
        0
    }
}

/// Builds the `(op, provenance)` iterator the kernel consumes.
fn pairs<'a>(
    chain: &'a [(CounterOp, &'a str)],
) -> impl Iterator<Item = (&'a CounterOp, &'a &'a str)> {
    chain.iter().map(|(op, provenance)| (op, provenance))
}

#[test]
fn empty_chain_is_absent() {
    let chain: [(CounterOp, &str); 0] = [];
    assert_eq!(
        resolve_family_chain(&CounterFamily, pairs(&chain)),
        FamilyResolution::Absent
    );
}

#[test]
fn dense_total_absorbs_stronger_delta_and_terminates() {
    // Delta(+2) is strongest, then a dense Total(10) terminates the fold; the
    // weaker Delta(+100) below the total never contributes.
    let chain = [
        (CounterOp::Delta(2), "user"),
        (CounterOp::Total(10), "workspace"),
        (CounterOp::Delta(100), "defaults"),
    ];
    assert_eq!(
        resolve_family_chain(&CounterFamily, pairs(&chain)),
        FamilyResolution::Resolved {
            value: 12,
            provenance: "user",
        }
    );
}

#[test]
fn sparse_over_sparse_materializes_over_seed() {
    // No dense member: deltas materialize weakest-first over seed 0.
    let chain = [
        (CounterOp::Delta(3), "user"),
        (CounterOp::Delta(4), "defaults"),
    ];
    assert_eq!(
        resolve_family_chain(&CounterFamily, pairs(&chain)),
        FamilyResolution::Resolved {
            value: 7,
            provenance: "user",
        }
    );
}

#[test]
fn block_cuts_but_accumulated_edits_materialize() {
    // Reset cuts the chain, but the stronger Delta(+5) still materializes over
    // the seed; the weaker Total(100) below the reset is discarded.
    let chain = [
        (CounterOp::Delta(5), "user"),
        (CounterOp::Reset, "workspace"),
        (CounterOp::Total(100), "defaults"),
    ];
    assert_eq!(
        resolve_family_chain(&CounterFamily, pairs(&chain)),
        FamilyResolution::Resolved {
            value: 5,
            provenance: "user",
        }
    );
}

#[test]
fn block_before_any_contribution_is_blocked() {
    let chain = [
        (CounterOp::Reset, "workspace"),
        (CounterOp::Delta(5), "defaults"),
    ];
    assert_eq!(
        resolve_family_chain(&CounterFamily, pairs(&chain)),
        FamilyResolution::Blocked {
            provenance: "workspace",
        }
    );
}

#[test]
fn foreign_op_is_skipped_and_reported() {
    // The strongest op is foreign: it is skipped, does not set provenance, and
    // the weaker Delta is what resolves.
    let chain = [
        (CounterOp::Label("note"), "user"),
        (CounterOp::Delta(5), "defaults"),
    ];

    let report: FamilyReport<i64, &str> =
        resolve_family_chain_report(&CounterFamily, pairs(&chain));
    assert_eq!(
        report.resolution,
        FamilyResolution::Resolved {
            value: 5,
            provenance: "defaults",
        }
    );
    assert_eq!(
        report.events,
        vec![
            FamilyEvent::Ignored {
                provenance: "user",
                reason: IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::Set,
                    ignored: OpinionKind::Set,
                },
            },
            FamilyEvent::ContributedSparse {
                provenance: "defaults",
            },
        ]
    );
}

#[test]
fn report_records_dense_termination() {
    let chain = [
        (CounterOp::Delta(2), "user"),
        (CounterOp::Total(10), "workspace"),
    ];
    let report = resolve_family_chain_report(&CounterFamily, pairs(&chain));
    assert_eq!(
        report.events,
        vec![
            FamilyEvent::ContributedSparse { provenance: "user" },
            FamilyEvent::ContributedDense {
                provenance: "workspace",
            },
        ]
    );
}

#[test]
fn fold_stops_pulling_at_dense_member_or_block() {
    // Callers that sample or clone lazily per opinion (e.g. time-sampled
    // domains) rely on the kernel never pulling past the member that ends the
    // fold.
    let dense = [
        (CounterOp::Delta(1), "strong"),
        (CounterOp::Total(10), "dense"),
        (CounterOp::Delta(100), "hidden"),
        (CounterOp::Total(1000), "hidden"),
    ];
    let mut pulled = 0;
    let resolved = resolve_family_chain(&CounterFamily, pairs(&dense).inspect(|_| pulled += 1));
    assert_eq!(resolved.resolved(), Some((11, "strong")));
    assert_eq!(
        pulled, 2,
        "no opinion weaker than the dense total is pulled"
    );

    let blocked = [
        (CounterOp::Delta(1), "strong"),
        (CounterOp::Reset, "block"),
        (CounterOp::Total(1000), "hidden"),
    ];
    let mut pulled = 0;
    let report =
        resolve_family_chain_report(&CounterFamily, pairs(&blocked).inspect(|_| pulled += 1));
    assert_eq!(report.resolution.resolved(), Some((1, "strong")));
    assert_eq!(pulled, 2, "no opinion weaker than the block is pulled");
}
