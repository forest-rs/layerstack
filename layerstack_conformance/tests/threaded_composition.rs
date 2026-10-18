// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composing on several threads composes the same stage as on one.
//!
//! Every fixture of the supplemental composition suite is loaded twice and
//! composed once with [`StageOptions::threads`] at 8 and once at 1. Each
//! load interns its own paths, in an order the threads decide for the
//! first, so the stages are compared by what they say, not by id: every
//! prim in traversal order with its children, prim stack and property
//! names, and the composition errors in order, each by kind and the prim
//! it names.
//!
//! The fixtures are small; a generated scene wide and deep enough for every
//! thread to take work checks the same of 2, 8 and 16 threads.

use std::fmt::Write as _;
use std::path::PathBuf;

use std::collections::BTreeMap;

use layerstack::{InMemoryStore, LayerId, LayerStore, Stage, StageOptions};
use layerstack_conformance::{
    pcp_txt::load_pcp_txt,
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};

fn assets_dir() -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("composition")
        .join("tests")
        .join("assets")
}

/// `loaded`'s stage composed on `threads` threads, rendered without ids.
fn render(mut loaded: LoadedStage, threads: usize) -> String {
    render_store(
        &mut loaded.store,
        loaded.root_layer,
        &loaded.layer_names,
        threads,
    )
}

/// The stage of `root` in `store` composed on `threads` threads, rendered
/// without ids, each layer by its name in `layer_names`.
fn render_store(
    store: &mut InMemoryStore,
    root: LayerId,
    layer_names: &BTreeMap<LayerId, String>,
    threads: usize,
) -> String {
    let standin = store.tokens.intern("standin");
    let render = store.tokens.intern("render");
    let stage = Stage::compose(
        store,
        root,
        StageOptions {
            with_provenance: true,
            variant_fallbacks: [(standin, vec![render])].into_iter().collect(),
            threads,
            ..StageOptions::default()
        },
    );
    let store = &*store;
    let (paths, tokens) = (store.paths(), store.tokens());
    let path = |id| paths.display(id, tokens);
    let mut out = String::new();
    let root = paths.lookup(&layerstack::Path::root()).expect("root");
    for prim in stage.traverse_all(root) {
        writeln!(out, "{}", path(prim)).unwrap();
        for child in stage.children_of(prim).unwrap_or(&[]) {
            writeln!(out, "  child {}", path(*child)).unwrap();
        }
        for key in stage.explain_prim(prim).unwrap_or(&[]) {
            let layer = layer_names.get(&key.layer_id).cloned();
            let site = key.spec_path.display(paths, tokens);
            writeln!(out, "  site {} {site}", layer.unwrap_or_default()).unwrap();
        }
        for name in stage.property_names(prim, store) {
            writeln!(out, "  property {}", tokens.resolve(name)).unwrap();
        }
    }
    for error in stage.composition_errors() {
        let prim = error.prim().map(path).unwrap_or_default();
        writeln!(out, "error {:?} {prim}", core::mem::discriminant(error)).unwrap();
    }
    out
}

#[test]
fn eight_threads_compose_the_stage_one_does() {
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(assets_dir())
        .expect("composition assets")
        .map(|entry| entry.expect("fixture").path())
        .filter(|dir| dir.join("pcp.txt").is_file())
        .collect();
    fixtures.sort();
    assert!(fixtures.len() > 100, "found {} fixtures", fixtures.len());
    for dir in fixtures {
        let entry = dir
            .join("usda")
            .join(load_pcp_txt(&dir.join("pcp.txt")).entry);
        let threaded = render(load_entry_usda(&entry), 8);
        let single = render(load_entry_usda(&entry), 1);
        assert_eq!(threaded, single, "{}", dir.display());
    }
}

/// A scene of `groups` groups of `width` prims each: instances of a few
/// prototypes (so instances share and claim prototypes), prims inheriting
/// a class, prims selecting a variant, and inactive prims, each with
/// descendants of its own.
fn wide_scene(groups: usize, width: usize) -> String {
    let mut out = String::from("#usda 1.0\n");
    for proto in 0..4 {
        writeln!(
            out,
            r#"def "Proto{proto}" (
    variants = {{ string look = "plain" }}
    prepend variantSets = "look"
) {{
    def "Body" {{
        double size = {proto}
        def "Part" {{ token kind = "leaf" }}
    }}
    variantSet "look" = {{
        "plain" {{ def "Trim" {{ }} }}
        "fancy" {{ def "Trim" {{ def "Bead" {{ }} }} def "Extra" {{ }} }}
    }}
}}"#
        )
        .unwrap();
    }
    out.push_str(
        r#"class "_Shared" {
    def "FromClass" { int count = 3 }
}
def "World" {
"#,
    );
    for group in 0..groups {
        writeln!(out, "    def \"Group{group}\" {{").unwrap();
        for item in 0..width {
            let proto = (group + item) % 4;
            let body = match item % 5 {
                0 | 1 => format!(
                    "(\n            instanceable = true\n            prepend references = </Proto{proto}>\n        )"
                ),
                2 => String::from("(\n            prepend inherits = </_Shared>\n        )"),
                3 => format!(
                    "(\n            prepend references = </Proto{proto}>\n            variants = {{ string look = \"fancy\" }}\n        )"
                ),
                _ => String::from("(\n            active = false\n        )"),
            };
            writeln!(
                out,
                "        def \"Item{item}\" {body} {{\n            def \"Own\" {{ }}\n        }}"
            )
            .unwrap();
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    out
}

#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn a_wide_scene_composes_alike_on_any_number_of_threads() {
    struct NoAssets;
    impl layerstack::AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &layerstack::TokenInterner,
            _: &layerstack::PathInterner,
        ) -> Result<layerstack::ResolvedAsset, layerstack::AssetResolveError> {
            Err(layerstack::AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }
    let source = wide_scene(24, 40);
    let compose = |threads| {
        let mut store = InMemoryStore::default();
        let result = layerstack_usda::read_usda(
            &source,
            LayerId(1),
            &store.tokens,
            &store.paths,
            &mut NoAssets,
        );
        assert!(!result.emitted.rejected, "{:?}", result.emitted.diagnostics);
        store.insert_layer(result.emitted.layer);
        let names = BTreeMap::from([(LayerId(1), String::from("scene"))]);
        render_store(&mut store, LayerId(1), &names, threads)
    };
    let single = compose(1);
    assert!(
        single.lines().filter(|line| !line.starts_with(' ')).count() > 24 * 40 * 3,
        "the scene composes its prims and their descendants"
    );
    assert!(
        single.contains("/World/Group3/Item3/Trim/Bead"),
        "a variant"
    );
    assert!(
        single.contains("/World/Group5/Item0/Body/Part"),
        "an instance"
    );
    for threads in [2, 8, 16] {
        // Each composition interns in an order its threads decide.
        for _ in 0..3 {
            assert_eq!(compose(threads), single, "{threads} threads");
        }
    }
}
