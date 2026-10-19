// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opening a stage on several threads reads the same layers, under the same
//! ids, with the same failures and diagnostics, as opening it on one.

use super::*;
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
    let doc = StageDocument::open_with(
        scene(),
        root,
        StageOptions {
            threads,
            ..StageOptions::default()
        },
        LoadOptions {
            import_policy,
            variant_arcs,
            ..LoadOptions::default()
        },
    );
    let doc = match doc {
        Ok(doc) => doc,
        Err(error) => return format!("{error:?}"),
    };
    let (tokens, paths) = (&doc.store.tokens, &doc.store.paths);
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
