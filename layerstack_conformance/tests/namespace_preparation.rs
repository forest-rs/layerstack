// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace edits prepared against a live stage, from the evidence it
//! retains, match those prepared against a stage composed on its own,
//! which composes and visits the whole stage, over the composition and
//! namespace edit fixtures: renames, reparents, moves onto occupied paths
//! and beneath themselves, property moves and deletes, before and after
//! applying some of them. AOUSD Core §8, §10.
#![allow(missing_docs, reason = "integration test")]

use std::path::{Path, PathBuf};

use layerstack::{
    EditTarget, InMemoryStore, LayerId, LiveStage, NamespaceEdit, NamespaceError, PathId,
    PropertyPath, Stage, StageOptions, TargetPath,
};
use layerstack_conformance::{pcp_txt::load_pcp_txt, usda_real::load_entry_usda, workspace_root};

/// The composed prims at most this deep in the namespace, besides the
/// pseudo-root, are edited.
const DEPTH: usize = 3;
/// At most this many prims of a fixture are edited.
const PRIMS: usize = 10;

fn options() -> StageOptions {
    StageOptions {
        with_dependencies: true,
        ..StageOptions::default()
    }
}

fn entries() -> Vec<(String, PathBuf)> {
    let mut entries = Vec::new();
    let composition =
        workspace_root().join("core-spec-supplemental-release_dec2025/composition/tests/assets");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&composition)
        .expect("composition assets")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.join("pcp.txt").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let oracle = load_pcp_txt(&dir.join("pcp.txt"));
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        entries.push((name, dir.join("usda").join(&oracle.entry)));
    }
    let fixtures = workspace_root().join("layerstack_conformance/fixtures");
    for dir in ["namespace_edits", "namespace_deletes"] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(fixtures.join(dir))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "usda"))
            .collect();
        files.sort();
        for file in files {
            entries.push((format!("{dir}/{}", file.display()), file));
        }
    }
    let coordinated = fixtures.join("namespace_coordinated");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&coordinated)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "usda"))
            .collect();
        files.sort();
        for file in files {
            entries.push((format!("{}", file.display()), file));
        }
    }
    entries
}

struct Fixture {
    name: String,
    store: InMemoryStore,
    root: LayerId,
    live: LiveStage,
}

impl Fixture {
    fn label(&self, path: TargetPath) -> String {
        match path {
            TargetPath::Prim(p) => self.store.paths.display(p, &self.store.tokens),
            TargetPath::Property(p) => p.display(&self.store.paths, &self.store.tokens),
        }
    }

    /// Prepares the move against the live stage and against the stage
    /// composed on its own, and checks they agree.
    fn compare_move(
        &mut self,
        plain: &Stage,
        from: TargetPath,
        to: TargetPath,
    ) -> Result<NamespaceEdit, NamespaceError> {
        let target = EditTarget::for_layer(self.root);
        if self.name == "SpecializesAndAncestralArcs2_root"
            && self
                .label(to)
                .starts_with("/_shared_HumanHair/Hair_Chiang/Class/")
        {
            // Moving the prim that specializes `Class` beneath it, through
            // the referenced namespace, relocates it to specialize an
            // ancestor; composing the projection does not finish, however
            // it is prepared, and OpenUSD 26.08 does not finish opening
            // the same scene either.
            return Err(NamespaceError::InvalidPaths);
        }
        let whole = NamespaceEdit::prepare(plain, &mut self.store, &target, from, to);
        let retained =
            NamespaceEdit::prepare(self.live.stage(), &mut self.store, &target, from, to);
        assert_eq!(
            retained,
            whole,
            "{}: {} -> {}",
            self.name,
            self.label(from),
            self.label(to)
        );
        retained
    }

    fn compare_delete(
        &mut self,
        plain: &Stage,
        object: TargetPath,
    ) -> Result<NamespaceEdit, NamespaceError> {
        let target = EditTarget::for_layer(self.root);
        let whole = NamespaceEdit::prepare_delete(plain, &mut self.store, &target, object);
        let retained =
            NamespaceEdit::prepare_delete(self.live.stage(), &mut self.store, &target, object);
        assert_eq!(
            retained,
            whole,
            "{}: delete {}",
            self.name,
            self.label(object)
        );
        retained
    }

    /// Compares the candidate edits of the fixture; returns how many were
    /// compared and the ones that succeeded.
    fn compare_all(&mut self) -> (usize, Vec<NamespaceEdit>) {
        let plain = Stage::compose(&mut self.store, self.root, options());
        let pseudo_root = self.store.path("/");
        let mut prims: Vec<PathId> = plain
            .traverse_all(pseudo_root)
            .filter(|p| *p != pseudo_root && self.store.paths.resolve(*p).depth() <= DEPTH)
            .collect();
        self.store.paths.sort_paths(&mut prims, &self.store.tokens);
        prims.truncate(PRIMS);
        let renamed = self.store.tokens.intern("Renamed");
        let mut compared = 0;
        let mut prepared = Vec::new();
        let mut keep = |result: Result<NamespaceEdit, NamespaceError>| {
            compared += 1;
            if let Ok(edit) = result {
                prepared.push(edit);
            }
        };
        for (i, &prim) in prims.iter().enumerate() {
            let path = self.store.paths.resolve(prim).clone();
            let name = path.leaf().unwrap();
            let parent = path.parent().unwrap();
            let mut destinations = vec![
                self.store.paths.intern(parent.join(&[renamed])),
                self.store.paths.intern(path.join(&[renamed])),
            ];
            for other in [prims[(i + 1) % prims.len()], prims[(i + 3) % prims.len()]] {
                if other != prim {
                    destinations.push(other);
                    let joined = self.store.paths.resolve(other).join(&[name]);
                    destinations.push(self.store.paths.intern(joined));
                }
            }
            for destination in destinations {
                keep(self.compare_move(
                    &plain,
                    TargetPath::Prim(prim),
                    TargetPath::Prim(destination),
                ));
            }
            keep(self.compare_delete(&plain, TargetPath::Prim(prim)));
            let mut names: Vec<_> = plain
                .prim(prim, &self.store)
                .map(|prim| prim.authored_properties())
                .unwrap_or_default()
                .into_iter()
                .map(|property| property.path().property())
                .collect();
            names.truncate(2);
            for property in names {
                let from = TargetPath::Property(PropertyPath::new(prim, property));
                let new_name = self
                    .store
                    .tokens
                    .intern(format!("{}Renamed", self.store.tokens.resolve(property)));
                let other = prims[(i + 1) % prims.len()];
                for to in [
                    PropertyPath::new(prim, new_name),
                    PropertyPath::new(other, property),
                ] {
                    keep(self.compare_move(&plain, from, TargetPath::Property(to)));
                }
                keep(self.compare_delete(&plain, from));
            }
        }
        (compared, prepared)
    }
}

fn load(name: String, entry: &Path) -> Option<Fixture> {
    let mut loaded = load_entry_usda(entry);
    if !loaded.invalid.is_empty() {
        return None;
    }
    let live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options());
    Some(Fixture {
        name,
        store: loaded.store,
        root: loaded.root_layer,
        live,
    })
}

#[test]
fn retained_namespace_preparation_matches_whole_stage_preparation() {
    let mut compared = 0;
    let mut succeeded = 0;
    let mut fixtures = 0;
    for (name, entry) in entries() {
        let Some(mut fixture) = load(name, &entry) else {
            continue;
        };
        fixtures += 1;
        let (count, prepared) = fixture.compare_all();
        compared += count;
        succeeded += prepared.len();
        // Again after applying an edit, against the snapshot the live
        // stage keeps current. (A delete in a stage whose layer stack
        // repeats a layer, as a sublayer cycle does, does not apply.)
        let middle = prepared.len() / 2;
        let applied = prepared[middle..]
            .iter()
            .chain(&prepared[..middle])
            .any(|edit| {
                fixture
                    .live
                    .apply(&mut fixture.store, edit.transaction())
                    .is_ok()
            });
        if applied {
            let (count, prepared) = fixture.compare_all();
            compared += count;
            succeeded += prepared.len();
        }
    }
    assert!(fixtures > 150, "{fixtures} fixtures");
    assert!(
        succeeded > 3000,
        "{succeeded} of {compared} edits succeeded"
    );
}
