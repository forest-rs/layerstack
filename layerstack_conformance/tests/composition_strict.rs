// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Strict, ordered composition conformance against OpenUSD's `pcp.txt`.
//!
//! # Oracle
//!
//! Every fixture under `core-spec-supplemental-release_dec2025/composition/
//! tests/assets` carries a `pcp.txt`: the output of OpenUSD's
//! `pxr/usd/pcp/testenv/testPcpCompositionResults.py --usd <entry>`
//! (see `pcpConverter.py`), generated with OpenUSD 0.25.8 (the
//! `pxrInternal_v0_25_8` namespace in their diagnostics) and composed with
//! that script's default variant fallback `{'standin': ['render']}`.
//! Regenerating every file with OpenUSD 26.3 (the `usd-core` wheel) and the
//! same script reproduces every prim stack and property stack exactly; the
//! vendored files differ only in the appended stderr diagnostics.
//! [`layerstack_conformance::pcp_txt`] parses them without losing order or
//! repeated sites, which the derived `pcp.json` files cannot represent.
//!
//! # What is compared
//!
//! For every fixture (not only those the membership harness in
//! `composition_pcp.rs` covers), with exact ordered equality:
//!
//! - the root layer stack;
//! - the populated prim namespace: every prim the oracle composes, and no
//!   other;
//! - each composed prim's full prim stack, [`Stage::explain_prim`], against
//!   `PcpPrimIndex::GetPrimStack()`, repeats included;
//! - each property stack, [`Stage::explain_property_path`], against
//!   `PcpPropertyIndex::GetPropertyStack()`;
//! - each resolved scalar and its winning spec, via
//!   [`check_scalar_values`]: the expected winner is the strongest oracle
//!   site whose layer authors a default, read from the ingested layer data
//!   (see [`derive_scalars`]). Checked against OpenUSD 26.3's
//!   `UsdAttribute::Get()`, these derived expectations agree except where
//!   ingestion stores a `bool` as an integer.
//!
//! Every fixture is composed with the same variant fallbacks as `pcp.txt`
//! ([`StageOptions::variant_fallbacks`]).
//!
//! [`KNOWN`] records every fixture that does not match, with the exact shape
//! of its mismatch (counts and [`Diff`] kinds), its [`Cause`]s and a
//! one-line reason. The test fails when a fixture starts or stops matching
//! or when a known mismatch changes shape, so the table cannot go stale.
//!
//! # Supported
//!
//! Exact prim stacks, property stacks and values for sublayer stacks
//! (including duplicate sublayers, cycles and time offsets), local opinions,
//! references, payloads and inherits nested to any depth, one node per arc
//! occurrence (a site reached twice contributes twice), ranked by walking
//! each prim's composition graph ([`Stage::explain_prim_graph`]), inherits
//! implied into every stronger layer stack on the way to the root, internal
//! references and payloads authored anywhere in a layer stack, the arcs
//! authored on the ancestors of subroot arc targets, list-edited arcs and
//! target paths, specializes propagated to the root of the graph and
//! implied like inherits, relocates of the stage's and of referenced layer
//! stacks (relocated prims composed at their targets beneath relocate
//! nodes, their sources prohibited), asset paths authored as variable
//! expressions, evaluated with the expression variables of the referencing
//! layer stacks, variant sets nested in other branches, each branch a
//! variant spec of its own, and variant selections, including fallbacks
//! and those a specialized class authors, that do not depend on the
//! features below.
//!
//! # Not supported
//!
//! - Relocates combined with implied classes ([`Cause::Relocates`]).
//! - Implied classes in population and variant selection
//!   ([`Cause::ImpliedClasses`]).
//! - Variant selections of the sites ancestral arcs reach, made before the
//!   prim's index is complete ([`Cause::AncestralArcs`]).
//!
//! The test prints the per-cause tally and the list of exact matches.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use layerstack::property::get_property;
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{
    LayerId, LayerStack, LayerStore, PropertyEntry, PropertyPath, SpecComponent, SpecPath, Stage,
    StageOptions, TokenId, Value,
};
use layerstack_conformance::{
    pcp_txt::{PcpSite, PcpTxt, load_pcp_txt},
    scalar::{ScalarExpectation, check_scalar_values},
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

/// How one ordered stack differs from the oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Diff {
    /// The root layer stack differs.
    LayerStack,
    /// The oracle composes a prim that Layerstack does not.
    MissingPrim,
    /// Layerstack populates a prim the oracle does not compose.
    ExtraPrim,
    /// A site in the oracle stack is absent from Layerstack's.
    MissingSite,
    /// Layerstack's stack has a site the oracle's lacks.
    ExtraSite,
    /// Same sites, but Layerstack repeats one more often.
    ExtraRepeat,
    /// Same sites, but the oracle repeats one more often.
    MissingRepeat,
    /// The shared sites appear in a different order.
    Order,
}

/// Classifies how `actual` differs from `expected`; empty when equal.
fn diff_stacks(expected: &[String], actual: &[String]) -> BTreeSet<Diff> {
    let mut diffs = BTreeSet::new();
    if expected == actual {
        return diffs;
    }
    let count = |stack: &[String]| {
        let mut counts = BTreeMap::<String, usize>::new();
        for site in stack {
            *counts.entry(site.clone()).or_default() += 1;
        }
        counts
    };
    let (want, have) = (count(expected), count(actual));
    for (site, n) in &want {
        match have.get(site) {
            None => {
                diffs.insert(Diff::MissingSite);
            }
            Some(m) if m < n => {
                diffs.insert(Diff::MissingRepeat);
            }
            _ => {}
        }
    }
    for (site, n) in &have {
        match want.get(site) {
            None => {
                diffs.insert(Diff::ExtraSite);
            }
            Some(m) if m < n => {
                diffs.insert(Diff::ExtraRepeat);
            }
            _ => {}
        }
    }
    // Compare the order of first occurrences of the shared sites.
    let first = |stack: &[String], other: &BTreeMap<String, usize>| {
        let mut seen = BTreeSet::new();
        stack
            .iter()
            .filter(|site| other.contains_key(*site) && seen.insert(site.as_str()))
            .cloned()
            .collect::<Vec<_>>()
    };
    if first(expected, &have) != first(actual, &want) || diffs.is_empty() {
        diffs.insert(Diff::Order);
    }
    diffs
}

#[test]
fn diff_stacks_classifies_differences() {
    let stack = |sites: &[&str]| sites.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let diamond = stack(&["r /Root", "a /A", "c /C", "b /B", "c /C"]);
    assert!(diff_stacks(&diamond, &diamond).is_empty());
    assert_eq!(
        diff_stacks(&diamond, &stack(&["r /Root", "a /A", "b /B", "c /C"])),
        BTreeSet::from([Diff::MissingRepeat, Diff::Order])
    );
    assert_eq!(
        diff_stacks(&stack(&["r /A", "m /A"]), &stack(&["r /A", "m /A", "m /A"])),
        BTreeSet::from([Diff::ExtraRepeat])
    );
    assert_eq!(
        diff_stacks(&stack(&["r /A", "m /B"]), &stack(&["m /B", "x /C"])),
        BTreeSet::from([Diff::MissingSite, Diff::ExtraSite])
    );
    // Same multiset, different order.
    assert_eq!(
        diff_stacks(
            &stack(&["m /A", "m /A", "c /C"]),
            &stack(&["m /A", "c /C", "m /A"])
        ),
        BTreeSet::from([Diff::Order])
    );
}

/// Observed strict mismatches for one fixture.
#[derive(Clone, Debug, Default)]
struct Observed {
    /// Prims whose full prim stack differs, including missing and extra
    /// prims.
    prims: usize,
    /// Properties whose property stack differs.
    props: usize,
    /// Properties whose resolved scalar or winning opinion differs.
    values: usize,
    /// Union of stack differences over prim and property stacks.
    diffs: BTreeSet<Diff>,
    /// Human-readable detail for failure output.
    detail: Vec<String>,
    /// Number of prims the oracle composes.
    oracle_prims: usize,
}

fn render_site(loaded: &LoadedStage, layer: LayerId, spec: &SpecPath) -> String {
    format!(
        "{} {}",
        loaded.layer_names.get(&layer).cloned().unwrap_or_default(),
        spec.display(&loaded.store.tokens)
    )
}

fn render_oracle(stack: &[PcpSite]) -> Vec<String> {
    stack
        .iter()
        .map(|site| format!("{} {}", site.layer, site.path))
        .collect()
}

/// What a layer authors for one property at one oracle spec path.
enum Authored {
    /// The spec shape is not one this lookup reads; stop deriving.
    Unknown,
    /// The spec exists but authors no scalar default for the property.
    NoDefault,
    /// The spec authors this default.
    Default(Value),
}

fn property_default(properties: &[PropertyEntry], name: TokenId) -> Authored {
    // A declaration without a default (`custom double x`) authors no value.
    match get_property(properties, name).and_then(|spec| spec.default.as_ref()) {
        Some(value) => Authored::Default(value.clone()),
        None => Authored::NoDefault,
    }
}

/// Reads the default authored at `spec` in `layer`, straight from the
/// ingested layer data (independent of composition).
///
/// Handles plain prim specs, `/Host{set=variant}` and
/// `/Host{set=variant}Child` specs; other variant shapes are `Unknown`. A
/// branch child is read from its own prim spec for that branch
/// ([`layerstack::Layer::prim_spec_in`]).
fn authored_default(loaded: &LoadedStage, layer: LayerId, spec: &SpecPath) -> Authored {
    let Some(layer) = loaded.store.layer(layer) else {
        return Authored::Unknown;
    };
    let Some(name) = spec.property() else {
        return Authored::Unknown;
    };
    let components = spec.components();
    let Some(at) = components
        .iter()
        .position(|c| matches!(c, SpecComponent::VariantSelection { .. }))
    else {
        return layer
            .prims
            .get(&spec.prim_path())
            .map_or(Authored::Unknown, |prim| {
                property_default(&prim.properties, name)
            });
    };
    let SpecComponent::VariantSelection { set, variant } = components[at] else {
        return Authored::Unknown;
    };
    let host: Vec<TokenId> = components[..at]
        .iter()
        .filter_map(|c| match c {
            SpecComponent::Prim(token) => Some(*token),
            SpecComponent::VariantSelection { .. } => None,
        })
        .collect();
    let Some(host_path) = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root().join(&host))
    else {
        return Authored::Unknown;
    };
    let Some(branch) = layer
        .prims
        .get(&host_path)
        .and_then(|prim| prim.variant_spec(&[(set, variant)]))
    else {
        return Authored::Unknown;
    };
    match &components[at + 1..] {
        [] => property_default(&branch.properties, name),
        [SpecComponent::Prim(child)] => {
            let site = VariantSelectionSite {
                host_path,
                set,
                variant,
            };
            let child_path = layerstack::Path::root().join(&host).join(&[*child]);
            loaded
                .store
                .paths
                .lookup(&child_path)
                .and_then(|path| layer.prim_spec_in(path, &[site]))
                .map_or(Authored::Unknown, |spec| {
                    property_default(&spec.properties, name)
                })
        }
        _ => Authored::Unknown,
    }
}

/// Derives the expected scalar winner for each oracle property stack.
///
/// The winner is the strongest oracle site whose layer authors a scalar
/// default for the property, read from the ingested layer data rather than
/// from Layerstack's composed opinions, so a missing or misordered opinion
/// shows up as a wrong value. A property is skipped when a stronger site's
/// spec shape cannot be read (see [`authored_default`]) or authors a block.
fn derive_scalars(
    loaded: &mut LoadedStage,
    oracle: &PcpTxt,
) -> Vec<(String, Value, String, String)> {
    let by_name: BTreeMap<String, LayerId> = loaded
        .layer_names
        .iter()
        .map(|(id, name)| (name.clone(), *id))
        .collect();
    let mut out = Vec::new();
    for prim in &oracle.prims {
        'props: for prop in &prim.property_stacks {
            for site in &prop.stack {
                let Some(&layer) = by_name.get(&site.layer) else {
                    continue 'props;
                };
                let Ok(spec) = SpecPath::parse(
                    &site.path,
                    &mut loaded.store.tokens,
                    &mut loaded.store.paths,
                ) else {
                    continue 'props;
                };
                match authored_default(loaded, layer, &spec) {
                    Authored::Unknown | Authored::Default(Value::Blocked) => continue 'props,
                    Authored::NoDefault => {}
                    Authored::Default(value) => {
                        out.push((
                            prop.property.clone(),
                            value,
                            site.layer.clone(),
                            site.path.clone(),
                        ));
                        continue 'props;
                    }
                }
            }
        }
    }
    out
}

fn observe(name: &str) -> Observed {
    let dir = assets_dir().join(name);
    let oracle = load_pcp_txt(&dir.join("pcp.txt"));
    let mut loaded = load_entry_usda(&dir.join("usda").join(&oracle.entry));
    let mut observed = Observed {
        oracle_prims: oracle.prims.len(),
        ..Observed::default()
    };

    let layer_stack: Vec<String> = LayerStack::gather(&loaded.store, loaded.root_layer)
        .layers
        .iter()
        .map(|id| loaded.layer_names.get(id).cloned().unwrap_or_default())
        .collect();
    if layer_stack != oracle.layer_stack {
        observed.diffs.insert(Diff::LayerStack);
        observed.detail.push(format!(
            "layer stack\n    expected {:?}\n    actual   {layer_stack:?}",
            oracle.layer_stack
        ));
    }

    // `testPcpCompositionResults.py`'s variant fallbacks, which `pcp.txt`
    // was composed with.
    let standin = loaded.store.tokens.intern("standin");
    let render = loaded.store.tokens.intern("render");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            variant_fallbacks: [(standin, vec![render])].into_iter().collect(),
            ..StageOptions::default()
        },
    );

    for prim in &oracle.prims {
        let expected = render_oracle(&prim.prim_stack);
        let path = layerstack::Path::parse_absolute(&prim.path, &mut loaded.store.tokens)
            .expect("oracle prim path");
        let prim_id = loaded.store.paths.intern(path.clone());
        let Some(sources) = stage.explain_prim(prim_id) else {
            observed.prims += 1;
            observed.diffs.insert(Diff::MissingPrim);
            observed.detail.push(format!(
                "missing prim {}\n    expected {expected:?}",
                prim.path
            ));
            continue;
        };
        let actual: Vec<String> = sources
            .iter()
            .map(|key| render_site(&loaded, key.layer_id, &key.spec_path))
            .collect();
        let diffs = diff_stacks(&expected, &actual);
        if !diffs.is_empty() {
            observed.prims += 1;
            observed.detail.push(format!(
                "prim stack {} {diffs:?}\n    expected {expected:?}\n    actual   {actual:?}",
                prim.path
            ));
            observed.diffs.extend(diffs);
        }

        for prop in &prim.property_stacks {
            let expected = render_oracle(&prop.stack);
            let name = prop.property.rsplit_once('.').expect("property path").1;
            let field = loaded.store.tokens.intern(name);
            let actual: Vec<String> = stage
                .explain_property_path(PropertyPath::new(prim_id, field))
                .unwrap_or_default()
                .iter()
                .map(|opinion| render_site(&loaded, opinion.key.layer_id, &opinion.key.spec_path))
                .collect();
            let diffs = diff_stacks(&expected, &actual);
            if !diffs.is_empty() {
                observed.props += 1;
                observed.detail.push(format!(
                    "property stack {} {diffs:?}\n    expected {expected:?}\n    actual   {actual:?}",
                    prop.property
                ));
                observed.diffs.extend(diffs);
            }
        }
    }

    // The populated namespace must match too: a prim the oracle does not
    // compose (including a prohibited child) is a mismatch.
    let composed: BTreeSet<&str> = oracle.prims.iter().map(|p| p.path.as_str()).collect();
    let root = loaded.store.paths.intern(layerstack::Path::root());
    for prim_id in stage.traverse(root).filter(|id| *id != root) {
        let path = loaded.store.paths.display(prim_id, &loaded.store.tokens);
        if !composed.contains(path.as_str()) {
            observed.prims += 1;
            observed.diffs.insert(Diff::ExtraPrim);
            observed.detail.push(format!("extra prim {path}"));
        }
    }

    let derived = derive_scalars(&mut loaded, &oracle);
    let expectations: Vec<ScalarExpectation<'_>> = derived
        .iter()
        .map(|(prop, value, layer, spec)| {
            (prop.as_str(), value.clone(), layer.as_str(), spec.as_str())
        })
        .collect();
    let failures = check_scalar_values(&mut loaded, &stage, &expectations);
    observed.values = failures.len();
    observed.detail.extend(failures);
    observed
}

/// Root causes of known strict mismatches, grouped as in the module docs.
///
/// A fixture lists every cause that contributes to its mismatch, strongest
/// contributor first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Cause {
    // Ordering: strength order differs from OpenUSD's node graph.
    /// Class arcs (inherits, specializes) authored inside referenced content
    /// are implied onto each stronger layer stack and ranked with that
    /// stack's node (AOUSD Core §10.4.2.4; `pxr/usd/pcp/primIndex.cpp`,
    /// `_EvalImpliedClasses`).
    /// Layerstack implies inherits that way, but the variant selections of
    /// an arc's target are resolved before the classes implied across that
    /// arc are known.
    ImpliedClasses,
    // Missing sources or extra opinions.
    /// The variant sets of a site that an arc authored on a namespace
    /// ancestor reaches are selected when that arc is expanded, before the
    /// prim's stronger sites are known; OpenUSD evaluates them once the
    /// prim's arcs are all added (`_EvalNodeAncestralVariantSets` in
    /// `pxr/usd/pcp/primIndex.cpp`).
    AncestralArcs,

    // Unsupported features.
    /// Relocates (AOUSD Core §10.3.2.6) move a prim's ancestral opinions
    /// to its relocation target, but variant opinions authored inside a
    /// relocation source are not composed as OpenUSD composes them.
    Relocates,
}

/// A fixture known to mismatch the oracle, with its exact mismatch shape.
struct Known {
    fixture: &'static str,
    causes: &'static [Cause],
    /// Prims whose prim stack differs, including missing and extra prims.
    prims: usize,
    /// Properties whose property stack differs.
    props: usize,
    /// Derived scalars whose resolved value or winning spec differs.
    values: usize,
    /// Union of the stack differences.
    diffs: &'static [Diff],
    reason: &'static str,
}

use Cause as C;
use Diff as D;

/// Fixtures that are not compared: OpenUSD rejects the entry layer, so
/// `pcp.txt` composes no prims. Layerstack's USDA reader must reject it too
/// ([`LoadedStage::invalid`]).
///
/// Each entry is `(fixture, reason)`.
const SKIPPED: &[(&str, &str)] = &[
    (
        "BasicInherits_root",
        "no oracle: OpenUSD rejects `root.usd` (inherit paths cannot contain variant \
         selections), so `pcp.txt` composes no prims",
    ),
    (
        "ErrorRelocateWithVariantSelection_root",
        "no oracle: OpenUSD rejects `root.usd` (a relocates source path contains a variant \
         selection), so `pcp.txt` composes no prims",
    ),
    (
        "SubrootReferenceAndVariants_root",
        "no oracle: OpenUSD rejects `root.usd` (reference paths cannot contain variant \
         selections), so `pcp.txt` composes no prims",
    ),
];

/// Every fixture that does not match the oracle exactly.
const KNOWN: &[Known] = &[
    Known {
        fixture: "ErrorOpinionAtRelocationSource_root",
        causes: &[C::Relocates],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite],
        reason: "`/VariantAtRelocateSource/Sibling` misses the variant opinions `root.usd` authors inside the relocated `/VariantAtRelocateSource/Child`",
    },
    Known {
        fixture: "TrickyVariantAncestralSelection_root",
        causes: &[C::AncestralArcs],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite],
        reason: "`/Root/B/C` selects the variants of `ref2.usd /A/B/C` and `/B/C`, reached through its ancestors' references, before its index is complete, so it misses their `{v1=C}` and `{v2=Z}`",
    },
    Known {
        fixture: "TypicalReferenceToRiggedModel_root",
        causes: &[C::ImpliedClasses],
        prims: 2,
        props: 2,
        values: 1,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraSite],
        reason: "implied `root.usd /Class` selects `pin=latest` in OpenUSD; here `mcat.usd`'s `pin=stable` wins",
    },
];

fn shape(prims: usize, props: usize, values: usize, diffs: &[Diff]) -> String {
    let diffs: Vec<String> = diffs.iter().map(|d| format!("D::{d:?}")).collect();
    format!(
        "prims: {prims}, props: {props}, values: {values}, diffs: &[{}]",
        diffs.join(", ")
    )
}

/// Compares every supplemental fixture against its `pcp.txt` and fails on
/// any drift from [`KNOWN`] and [`SKIPPED`]: a new mismatch, a changed
/// mismatch shape, or a known fixture that now matches.
///
/// Run with `STRICT_PRINT=1` and `--nocapture` to print the observed shape
/// of every mismatching fixture in table syntax.
#[test]
fn strict_prim_stacks_match_pcp_txt() {
    let mut names: Vec<String> = std::fs::read_dir(assets_dir())
        .expect("assets dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.join("pcp.txt").is_file())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let skipped: BTreeMap<_, _> = SKIPPED.iter().copied().collect();
    let known: BTreeMap<_, _> = KNOWN.iter().map(|k| (k.fixture, k)).collect();
    assert_eq!(known.len(), KNOWN.len(), "KNOWN lists a fixture twice");
    for fixture in known.keys().chain(skipped.keys()) {
        assert!(
            names.iter().any(|n| n == fixture),
            "table names unknown fixture {fixture}"
        );
    }

    let print_all = std::env::var_os("STRICT_PRINT").is_some();
    let mut drift = String::new();
    let mut passing = Vec::new();
    for name in &names {
        if skipped.contains_key(name.as_str()) {
            let dir = assets_dir().join(name);
            let entry = load_pcp_txt(&dir.join("pcp.txt")).entry;
            let loaded = load_entry_usda(&dir.join("usda").join(entry));
            if loaded.invalid.is_empty() {
                writeln!(
                    drift,
                    "{name}: OpenUSD rejects the entry layer; Layerstack loads it"
                )
                .unwrap();
            }
            continue;
        }
        let observed = observe(name);
        if observed.oracle_prims == 0 {
            writeln!(
                drift,
                "{name}: pcp.txt composes no prims; list it in SKIPPED"
            )
            .unwrap();
            continue;
        }
        let diffs: Vec<Diff> = observed.diffs.iter().copied().collect();
        let clean = observed.diffs.is_empty() && observed.values == 0;
        let actual = shape(observed.prims, observed.props, observed.values, &diffs);
        let entry = known.get(name.as_str());
        if print_all && !clean {
            println!("{name:?} => {actual}");
            if std::env::var("STRICT_ONLY")
                .is_ok_and(|only| only == "all" || only.split(',').any(|o| o == name))
            {
                for line in &observed.detail {
                    println!("    {}", line.replace('\n', "\n    "));
                }
            }
        }
        match entry {
            None if clean => passing.push(name.as_str()),
            None => writeln!(drift, "{name}: new mismatch: {actual}").unwrap(),
            Some(k) if clean => {
                writeln!(
                    drift,
                    "{name}: now matches the oracle; remove it from KNOWN ({:?}: {})",
                    k.causes, k.reason
                )
                .unwrap();
            }
            Some(k) => {
                let expected = shape(k.prims, k.props, k.values, k.diffs);
                if expected == actual {
                    continue;
                }
                writeln!(
                    drift,
                    "{name}: known as {:?}: {}\n  expected {expected}\n  observed {actual}",
                    k.causes, k.reason
                )
                .unwrap();
            }
        }
        if !clean {
            for line in &observed.detail {
                writeln!(drift, "    {}", line.replace('\n', "\n    ")).unwrap();
            }
        }
    }

    let mut primary = BTreeMap::<Cause, usize>::new();
    let mut any = BTreeMap::<Cause, usize>::new();
    for k in KNOWN {
        *primary.entry(k.causes[0]).or_default() += 1;
        for cause in k.causes {
            *any.entry(*cause).or_default() += 1;
        }
    }
    println!(
        "strict pcp.txt conformance: {} of {} fixtures match exactly, {} known mismatches, {} skipped",
        passing.len(),
        names.len(),
        KNOWN.len(),
        SKIPPED.len(),
    );
    println!("  cause: fixtures where primary / fixtures affected");
    for (cause, n) in &any {
        println!(
            "  {cause:?}: {} / {n}",
            primary.get(cause).copied().unwrap_or(0)
        );
    }
    println!("  exact matches: {}", passing.join(", "));
    assert!(
        drift.is_empty(),
        "strict conformance drifted from the KNOWN table:\n{drift}"
    );
}
