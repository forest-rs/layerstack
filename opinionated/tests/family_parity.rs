// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parity coverage: the in-crate family adapters over the family kernel produce
//! results identical to [`resolve_ordered_chain`] for the existing chain shapes.

use core::fmt::Debug;

use opinionated::{
    ChainOpinion, DictionaryFamily, FamilyResolution, ListFamily, ListOp, OpinionKind, OpinionOp,
    Resolution, Resolved, ResolvedValue, ScalarFamily, resolve_family_chain,
    resolve_family_chain_report, resolve_ordered_chain,
};

/// Maps a family resolution into the enum-layer [`Resolution`] for comparison.
fn to_resolution<T, V, I, K, P>(
    resolution: FamilyResolution<T, P>,
    wrap: impl FnOnce(T) -> ResolvedValue<V, I, K>,
) -> Resolution<V, I, K, P> {
    match resolution {
        FamilyResolution::Absent => Resolution::Absent,
        FamilyResolution::Blocked { provenance } => Resolution::Blocked { provenance },
        FamilyResolution::Resolved { value, provenance } => Resolution::Resolved(Resolved {
            value: wrap(value),
            provenance,
        }),
    }
}

/// Selects the family by the strongest op's kind and folds via the kernel.
fn kernel_resolve<V, I, K, P>(chain: &[(OpinionOp<V, I, K>, P)]) -> Resolution<V, I, K, P>
where
    V: Clone,
    I: Clone + Eq,
    K: Clone + Ord,
    P: Clone,
{
    let Some((strongest, _)) = chain.first() else {
        return Resolution::Absent;
    };
    let pairs = || chain.iter().map(|(op, provenance)| (op, provenance));
    match strongest.kind() {
        // A block resolves the same way regardless of the selected family; the
        // scalar family carries it.
        OpinionKind::Set | OpinionKind::Block => to_resolution(
            resolve_family_chain(&ScalarFamily, pairs()),
            ResolvedValue::Scalar,
        ),
        OpinionKind::List => to_resolution(
            resolve_family_chain(&ListFamily, pairs()),
            ResolvedValue::List,
        ),
        OpinionKind::Dictionary => to_resolution(
            resolve_family_chain(&DictionaryFamily, pairs()),
            ResolvedValue::Dictionary,
        ),
    }
}

/// Asserts the kernel and the direct resolver agree on `chain`.
fn assert_parity<V, I, K, P>(chain: Vec<(OpinionOp<V, I, K>, P)>)
where
    V: Clone + Debug + PartialEq,
    I: Clone + Eq + Debug,
    K: Clone + Ord + Debug,
    P: Clone + Debug + PartialEq,
{
    let direct = resolve_ordered_chain(
        chain
            .iter()
            .map(|(op, provenance)| ChainOpinion { op, provenance }),
    );
    let via_kernel = kernel_resolve(&chain);
    assert_eq!(via_kernel, direct, "kernel parity mismatch for chain");
}

#[test]
fn empty_chain_is_absent() {
    let chain: Vec<(OpinionOp<&str, &str, &str>, &str)> = Vec::new();
    assert_parity(chain);
}

#[test]
fn scalar_strongest_wins() {
    assert_parity(vec![
        (OpinionOp::<&str, &str, &str>::Set("dark"), "project"),
        (OpinionOp::Set("light"), "defaults"),
    ]);
}

#[test]
fn scalar_block_suppresses_weaker() {
    assert_parity(vec![
        (OpinionOp::<&str, &str, &str>::Block, "project"),
        (OpinionOp::Set("light"), "defaults"),
    ]);
}

#[test]
fn scalar_set_over_block_resolves() {
    assert_parity(vec![
        (OpinionOp::<&str, &str, &str>::Set("dark"), "project"),
        (OpinionOp::Block, "defaults"),
    ]);
}

#[test]
fn list_composes_until_block() {
    assert_parity(vec![
        (
            OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["theme"])),
            "user",
        ),
        (OpinionOp::Block, "workspace"),
        (
            OpinionOp::List(ListOp::explicit(vec!["search"])),
            "defaults",
        ),
    ]);
}

#[test]
fn list_composes_weak_to_strong() {
    assert_parity(vec![
        (
            OpinionOp::<&str, &str, &str>::List(
                ListOp::prepended(vec!["theme"]).with_deleted(vec!["outline"]),
            ),
            "user",
        ),
        (OpinionOp::List(ListOp::appended(vec!["lint"])), "workspace"),
        (
            OpinionOp::List(ListOp::explicit(vec!["search", "outline"])),
            "defaults",
        ),
    ]);
}

#[test]
fn list_ignores_incompatible_weaker_ops() {
    assert_parity(vec![
        (
            OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["theme"])),
            "user",
        ),
        (OpinionOp::Set("light"), "workspace"),
        (
            OpinionOp::List(ListOp::explicit(vec!["search"])),
            "defaults",
        ),
    ]);
}

#[test]
fn dictionary_combines_until_block() {
    assert_parity(vec![
        (
            OpinionOp::<u32, u32, &str>::Dictionary(vec![("font_size", 14)]),
            "project",
        ),
        (OpinionOp::Block, "workspace"),
        (
            OpinionOp::Dictionary(vec![("font_size", 12), ("max_tabs", 8)]),
            "defaults",
        ),
    ]);
}

#[test]
fn dictionary_combines_missing_weaker_keys() {
    assert_parity(vec![
        (
            OpinionOp::<u32, u32, &str>::Dictionary(vec![("font_size", 14)]),
            "workspace",
        ),
        (
            OpinionOp::Dictionary(vec![("font_size", 12), ("max_tabs", 8)]),
            "defaults",
        ),
    ]);
}

#[test]
fn dictionary_ignores_incompatible_weaker_ops() {
    assert_parity(vec![
        (
            OpinionOp::<u32, u32, &str>::Dictionary(vec![("max_tabs", 8)]),
            "project",
        ),
        (OpinionOp::List(ListOp::default()), "workspace"),
        (OpinionOp::Dictionary(vec![("font_size", 14)]), "defaults"),
    ]);
}

#[test]
fn report_and_lean_resolutions_agree() {
    let chain = [
        (
            OpinionOp::<&str, &str, &str>::List(ListOp::appended(vec!["theme"])),
            "user",
        ),
        (OpinionOp::Set("light"), "workspace"),
        (
            OpinionOp::List(ListOp::explicit(vec!["search"])),
            "defaults",
        ),
    ];
    let pairs = || chain.iter().map(|(op, provenance)| (op, provenance));
    assert_eq!(
        resolve_family_chain(&ListFamily, pairs()),
        resolve_family_chain_report(&ListFamily, pairs()).resolution
    );
}
