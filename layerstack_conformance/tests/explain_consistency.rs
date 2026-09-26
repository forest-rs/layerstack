// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value explanations resolve exactly what value resolution resolves.
//!
//! Every `Stage::explain_*` query mirrors a `resolve_*` query, and its
//! `value` must equal that query's value. This checks the pairs over the
//! whole corpus:
//!
//! - every composition fixture under
//!   `core-spec-supplemental-release_dec2025/composition/tests/assets`;
//! - every value resolution fixture under
//!   `core-spec-supplemental-release_dec2025/value_resolution/tests/assets`;
//! - every case of `tests/data/temporal_sparse.json`, which composes sparse
//!   array edits over time samples, blocks and layer offsets.
//!
//! For each composed prim it checks every property at the default time and
//! at a spread of times under held and linear interpolation, every prim
//! metadata field, and both again with schema fallbacks: a synthetic
//! registry gives every prim type a fallback for every property name
//! (array-valued names get an array fallback, so sparse edits compose over
//! it).
//!
//! Two properties of the schema-aware explanations are checked as well:
//!
//! - with no fallbacks at all, every authored property is still explained,
//!   blocks included;
//! - an authored value whose explanation reports no use of the fallback
//!   (`seeded_by_fallback` is false, per composed time sample too) resolves
//!   to the same value under a different fallback.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, Layer, LayerId, PathId,
    PathInterner, PrimSpec, PropertyPath, ResolvedAsset, SchemaDefinition, SchemaRegistry, Stage,
    StageOptions, TokenId, TokenInterner, Value, ValueSource,
};
use layerstack_conformance::{pcp_txt::load_pcp_txt, usda_real::load_entry_usda, workspace_root};
use layerstack_usda::{emit, lower, parser::parse_cst};
use serde::Deserialize;

const TIMES: [f64; 12] = [
    -10.0, -1.0, 0.0, 0.25, 0.5, 1.0, 1.5, 2.0, 2.5, 5.0, 10.0, 101.0,
];
const INTERPOLATIONS: [InterpolationType; 2] = [InterpolationType::Held, InterpolationType::Linear];

/// Counts the pairs checked and collects mismatches.
#[derive(Default)]
struct Tally {
    checked: usize,
    failures: String,
}

impl Tally {
    fn check<T: PartialEq + std::fmt::Debug>(
        &mut self,
        label: impl FnOnce() -> String,
        explained: Option<T>,
        resolved: Option<T>,
    ) {
        self.checked += 1;
        if explained != resolved {
            let _ = writeln!(
                self.failures,
                "{}: explained {explained:?} != resolved {resolved:?}",
                label()
            );
        }
    }
}

/// Every prim spec of a layer, including those inside variant branches.
fn prim_specs(layer: &Layer) -> impl Iterator<Item = &PrimSpec> {
    layer
        .prims
        .values()
        .chain(layer.variant_prims.values().flatten())
}

/// Property and metadata names authored anywhere in `store`, with whether
/// some opinion of the property authors an array, and every prim type.
struct Names {
    properties: BTreeMap<TokenId, bool>,
    fields: BTreeSet<TokenId>,
    types: BTreeSet<TokenId>,
}

fn names(store: &InMemoryStore) -> Names {
    let mut names = Names {
        properties: BTreeMap::new(),
        fields: BTreeSet::new(),
        types: BTreeSet::new(),
    };
    let is_array = |value: &Value| matches!(value, Value::Array(_) | Value::ArrayEdit(_));
    for layer in store.layers.values() {
        for spec in prim_specs(layer) {
            names.types.extend(spec.type_name);
            let variants = spec.variant_branches().map(|branch| branch.spec);
            let fields = spec
                .fields
                .iter()
                .chain(variants.clone().flat_map(|v| &v.fields));
            names.fields.extend(fields.map(|field| field.name));
            let properties = spec
                .properties
                .iter()
                .chain(variants.flat_map(|v| &v.properties));
            for property in properties {
                let spec = &property.spec;
                let array = spec.default.as_ref().is_some_and(is_array)
                    || spec
                        .time_samples
                        .as_ref()
                        .is_some_and(|samples| samples.iter().any(|(_, v)| is_array(v)));
                *names.properties.entry(property.name).or_default() |= array;
            }
        }
    }
    names
}

/// A registry giving every prim type a fallback for every property name;
/// `alternate` picks different fallbacks (of another array length).
fn registry(names: &Names, alternate: bool) -> SchemaRegistry {
    let mut registry = SchemaRegistry::new();
    for &type_name in &names.types {
        let mut schema = SchemaDefinition::typed(type_name);
        for (&property, &array) in &names.properties {
            let fallback = match (array, alternate) {
                (true, false) => Value::Array(vec![Value::Float(7.0), Value::Float(8.0)]),
                (true, true) => Value::Array(vec![Value::Float(1.0); 3]),
                (false, false) => Value::Double(0.5),
                (false, true) => Value::Double(0.25),
            };
            schema = schema.with_property(property, fallback);
        }
        registry.register(schema);
    }
    registry
}

/// Checks every explain/resolve pair over a composed stage.
fn check_stage(label: &str, store: &InMemoryStore, stage: &Stage, tally: &mut Tally) {
    let names = names(store);
    let alternate = registry(&names, true);
    let registry = registry(&names, false);
    let empty = SchemaRegistry::new();
    let root = store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("root path");
    let prims: Vec<PathId> = stage.traverse(root).collect();
    for prim in prims {
        let prim_label = || format!("{label} {}", store.paths.display(prim, &store.tokens));
        for &field in &names.fields {
            if stage.explain_field(prim, field).is_none() {
                continue;
            }
            tally.check(
                || format!("{} metadata {}", prim_label(), store.tokens.resolve(field)),
                stage.explain_value(prim, field).and_then(|e| e.value),
                stage.resolve_value(prim, field).map(|r| r.value),
            );
        }
        for &property in names.properties.keys() {
            let path = PropertyPath::new(prim, property);
            let name = || format!("{}.{}", prim_label(), store.tokens.resolve(property));
            let authored = stage.explain_property_path(path).is_some();
            if authored {
                tally.check(
                    || format!("{} default", name()),
                    stage.explain_property_value(path).and_then(|e| e.value),
                    stage.resolve_property_path(path).map(|r| r.value),
                );
            }
            tally.check(
                || format!("{} default with schema", name()),
                stage
                    .explain_value_with_schema(prim, property, store, &registry, None)
                    .and_then(|e| e.value),
                stage
                    .resolve_value_with_schema(prim, property, store, &registry, None)
                    .map(|r| r.value),
            );
            // Without fallbacks, an authored property is always explained,
            // blocks included.
            let unexplained = stage
                .explain_value_with_schema(prim, property, store, &empty, None)
                .map(|e| e.value);
            tally.check(
                || format!("{} default without fallback", name()),
                unexplained.clone().flatten(),
                stage
                    .resolve_value_with_schema(prim, property, store, &empty, None)
                    .map(|r| r.value),
            );
            tally.check(
                || format!("{} default without fallback is explained", name()),
                Some(unexplained.is_some()),
                Some(authored),
            );
            // An authored value that reports no use of the fallback is the
            // same under another fallback.
            if let Some(explained) =
                stage.explain_value_with_schema(prim, property, store, &registry, None)
                && !explained.seeded_by_fallback
                && !matches!(explained.source, ValueSource::Fallback | ValueSource::None)
            {
                tally.check(
                    || format!("{} default independent of the fallback", name()),
                    explained.value,
                    stage
                        .resolve_value_with_schema(prim, property, store, &alternate, None)
                        .map(|r| r.value),
                );
            }
            for time in TIMES {
                for interp in INTERPOLATIONS {
                    if authored {
                        tally.check(
                            || format!("{} at {time} {interp:?}", name()),
                            stage
                                .explain_property_value_at_time(path, time, interp)
                                .and_then(|e| e.value),
                            stage
                                .resolve_property_path_at_time(path, time, interp)
                                .map(|r| r.value),
                        );
                    }
                    let explained = stage.explain_value_at_time_with_schema(
                        prim, property, time, interp, store, &registry, None,
                    );
                    let seeded = explained.as_ref().is_none_or(|e| {
                        e.seeded_by_fallback
                            || matches!(e.source, ValueSource::Fallback | ValueSource::None)
                    });
                    let value = explained.and_then(|e| e.value);
                    tally.check(
                        || format!("{} at {time} {interp:?} with schema", name()),
                        value.clone(),
                        stage
                            .resolve_value_at_time_with_schema(
                                prim, property, time, interp, store, &registry, None,
                            )
                            .map(|r| r.value),
                    );
                    if !seeded {
                        tally.check(
                            || {
                                format!(
                                    "{} at {time} {interp:?} independent of the fallback",
                                    name()
                                )
                            },
                            value,
                            stage
                                .resolve_value_at_time_with_schema(
                                    prim, property, time, interp, store, &alternate, None,
                                )
                                .map(|r| r.value),
                        );
                    }
                    let unexplained = stage
                        .explain_value_at_time_with_schema(
                            prim, property, time, interp, store, &empty, None,
                        )
                        .map(|e| e.value);
                    tally.check(
                        || format!("{} at {time} {interp:?} without fallback", name()),
                        unexplained.clone().flatten(),
                        stage
                            .resolve_value_at_time_with_schema(
                                prim, property, time, interp, store, &empty, None,
                            )
                            .map(|r| r.value),
                    );
                    tally.check(
                        || {
                            format!(
                                "{} at {time} {interp:?} without fallback is explained",
                                name()
                            )
                        },
                        Some(unexplained.is_some()),
                        Some(authored),
                    );
                }
            }
        }
    }
}

fn spec_assets(area: &str) -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join(area)
        .join("tests")
        .join("assets")
}

fn sorted_dirs(dir: &PathBuf) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("assets dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn check_file(label: &str, entry: &std::path::Path, tally: &mut Tally) {
    let mut loaded = load_entry_usda(entry);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    check_stage(label, &loaded.store, &stage, tally);
}

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    layers: BTreeMap<String, String>,
}

/// Resolves asset paths against a case's in-memory USDA sources.
struct MemoryResolver<'a> {
    sources: &'a BTreeMap<String, String>,
    by_name: BTreeMap<String, LayerId>,
    next_layer_id: u64,
    pending: Vec<Layer>,
}

impl AssetResolver for MemoryResolver<'_> {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let name = asset_path.trim_start_matches("./");
        if let Some(id) = self.by_name.get(name) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(name),
                layer: None,
            });
        }
        let source = self.sources.get(name).ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.by_name.insert(name.to_string(), layer_id);
        let layer = emit_layer(source, layer_id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(name),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn emit_layer(
    source: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut MemoryResolver<'_>,
) -> Layer {
    let cst = parse_cst(source);
    let ast = lower::lower(&cst.tree, source);
    let result = emit::emit(&ast.layer, layer_id, tokens, paths, resolver);
    resolver.pending.extend(result.resolved_layers);
    result.layer
}

#[test]
fn explanations_resolve_what_resolution_resolves() {
    let mut tally = Tally::default();

    let composition = spec_assets("composition");
    let mut fixtures = 0;
    for dir in sorted_dirs(&composition) {
        let name = dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if !dir.join("pcp.txt").is_file() {
            continue;
        }
        let oracle = load_pcp_txt(&dir.join("pcp.txt"));
        check_file(&name, &dir.join("usda").join(&oracle.entry), &mut tally);
        fixtures += 1;
    }

    for dir in sorted_dirs(&spec_assets("value_resolution")) {
        let name = dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let usda = dir.join("usda");
        let entry = ["entry.usda", "root.usda"]
            .iter()
            .map(|file| usda.join(file))
            .find(|path| path.is_file())
            .expect("entry layer");
        check_file(&name, &entry, &mut tally);
        fixtures += 1;
    }

    let vectors: Vectors =
        serde_json::from_str(include_str!("data/temporal_sparse.json")).expect("vectors");
    for case in &vectors.cases {
        let mut store = InMemoryStore::default();
        let mut resolver = MemoryResolver {
            sources: &case.layers,
            by_name: BTreeMap::from([("root.usda".to_string(), LayerId(1))]),
            next_layer_id: 2,
            pending: Vec::new(),
        };
        let layer = emit_layer(
            &case.layers["root.usda"],
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut resolver,
        );
        store.insert_layer(layer);
        for layer in resolver.pending.drain(..) {
            store.insert_layer(layer);
        }
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        check_stage(&case.name, &store, &stage, &mut tally);
        fixtures += 1;
    }

    println!("{fixtures} fixtures, {} pairs checked", tally.checked);
    assert!(
        tally.failures.is_empty(),
        "explanations disagree with resolution:\n{}",
        tally.failures
    );
    assert!(tally.checked > 10_000, "the corpus exercises the queries");
}
