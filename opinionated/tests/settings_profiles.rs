// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Settings-profile coverage for layered sparse opinions.

use opinionated::{
    IgnoreReason, ListOp, OpinionKey, OpinionKind, OpinionOp, Resolution, ResolutionEvent,
    SparseComposer, UnknownLayer,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layer {
    User,
    Workspace,
    Defaults,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Address {
    Editor,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Field {
    Theme,
    EnabledFeatures,
    Limits,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Setting {
    Text(&'static str),
    Number(u32),
}

type Composer =
    SparseComposer<Layer, Address, Field, Setting, &'static str, &'static str, &'static str>;

fn composer() -> Composer {
    SparseComposer::try_new([Layer::User, Layer::Workspace, Layer::Defaults]).unwrap()
}

#[test]
fn stronger_scalar_overrides_weaker_default() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::Theme)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&Setting::Text("dark")));
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn blocked_value_suppresses_weaker_opinion() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Block,
            "user",
        )
        .unwrap();

    assert_eq!(
        composer.resolve(Address::Editor, Field::Theme),
        Resolution::Blocked { provenance: "user" }
    );
}

#[test]
fn blocked_is_distinct_from_absent() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Block,
            "user",
        )
        .unwrap();

    assert!(composer.resolve(Address::Editor, Field::Theme).is_blocked());
    assert!(
        composer
            .resolve(Address::Terminal, Field::Theme)
            .is_absent()
    );
}

#[test]
fn list_ops_compose_strong_to_weak() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::explicit(vec!["search", "outline"])),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::appended(vec!["lint"])),
            "workspace",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::prepended(vec!["theme"]).with_deleted(vec!["outline"])),
            "user",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::EnabledFeatures)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["theme", "search", "lint"].as_slice())
    );
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn explicit_list_makes_other_edits_spurious() {
    let op = ListOp {
        explicit: Some(vec!["search", "outline"]),
        prepend: vec!["theme"],
        append: vec!["lint"],
        delete: vec!["outline"],
    };

    assert_eq!(op.apply_to(&["base"]), vec!["search", "outline"]);
}

#[test]
fn list_block_suppresses_weaker_list_ops() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::explicit(vec!["search", "outline"])),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::Block,
            "workspace",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::appended(vec!["theme"])),
            "user",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::EnabledFeatures)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_list(), Some(["theme"].as_slice()));
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn dictionary_combines_missing_weaker_keys() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Limits,
            OpinionOp::Dictionary(vec![
                ("max_tabs", Setting::Number(8)),
                ("font_size", Setting::Number(12)),
            ]),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::Limits,
            OpinionOp::Dictionary(vec![("font_size", Setting::Number(14))]),
            "workspace",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::Limits)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_dictionary(),
        Some(
            [
                ("font_size", Setting::Number(14)),
                ("max_tabs", Setting::Number(8))
            ]
            .as_slice()
        )
    );
    assert_eq!(resolved.provenance, "workspace");
}

#[test]
fn dictionary_block_suppresses_weaker_dictionaries() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Limits,
            OpinionOp::Dictionary(vec![("max_tabs", Setting::Number(8))]),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::Limits,
            OpinionOp::Block,
            "workspace",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Limits,
            OpinionOp::Dictionary(vec![("font_size", Setting::Number(14))]),
            "user",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::Limits)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_dictionary(),
        Some([("font_size", Setting::Number(14))].as_slice())
    );
    assert_eq!(resolved.provenance, "user");
}

#[test]
fn unrelated_address_does_not_affect_resolution() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::User,
            Address::Terminal,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();

    assert!(composer.resolve(Address::Editor, Field::Theme).is_absent());
}

#[test]
fn setting_again_replaces_previous_opinion_in_layer() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "first",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("contrast")),
            "second",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Editor, Field::Theme)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&Setting::Text("contrast")));
    assert_eq!(resolved.provenance, "second");
    assert_eq!(
        composer.opinion_stack(Address::Editor, Field::Theme).len(),
        1
    );
}

#[test]
fn remove_opinion_restores_weaker_value() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();

    assert_eq!(
        composer.remove_opinion(Layer::User, Address::Editor, Field::Theme),
        Ok(true)
    );
    let resolved = composer
        .resolve(Address::Editor, Field::Theme)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&Setting::Text("light")));

    assert_eq!(
        composer.remove_opinion(Layer::User, Address::Editor, Field::Theme),
        Ok(false)
    );
}

#[test]
fn clear_layer_removes_all_opinions_for_layer() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Terminal,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();

    assert_eq!(composer.clear_layer(Layer::User), Ok(2));

    let resolved = composer
        .resolve(Address::Editor, Field::Theme)
        .resolved()
        .unwrap();
    assert_eq!(resolved.value.as_scalar(), Some(&Setting::Text("light")));
    assert!(
        composer
            .resolve(Address::Terminal, Field::Theme)
            .is_absent()
    );
}

#[test]
fn keys_enumerate_authored_opinions() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Terminal,
            Field::Limits,
            OpinionOp::Dictionary(vec![("max_tabs", Setting::Number(8))]),
            "defaults",
        )
        .unwrap();

    let keys: Vec<_> = composer.keys().collect();
    assert_eq!(
        keys,
        vec![
            &OpinionKey::new(Address::Editor, Field::Theme),
            &OpinionKey::new(Address::Terminal, Field::Limits),
        ]
    );
}

#[test]
fn unknown_layer_is_rejected() {
    let mut composer = composer();

    assert_eq!(
        composer.set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        ),
        Ok(())
    );

    let mut limited =
        SparseComposer::<Layer, Address, Field, Setting>::try_new([Layer::User]).unwrap();
    assert_eq!(
        limited.set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            (),
        ),
        Err(UnknownLayer {
            layer: Layer::Defaults
        })
    );
    assert_eq!(
        limited.remove_opinion(Layer::Defaults, Address::Editor, Field::Theme),
        Err(UnknownLayer {
            layer: Layer::Defaults
        })
    );
    assert_eq!(
        limited.clear_layer(Layer::Defaults),
        Err(UnknownLayer {
            layer: Layer::Defaults
        })
    );
}

#[test]
fn duplicate_layers_are_rejected() {
    let err = SparseComposer::<Layer, Address, Field, Setting>::try_new([Layer::User, Layer::User])
        .unwrap_err();
    assert_eq!(err.layer, Layer::User);
}

#[test]
fn explain_reports_scalar_winner_ignores_weaker_opinions() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::explicit(vec!["search"])),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::Block,
            "workspace",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::Set(Setting::Text("not-a-list")),
            "user",
        )
        .unwrap();

    let report = composer.explain(Address::Editor, Field::EnabledFeatures);
    assert_eq!(
        report.resolution.resolved().unwrap().value.as_scalar(),
        Some(&Setting::Text("not-a-list"))
    );
    assert_eq!(
        report.events,
        vec![
            ResolutionEvent::Contributed {
                provenance: "user",
                kind: OpinionKind::Set,
            },
            ResolutionEvent::Ignored {
                provenance: "workspace",
                reason: IgnoreReason::WeakerThanSet,
            },
            ResolutionEvent::Ignored {
                provenance: "defaults",
                reason: IgnoreReason::WeakerThanSet,
            },
        ]
    );
}

#[test]
fn explain_reports_list_contributors_and_blocked_weaker_opinions() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::explicit(vec!["search"])),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Workspace,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::Block,
            "workspace",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::EnabledFeatures,
            OpinionOp::List(ListOp::appended(vec!["theme"])),
            "user",
        )
        .unwrap();

    let report = composer.explain(Address::Editor, Field::EnabledFeatures);
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
            ResolutionEvent::StoppedByBlock {
                provenance: "workspace",
            },
            ResolutionEvent::Ignored {
                provenance: "defaults",
                reason: IgnoreReason::WeakerThanBlock,
            },
        ]
    );
}

#[test]
fn explain_reports_block_outcome() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Block,
            "user",
        )
        .unwrap();

    let report = composer.explain(Address::Editor, Field::Theme);
    assert_eq!(
        report.resolution,
        Resolution::Blocked { provenance: "user" }
    );
    assert_eq!(
        report.events,
        vec![
            ResolutionEvent::StoppedByBlock { provenance: "user" },
            ResolutionEvent::Ignored {
                provenance: "defaults",
                reason: IgnoreReason::WeakerThanBlock,
            },
        ]
    );
}

#[test]
fn opinion_stack_reports_strength_order() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("light")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::User,
            Address::Editor,
            Field::Theme,
            OpinionOp::Set(Setting::Text("dark")),
            "user",
        )
        .unwrap();

    let stack = composer.opinion_stack(Address::Editor, Field::Theme);
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0].layer, &Layer::User);
    assert_eq!(stack[0].provenance, &"user");
    assert_eq!(stack[1].layer, &Layer::Defaults);
    assert_eq!(stack[1].provenance, &"defaults");
}
