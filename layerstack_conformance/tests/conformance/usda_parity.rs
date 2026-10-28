// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA layers read and written as OpenUSD 26.11's text format reads and
//! writes them.
//!
//! `scripts/usda_parity_oracle.py` opens each case of
//! `fixtures/usda_parity/oracle.json` with OpenUSD and records whether it
//! reads, its `ExportToString` and, for the properties a case queries, the
//! composed targets or connections (see the script for every case). Here:
//!
//! - a case OpenUSD reads, layerstack's [`read_usda`] reads with no
//!   error, [`save_usda`] writes OpenUSD's text byte for byte, and a
//!   stage on the layer composes the same targets;
//! - a case OpenUSD refuses, layerstack's reader refuses too: it reports an
//!   error, which a strict import fails on. A warning is allowed where
//!   OpenUSD reads a statement and silently ignores it.
//!
//! [`DIFFERENCES`] lists the cases layerstack knowingly handles otherwise,
//! each with its reason and outcome; a listed case whose outcome changes
//! fails the test too, so the list stays current.
//!
//! Spec: AOUSD Core §16 (the USDA text format), §12.4 (list ops).

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset, Stage,
    StageOptions, TokenInterner,
};
use layerstack_usda::diagnostic::{Diagnostic, Severity};
use layerstack_usda::read_usda;
use layerstack_usda::save::{save_usda, save_usda_to};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/usda_parity/oracle.json");

/// Resolves no asset: arcs keep their authored asset paths.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// What layerstack does with a case OpenUSD reads, where it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handling {
    /// The reader reports an error, so a strict import refuses the layer.
    Refused,
    /// The reader reads the layer, and the writer refuses to save it.
    SaveRefused,
}

/// A case layerstack knowingly handles otherwise than OpenUSD 26.11.
struct Difference {
    /// The case's name.
    case: &'static str,
    /// What layerstack does with it.
    handling: Handling,
    /// Why.
    reason: &'static str,
}

/// The cases layerstack handles otherwise than OpenUSD 26.11, in name
/// order. Both are strings OpenUSD holds as bytes in a `std::string`,
/// where a layerstack string is UTF-8 (`Arc<str>`) and the writer never
/// drops part of a value.
const DIFFERENCES: &[Difference] = &[
    Difference {
        case: "read_string_byte_escapes",
        handling: Handling::Refused,
        reason: "escapes that make bytes that are not UTF-8 (`\\xff`) are \
                 reported, as for `46_weirdStringContent.usda` in `usda_corpus`: \
                 OpenUSD keeps the bytes and writes them back escaped, and the \
                 reader refuses to change them rather than keep them",
    },
    Difference {
        case: "write_string_nul",
        handling: Handling::SaveRefused,
        reason: "OpenUSD reads `\"a\\x00b\"` whole and writes `\"a\"` \
                 (`Sdf_FileIOUtility::Quote` stops at the first NUL), so its own \
                 round trip shortens the value; the writer refuses the string \
                 (`WriteError::NulInString`) rather than drop the rest of it",
    },
];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// What layerstack makes of one case's source.
struct Outcome {
    /// Every diagnostic the reader reported, one per line.
    diagnostics: Vec<String>,
    /// The saved text, or the save error.
    saved: Result<String, String>,
    /// The composed targets of each queried property, in query order.
    targets: Vec<(String, Vec<String>)>,
}

fn outcome(source: &str, queries: &[&str]) -> Outcome {
    let mut store = InMemoryStore::default();
    let read = read_usda(
        source,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    // Errors, which a strict import fails on; a warning reports what is
    // read as OpenUSD reads it.
    let error = |d: &&Diagnostic| d.severity == Severity::Error;
    let diagnostics = read
        .parse_diagnostics
        .iter()
        .filter(error)
        .map(|d| format!("parse: {d:?}"))
        .chain(
            read.lower_diagnostics
                .iter()
                .filter(error)
                .map(|d| format!("lower: {d:?}")),
        )
        .chain(
            read.emitted
                .diagnostics
                .iter()
                .filter(error)
                .map(|d| format!("emit: {d:?}")),
        )
        .collect();
    let saved =
        save_usda(&read.emitted.layer, store.tokens(), store.paths()).map_err(|e| format!("{e:?}"));
    let mut streamed = String::new();
    let streaming = save_usda_to(
        &read.emitted.layer,
        store.tokens(),
        store.paths(),
        &mut streamed,
    )
    .map(|()| streamed)
    .map_err(|e| format!("{e:?}"));
    assert_eq!(streaming, saved, "streamed text");
    store.insert_layer(read.emitted.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let view = stage.view(&store);
    let targets = queries
        .iter()
        .map(|query| {
            let property = view
                .property(*query)
                .unwrap_or_else(|| panic!("no {query}"));
            let items = match property.as_relationship() {
                Some(relationship) => relationship.targets(),
                None => property.as_attribute().expect("attribute").connections(),
            };
            let items = items
                .iter()
                .map(|t| t.display(store.paths(), store.tokens()))
                .collect();
            (String::from(*query), items)
        })
        .collect();
    Outcome {
        diagnostics,
        saved,
        targets,
    }
}

/// Every case reads, writes and composes as OpenUSD's, or is refused as
/// OpenUSD refuses it. All cases run; the failure lists every one that
/// differs.
#[test]
fn cases_read_and_write_as_openusd() {
    let oracle = oracle();
    let mut failures = Vec::new();
    let cases = oracle["cases"].as_object().unwrap();
    for (name, case) in cases {
        let source = case["source"].as_str().unwrap();
        let expected_targets = case["targets"].as_object();
        let queries: Vec<&str> = expected_targets
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let got = outcome(source, &queries);
        if let Some(difference) = DIFFERENCES.iter().find(|d| d.case == name) {
            let handling = match (got.diagnostics.is_empty(), &got.saved) {
                (false, _) => Some(Handling::Refused),
                (true, Err(_)) => Some(Handling::SaveRefused),
                (true, Ok(_)) => None,
            };
            if !case["reads"].as_bool().unwrap() || handling != Some(difference.handling) {
                failures.push(format!(
                    "{name}: listed as {:?} ({}), now {handling:?}",
                    difference.handling, difference.reason
                ));
            }
            continue;
        }
        if !case["reads"].as_bool().unwrap() {
            if got.diagnostics.is_empty() {
                failures.push(format!(
                    "{name}: OpenUSD refuses it ({}), layerstack reads it",
                    case["error"]
                ));
            }
            continue;
        }
        if !got.diagnostics.is_empty() {
            failures.push(format!("{name}: diagnostics {:#?}", got.diagnostics));
        }
        let text = case["text"].as_str().unwrap();
        match &got.saved {
            Ok(saved) if saved == text => {}
            Ok(saved) => failures.push(format!(
                "{name}: saved text differs\n--- OpenUSD\n{text}--- layerstack\n{saved}"
            )),
            Err(error) => failures.push(format!("{name}: save refused: {error}")),
        }
        for (query, items) in &got.targets {
            let expected: Vec<&str> = expected_targets.unwrap()[query]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_str().unwrap())
                .collect();
            if *items != expected {
                failures.push(format!(
                    "{name}: {query} composes {items:?}, OpenUSD {expected:?}"
                ));
            }
        }
    }
    for difference in DIFFERENCES {
        if !cases.contains_key(difference.case) {
            failures.push(format!("{}: listed, but no such case", difference.case));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
