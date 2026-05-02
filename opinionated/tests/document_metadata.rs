// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document-metadata coverage for typed address reuse outside settings.

use opinionated::{ListOp, OpinionOp, SparseComposer};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layer {
    LocalDraft,
    Template,
    Defaults,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Address {
    Document(u32),
    Section(u32),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Field {
    Title,
    Authors,
    Attributes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Metadata {
    Text(&'static str),
    Bool(bool),
}

type Composer =
    SparseComposer<Layer, Address, Field, Metadata, &'static str, &'static str, &'static str>;

fn composer() -> Composer {
    SparseComposer::try_new([Layer::LocalDraft, Layer::Template, Layer::Defaults]).unwrap()
}

#[test]
fn document_and_section_addresses_are_independent() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Document(1),
            Field::Title,
            OpinionOp::Set(Metadata::Text("Untitled")),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::LocalDraft,
            Address::Section(1),
            Field::Title,
            OpinionOp::Set(Metadata::Text("Overview")),
            "draft",
        )
        .unwrap();

    assert_eq!(
        composer
            .resolve(Address::Document(1), Field::Title)
            .resolved()
            .unwrap()
            .value
            .as_scalar(),
        Some(&Metadata::Text("Untitled"))
    );
    assert_eq!(
        composer
            .resolve(Address::Section(1), Field::Title)
            .resolved()
            .unwrap()
            .value
            .as_scalar(),
        Some(&Metadata::Text("Overview"))
    );
}

#[test]
fn authors_compose_as_unique_ordered_list() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Document(1),
            Field::Authors,
            OpinionOp::List(ListOp {
                explicit: Some(vec!["template"]),
                ..ListOp::default()
            }),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::LocalDraft,
            Address::Document(1),
            Field::Authors,
            OpinionOp::List(ListOp {
                prepend: vec!["alice"],
                append: vec!["reviewer"],
                ..ListOp::default()
            }),
            "draft",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Document(1), Field::Authors)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_list(),
        Some(["alice", "template", "reviewer"].as_slice())
    );
    assert_eq!(resolved.provenance, "draft");
}

#[test]
fn attributes_combine_across_layers() {
    let mut composer = composer();
    composer
        .set_opinion(
            Layer::Defaults,
            Address::Document(1),
            Field::Attributes,
            OpinionOp::Dictionary(vec![
                ("indexed", Metadata::Bool(true)),
                ("status", Metadata::Text("draft")),
            ]),
            "defaults",
        )
        .unwrap();
    composer
        .set_opinion(
            Layer::Template,
            Address::Document(1),
            Field::Attributes,
            OpinionOp::Dictionary(vec![("status", Metadata::Text("template"))]),
            "template",
        )
        .unwrap();

    let resolved = composer
        .resolve(Address::Document(1), Field::Attributes)
        .resolved()
        .unwrap();
    assert_eq!(
        resolved.value.as_dictionary(),
        Some(
            [
                ("indexed", Metadata::Bool(true)),
                ("status", Metadata::Text("template"))
            ]
            .as_slice()
        )
    );
    assert_eq!(resolved.provenance, "template");
}
