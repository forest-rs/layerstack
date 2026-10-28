// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What each fuzz target does with one input.
//!
//! The readers take bytes from anywhere: a browser page opens whatever
//! file it is given, and in WebAssembly a panic aborts the whole module.
//! Every function here feeds arbitrary bytes to one reader and requires
//! only that it returns: malformed input comes back as an error, or as a
//! recovered read with diagnostics. A panic, a hang or an allocation the
//! input cannot justify is a finding.
//!
//! `layerstack_conformance` runs the minimized inputs kept in
//! `fuzz/regressions/<target>/` through these same functions on stable,
//! including this file as a module, so it uses only the crates both
//! depend on.

use std::{collections::BTreeMap, sync::Arc};

use layerstack::{
    AssetResolveError, AssetResolver, CompositionLimits, FormatArgs, InMemoryStore, LayerId, Path,
    PathExpression, PathInterner, PropertyPath, ResolvedAsset, SpecPath, StageOptions, TargetPath,
    Time, TokenInterner, ValueType,
    asset::{PackagePath, UdimPattern},
    encoded_bytes::EncodedBytes,
    variable_expression::{ExpressionVariables, VariableExpression},
};
use layerstack_io::{
    ImportPolicy, IoError, IoErrorKind, LoadOptions, StageDocument, Storage, UsdcArrayLoading,
    UsdcReadOptions,
};
use layerstack_usda::diagnostic::Severity;
use layerstack_usdz::{
    range::{RangeLimits, RangePackage},
    snapshot::{PackageLimits, PackageSnapshot},
};

/// A resolver that finds no asset, so a fuzzed layer reads on its own.
#[derive(Debug, Default)]
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

/// The identifier of the root layer, the only source the fuzzed storage
/// holds: its extension follows the input's header, as a host names a
/// file it was given.
fn root_identifier(data: &[u8]) -> &'static str {
    if data.starts_with(b"PXR-USDC") {
        "root.usdc"
    } else if data.starts_with(b"PK\x03\x04") {
        "root.usdz"
    } else {
        "root.usda"
    }
}

/// Storage holding one source, the fuzzed root layer; every other asset
/// is not found.
#[derive(Debug)]
struct OneSource {
    sources: BTreeMap<String, Vec<u8>>,
}

impl Storage for OneSource {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.sources
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, identifier))
    }

    fn read_bounded(&self, identifier: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
        let bytes = self.read(identifier)?;
        if bytes.len() as u64 > max_bytes {
            return Err(IoError::new(IoErrorKind::Rejected, "source too large"));
        }
        Ok(bytes)
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.sources.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
}

/// Parses USDA text into the AST and the lossless syntax tree, and imports
/// it as a layer.
pub fn usda(data: &[u8]) {
    let Ok(source) = core::str::from_utf8(data) else {
        return;
    };
    let _ = layerstack_usda::parser::parse(source);
    let _ = layerstack_usda::parser::parse_cst(source);
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
    // The writers take whatever a recovered read holds, and refuse what
    // they cannot write with an error.
    let _ = layerstack_usda::save::save_usda(&read.emitted.layer, &tokens, &paths);
    let _ = layerstack_usdc::writer::save_layer(&read.emitted.layer, &tokens, &paths);
}

/// Imports USDA text, and when it imports without errors, saves it and
/// reads it back: the saved text must import without errors and save to
/// the same text again. A layer that also saves as a crate file must read
/// back from it to the same text.
pub fn usda_round_trip(data: &[u8]) {
    let Ok(source) = core::str::from_utf8(data) else {
        return;
    };
    let Some(first) = save_clean(source) else {
        return;
    };
    let second = save_clean(&first)
        .unwrap_or_else(|| panic!("saved text does not read back cleanly:\n{first}"));
    assert_eq!(first, second, "saving the saved layer changes it");

    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = layerstack_usda::read_usda(&first, LayerId(1), &tokens, &paths, &mut NoAssets);
    let Ok(crate_file) = layerstack_usdc::writer::save_layer(&read.emitted.layer, &tokens, &paths)
    else {
        return;
    };
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let reread =
        layerstack_usdc::read_usdc(&crate_file, LayerId(1), &tokens, &paths, &mut NoAssets)
            .unwrap_or_else(|error| {
                panic!("saved crate file does not read back ({error}):\n{first}")
            });
    let third = layerstack_usda::save::save_usda(&reread.layer, &tokens, &paths)
        .unwrap_or_else(|error| panic!("crate layer does not save ({error}):\n{first}"));
    assert_eq!(first, third, "the crate file reads back differently");
}

/// The saved text of `source`'s layer, when it imports without error
/// diagnostics and saves.
fn save_clean(source: &str) -> Option<String> {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
    let clean = !read.emitted.rejected
        && read
            .parse_diagnostics
            .iter()
            .chain(&read.lower_diagnostics)
            .chain(&read.emitted.diagnostics)
            .all(|diagnostic| diagnostic.severity != Severity::Error);
    if !clean {
        return None;
    }
    layerstack_usda::save::save_usda(&read.emitted.layer, &tokens, &paths).ok()
}

/// Reads a crate file eagerly, retained (decoding every retained value by
/// saving the layer), and field by field through `CrateFile`.
pub fn usdc(data: &[u8]) {
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let _ = layerstack_usdc::read_usdc(data, LayerId(1), &tokens, &paths, &mut NoAssets);

    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    if let Ok(read) =
        layerstack_usdc::read_usdc_lazy(Arc::from(data), LayerId(1), &tokens, &paths, &mut NoAssets)
    {
        // Saving reads every value, so every retained one decodes.
        let _ = layerstack_usda::save::save_usda(&read.assembled.layer, &tokens, &paths);
        let _ = read.values.stats();
    }

    let mut budget = layerstack_usdc::DecodeBudget::for_input(data.len());
    if let Ok(file) = layerstack_usdc::CrateFile::open(data, &mut budget) {
        for spec in file.specs() {
            let _ = spec.path();
            for field in spec.fields() {
                let _ = field.name();
                let _ = field.decode(&mut budget);
            }
        }
    }
}

/// Opens a package, eagerly and retained, and reads every member it lists
/// through a snapshot and through range reads.
pub fn usdz(data: &[u8]) {
    for arrays in [UsdcArrayLoading::Eager, UsdcArrayLoading::Retained] {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let options = UsdcReadOptions {
            arrays,
            decode_budget: None,
        };
        let _ = layerstack_usdz::read_usdz_with_options(
            data,
            LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            &[],
            options,
        );
    }

    let bytes = EncodedBytes::from(data.to_vec());
    if let Ok(snapshot) = PackageSnapshot::new(bytes.clone(), PackageLimits::default())
        && let Some(root) = snapshot.root_member().map(str::to_owned)
    {
        let _ = snapshot.layer(&root);
        let _ = snapshot.member(&root);
    }
    if let Ok(mut package) = RangePackage::open(bytes, RangeLimits::default()) {
        let members: Vec<String> = package.members().map(str::to_owned).collect();
        for member in &members {
            let _ = package.member(member);
            let _ = package.layer(member);
        }
    }
}

/// Parses the text forms the readers parse on their own: paths, path
/// expressions, variable expressions, asset paths with format arguments,
/// package paths, UDIM patterns and type names.
pub fn text(data: &[u8]) {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let tokens = TokenInterner::default();
    let paths = PathInterner::default();
    let _ = Path::parse_absolute(text, &tokens);
    let _ = PropertyPath::parse(text, &tokens, &paths);
    let _ = TargetPath::parse(text, &tokens, &paths);
    let _ = SpecPath::parse(text, &tokens, &paths);
    let _ = PathExpression::parse(text);
    let expression = VariableExpression::parse(text);
    let _ = expression.evaluate(&ExpressionVariables::new());
    if let Ok((path, args)) = FormatArgs::split(text) {
        let _ = args.identifier(path);
    }
    let _ = FormatArgs::strip(text);
    let _ = PackagePath::parse(text, 32);
    if let Some(pattern) = UdimPattern::parse(text) {
        let _ = pattern.tile_of(text);
    }
    let _ = ValueType::parse(text);
    let _ = layerstack::asset::normalize_asset_path(text);
    let _ = layerstack::asset::anchor_asset_path(text, "/fuzz/root.usda");
}

/// The composition limits a stage of fuzzed input composes within, as a
/// host composing untrusted input bounds it: scene description can compose
/// without end, and composition stops at a limit and says so
/// (`CompositionError::LimitExceeded`). The endless namespaces fuzzing
/// found (`regressions/stage/`) compose to them within the fuzzer's time
/// and memory for one input, sanitized; the layers of 64 KiB it reads that
/// compose an end reach neither.
const LIMITS: CompositionLimits = CompositionLimits {
    prim_index_nodes: Some(100_000),
    nodes_per_prim_index: Some(512),
    composed_depth: Some(32),
};

/// Opens the input as the root layer of a stage document, under strict
/// import and under recovery, then traverses the composed stage and reads
/// every property.
pub fn stage(data: &[u8]) {
    let identifier = root_identifier(data);
    for import_policy in [ImportPolicy::Strict, ImportPolicy::AllowRecovery] {
        let storage = OneSource {
            sources: BTreeMap::from([(identifier.to_owned(), data.to_vec())]),
        };
        let loading = LoadOptions {
            import_policy,
            ..LoadOptions::default()
        };
        let Ok(document) = StageDocument::open_in_with(
            storage,
            InMemoryStore::default(),
            identifier,
            StageOptions {
                limits: LIMITS,
                ..StageOptions::default()
            },
            loading,
        ) else {
            continue;
        };
        traverse(&document);
    }
}

/// Reads every property of every prim the stage's default traversal
/// visits, at the default time and at each authored sample.
fn traverse(document: &StageDocument<OneSource>) {
    let view = document.view();
    let _ = view.default_prim();
    for prim in view.traverse() {
        for property in prim.properties() {
            if let Some(attribute) = property.as_attribute() {
                let _ = attribute.try_get(Time::Default);
                for code in attribute.sample_times() {
                    let _ = attribute.try_get(Time::at(code));
                }
            } else if let Some(relationship) = property.as_relationship() {
                let _ = relationship.targets();
            }
        }
    }
}
