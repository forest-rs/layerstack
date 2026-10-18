// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A generated parts library (`layerstack_conformance::library_scene`,
//! full shape: about 10,000 prims, 4,000 leaves, 15,000 slot
//! relationships, 40 collections list-edited across three sublayers, and
//! 800,000 `token[]` cells), with 150 slot relationships and two paths of
//! each rule-map collection that name nothing, from USDA and from USDC:
//!
//! - `library_open`: open and compose through `StageDocument`, USDC with
//!   retained arrays; the schema registry is built outside the measurement;
//! - `library_reverse_build`: `MembershipIndex::build`;
//! - `library_reverse_query`: `collections_including` for every prim, and
//!   for every leaf, reported per prim;
//! - `library_forward_query`: the same question without the index, asking
//!   each of the 40 compiled `MembershipQuery`s, per prim;
//! - `library_reverse_update`: `apply_changes` for one appended include;
//! - `library_dangling`: `DanglingReport::compute`, every relationship
//!   target, connection, include and exclude checked, with eight examples
//!   of each name and collection, and `Stage::dangling_targets` alone.
//!
//! The scene is written once into Cargo's benchmark temporary directory
//! and removed at the end.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use layerstack::edit::{EditTarget, Transaction};
use layerstack::{InMemoryStore, ListOp, PathId, PropertyPath, StageOptions, TargetPath};
use layerstack_conformance::library_scene::{
    LibraryFormat, LibraryScene, LibraryShape, WrittenLibrary,
};
use layerstack_io::{Filesystem, LoadOptions, StageDocument, UsdcArrayLoading, UsdcReadOptions};
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::{DanglingReport, MembershipIndex, MembershipQuery, Scene};

/// Dangling slot relationships: one in a hundred.
const DANGLING: usize = 150;

/// Examples kept of each relationship name and collection.
const EXAMPLES: usize = 8;

fn open(root: &Path) -> StageDocument<Filesystem> {
    let mut store = InMemoryStore::default();
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    open_with(root, store, options)
}

fn open_with(
    root: &Path,
    store: InMemoryStore,
    options: StageOptions,
) -> StageDocument<Filesystem> {
    let dir = root.parent().expect("a directory");
    let storage = Filesystem::new(dir, []).expect("the directory");
    let name = root.file_name().and_then(|n| n.to_str()).expect("a name");
    StageDocument::open_in_with(
        storage,
        store,
        name,
        options,
        LoadOptions {
            usdc: UsdcReadOptions {
                arrays: UsdcArrayLoading::Retained,
                ..UsdcReadOptions::default()
            },
            ..LoadOptions::default()
        },
    )
    .expect("the library opens")
}

fn prims(scene: &Scene<'_>) -> Vec<PathId> {
    let root = scene
        .store()
        .paths()
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    scene.stage().traverse_all(root).skip(1).collect()
}

fn written() -> (LibraryScene, Vec<(LibraryFormat, WrittenLibrary)>, PathBuf) {
    let scene = LibraryScene::generate(&LibraryShape {
        dangling: DANGLING,
        ..LibraryShape::FULL
    });
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library_membership");
    let files = [LibraryFormat::Usda, LibraryFormat::Usdc]
        .into_iter()
        .map(|format| {
            let written = scene
                .write(&dir.join(format.extension()), format)
                .expect("the library writes");
            (format, written)
        })
        .collect();
    (scene, files, dir)
}

fn bench_library(c: &mut Criterion) {
    let (library, files, dir) = written();
    eprintln!("library: {}", library.summary());
    for (format, written) in &files {
        eprintln!(
            "library {}: {} bytes in {} files",
            format.extension(),
            written.bytes(),
            written.files.len()
        );
    }

    let mut group = c.benchmark_group("library_open");
    group.sample_size(10);
    for (format, written) in &files {
        group.bench_function(format.extension(), |b| {
            b.iter_batched(
                || {
                    let mut store = InMemoryStore::default();
                    let options = StageOptions {
                        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
                        ..StageOptions::default()
                    };
                    (store, options)
                },
                |(store, options)| black_box(open_with(&written.root, store, options)),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();

    let documents: Vec<(LibraryFormat, StageDocument<Filesystem>)> = files
        .iter()
        .map(|(format, written)| (*format, open(&written.root)))
        .collect();
    for (format, document) in &documents {
        let layers = &document.load_report().layers;
        let live: usize = layers
            .iter()
            .filter_map(|layer| document.retained_values(*layer))
            .map(|values| values.stats().live_arrays)
            .sum();
        eprintln!(
            "library {}: {} layers, {live} retained arrays",
            format.extension(),
            layers.len()
        );
    }

    let mut group = c.benchmark_group("library_dangling");
    group.sample_size(20);
    for (format, document) in &documents {
        let scene = Scene::new(document.stage().stage(), document.store());
        let report = DanglingReport::compute(&scene, EXAMPLES);
        let targets = report.targets.total();
        let collections: usize = report.collections.iter().map(|c| c.total()).sum();
        let counts = &library.counts;
        assert_eq!(
            (targets.no_prim, targets.no_property),
            (
                counts.dangling_prims + counts.dangling_collection_paths,
                counts.dangling_properties
            ),
            "every injected target, and nothing else, dangles"
        );
        assert_eq!(
            collections, counts.dangling_collection_paths,
            "every injected collection path dangles"
        );
        eprintln!(
            "library {}: {} prims, {} properties, {} targets checked; dangling {targets:?}, \
             {collections} collection paths",
            format.extension(),
            report.targets.prims,
            report.targets.properties,
            report.targets.targets,
        );
        group.bench_function(format!("{}/report", format.extension()), |b| {
            b.iter(|| black_box(DanglingReport::compute(&scene, EXAMPLES)));
        });
        let stage = document.stage().stage();
        group.bench_function(format!("{}/targets", format.extension()), |b| {
            b.iter(|| black_box(stage.dangling_targets(document.store(), EXAMPLES)));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("library_reverse_build");
    group.sample_size(20);
    for (format, document) in &documents {
        let scene = Scene::new(document.stage().stage(), document.store());
        let stats = MembershipIndex::build(&scene).stats();
        eprintln!("library {}: {stats:?}", format.extension());
        group.bench_function(format.extension(), |b| {
            b.iter(|| black_box(MembershipIndex::build(&scene)));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("library_reverse_query");
    for (format, document) in &documents {
        let scene = Scene::new(document.stage().stage(), document.store());
        let index = MembershipIndex::build(&scene);
        let all = prims(&scene);
        let leaves: Vec<PathId> = all
            .iter()
            .copied()
            .filter(|prim| {
                let store = scene.store();
                let text = store.paths().display(*prim, store.tokens());
                text.matches('/').count() == 4 && text.starts_with("/Library/")
            })
            .collect();
        assert_eq!(leaves.len(), library.counts.leaves, "every leaf is found");
        for (name, paths) in [("all_prims", &all), ("leaves", &leaves)] {
            group.throughput(Throughput::Elements(paths.len() as u64));
            group.bench_function(format!("{}/{name}", format.extension()), |b| {
                b.iter(|| {
                    let mut found = 0;
                    for prim in paths {
                        found += index
                            .collections_including(&scene, TargetPath::Prim(*prim))
                            .len();
                    }
                    black_box(found)
                });
            });
        }
    }
    group.finish();

    let mut group = c.benchmark_group("library_forward_query");
    for (format, document) in &documents {
        let scene = Scene::new(document.stage().stage(), document.store());
        let all = prims(&scene);
        let queries: Vec<MembershipQuery> = all
            .iter()
            .flat_map(|prim| CollectionApi::instances(&scene, *prim))
            .map(|collection| collection.membership_query())
            .collect();
        group.throughput(Throughput::Elements(all.len() as u64));
        group.bench_function(format!("{}/all_prims", format.extension()), |b| {
            b.iter(|| {
                let mut found = 0;
                for prim in &all {
                    found += queries
                        .iter()
                        .filter(|query| {
                            query
                                .is_included(&scene, TargetPath::Prim(*prim))
                                .is_included()
                        })
                        .count();
                }
                black_box(found)
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("library_reverse_update");
    for (format, mut document) in documents {
        // The sets layer authors `set03`'s second includes: replacing them
        // edits an existing spec, which the change report inventories.
        let sets = *document
            .load_report()
            .layers
            .iter()
            .find(|layer| {
                document
                    .identifier(**layer)
                    .is_some_and(|name| name.contains("library_sets"))
            })
            .expect("the sets layer");
        let (store, live) = document.parts_mut();
        let library_prim = store.path("/Library");
        let leaf = store.path("/Library/Stones/Granite/Granite_000");
        let includes = PropertyPath::new(
            library_prim,
            store.tokens.intern("collection:set03:includes"),
        );
        let scene = Scene::new(live.stage(), store);
        let mut index = MembershipIndex::build(&scene);
        let mut tx = Transaction::new();
        tx.set_targets(
            EditTarget::for_layer(sets).property(includes),
            ListOp::appended(vec![TargetPath::Prim(leaf)]),
        );
        let applied = live.apply(store, &tx).expect("the edit applies");
        let scene = Scene::new(live.stage(), store);
        let update = index.apply_changes(&scene, &applied.changes);
        eprintln!(
            "library {}: one appended include: {} resynced, recompiled {:?}",
            format.extension(),
            applied.changes.resynced.len(),
            update
                .recompiled
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
        );
        group.bench_function(format.extension(), |b| {
            b.iter(|| black_box(index.apply_changes(&scene, &applied.changes)));
        });
    }
    group.finish();

    for (_, written) in &files {
        written.remove();
    }
    let _ = std::fs::remove_dir_all(dir);
}

criterion_group!(benches, bench_library);
criterion_main!(benches);
