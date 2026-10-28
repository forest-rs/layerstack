// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Streaming a USDA save writes the text `save_usda` returns.
//!
//! Every USDA fixture in the workspace that reads without an error is
//! saved both ways: as one string ([`save_usda`]) and piece by piece into
//! a sink ([`save_usda_to`]). The pieces must join into the same bytes, and
//! a layer `save_usda` refuses must be refused before any piece is handed
//! out. OpenUSD's own parsing corpus is checked the same way by
//! `usda_corpus`, when it runs.
//!
//! Spec: AOUSD Core §16.2 (USDA).

use std::fmt;
use std::path::{Path, PathBuf};

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset,
    TokenInterner,
};
use layerstack_conformance::workspace_root;
use layerstack_usda::diagnostic::Severity;
use layerstack_usda::read_usda;
use layerstack_usda::save::{save_usda, save_usda_to};

/// Leaves every asset unresolved; arcs keep their authored paths.
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

/// A sink joining the pieces it is handed.
#[derive(Default)]
struct Pieces {
    text: String,
    pieces: usize,
}

impl fmt::Write for Pieces {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.text.push_str(s);
        self.pieces += 1;
        Ok(())
    }
}

fn usda_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            usda_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "usda") {
            files.push(path);
        }
    }
}

/// What saving one fixture both ways found.
enum Saved {
    /// Read with an error, or not UTF-8: not saved.
    Unread,
    /// Both ways refused it with the same error.
    Refused,
    /// Both ways wrote the same text.
    Same,
}

fn save_both_ways(file: &Path) -> Result<Saved, String> {
    let Ok(source) = std::fs::read_to_string(file) else {
        return Ok(Saved::Unread);
    };
    let store = InMemoryStore::default();
    let read = read_usda(
        &source,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    let errors = read
        .parse_diagnostics
        .iter()
        .chain(&read.lower_diagnostics)
        .chain(&read.emitted.diagnostics)
        .any(|d| d.severity == Severity::Error);
    if errors {
        return Ok(Saved::Unread);
    }
    let layer = &read.emitted.layer;
    let mut streamed = Pieces::default();
    let result = save_usda_to(layer, store.tokens(), store.paths(), &mut streamed);
    match (save_usda(layer, store.tokens(), store.paths()), result) {
        (Ok(text), Ok(())) if text == streamed.text => Ok(Saved::Same),
        (Ok(_), Ok(())) => Err("streamed text differs".into()),
        (Err(a), Err(b)) if a == b && streamed.pieces == 0 => Ok(Saved::Refused),
        (a, b) => Err(format!(
            "string {a:?}, streamed {b:?} after {} pieces",
            streamed.pieces
        )),
    }
}

/// Every fixture saves to the same text, or is refused alike, either way.
#[test]
fn every_fixture_streams_the_text_it_saves_to() {
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in [
        "layerstack_conformance/fixtures",
        "layerstack_conformance/tests/assets",
        "core-spec-supplemental-release_dec2025",
    ] {
        usda_files(&root.join(dir), &mut files);
    }
    files.sort();
    let (mut same, mut refused, mut failures) = (0, 0, Vec::new());
    for file in &files {
        match save_both_ways(file) {
            Ok(Saved::Same) => same += 1,
            Ok(Saved::Refused) => refused += 1,
            Ok(Saved::Unread) => {}
            Err(problem) => failures.push(format!("{}: {problem}", file.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(same > 300, "{same} fixtures saved, {refused} refused");
}
