// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage flattening ([`Stage::flatten`]) against the stage it flattens and
//! against OpenUSD's `UsdStage::Flatten`.
//!
//! The cases are the scenes under `fixtures/flatten` (references with
//! offsets, payloads, inherits, variants, relocates, sublayers with
//! offsets, time samples, relationship targets across a reference and
//! instancing) and every supplemental composition fixture that Layerstack
//! composes exactly: all but those `composition_strict.rs` lists in `KNOWN`
//! and `SKIPPED` ([`not_exact`]), composed, like there, with the variant
//! fallback `{standin: [render]}`.
//!
//! Within the workspace, every case flattens without loss under the default
//! [`FlattenRequirements`]; the flattened layer is saved as USDA and as
//! USDC, each file is read back and composed on its own, and
//! [`Stage::verify_flattened`] must find it equivalent to the case at the
//! default time, at every sample time and at [`TIMES`]. The scenes under
//! `fixtures/flatten_losses` hold what a flattened layer cannot hold yet;
//! their reports and refusals are checked finding by finding.
//!
//! With a Python that imports OpenUSD's `pxr` (`LAYERSTACK_USD_PYTHON`, or
//! `python3`), `scripts/flatten_oracle.py` also flattens every case with
//! OpenUSD, and:
//!
//! - OpenUSD must read the USDA and USDC files of Layerstack's flattened
//!   layer as the same layer as its own flatten (every spec and field, see
//!   the script for the two things it leaves out);
//! - OpenUSD must compose each of those files as it composes its own
//!   flatten: every prim, property, metadata field, target and value at
//!   the default time, at every sample time and at [`TIMES`]. (Its own
//!   flatten composes as the case does, but for what flattening anchors
//!   or retimes: a metadata asset path the stage reports as authored, for
//!   one.)
//!
//! Without such a Python that check reports that it skipped and passes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use layerstack::stage::flatten::{
    AssetPaths, ExternalDependency, FindingKind, FlattenError, FlattenReport, FlattenRequirements,
    FlattenVerification, Loss, LossPolicy, MismatchKind, Preserved, Requirement, SkipReason,
    Transformation,
};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, ListOp, PathInterner,
    PropertyDefinition, ResolvedAsset, SchemaDefinition, SchemaKind, SchemaRegistry, Stage,
    StageOptions, TokenInterner, Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};

/// Numeric times every value is compared at, beside the default time.
const TIMES: &[f64] = &[-2.0, 0.0, 1.5, 3.0, 5.0, 8.0, 12.5, 20.0, 41.0];

/// The supplemental fixtures Layerstack does not compose exactly: every
/// fixture `composition_strict.rs` names in its `KNOWN` and `SKIPPED`
/// tables, read from its source so this list follows those tables.
fn not_exact() -> Vec<&'static str> {
    include_str!("composition_strict.rs")
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|literal| {
            literal.ends_with("_root")
                && literal
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .collect()
}

/// Unregistered metadata the cases author. The USDA reader types its value
/// from its text; a USDC file holds the text, as OpenUSD's text parser
/// records it (an `SdfUnregisteredValue` string), so it reads back from the
/// USDC as a string, and each must still differ that way. OpenUSD's own
/// flatten drops it, as it cannot set it on a spec; this flatten keeps it.
const UNREGISTERED: &[(&str, &str)] = &[("BasicVariantWithConnections_root", "avar")];

/// Cases where OpenUSD's composition differs from Layerstack's in what the
/// strict comparison does not cover, so the flattened layers differ too.
/// Each must still differ, so the table cannot go stale.
const COMPOSED_DIFFERENTLY: &[(&str, &str)] = &[
    (
        "BasicRelocateToAnimInterfaceAsNewRootPrim_root",
        "Layerstack keeps the target `/Model/Rig/PathRig/Path/Anim.track` of \
         `relationshipToPathAvar`, which OpenUSD drops",
    ),
    (
        "ErrorArcCycle_root",
        "OpenUSD populates `/AnotherParent/AnotherChild/AnotherChild`, named through the \
         reference the cycle cuts; `pcp.txt` does not list it and Layerstack does not \
         compose it",
    ),
    (
        "ErrorInconsistentProperties_root",
        "OpenUSD takes the variability of `/InconsistentPropertyType.x` from the weaker \
         relationship of that name, which composition discards as inconsistent",
    ),
    (
        "ErrorInvalidInstanceTargetPath_root",
        "Layerstack drops the connection from inside an instance to \
         `/ConnectionToLocalClass/Instance_2.y`, which OpenUSD keeps",
    ),
];

/// One scene to flatten.
struct Case {
    name: String,
    entry: PathBuf,
    /// Whether the scene is composed with the supplemental fixtures'
    /// variant fallback `{standin: [render]}`.
    fallbacks: bool,
}

/// The scenes in each directory of `fixtures/<dir>`, by name.
fn fixture_cases(dir: &str) -> Vec<Case> {
    let dir = workspace_root()
        .join("layerstack_conformance")
        .join("fixtures")
        .join(dir);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("flatten fixtures")
        .map(|entry| entry.expect("dir entry").file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| Case {
            entry: dir.join(&name).join("root.usda"),
            name,
            fallbacks: false,
        })
        .collect()
}

fn supplemental_cases() -> Vec<Case> {
    let not_exact = not_exact();
    assert!(not_exact.len() >= 3, "reads composition_strict.rs's tables");
    let dir = workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("composition")
        .join("tests")
        .join("assets");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("assets dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.join("pcp.txt").is_file())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|name| !not_exact.contains(&name.as_str()))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let entry =
                layerstack_conformance::pcp_txt::load_pcp_txt(&dir.join(&name).join("pcp.txt"))
                    .entry;
            Case {
                entry: dir.join(&name).join("usda").join(entry),
                name,
                fallbacks: true,
            }
        })
        .collect()
}

fn all_cases() -> Vec<Case> {
    let mut cases = fixture_cases("flatten");
    cases.extend(supplemental_cases());
    cases
}

fn options(store: &mut InMemoryStore, fallbacks: bool) -> StageOptions {
    let mut options = StageOptions::default();
    if fallbacks {
        let standin = store.tokens.intern("standin");
        let render = store.tokens.intern("render");
        options.variant_fallbacks = [(standin, vec![render])].into_iter().collect();
    }
    options
}

/// A case composed and flattened.
struct Flattened {
    store: InMemoryStore,
    stage: Stage,
    layer: Layer,
    report: FlattenReport,
}

/// Flattens `case` with its asset paths anchored and its schema
/// properties declared, as OpenUSD's flatten writes them, under otherwise
/// default requirements.
fn flatten(case: &Case) -> Result<Flattened, String> {
    flatten_with(case, FlattenRequirements::default()).map_err(|e| e.to_string())
}

/// Flattens `case` under `requirements`, anchoring its asset paths to the
/// files its layers are read from.
fn flatten_with(
    case: &Case,
    requirements: FlattenRequirements<'_>,
) -> Result<Flattened, FlattenError> {
    let mut loaded = load_entry_usda(&case.entry);
    let options = StageOptions {
        schemas: Some(Arc::new(schemas(&mut loaded.store))),
        ..options(&mut loaded.store, case.fallbacks)
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let id = LayerId(loaded.store.layers.keys().map(|id| id.0).max().unwrap_or(0) + 1);
    let directory = case_directory(case);
    let locations = Locations {
        layers: loaded
            .layer_names
            .iter()
            .map(|(&id, name)| (id, directory.join(name).to_string_lossy().into_owned()))
            .collect(),
    };
    let requirements = FlattenRequirements {
        asset_paths: AssetPaths::Anchored(&locations),
        ..requirements
    };
    let flat = stage.flatten(&mut loaded.store, loaded.root_layer, id, &requirements)?;
    Ok(Flattened {
        store: loaded.store,
        stage,
        layer: flat.layer,
        report: flat.report,
    })
}

/// The `UsdGeom` schema properties the cases author, as OpenUSD 26.8
/// defines them (`pxr/usd/usdGeom/schema.usda`): `Imageable`'s `visibility`
/// and uniform `purpose`, and `Xformable`'s uniform `xformOpOrder`.
fn schemas(store: &mut InMemoryStore) -> SchemaRegistry {
    let mut token = |name: &str| store.tokens.intern(name);
    let (imageable, xformable, xform) = (token("Imageable"), token("Xformable"), token("Xform"));
    let (visibility, purpose, order) =
        (token("visibility"), token("purpose"), token("xformOpOrder"));
    let (inherited, default) = (token("inherited"), token("default"));
    let property = |name, fallback| PropertyDefinition::attribute(name).with_fallback(fallback);
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::new(imageable, SchemaKind::AbstractTyped)
                .with_property(property(visibility, Value::Token(inherited)))
                .with_property(property(purpose, Value::Token(default)).uniform()),
        )
        .register(
            SchemaDefinition::new(xformable, SchemaKind::AbstractTyped)
                .with_parent(imageable)
                .with_property(property(order, Value::Array(Vec::new())).uniform()),
        )
        .register(SchemaDefinition::typed(xform).with_parent(xformable));
    builder.build(&mut store.tokens)
}

/// The directory of a case's entry layer, canonical as OpenUSD resolves
/// it; under WASI, whose preopened directories have no canonical path, as
/// found. On Windows the verbatim prefix `canonicalize` adds (`\\?\`) is
/// dropped, as OpenUSD's paths have none.
fn case_directory(case: &Case) -> PathBuf {
    let directory = case.entry.parent().expect("a directory");
    if cfg!(target_os = "wasi") {
        return directory.to_path_buf();
    }
    let canonical = std::fs::canonicalize(directory).unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

/// Where each layer of a case is read from, for anchoring its asset paths
/// as OpenUSD's default resolver anchors them.
struct Locations {
    layers: std::collections::HashMap<LayerId, String>,
}

impl AssetResolver for Locations {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layers.get(&id).map(String::as_str)
    }

    /// A search path (`textures/bark.png`) is anchored only when the asset
    /// exists beside its layer (`ArDefaultResolver::_CreateIdentifier`).
    fn anchor_asset_path(&self, asset_path: &str, anchor: LayerId) -> Option<String> {
        use layerstack::asset::{anchor_asset_path, normalize_asset_path};
        let location = self.resolved_path(anchor)?;
        let anchored = anchor_asset_path(asset_path, location)?;
        let search = anchored == normalize_asset_path(asset_path)
            && !asset_path.starts_with('/')
            && !asset_path.contains(':');
        if search
            && let Some(beside) = anchor_asset_path(&format!("./{asset_path}"), location)
            && Path::new(&beside).exists()
        {
            return Some(beside);
        }
        Some(anchored)
    }
}

/// Resolves no asset: a flattened layer names none.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Reads `text` back into `store` as layer `id`.
fn read_usda(store: &mut InMemoryStore, id: LayerId, text: &str) -> Result<(), String> {
    let parsed = layerstack_usda::parser::parse(text);
    if !parsed.diagnostics.is_empty() {
        return Err(format!("{:?}", parsed.diagnostics));
    }
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        id,
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    if !result.diagnostics.is_empty() {
        return Err(format!("{:?}", result.diagnostics));
    }
    store.insert_layer(result.layer);
    Ok(())
}

/// Reads `bytes` back into `store` as layer `id`.
fn read_usdc(store: &mut InMemoryStore, id: LayerId, bytes: &[u8]) -> Result<(), String> {
    let result = layerstack_usdc::read_usdc(
        bytes,
        id,
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .map_err(|e| format!("{e:?}"))?;
    store.insert_layer(result.layer);
    Ok(())
}

/// The paths of the arcs left in `layer` other than the internal
/// references of instances to their prototypes.
fn leftover_arcs(layer: &Layer, store: &InMemoryStore) -> Vec<String> {
    let mut found = Vec::new();
    if !layer.sublayers.is_empty() || !layer.relocates.is_empty() {
        found.push("/ (sublayers or relocates)".into());
    }
    for (&path, spec) in &layer.prims {
        let references = spec
            .references
            .explicit
            .iter()
            .flatten()
            .chain(&spec.references.prepend)
            .chain(&spec.references.append);
        let internal = references
            .clone()
            .all(|r| r.asset.is_none() && r.layer == layer.id);
        let prototype = references.count() <= 1
            && spec.instanceable == Some(true)
            && internal
            && spec.authored_children.is_empty();
        let arcs = spec.payloads != ListOp::default()
            || spec.inherits != ListOp::default()
            || spec.specializes != ListOp::default()
            || !spec.variant_sets.is_empty()
            || !spec.variant_selections.is_empty()
            || (spec.references != ListOp::default() && !prototype);
        if arcs {
            found.push(store.paths.display(path, &store.tokens));
        }
    }
    found.extend(
        layer
            .variant_prims
            .keys()
            .map(|&path| store.paths.display(path, &store.tokens)),
    );
    found.sort();
    found
}

/// The first mismatches of a verification, as text.
fn mismatches(verification: &FlattenVerification) -> String {
    verification
        .mismatches
        .iter()
        .take(5)
        .map(|m| {
            format!(
                "{} {:?}\n--- stage\n{}\n--- flattened\n{}",
                m.path, m.kind, m.expected, m.found
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Composes layer `id` of `flat.store` on its own and verifies it against
/// the case, returning the verification.
fn verify(flat: &mut Flattened, id: LayerId) -> Result<FlattenVerification, String> {
    let stage = Stage::compose(&mut flat.store, id, StageOptions::default());
    let errors = stage.composition_errors();
    if !errors.is_empty() {
        return Err(format!("composition errors {errors:?}"));
    }
    Ok(flat
        .stage
        .verify_flattened(&stage, &flat.store, &flat.report, TIMES))
}

/// Every case flattens without loss under the default requirements; its
/// flattened layer holds no arcs, saves as USDA and USDC, and the layer and
/// each saved file, read back and composed on its own, verify against the
/// case ([`Stage::verify_flattened`]).
#[test]
fn flattened_layers_compose_the_flattened_stage() {
    let mut failures = Vec::new();
    let mut count = 0;
    let mut values = 0;
    let mut preserved = Preserved::default();
    let mut findings = 0;
    for case in all_cases() {
        let name = &case.name;
        let mut flat = match flatten(&case) {
            Ok(flat) => flat,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        count += 1;
        assert!(flat.report.is_lossless(), "the default refuses any loss");
        preserved.prims += flat.report.preserved.prims;
        preserved.properties += flat.report.preserved.properties;
        preserved.time_samples += flat.report.preserved.time_samples;
        findings += flat.report.findings.len();
        let arcs = leftover_arcs(&flat.layer, &flat.store);
        if !arcs.is_empty() {
            failures.push(format!("{name}: arcs left at {arcs:?}"));
        }
        let (usda, usdc) = save(&flat.layer, &flat.store);
        for error in [usda.as_ref().err(), usdc.as_ref().err()]
            .into_iter()
            .flatten()
        {
            failures.push(format!("{name}: cannot save: {error}"));
        }
        let base = flat.layer.id.0;
        flat.store.insert_layer(flat.layer.clone());
        let mut reads = vec![("layer", flat.layer.id, Ok(()))];
        if let Ok(text) = &usda {
            let id = LayerId(base + 1);
            reads.push(("usda", id, read_usda(&mut flat.store, id, text)));
        }
        if let Ok(bytes) = &usdc {
            let id = LayerId(base + 2);
            reads.push(("usdc", id, read_usdc(&mut flat.store, id, bytes)));
        }
        for (format, id, read) in reads {
            if let Err(e) = read {
                failures.push(format!("{name}: cannot read the {format} back: {e}"));
                continue;
            }
            let unregistered: Vec<&str> = UNREGISTERED
                .iter()
                .filter(|(case, _)| format == "usdc" && case == name)
                .map(|(_, field)| *field)
                .collect();
            let verified = verify(&mut flat, id).map(|mut verification| {
                let before = verification.mismatches.len();
                verification.mismatches.retain(|m| {
                    !matches!(&m.kind, MismatchKind::PropertyMetadata { field }
                        if unregistered.contains(&field.as_str())
                            && m.found.starts_with("String("))
                });
                if !unregistered.is_empty() && before == verification.mismatches.len() {
                    failures.push(format!("{name}: {unregistered:?} now reads back typed"));
                }
                verification
            });
            match verified {
                Err(e) => failures.push(format!("{name} ({format}): {e}")),
                Ok(verification) if !verification.is_equivalent() => failures.push(format!(
                    "{name} ({format}): composes differently\n{}",
                    mismatches(&verification)
                )),
                Ok(verification) => values += verification.scope.values,
            }
        }
    }
    eprintln!(
        "flattened {count} cases: {} prims, {} properties and {} time samples exact, \
         {findings} findings; verified {values} values",
        preserved.prims, preserved.properties, preserved.time_samples
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Each scene under `fixtures/flatten_losses` is refused under the default
/// requirements with every unmet requirement listed, flattens once its
/// losses are accepted, reports exactly what it lost, and verifies
/// everywhere else.
#[test]
fn known_losses_are_reported_exactly() {
    type Unmet = (Requirement, &'static str, Loss, &'static str);
    let expected: &[(&str, &[Unmet])] = &[
        (
            "clips",
            &[(
                Requirement::ExactAnimation,
                "/Hedge",
                Loss::ValueClips,
                "/Hedge",
            )],
        ),
        (
            "rates",
            &[(
                Requirement::ExactAnimation,
                "/Hedge.sway",
                Loss::TimeCodesPerSecond,
                "/Hedge.sway",
            )],
        ),
    ];
    let cases = fixture_cases("flatten_losses");
    assert_eq!(
        cases.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        expected.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "every scene has its expected losses"
    );
    for (case, (name, unmet)) in cases.iter().zip(expected) {
        let refusal = match flatten_with(case, FlattenRequirements::default()) {
            Err(FlattenError::Refused(refusal)) => refusal,
            other => panic!("{name}: expected a refusal, got {:?}", other.err()),
        };
        let found: Vec<(Requirement, String, Loss, String)> = refusal
            .unmet
            .iter()
            .map(|unmet| {
                let FindingKind::Lost(loss) = unmet.finding.kind else {
                    panic!("{name}: an unmet requirement is a loss");
                };
                let source = unmet.finding.source.as_ref().expect("a source");
                (
                    unmet.requirement,
                    unmet.finding.path.to_string(),
                    loss,
                    source.spec.clone(),
                )
            })
            .collect();
        let want: Vec<(Requirement, String, Loss, String)> = unmet
            .iter()
            .map(|&(r, path, loss, spec)| (r, path.into(), loss, spec.into()))
            .collect();
        assert_eq!(found, want, "{name}");

        // Accepting the losses flattens; the report lists the same losses.
        let lenient = FlattenRequirements::default().accepting(&refusal.unmet);
        assert_eq!(lenient.losses, LossPolicy::RefuseRequired);
        let mut flat = flatten_with(case, lenient).expect("the losses are accepted");
        let lost: Vec<(String, FindingKind)> = flat
            .report
            .lost()
            .map(|f| (f.path.to_string(), f.kind.clone()))
            .collect();
        let want_lost: Vec<(String, FindingKind)> = unmet
            .iter()
            .map(|&(_, path, loss, _)| (path.into(), FindingKind::Lost(loss)))
            .collect();
        assert_eq!(lost, want_lost, "{name}");

        let id = flat.layer.id;
        flat.store.insert_layer(flat.layer.clone());
        let verification = verify(&mut flat, id).expect("composes");
        assert!(
            verification.is_equivalent(),
            "{name}: {}",
            mismatches(&verification)
        );
        let skipped: Vec<(String, SkipReason)> = verification
            .scope
            .skipped
            .iter()
            .map(|s| (s.path.to_string(), s.reason))
            .collect();
        let want_skipped: Vec<(String, SkipReason)> = unmet
            .iter()
            .map(|&(_, path, loss, _)| (path.into(), SkipReason::Lost(loss)))
            .collect();
        assert_eq!(skipped, want_skipped, "{name}: only the losses are skipped");
    }
}

/// The asset paths of `fixtures/flatten/asset_paths` are anchored to the
/// layer that authors each, and the report lists each anchoring and each
/// asset the layer still names.
#[test]
fn asset_paths_are_anchored_and_reported() {
    let case = fixture_cases("flatten")
        .into_iter()
        .find(|case| case.name == "asset_paths")
        .expect("the asset_paths scene");
    let flat = flatten(&case).expect("flattens");
    // Anchored paths use `/` separators, as `TfNormPath` writes them.
    let directory = case_directory(&case).to_string_lossy().replace('\\', "/");
    let local = |path: &str| path.replace(directory.as_str(), "<dir>");
    let mut anchored: Vec<String> = flat
        .report
        .transformed()
        .filter_map(|finding| match &finding.kind {
            FindingKind::Transformed(Transformation::AssetPathAnchored { authored, anchored }) => {
                Some(format!(
                    "{} {authored} -> {}",
                    finding.path,
                    local(anchored)
                ))
            }
            _ => None,
        })
        .collect();
    anchored.sort();
    assert_eq!(
        anchored,
        [
            "/ ./maps/grove.png -> <dir>/maps/grove.png",
            "/Grove ./moss.png -> <dir>/moss.png",
            "/Grove/Birch ./birch.usda -> <dir>/parts/birch.usda",
            "/Grove/Birch.bark ./textures/bark.png -> <dir>/parts/textures/bark.png",
            "/Grove/Birch.glow ./glow_override.png -> <dir>/glow_override.png",
            "/Grove/Birch.glow ./mask.png -> <dir>/parts/mask.png",
            "/Grove/Birch.leaves ../shared/leaf.png -> <dir>/shared/leaf.png",
            "/Grove/Birch.lichen textures/lichen.png -> <dir>/parts/textures/lichen.png",
            "/Grove/Birch.shadow ./shadow0.png -> <dir>/parts/shadow0.png",
            "/Grove/Birch.shadow ./shadow5.png -> <dir>/parts/shadow5.png",
        ]
    );
    let mut external: Vec<String> = flat
        .report
        .external()
        .map(|finding| {
            let FindingKind::External(ExternalDependency::AssetPath(asset)) = &finding.kind else {
                unreachable!("an external finding")
            };
            format!("{} {}", finding.path, local(asset))
        })
        .filter(|entry| !entry.contains("<dir>"))
        .collect();
    external.sort();
    assert_eq!(
        external,
        [
            "/Grove.sky https://example.com/sky.hdr",
            "/Grove/Birch.fungus textures/fungus.png",
            "/Grove/Birch.leaves /opt/assets/leaf.png",
        ],
        "absolute paths, URIs and search paths with nothing beside their layer stay"
    );
    assert!(flat.report.is_lossless());
}

/// The properties of `fixtures/flatten/schemas` are declared as OpenUSD's
/// flatten declares them, and the report says which differ from the
/// stage's declarations.
#[test]
fn declarations_follow_the_schema_and_the_weakest_opinion() {
    let case = fixture_cases("flatten")
        .into_iter()
        .find(|case| case.name == "schemas")
        .expect("the schemas scene");
    let flat = flatten(&case).expect("flattens");
    let declared: Vec<String> = flat
        .report
        .transformed()
        .filter(|finding| {
            matches!(
                finding.kind,
                FindingKind::Transformed(
                    Transformation::CustomFromWeakestOpinion { .. }
                        | Transformation::DefinedBySchema { .. }
                )
            )
        })
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        declared,
        [
            "/Grove/Tree.height: transformed: custom = false, from the weakest opinion \
             (layer 1, /Grove/Tree.height)",
            "/Grove/Tree.purpose: transformed: declared by its schema (Uniform, not custom) \
             (layer 2, /Tree.purpose)",
            "/Grove/Tree.visibility: transformed: declared by its schema (Varying, not custom) \
             (layer 2, /Tree.visibility)",
            "/Grove/Tree.xformOpOrder: transformed: declared by its schema (Uniform, not \
             custom) (layer 1, /Grove/Tree.xformOpOrder)",
        ]
    );
}

/// The flattened layer saved as USDA and as USDC.
fn save(layer: &Layer, store: &InMemoryStore) -> (Result<String, String>, Result<Vec<u8>, String>) {
    let (tokens, paths) = (&store.tokens, &store.paths);
    (
        layerstack_usda::save::save_usda(layer, tokens, paths).map_err(|e| e.to_string()),
        layerstack_usdc::writer::save_layer(layer, tokens, paths).map_err(|e| e.to_string()),
    )
}

/// A per-test directory under Cargo's integration-test scratch space.
///
/// Under WASI only the crate directory and its parent are preopened (see
/// `.cargo/config.toml`), so the path is made relative to them there.
fn scratch_dir(name: &str) -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    base.join(format!("flatten-{name}"))
}

fn oracle_script() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/flatten_oracle.py")
}

/// A Python that imports OpenUSD's `pxr`, with the OpenUSD version it
/// reports.
fn usd_python() -> Option<(String, String)> {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python)
        .arg(oracle_script())
        .arg("version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some((python, String::from_utf8_lossy(&out.stdout).trim().into()))
}

/// Every case flattens as OpenUSD flattens it, and every saved flattened
/// layer composes in OpenUSD as the case does.
#[test]
fn flatten_matches_openusd() {
    let Some((python, version)) = usd_python() else {
        eprintln!("skipped: no Python with OpenUSD's pxr (set LAYERSTACK_USD_PYTHON)");
        return;
    };
    eprintln!("OpenUSD {version} via {python}");
    let dir = scratch_dir("oracle");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let absolute = |path: &Path| std::fs::canonicalize(path).unwrap();

    let mut failures = Vec::new();
    let mut jobs = Vec::new();
    let mut names = Vec::new();
    for case in all_cases() {
        let name = &case.name;
        let flat = match flatten(&case) {
            Ok(flat) => flat,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let usda = dir.join(format!("{name}.usda"));
        let usdc = dir.join(format!("{name}.usdc"));
        match save(&flat.layer, &flat.store) {
            (Ok(text), Ok(bytes)) => {
                std::fs::write(&usda, text).unwrap();
                std::fs::write(&usdc, bytes).unwrap();
            }
            // `flattened_layers_compose_the_flattened_stage` reports why.
            _ => continue,
        }
        let fallbacks = if case.fallbacks {
            serde_json::json!({"standin": ["render"]})
        } else {
            serde_json::json!({})
        };
        jobs.push(serde_json::json!({
            "root": absolute(&case.entry),
            "flattened": [absolute(&usda), absolute(&usdc)],
            "export": dir.join(format!("{name}.openusd.usda")),
            "times": TIMES,
            "fallbacks": fallbacks,
        }));
        names.push(name.clone());
    }
    let jobs_path = dir.join("jobs.json");
    let results_path = dir.join("results.json");
    std::fs::write(&jobs_path, serde_json::to_string(&jobs).unwrap()).unwrap();
    let out = Command::new(&python)
        .arg(oracle_script())
        .arg("batch")
        .arg(&jobs_path)
        .arg(&results_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "flatten_oracle.py failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let results: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&results_path).unwrap()).unwrap();
    for (name, result) in names.iter().zip(&results) {
        let mut differences = Vec::new();
        for (i, format) in ["usda", "usdc"].iter().enumerate() {
            if result["ours"][i] != result["openusd"] {
                differences.push(format!(
                    "{name} ({format}): OpenUSD reads a different layer than its own flatten\n{}",
                    json_difference(&result["openusd"], &result["ours"][i])
                ));
            }
            // OpenUSD's flatten drops unregistered metadata, which it cannot
            // set on a spec; this flatten keeps it.
            let unregistered: Vec<&str> = UNREGISTERED
                .iter()
                .filter(|(case, _)| case == name)
                .map(|(_, field)| *field)
                .collect();
            let reopened = without(&result["reopened"][i], &unregistered);
            let openusd = without(&result["openusd_reopened"], &unregistered);
            if reopened != openusd {
                differences.push(format!(
                    "{name} ({format}): OpenUSD composes it differently from its own flatten\n{}",
                    json_difference(&openusd, &reopened)
                ));
            }
        }
        let known = COMPOSED_DIFFERENTLY.iter().any(|(case, _)| case == name);
        match (known, differences.is_empty()) {
            (false, _) => failures.extend(differences),
            (true, true) => failures.push(format!(
                "{name}: now matches OpenUSD; remove it from COMPOSED_DIFFERENTLY"
            )),
            (true, false) => {}
        }
    }
    eprintln!("compared {} cases with OpenUSD {version}", names.len());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A stage dump without the property metadata fields `fields`.
fn without(dump: &serde_json::Value, fields: &[&str]) -> serde_json::Value {
    let mut dump = dump.clone();
    for prim in dump
        .as_object_mut()
        .into_iter()
        .flat_map(|prims| prims.values_mut())
    {
        let properties = prim.get_mut("properties").and_then(|p| p.as_object_mut());
        for property in properties.into_iter().flat_map(|p| p.values_mut()) {
            let Some(property) = property.as_object_mut() else {
                continue;
            };
            if let Some(metadata) = property.get_mut("metadata").and_then(|m| m.as_object_mut()) {
                for field in fields {
                    metadata.remove(*field);
                }
            }
        }
    }
    dump
}

/// The first place two JSON values differ, as `path: expected / actual`.
fn json_difference(want: &serde_json::Value, got: &serde_json::Value) -> String {
    fn walk(want: &serde_json::Value, got: &serde_json::Value, at: &str) -> Option<String> {
        use serde_json::Value as J;
        match (want, got) {
            (J::Object(a), J::Object(b)) => {
                let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
                keys.sort();
                keys.dedup();
                keys.into_iter().find_map(|k| {
                    let (x, y) = (a.get(k), b.get(k));
                    match (x, y) {
                        (Some(x), Some(y)) => walk(x, y, &format!("{at}/{k}")),
                        _ => Some(format!("{at}/{k}: {x:?} / {y:?}")),
                    }
                })
            }
            _ if want == got => None,
            _ => Some(format!("{at}: {want} / {got}")),
        }
    }
    walk(want, got, "").unwrap_or_default()
}
