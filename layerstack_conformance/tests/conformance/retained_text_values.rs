// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained `token[]` and `string[]` defaults read, compose, save and edit
//! as eagerly decoded ones do, decode once however many threads read them,
//! and report a payload that does not decode.
//!
//! AOUSD Core §16.3.9–§16.3.10 (crate values), §12.3 (value resolution).

use std::path::{Path, PathBuf};
use std::sync::Barrier;

use layerstack::edit::{Address, Transaction};
use layerstack::{
    ArrayReadError, AssetResolveError, AssetResolver, FromValue, InMemoryStore, Layer, LayerId,
    PathInterner, PrimSpec, PropertyPath, PropertySpec, PropertyType, ResolvedAsset, Stage,
    StageOptions, SublayerEntry, Time, TokenId, TokenInterner, TypedArray, Value,
};
use layerstack_conformance::library_scene::{LibraryFormat, LibraryScene, LibraryShape};
use layerstack_conformance::save_corpus::in_text_order;
use layerstack_usdc::writer::{Spec, SpecForm, Specifier, Value as W, write_crate};
use layerstack_usdc::{CrateFile, DecodeBudget, LazyReadResult, read_usdc, read_usdc_lazy};

/// The opinions of the property at `path`, strongest first; `None` when
/// none is authored.
fn opinions<'a>(
    stage: &'a Stage,
    store: &'a InMemoryStore,
    path: PropertyPath,
) -> Option<&'a [layerstack::Opinion]> {
    stage
        .view(store)
        .property(path)
        .map(|property| property.opinions())
        .filter(|opinions| !opinions.is_empty())
}

/// The value opinions author for the attribute at `path` at `time`,
/// without a schema fallback.
fn authored(
    stage: &Stage,
    store: &InMemoryStore,
    path: PropertyPath,
    time: Time,
) -> Option<layerstack::Resolved<Value>> {
    stage
        .view(store)
        .attribute(path)
        .and_then(|attribute| attribute.authored_value(time))
}

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

/// The workspace root, relative where WASI preopens it (see
/// `.cargo/config.toml`).
fn workspace() -> PathBuf {
    layerstack_conformance::workspace_root()
}

/// The crate files below `dir`, recursively.
fn crate_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            crate_files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("usdc" | "usd")
        ) && std::fs::read(&path).is_ok_and(|bytes| bytes.starts_with(b"PXR-USDC"))
        {
            out.push(path);
        }
    }
}

/// The generated parts library in `shape`, written as USDC into `dir`.
fn write_library(shape: &LibraryShape, dir: &str) -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(dir);
    let written = LibraryScene::generate(shape)
        .write(&dir, LibraryFormat::Usdc)
        .expect("the library writes");
    written
        .files
        .iter()
        .map(|(path, _)| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(path).expect("a written layer"))
        })
        .collect()
}

/// The small parts library with `tags` arrays long enough to be retained:
/// the small shape's average of 8 elements is below
/// [`layerstack_usdc::MIN_RETAINED_TEXT_ARRAY`].
fn library() -> &'static [(String, Vec<u8>)] {
    static FILES: std::sync::OnceLock<Vec<(String, Vec<u8>)>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        let shape = LibraryShape {
            average_tags: 40,
            ..LibraryShape::SMALL
        };
        write_library(&shape, "retained_text_values/long_tags")
    })
}

/// The conformance USDC corpus and the small parts library.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let root = workspace();
    let mut paths = Vec::new();
    for dir in [
        root.join("core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary"),
        root.join("layerstack_conformance/fixtures"),
        root.join("layerstack_conformance/tests/assets"),
    ] {
        crate_files(&dir, &mut paths);
    }
    let mut files: Vec<(String, Vec<u8>)> = paths
        .iter()
        .map(|path| {
            let name = path.strip_prefix(&root).unwrap().display().to_string();
            (name, std::fs::read(path).unwrap())
        })
        .collect();
    files.extend(write_library(
        &LibraryShape::SMALL,
        "retained_text_values/small",
    ));
    files.extend(library().iter().cloned());
    files
}

/// One file read both ways into the same interners.
struct Pair {
    tokens: TokenInterner,
    paths: PathInterner,
    eager: Layer,
    lazy: LazyReadResult,
}

fn read_both(bytes: &[u8]) -> Option<Pair> {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let eager = read_usdc(bytes, LayerId(1), &tokens, &paths, &mut NoAssets).ok()?;
    let lazy = read_usdc_lazy(bytes.into(), LayerId(1), &tokens, &paths, &mut NoAssets)
        .expect("a file that reads eagerly reads retained");
    Some(Pair {
        tokens,
        paths,
        eager: eager.layer,
        lazy,
    })
}

/// The deferred defaults of `layer`.
fn deferred_defaults(layer: &Layer) -> usize {
    layer
        .prims()
        .values()
        .chain(layer.variant_prims().values().flatten())
        .flat_map(|prim| &prim.properties)
        .filter(|entry| matches!(entry.spec.default, Some(Value::Deferred(_))))
        .count()
}

fn same(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.same_representation(b),
        (None, None) => true,
        _ => false,
    }
}

/// Every attribute's default, time samples and metadata, and every other
/// field, agree between the two reads, in value and representation.
fn assert_reads_agree(name: &str, eager: &Layer, lazy: &Layer) {
    assert!(eager == lazy, "{name}: the layers differ");
    fn prims(layer: &Layer) -> Vec<(layerstack::PathId, &[PrimSpec])> {
        let mut prims: Vec<_> = layer
            .prims()
            .iter()
            .map(|(path, prim)| (*path, core::slice::from_ref(prim)))
            .chain(
                layer
                    .variant_prims()
                    .iter()
                    .map(|(path, prims)| (*path, prims)),
            )
            .collect();
        prims.sort_by_key(|(path, prims)| (*path, prims.len()));
        prims
    }
    let (eager_prims, lazy_prims) = (prims(eager), prims(lazy));
    assert_eq!(eager_prims.len(), lazy_prims.len(), "{name}");
    for ((path, eager), (_, lazy)) in eager_prims.iter().zip(&lazy_prims) {
        for (eager, lazy) in eager.iter().zip(lazy.iter()) {
            assert_eq!(
                eager.properties.len(),
                lazy.properties.len(),
                "{name} {path:?}"
            );
            for (e, l) in eager.properties.iter().zip(&lazy.properties) {
                let at = format!("{name} {path:?} property {:?}", e.name);
                assert_eq!(e.name, l.name, "{at}");
                assert!(
                    same(e.spec.default.as_ref(), l.spec.default.as_ref()),
                    "{at}: default"
                );
                let samples = |spec: &PropertySpec| spec.time_samples.clone().into_vec();
                match (samples(&e.spec), samples(&l.spec)) {
                    (Some(a), Some(b)) => {
                        assert_eq!(a.len(), b.len(), "{at}: samples");
                        for ((ta, va), (tb, vb)) in a.iter().zip(b.iter()) {
                            assert!(
                                ta.to_bits() == tb.to_bits() && va.same_representation(vb),
                                "{at}: sample at {ta}"
                            );
                        }
                    }
                    (a, b) => assert_eq!(a.is_some(), b.is_some(), "{at}: samples"),
                }
                assert_eq!(e.spec.metadata, l.spec.metadata, "{at}: metadata");
                assert!(*e.spec == *l.spec, "{at}");
            }
        }
    }
}

#[test]
fn retained_reads_agree_with_eager_reads_across_the_usdc_corpus() {
    let mut files = 0;
    let mut deferred = 0;
    for (name, bytes) in corpus() {
        let Some(pair) = read_both(&bytes) else {
            continue;
        };
        files += 1;
        deferred += deferred_defaults(&pair.lazy.assembled.layer);
        assert_reads_agree(&name, &pair.eager, &pair.lazy.assembled.layer);
        let stats = pair.lazy.values.stats();
        assert_eq!(stats.failed_values, 0, "{name}");
        assert_eq!(
            stats.materialized_values, stats.live_values,
            "{name}: comparing decoded every retained value"
        );
    }
    assert!(files > 60, "the corpus is found ({files} files)");
    assert!(deferred > 0, "the corpus retains text arrays");
}

#[test]
fn composition_and_raw_queries_read_text_arrays_without_decoding_them() {
    let (_, bytes) = library()
        .iter()
        .find(|(name, _)| name == "library_base.usdc")
        .expect("the base layer");
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .unwrap();
    let retained = read.values.stats().live_values;
    assert_eq!(retained, deferred_defaults(&read.assembled.layer));
    assert!(retained > 0, "the library's tags are retained");
    store.insert_layer(read.assembled.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        read.values.stats().decode_attempts,
        0,
        "composition stays structural"
    );
    // A read decodes the value it selects, once.
    let tags = store.tokens.lookup("tags").unwrap();
    let root = store.paths.lookup(&layerstack::Path::root()).unwrap();
    let tagged: Vec<PropertyPath> = stage
        .traverse_all(root)
        .map(|prim| PropertyPath::new(prim, tags))
        .filter(|path| opinions(&stage, &store, *path).is_some())
        .collect();
    let first = stage
        .view(&store)
        .attribute(tagged[0])
        .and_then(|attribute| {
            attribute.read(Time::Default, |v| {
                Vec::<TokenId>::from_value(v, &store.tokens)
            })
        })
        .unwrap()
        .value;
    assert!(!first.is_empty());
    assert_eq!(read.values.stats().decode_attempts, 1);
    assert_eq!(read.values.stats().materialized_values, 1);
    let Value::TypedArray(TypedArray::Token(items)) =
        authored(&stage, &store, tagged[0], Time::Default)
            .unwrap()
            .value
    else {
        panic!("the decoded native array, as an eager read holds it");
    };
    assert_eq!(items[..], first[..]);
    assert_eq!(read.values.stats().decode_attempts, 1, "decoded once");
}

#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn retained_text_arrays_decode_once_across_threads() {
    let (_, bytes) = library()
        .iter()
        .find(|(name, _)| name == "library_base.usdc")
        .expect("the base layer");
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .unwrap();
    store.insert_layer(read.assembled.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let tags = store.tokens.lookup("tags").unwrap();
    let root = store.paths.lookup(&layerstack::Path::root()).unwrap();
    let tagged: Vec<PropertyPath> = stage
        .traverse_all(root)
        .map(|prim| PropertyPath::new(prim, tags))
        .filter(|path| opinions(&stage, &store, *path).is_some())
        .collect();
    const THREADS: usize = 8;
    let start = Barrier::new(THREADS);
    let cells: Vec<usize> = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    let view = stage.view(&store);
                    start.wait();
                    tagged
                        .iter()
                        .map(
                            |path| match view.attribute(*path).unwrap().try_get(Time::Default) {
                                Ok(Some(resolved)) => match resolved.value {
                                    Value::TypedArray(TypedArray::Token(items)) => items.len(),
                                    other => panic!("a decoded native array, not {other:?}"),
                                },
                                other => panic!("a value, not {other:?}"),
                            },
                        )
                        .sum()
                })
            })
            .collect();
        readers.into_iter().map(|r| r.join().unwrap()).collect()
    });
    assert!(cells.iter().all(|&n| n == cells[0] && n > 0), "{cells:?}");
    let stats = read.values.stats();
    assert_eq!(stats.failed_values, 0);
    assert_eq!(stats.materialized_values, stats.live_values);
    assert_eq!(
        stats.decode_attempts,
        stats.materialized_values + stats.materialized_arrays,
        "each retained source decoded once for {THREADS} readers"
    );
    assert_eq!(stats.value_elements, cells[0]);
}

#[test]
fn retained_text_arrays_save_as_eager_reads_do() {
    let mut checked = 0;
    for (name, bytes) in library() {
        let pair = read_both(bytes).unwrap();
        let lazy = &pair.lazy.assembled.layer;
        let save_usda = |layer| layerstack_usda::save::save_usda(layer, &pair.tokens, &pair.paths);
        let save_usdc =
            |layer| layerstack_usdc::writer::save_layer(layer, &pair.tokens, &pair.paths);
        checked += deferred_defaults(lazy);
        let text = save_usda(lazy).unwrap();
        assert_eq!(text, save_usda(&pair.eager).unwrap(), "{name}: USDA");
        let binary = save_usdc(lazy).unwrap();
        assert_eq!(binary, save_usdc(&pair.eager).unwrap(), "{name}: USDC");
        // Both read back as the layer they were saved from.
        let again = read_usdc(
            &binary,
            LayerId(1),
            &pair.tokens,
            &pair.paths,
            &mut NoAssets,
        )
        .unwrap()
        .layer;
        assert_reads_agree(name, &pair.eager, &again);
        let text =
            layerstack_usda::read_usda(&text, LayerId(1), &pair.tokens, &pair.paths, &mut NoAssets);
        assert!(text.emitted.diagnostics.is_empty(), "{name}");
        // In the order the text writer sorts fields and properties into.
        let mut sorted = pair.eager.clone();
        in_text_order(&mut sorted, &pair.tokens);
        assert!(text.emitted.layer == sorted, "{name}: USDA reads back");
    }
    assert!(checked > 0, "the library retains text arrays");
}

#[test]
fn edits_replace_and_restore_retained_values() {
    let (_, bytes) = library()
        .iter()
        .find(|(name, _)| name == "library_base.usdc")
        .expect("the base layer");
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .unwrap();
    let eager = read_usdc(
        bytes,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .unwrap()
    .layer;
    let (prim, entry) = read
        .assembled
        .layer
        .prims()
        .iter()
        .find_map(|(path, prim)| {
            let entry = prim
                .properties
                .iter()
                .find(|e| matches!(e.spec.default, Some(Value::Deferred(_))))?;
            Some((*path, entry.clone()))
        })
        .expect("a retained default");
    let decoded = eager.prims()[&prim]
        .property(entry.name)
        .unwrap()
        .default
        .clone()
        .unwrap();
    store.insert_layer(read.assembled.layer);
    let at = Address::spec(
        LayerId(1),
        layerstack::SpecPath::from_property_path(PropertyPath::new(prim, entry.name)),
    );
    // A guard compares the decoded value, as it would an eager one.
    Transaction::new()
        .expect_default(at.clone(), Some(decoded.clone()))
        .apply(&mut store)
        .expect("the retained value is the decoded one");
    let rock = store.tokens.intern("rock");
    let inverse = Transaction::new()
        .set_default(at.clone(), Value::Array(vec![Value::Token(rock)]))
        .apply(&mut store)
        .unwrap();
    let default = |store: &InMemoryStore| {
        store.layer(LayerId(1)).unwrap().prims()[&prim]
            .property(entry.name)
            .unwrap()
            .default
            .clone()
    };
    assert_eq!(
        default(&store),
        Some(Value::Array(vec![Value::Token(rock)]))
    );
    inverse.apply(&mut store).unwrap();
    let Some(Value::Deferred(restored)) = default(&store) else {
        panic!("undo restores the retained value");
    };
    let Some(Value::Deferred(original)) = &entry.spec.default else {
        unreachable!()
    };
    assert!(restored.shares_source(original), "and keeps its source");
    assert_eq!(default(&store), Some(decoded));
    assert_eq!(read.values.stats().decode_attempts, 1);
}

/// A crate file whose `/Rock.tags` default declares more elements than it
/// holds (within the decode budget), so it does not decode.
fn malformed() -> Vec<u8> {
    let tags: Vec<String> = (0..24).map(|i| format!("pebble{i}")).collect();
    let mut bytes = write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot)
            .with_field("primChildren", W::TokenVector(vec!["Rock".into()])),
        Spec::new("/Rock", SpecForm::Prim)
            .with_field("specifier", W::Specifier(Specifier::Def))
            .with_field("properties", W::TokenVector(vec!["tags".into()])),
        Spec::new("/Rock.tags", SpecForm::Attribute)
            .with_field("typeName", W::Token("token[]".into()))
            .with_field("default", W::TokenArray(tags)),
    ])
    .unwrap();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let offset = file
        .spec("/Rock.tags")
        .unwrap()
        .field("default")
        .unwrap()
        .representation()
        .payload_offset();
    let offset = usize::try_from(offset).unwrap();
    drop(file);
    bytes[offset..offset + 8].copy_from_slice(&100_000_u64.to_le_bytes());
    bytes
}

#[test]
fn a_retained_text_array_that_does_not_decode_is_an_error_when_read() {
    let bytes = malformed();
    let mut store = InMemoryStore::default();
    assert!(
        read_usdc(
            &bytes,
            LayerId(1),
            &store.tokens,
            &store.paths,
            &mut NoAssets
        )
        .is_err(),
        "an eager read fails at once"
    );
    let read = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .expect("a retained read leaves the payload for later");
    let saved =
        layerstack_usda::save::save_usda(&read.assembled.layer, &store.tokens, &store.paths);
    assert!(
        matches!(
            saved,
            Err(layerstack_usda::save::SaveError::ArrayRead {
                error: ArrayReadError::InvalidData(_),
                ..
            })
        ),
        "saving reports it: {saved:?}"
    );
    store.insert_layer(read.assembled.layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let tags = store.property_path("/Rock.tags");
    let tags_attribute = stage.view(&store).attribute(tags).unwrap();
    let checked = tags_attribute.try_get(Time::Default);
    assert!(
        matches!(checked, Err(ArrayReadError::InvalidData(_))),
        "{checked:?}"
    );
    let typed = tags_attribute.try_read(Time::Default, |v| {
        Vec::<TokenId>::from_value(v, &store.tokens)
    });
    assert!(
        matches!(typed, Err(ArrayReadError::InvalidData(_))),
        "{typed:?}"
    );
    // An unchecked read finds no value of any type, as for a numeric array.
    let view = stage.view(&store);
    let attribute = view.attribute(tags).unwrap();
    assert_eq!(attribute.value::<Vec<TokenId>>(Time::Default), None);
    assert!(attribute.try_get(Time::Default).is_err());
    let stats = read.values.stats();
    assert_eq!((stats.failed_values, stats.decode_attempts), (1, 1));
}

/// A retained numeric array that does not decode.
#[derive(Debug)]
struct BrokenNumbers(ArrayReadError);
impl layerstack::DeferredArraySource for BrokenNumbers {
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.0)
    }
    fn element_kind(&self) -> Value {
        Value::Float(0.0)
    }
}

/// A stronger retained value that does not decode answers a read with its
/// failure at the default time as at a time code, as a retained numeric
/// array does: never with the weaker opinion's value.
#[test]
fn a_failed_retained_text_array_hides_weaker_values_at_every_time() {
    let bytes = malformed();
    let mut store = InMemoryStore::default();
    let read = read_usdc_lazy(
        bytes.as_slice().into(),
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .unwrap();
    let mut stronger = read.assembled.layer;
    let rock = store.path("/Rock");
    let (tags, sizes) = (store.tokens.intern("tags"), store.tokens.intern("sizes"));
    let float_array = PropertyType::parse("float[]").unwrap();
    let broken = ArrayReadError::InvalidData("broken float array".into());
    stronger.prims_mut().get_mut(&rock).unwrap().set_property(
        sizes,
        PropertySpec::typed_attribute(float_array.clone()).with_default(Value::TypedArray(
            TypedArray::Deferred(std::sync::Arc::new(BrokenNumbers(broken))),
        )),
    );
    store.insert_layer(stronger);
    let mut weaker = Layer::new(LayerId(2));
    weaker.insert_prim(
        rock,
        PrimSpec::def()
            .with_property(
                tags,
                PropertySpec::typed_attribute(PropertyType::parse("token[]").unwrap())
                    .with_default(Value::array(vec![Value::Token(
                        store.tokens.intern("pebble"),
                    )])),
            )
            .with_property(
                sizes,
                PropertySpec::typed_attribute(float_array)
                    .with_default(Value::array(vec![Value::Float(1.0)])),
            ),
    );
    store.insert_layer(weaker);
    let mut root = Layer::new(LayerId(3));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(1)));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    store.insert_layer(root);
    let stage = Stage::compose(&mut store, LayerId(3), StageOptions::default());
    let view = stage.view(&store);
    let undecoded = |value: &Value| {
        matches!(
            value,
            Value::Deferred(_) | Value::TypedArray(TypedArray::Deferred(_))
        )
    };
    for name in ["/Rock.tags", "/Rock.sizes"] {
        let attribute = view.attribute(store.property_path(name)).unwrap();
        for time in [Time::Default, Time::at(1.0)] {
            let tokens = attribute.read(time, |v| Vec::<TokenId>::from_value(v, &store.tokens));
            let numbers = attribute.read(time, |v| Vec::<f32>::from_value(v, &store.tokens));
            assert!(
                tokens.is_none() && numbers.is_none(),
                "{name} at {time:?}: {tokens:?} {numbers:?}"
            );
            assert!(attribute.try_get(time).is_err(), "{name} at {time:?}");
            // An untyped read finds no value at the default time, and the
            // undecoded value at a time code; never the weaker one.
            let untyped = attribute.get(time).map(|resolved| resolved.value);
            match time {
                Time::Default => assert!(untyped.is_none(), "{name}: {untyped:?}"),
                Time::At { .. } => assert!(untyped.as_ref().is_some_and(undecoded), "{name}"),
            }
        }
    }
}
