// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opening a stage on several threads reads the same layers, under the same
//! ids, with the same failures and diagnostics, as opening it on one.

use super::*;
use crate::layer_ids;
use core::fmt::Write as _;

/// A root reaching a layer twice, layers through layers, missing layers,
/// layers the parser recovers, rejects (twice) and cannot decode, a cycle
/// back to the root and arcs in an unselected variant branch.
fn scene() -> Memory {
    let mut memory = Memory::with(&[
        (
            "root.usda",
            r#"#usda 1.0
(
    subLayers = [@sub.usda@, @missing_sub.usda@]
)
def "A" (prepend references = @lib.usda@</Lib>) {}
def "B" (prepend references = [@lib.usda@</Lib>, @bad.usda@</X>]) {}
def "C" (prepend payload = @cycle.usda@</Back>) {}
def "D" (prepend references = @missing.usda@</M>) {}
def "F" (prepend references = [@rejected.usda@</R>, @binary.usda@</Z>]) {}
def "G" (prepend references = @rejected.usda@</R>) {}
def "E" (
    variants = { string look = "a" }
    prepend variantSets = "look"
) {
    variantSet "look" = {
        "a" (prepend references = @look_a.usda@</Look>) {}
        "b" (prepend references = @look_b.usda@</Look>) {}
    }
}
"#,
        ),
        (
            "sub.usda",
            "#usda 1.0\ndef \"S\" (prepend references = @nested.usda@</N>) {}\n",
        ),
        (
            "lib.usda",
            "#usda 1.0\ndef \"Lib\" (prepend references = @nested.usda@</N>) {\n int x = 1\n}\n",
        ),
        ("nested.usda", "#usda 1.0\ndef \"N\" {\n int y = 2\n}\n"),
        (
            "cycle.usda",
            "#usda 1.0\ndef \"Back\" (prepend references = @root.usda@</A>) {}\n",
        ),
        (
            "bad.usda",
            "#usda 1.0\ndef \"X\" (prepend references = @nested.usda@</N> bogus\n",
        ),
        ("look_a.usda", "#usda 1.0\ndef \"Look\" {\n int a = 1\n}\n"),
        ("look_b.usda", "#usda 1.0\ndef \"Look\" {\n int b = 1\n}\n"),
        (
            "rejected.usda",
            "#usda 1.0\ndef \"R\" (prepend references = @lib.usda@</Lib>) {}\ndef \"Q\" (prepend references = </A{v=x}B>) {}\n",
        ),
    ]);
    memory
        .files
        .insert("binary.usda".into(), vec![b'#', 0xff, 0xfe, b'\n']);
    let package = layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
        "member.usda",
        b"#usda 1.0\ndef \"M\" (prepend references = @nested.usda@</N>) {}\n",
    )])
    .unwrap();
    memory.files.insert("package.usdz".into(), package);
    // Layers whose ids all collide ([`crate::layer_ids::WEAK_HASH`]): the
    // threads find `b` before `c`, one thread `c` before `b`.
    for (name, text) in [
        (
            "weak-hash/root.usda",
            "#usda 1.0\ndef \"A\" (prepend references = @weak-hash/a.usda@</A>) {}\ndef \"B\" (prepend references = [@weak-hash/b.usda@</B>, @weak-hash/missing.usda@</M>]) {}\n",
        ),
        (
            "weak-hash/a.usda",
            "#usda 1.0\ndef \"A\" (prepend references = @weak-hash/c.usda@</C>) {}\n",
        ),
        (
            "weak-hash/b.usda",
            "#usda 1.0\ndef \"B\" (prepend references = @weak-hash/c.usda@</C>) {\n int b = 1\n}\n",
        ),
        (
            "weak-hash/c.usda",
            "#usda 1.0\ndef \"C\" {\n def \"Leaf\" {}\n}\n",
        ),
    ] {
        memory.files.insert(name.into(), text.as_bytes().to_vec());
    }
    memory.files.insert(
        "packaged_root.usda".into(),
        b"#usda 1.0\ndef \"P\" (prepend references = @package.usdz@</M>) {}\ndef \"Q\" (prepend references = @lib.usda@</Lib>) {}\n"
            .to_vec(),
    );
    memory
}

/// What opening `root` of `scene` on `threads` threads read, without path
/// ids, or the error opening it.
fn render(
    root: &str,
    threads: usize,
    import_policy: ImportPolicy,
    variant_arcs: VariantArcLoading,
) -> String {
    let options = StageOptions {
        threads,
        ..StageOptions::default()
    };
    render_with(root, options, import_policy, variant_arcs)
}

/// What opening `root` of `scene` with `options` read, as [`render`]
/// renders it.
fn render_with(
    root: &str,
    options: StageOptions,
    import_policy: ImportPolicy,
    variant_arcs: VariantArcLoading,
) -> String {
    let doc = StageDocument::open(
        scene(),
        root,
        OpenOptions {
            stage: options,
            loading: LoadOptions {
                import_policy,
                variant_arcs,
                ..LoadOptions::default()
            },
            ..OpenOptions::default()
        },
    );
    let doc = match doc {
        Ok(doc) => doc,
        Err(error) => return format!("{error:?}"),
    };
    let (tokens, paths) = (&doc.store.tokens(), &doc.store.paths());
    let mut out = String::new();
    let report = doc.load_report();
    writeln!(out, "layers {:?}", report.layers).unwrap();
    for failure in &report.asset_failures {
        writeln!(out, "failure {failure:?}").unwrap();
    }
    for diagnostic in &report.diagnostics {
        writeln!(
            out,
            "diagnostic {} {:?}",
            diagnostic.member, diagnostic.layer_id
        )
        .unwrap();
    }
    let mut ids: Vec<LayerId> = doc.store.layer_ids().into_iter().collect();
    ids.sort();
    for id in ids {
        let layer = &doc.store.layer(id).unwrap();
        writeln!(
            out,
            "layer {id:?} {:?} {:?}",
            layer.id(),
            doc.identifier(id)
        )
        .unwrap();
        for sublayer in layer.sublayers() {
            writeln!(out, "  sublayer {:?} {:?}", sublayer.layer, sublayer.asset).unwrap();
        }
        let mut prims: Vec<_> = layer.prims().iter().collect();
        prims.sort_by_key(|(path, _)| paths.display(**path, tokens));
        for (path, spec) in prims {
            let arcs = spec.references.items().chain(spec.payloads.items());
            for arc in arcs {
                let path = paths.display(*path, tokens);
                writeln!(out, "  {path} {:?}", arc.source()).unwrap();
            }
        }
    }
    let root = paths.lookup(&layerstack::Path::root()).unwrap();
    for prim in doc.stage.stage().traverse_all(root) {
        writeln!(out, "prim {}", paths.display(prim, tokens)).unwrap();
    }
    out
}

#[test]
fn four_threads_read_the_layers_one_does() {
    for variant_arcs in [VariantArcLoading::All, VariantArcLoading::Selected] {
        let single = render("root.usda", 1, ImportPolicy::AllowRecovery, variant_arcs);
        assert!(single.contains("failure"), "{single}");
        assert!(single.contains("missing.usda"), "{single}");
        let strict = render("root.usda", 1, ImportPolicy::Strict, variant_arcs);
        assert!(strict.starts_with("IoError"), "{strict}");
        for _ in 0..8 {
            let threaded = render("root.usda", 4, ImportPolicy::AllowRecovery, variant_arcs);
            assert_eq!(threaded, single);
            let strict_threaded = render("root.usda", 4, ImportPolicy::Strict, variant_arcs);
            assert_eq!(strict_threaded, strict);
        }
    }
}

/// A stage opened on threads watches every layer it reads, though the
/// open commits each new layer with its bindings and no recorded change:
/// an edit of a layer two arcs deep, made on the store without telling the
/// stage, is found and composed as a stage opened on one thread finds it.
#[test]
fn a_stage_opened_on_threads_finds_an_edit_of_a_deep_layer_as_one_thread_does() {
    let edited = |threads: usize, variant_arcs: VariantArcLoading| {
        let options = StageOptions {
            threads,
            ..StageOptions::default()
        };
        let loading = LoadOptions {
            import_policy: ImportPolicy::AllowRecovery,
            variant_arcs,
            ..LoadOptions::default()
        };
        let mut doc = StageDocument::open(
            scene(),
            "root.usda",
            OpenOptions {
                stage: options,
                loading,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        let nested = doc.catalog.ids["nested.usda"];
        let added = doc.store.path("/N/Added");
        let layer = doc.store_mut().layer_mut(nested).unwrap();
        layer.insert_prim(added, PrimSpec::def());
        let (store, stage) = doc.parts_mut();
        let changed = stage.notify_changed_layers(store);
        stage.synchronize(store);
        let (tokens, paths) = (&doc.store.tokens(), &doc.store.paths());
        let prims: Vec<String> = doc
            .view()
            .traverse()
            .map(|prim| paths.display(prim.path(), tokens))
            .collect();
        (nested, changed, prims)
    };
    for variant_arcs in [VariantArcLoading::All, VariantArcLoading::Selected] {
        let single = edited(1, variant_arcs);
        assert_eq!(single.1, [single.0], "{single:?}");
        assert!(
            single.2.iter().any(|path| path.ends_with("/Added")),
            "{single:?}"
        );
        assert_eq!(edited(4, variant_arcs), single);
    }
}

#[test]
fn a_stage_reaching_a_package_reads_it_on_one_thread() {
    let single = render(
        "packaged_root.usda",
        1,
        ImportPolicy::Strict,
        VariantArcLoading::All,
    );
    assert!(single.contains("package.usdz"), "{single}");
    assert!(single.contains("prim /P"), "{single}");
    let threaded = render(
        "packaged_root.usda",
        4,
        ImportPolicy::Strict,
        VariantArcLoading::All,
    );
    assert_eq!(threaded, single);
}

/// A host executor that counts the jobs it starts and runs each on the
/// thread that waits for it, once the body returns: it starts no thread.
#[cfg(feature = "std")]
#[derive(Default)]
struct Counting {
    jobs: core::sync::atomic::AtomicUsize,
}

#[cfg(feature = "std")]
struct CountingScope<'a, 'env> {
    queue: std::sync::Mutex<Vec<layerstack::parallel::Job<'env>>>,
    jobs: &'a core::sync::atomic::AtomicUsize,
}

#[cfg(feature = "std")]
impl<'env> layerstack::parallel::Scope<'env> for CountingScope<'_, 'env> {
    fn spawn(&self, job: layerstack::parallel::Job<'env>) -> bool {
        self.jobs.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
        self.queue.lock().unwrap().push(job);
        true
    }
}

#[cfg(feature = "std")]
impl layerstack::parallel::Executor for Counting {
    fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn layerstack::parallel::Scope<'env>) + 'env)) {
        let scope = CountingScope {
            queue: std::sync::Mutex::new(Vec::new()),
            jobs: &self.jobs,
        };
        body(&scope);
        loop {
            let job = scope.queue.lock().unwrap().pop();
            match job {
                Some(job) => job(&scope),
                None => break,
            }
        }
    }
}

/// Opening a stage on a host's executor reads what opening it on one
/// thread does, and both the loader and composition start their helpers
/// as the executor's jobs: composing the opened stage again starts fewer
/// than opening it did.
#[cfg(feature = "std")]
#[test]
fn opening_on_a_host_executor_reads_the_layers_one_thread_does() {
    let single = render(
        "root.usda",
        1,
        ImportPolicy::AllowRecovery,
        VariantArcLoading::All,
    );
    let counting = Arc::new(Counting::default());
    let options = StageOptions {
        threads: 4,
        executor: Some(Arc::clone(&counting) as Arc<dyn layerstack::parallel::Executor>),
        ..StageOptions::default()
    };
    let opened = render_with(
        "root.usda",
        options.clone(),
        ImportPolicy::AllowRecovery,
        VariantArcLoading::All,
    );
    assert_eq!(opened, single);
    let jobs = || counting.jobs.load(core::sync::atomic::Ordering::SeqCst);
    let opening = jobs();
    assert!(opening > 0);
    let loading = LoadOptions {
        import_policy: ImportPolicy::AllowRecovery,
        ..LoadOptions::default()
    };
    let mut doc = StageDocument::open(
        scene(),
        "root.usda",
        OpenOptions {
            loading,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let root = doc.root_layer();
    let before = jobs();
    drop(LiveStage::compose(&mut doc.store, root, options));
    let composing = jobs() - before;
    assert!(composing < opening, "{composing} of {opening} jobs compose");
}

/// A layer named by identifier gets the id derived from it, on one thread
/// as on several, and an anonymous layer one below the derived range.
#[test]
fn layers_read_get_the_ids_their_identifiers_derive() {
    for threads in [1, 4] {
        let options = StageOptions {
            threads,
            ..StageOptions::default()
        };
        let loading = LoadOptions {
            import_policy: ImportPolicy::AllowRecovery,
            ..LoadOptions::default()
        };
        let mut doc = StageDocument::open(
            scene(),
            "root.usda",
            OpenOptions {
                stage: options,
                loading,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        for (identifier, &id) in &doc.catalog.ids {
            assert_eq!(id, layer_ids::candidate(identifier, 0), "{identifier}");
        }
        assert!(doc.catalog.ids.contains_key("nested.usda"));
        let anonymous = doc.create_anonymous_layer().unwrap();
        assert!(anonymous < LayerId::FIRST_DERIVED, "{anonymous:?}");
    }
}

/// Identifiers whose ids collide get distinct ones, numbered in the order
/// one thread finds them, though the threads find them in another: the
/// same ids, arcs, failures and prims on any number of threads.
#[test]
fn colliding_identifiers_get_the_ids_one_thread_gives_them() {
    let ids = |threads: usize| {
        let options = StageOptions {
            threads,
            ..StageOptions::default()
        };
        let loading = LoadOptions {
            import_policy: ImportPolicy::AllowRecovery,
            ..LoadOptions::default()
        };
        let doc = StageDocument::open(
            scene(),
            "weak-hash/root.usda",
            OpenOptions {
                stage: options,
                loading,
                ..OpenOptions::default()
            },
        )
        .unwrap();
        let mut ids: Vec<(String, LayerId)> = doc
            .catalog
            .ids
            .iter()
            .map(|(identifier, &id)| (String::from(&**identifier), id))
            .collect();
        ids.sort();
        ids
    };
    let attempt = |attempt: u64| LayerId(LayerId::FIRST_DERIVED.0 | attempt);
    let single = ids(1);
    assert_eq!(
        single,
        [
            ("weak-hash/a.usda".into(), attempt(1)),
            ("weak-hash/b.usda".into(), attempt(3)),
            ("weak-hash/c.usda".into(), attempt(2)),
            ("weak-hash/missing.usda".into(), attempt(4)),
            ("weak-hash/root.usda".into(), attempt(0)),
        ]
    );
    let rendered = render(
        "weak-hash/root.usda",
        1,
        ImportPolicy::AllowRecovery,
        VariantArcLoading::All,
    );
    for prim in ["prim /A/Leaf", "prim /B/Leaf", "failure"] {
        assert!(rendered.contains(prim), "{rendered}");
    }
    for _ in 0..8 {
        assert_eq!(ids(4), single);
        let threaded = render(
            "weak-hash/root.usda",
            4,
            ImportPolicy::AllowRecovery,
            VariantArcLoading::All,
        );
        assert_eq!(threaded, rendered);
    }
}

/// A host's layer holding an identifier's first candidate keeps it: the
/// layer read steps past it, on one thread as on several.
#[test]
fn a_derived_id_steps_past_a_host_layer_holding_it() {
    for threads in [1, 4] {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId::FIRST_DERIVED));
        let options = StageOptions {
            threads,
            ..StageOptions::default()
        };
        let loading = LoadOptions {
            import_policy: ImportPolicy::AllowRecovery,
            ..LoadOptions::default()
        };
        let doc = StageDocument::open(
            scene(),
            "weak-hash/root.usda",
            OpenOptions {
                stage: options,
                loading,
                store: Some(store),
                ..OpenOptions::default()
            },
        )
        .unwrap();
        let root = doc.catalog.ids["weak-hash/root.usda"];
        assert_eq!(root, LayerId(LayerId::FIRST_DERIVED.0 | 1));
        assert_eq!(doc.stage().stage().root_layer(), Some(root));
        let host = doc.store.layer(LayerId::FIRST_DERIVED).unwrap();
        assert!(host.prims().is_empty(), "the host's layer is untouched");
        let mut ids: Vec<LayerId> = doc.catalog.ids.values().copied().collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), doc.catalog.ids.len());
        assert!(!ids.contains(&LayerId::FIRST_DERIVED));
    }
}
