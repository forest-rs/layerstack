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
//!   ingestion stores a `bool` as an integer, and where `pcp.txt`'s variant
//!   fallback selects a branch that `UsdStage` without fallbacks would not.
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
//! single-level references, payloads and inherits, list-edited arcs and
//! target paths, and variant selections that do not depend on the features
//! below. Where only [`Cause::DuplicateSources`] is listed, composed values
//! are unaffected: the repeated sites carry identical opinions.
//!
//! # Not supported
//!
//! - Relocates ([`Cause::Relocates`]): diagnosed and ignored.
//! - OpenUSD's node-graph strength order: implied class arcs
//!   ([`Cause::ImpliedClasses`]), arcs nested two or more deep
//!   ([`Cause::NestedArcDepth`]) and the global placement of specializes
//!   ([`Cause::SpecializesPlacement`]). These change resolved values in
//!   several fixtures. Fixing them needs a composition context that records
//!   the arc path (node) of each source instead of one nested arc kind.
//! - One site per arc path ([`Cause::CollapsedNodes`]) and one registration
//!   per arc path ([`Cause::DuplicateSources`]); the same node model would
//!   remove both.
//! - Ancestral arcs of subroot arc targets ([`Cause::AncestralArcs`]),
//!   some nested variant specs ([`Cause::VariantSpecs`]), internal arcs
//!   authored in sublayers ([`Cause::InternalArcAnchoring`]), conflicting
//!   property spec types ([`Cause::PropertyTypeConflict`]), asset-path
//!   expressions ([`Cause::ExpressionVariables`]) and variant fallbacks
//!   ([`Cause::FallbackVariants`]).
//!
//! The test prints the per-cause tally and the list of exact matches.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use layerstack::property::get_property;
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
/// `/Host{set=variant}Child` specs; other variant shapes are `Unknown`.
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
    let Some(host) = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root().join(&host))
    else {
        return Authored::Unknown;
    };
    let Some(branch) = layer
        .prims
        .get(&host)
        .and_then(|prim| prim.variant_sets.get(&set))
        .and_then(|set| set.variants.get(&variant))
    else {
        return Authored::Unknown;
    };
    match &components[at + 1..] {
        [] => property_default(&branch.properties, name),
        [SpecComponent::Prim(child)] => match branch.child_properties.get(child) {
            Some(properties) => property_default(properties, name),
            None if branch.child_fields.contains_key(child) => Authored::NoDefault,
            None => Authored::Unknown,
        },
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

    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
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
    // Presentation: the same sites, counted differently.
    /// Layerstack registers a site more than once for one arc path:
    /// arc-target sources are forwarded again under a nested arc kind (the
    /// late source copies in `add_reference_edge_opinions`,
    /// `add_inherit_edge_opinions` and specializes). The duplicate opinions
    /// carry the same value, so resolution is unaffected.
    DuplicateSources,
    /// OpenUSD keeps one node per arc path, so a site reached twice (a
    /// reference or payload diamond, a reference listed repeatedly with
    /// different offsets) appears twice; Layerstack visits each
    /// `(destination, layer, target)` once.
    CollapsedNodes,

    // Ordering: strength order differs from OpenUSD's node graph.
    /// Class arcs (inherits, specializes) authored inside referenced content
    /// are implied onto each stronger layer stack and ranked with that
    /// stack's node (AOUSD Core §10.4.2.4; `pxr/usd/pcp/primIndex.cpp`,
    /// `_EvalImpliedClasses`).
    /// Layerstack ranks every implied copy inside the nested arc's bucket, so
    /// `root.usd /Class` sorts after the reference target.
    ImpliedClasses,
    /// `OpinionKey::nested_arc_kind` holds one nested arc kind, while OpenUSD
    /// orders arcs per introducing layer stack (AOUSD Core §10.4;
    /// `pxr/usd/pcp/strengthOrdering.cpp`),
    /// so arcs nested two or more deep, and the arcs of a nested target,
    /// interleave or outrank their own target.
    NestedArcDepth,
    /// OpenUSD propagates specializes nodes to the root and ranks them after
    /// every other arc of the whole graph (AOUSD Core §10.4.1;
    /// `pxr/usd/pcp/primIndex.cpp`, `_EvalImpliedSpecializes`). Layerstack
    /// ranks nested specializes inside their outer arc's bucket.
    SpecializesPlacement,

    // Missing sources or extra opinions.
    /// Arcs on namespace ancestors map wrongly into descendants: a subroot
    /// arc target misses its ancestors' arcs and variant selections, or an
    /// ancestral site is added where OpenUSD has none.
    AncestralArcs,
    /// Variant branch specs are missing: directly nested variant sets, child
    /// specs inside a referenced layer's selected branch, or branches that
    /// USDA ingestion stores by namespace path and overwrites.
    VariantSpecs,
    /// An internal arc authored in a sublayer sees only that sublayer, not the
    /// containing layer stack (AOUSD Core §10.3.2.1: "the layer stack
    /// containing the reference is assumed").
    InternalArcAnchoring,
    /// A property spec whose type conflicts with the defining spec is kept;
    /// OpenUSD ignores it.
    PropertyTypeConflict,

    // Unsupported features.
    /// Relocates (AOUSD Core §10.3.2.6) are not composed; USDA ingestion
    /// diagnoses and drops them. Relocated prims are missing at their
    /// targets, and relocation sources, which OpenUSD prohibits, are
    /// populated as extra prims.
    Relocates,
    /// Asset-path variable expressions are not evaluated.
    ExpressionVariables,
    /// `testPcpCompositionResults.py` composes with the variant fallback
    /// `{'standin': ['render']}`; Layerstack has no fallback selections.
    FallbackVariants,
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

/// Fixtures that are not compared: no usable oracle, or Layerstack cannot
/// compose them at all.
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
        fixture: "BasicAncestralReference_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 2,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/A` lists `A.usd /A` twice: the reference target's sources are forwarded again under a nested reference kind",
    },
    Known {
        fixture: "BasicInstancingAndVariants_root",
        causes: &[C::DuplicateSources],
        prims: 6,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/InstancedModel` repeats `ref.usd /Model{y=a}`",
    },
    Known {
        fixture: "BasicInstancing_root",
        causes: &[C::ImpliedClasses],
        prims: 3,
        props: 3,
        values: 0,
        diffs: &[D::Order],
        reason: "implied `root.usd /_class_Prop` ranks after `set.usd /Set/InstancedProp`, and `prop.usd /_class_Prop` before `prop.usd /Prop`",
    },
    Known {
        fixture: "BasicListEditingWithInherits_root",
        causes: &[C::DuplicateSources],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/Model` repeats `model.usd /Model`",
    },
    Known {
        fixture: "BasicLocalAndGlobalClassCombination_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /Model_1/_class_Nested` ranks after the reference target `model.usd /Model/Instance`",
    },
    Known {
        fixture: "BasicNestedPayload_root",
        causes: &[C::NestedArcDepth, C::DuplicateSources, C::CollapsedNodes],
        prims: 8,
        props: 3,
        values: 1,
        diffs: &[D::ExtraRepeat, D::MissingRepeat, D::Order],
        reason: "`/Set2/Prop/PropScope.x` resolves from `set_payload.usd`, not the stronger nested `prop_payload.usd`: payloads nested in payloads share one strength bucket",
    },
    Known {
        fixture: "BasicNestedVariantsWithSameName_root",
        causes: &[C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/foo/bar` repeats `root.usd /foo{commonName=c}bar`",
    },
    Known {
        fixture: "BasicNestedVariants_root",
        causes: &[C::VariantSpecs, C::DuplicateSources],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat],
        reason: "`/DirectlyNestedVariants` lacks the outer `{standin=anim}` branch of directly nested variant sets",
    },
    Known {
        fixture: "BasicPayloadDiamond_root",
        causes: &[C::CollapsedNodes, C::NestedArcDepth],
        prims: 2,
        props: 2,
        values: 0,
        diffs: &[D::MissingRepeat, D::Order],
        reason: "`C.usd /C` is reached through `A` and `B` but listed once, after both",
    },
    Known {
        fixture: "BasicPayload_root",
        causes: &[
            C::InternalArcAnchoring,
            C::AncestralArcs,
            C::NestedArcDepth,
            C::DuplicateSources,
        ],
        prims: 11,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "internal arcs authored in `sublayer.usd` see only that sublayer, not the containing root layer stack, and `payload = <>` finds no default prim",
    },
    Known {
        fixture: "BasicReferenceAndClassDiamond_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /Model_1/LocalClass` ranks after `model.usd /Model/Instance`",
    },
    Known {
        fixture: "BasicReferenceAndClass_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /Class` ranks after the reference target `model.usd /Model`",
    },
    Known {
        fixture: "BasicReferenceDiamond_root",
        causes: &[C::CollapsedNodes, C::NestedArcDepth],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::MissingRepeat, D::Order],
        reason: "`C.usd /C` is reached through `A` and `B` but listed once, after both",
    },
    Known {
        fixture: "BasicRelocateToAnimInterfaceAsNewRootPrim_root",
        causes: &[C::Relocates],
        prims: 4,
        props: 1,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite],
        reason: "ignores `</CharRig/Rig/PathRig/Path>` -> `</Path>` authored in `charRig.usd`",
    },
    Known {
        fixture: "BasicRelocateToAnimInterface_root",
        causes: &[C::Relocates],
        prims: 4,
        props: 1,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite],
        reason: "ignores `</CharRig/Anim/Path/Anim>` -> `</CharRig/Anim/Path/AnimScope>` authored in `root.usd`",
    },
    Known {
        fixture: "BasicSpecializesAndInherits_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/Instance_1` ranks `/Inherits_1`, inherited by the specialized class, before `/Specializes_1`",
    },
    Known {
        fixture: "BasicSpecializesAndReferences_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/ShaderBindings/ShinyPlastic_BlueShinyPlastic` ranks the nested specializes target `/ShinyPlasticLook` before `ShinyPlastic`",
    },
    Known {
        fixture: "BasicSpecializesAndVariants_root",
        causes: &[C::DuplicateSources, C::SpecializesPlacement],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/Root` repeats `ref.usd /Ref{v=ref}`; specializes inside variants reorder",
    },
    Known {
        fixture: "BasicSpecializes_root",
        causes: &[C::DuplicateSources, C::SpecializesPlacement],
        prims: 8,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/Basic` repeats `root.usd /BasicSpecializes2`; specializes chains inside references reorder",
    },
    Known {
        fixture: "BasicVariantWithConnections_root",
        causes: &[C::NestedArcDepth, C::DuplicateSources],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "classes inherited inside `camera_perspective.usd`, referenced from a variant, outrank the variant and the reference target",
    },
    Known {
        fixture: "BasicVariantWithReference_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/ModelRef` repeats `model.usd /Model{vset=without_children}`",
    },
    Known {
        fixture: "ElidedAncestralRelocates_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 16,
        props: 0,
        values: 0,
        diffs: &[
            D::MissingPrim,
            D::ExtraPrim,
            D::MissingSite,
            D::ExtraSite,
            D::ExtraRepeat,
        ],
        reason: "ignores `</CharBase/CharBaseRig/CollisionRig/Collision>` -> `</CharBase/Anim/Collision>` authored in `base.usd`",
    },
    Known {
        fixture: "ErrorArcCycle_root",
        causes: &[C::Relocates],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim],
        reason: "ignores `</RelocatedInheritOfChild/Child/Object>` -> `</RelocatedInheritOfChild/Object>`, so the relocated prim is missing; every arc cycle is skipped as in OpenUSD",
    },
    Known {
        fixture: "ErrorConnectionPermissionDenied_root",
        causes: &[C::DuplicateSources],
        prims: 5,
        props: 2,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/PermissionsAcrossInherits/Instance/PrivatePrimFromClass` repeats its class site",
    },
    Known {
        fixture: "ErrorInconsistentProperties_root",
        causes: &[C::PropertyTypeConflict],
        prims: 0,
        props: 1,
        values: 0,
        diffs: &[D::ExtraSite],
        reason: "the relationship `ref.usd /InconsistentPropType.x` stays in the stack of the attribute `x`",
    },
    Known {
        fixture: "ErrorInvalidAuthoredRelocates_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "ignores `</Model_1/Instance/Test>` -> `</Model_1/Instance/Test>` authored in `root.usd`",
    },
    Known {
        fixture: "ErrorInvalidConflictingRelocates_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 43,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</Model_1/Instance/ClassChild>` -> `</Model_1/Instance/Test>` authored in `root.usd`",
    },
    Known {
        fixture: "ErrorInvalidInstanceTargetPath_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 8,
        props: 2,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat],
        reason: "ignores `</BrowRig/LBrow/Anim/Brow>` -> `</BrowRig/Anim/LBrow>` authored in `ref.usd`",
    },
    Known {
        fixture: "ErrorInvalidPreRelocateTargetPath_root",
        causes: &[C::Relocates],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim],
        reason: "ignores `</SublayerRoot/Rig/Scope>` -> `</SublayerRoot/Anim/Scope>` authored in `root.usd`",
    },
    Known {
        fixture: "ErrorInvalidReferenceToRelocationSource_root",
        causes: &[C::Relocates],
        prims: 14,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite, D::ExtraSite],
        reason: "ignores `</Char/PreRelo>` -> `</Char/Relocated>` authored in `char.usd`",
    },
    Known {
        fixture: "ErrorOpinionAtRelocationSource_root",
        causes: &[C::Relocates],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite],
        reason: "ignores `</CharRig/Rig/PathRig/Path>` -> `</CharRig/Anim/Path>` authored in `root.usd`",
    },
    Known {
        fixture: "ErrorPermissionDenied_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/Parent` repeats `A.usd /Parent`",
    },
    Known {
        fixture: "ExpressionsInPayloads_root",
        causes: &[C::ExpressionVariables],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::MissingRepeat],
        reason: "payload asset paths such as `` @`\"./${REF}.usd\"`@ `` are variable expressions",
    },
    Known {
        fixture: "ExpressionsInReferences_root",
        causes: &[C::ExpressionVariables],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::MissingRepeat],
        reason: "reference asset paths such as `` @`\"./${REF}.usd\"`@ `` are variable expressions",
    },
    Known {
        fixture: "ImpliedAndAncestralInherits_ComplexEvaluation_root",
        causes: &[C::NestedArcDepth, C::DuplicateSources],
        prims: 16,
        props: 7,
        values: 3,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "the inherits of a nested arc target outrank the target: `ref.usd /Ref/C/_Z` sorts before `ref.usd /Ref/C/D`, so `D.prop` resolves to `ref:weak`",
    },
    Known {
        fixture: "ImpliedAndAncestralInherits_root",
        causes: &[C::NestedArcDepth],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::Order],
        reason: "`CharGroupRig.usd /_class_CharGroupRig` outranks `/CHARGROUP`, the nested reference target that inherits it",
    },
    Known {
        fixture: "PayloadsAndAncestralArcs2_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "ignores `</Ref/PayloadChild>` -> `</Ref/Child>` authored in `relocates.usd`",
    },
    Known {
        fixture: "PayloadsAndAncestralArcs3_root",
        causes: &[C::Relocates],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim],
        reason: "ignores `</Model/Anim>` -> `</Model/Test/Anim>` authored in `model.usd`",
    },
    Known {
        fixture: "ReferenceListOpsWithOffsets_root",
        causes: &[C::CollapsedNodes],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::MissingRepeat],
        reason: "`@ref.usd@</Ref>` listed four times with different offsets is one source, not four nodes",
    },
    Known {
        fixture: "RelocatePrimsWithSameName_root",
        causes: &[C::Relocates, C::DuplicateSources, C::CollapsedNodes],
        prims: 19,
        props: 0,
        values: 0,
        diffs: &[
            D::MissingPrim,
            D::ExtraPrim,
            D::ExtraRepeat,
            D::MissingRepeat,
            D::Order,
        ],
        reason: "ignores `</Ref1/Child>` -> `</Ref1/Child_1>` authored in `ref_2.usd`",
    },
    Known {
        fixture: "RelocateToNone_root",
        causes: &[C::Relocates],
        prims: 14,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraSite],
        reason: "ignores `</Char/ToBeDeleted>` -> `<>` authored in `root.usd`",
    },
    Known {
        fixture: "SpecializesAndAncestralArcs2_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/HumanMaleStdHair` repeats and reorders the specializes target `/_shared_HumanHair` across the reference",
    },
    Known {
        fixture: "SpecializesAndAncestralArcs3_root",
        causes: &[C::AncestralArcs, C::DuplicateSources],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat],
        reason: "`/Root/Child/GrandChild` misses `ref2.usd /Ref2Root/Ref2Child`, referenced from its ancestor inside `ref.usd`",
    },
    Known {
        fixture: "SpecializesAndAncestralArcs4_root",
        causes: &[
            C::SpecializesPlacement,
            C::AncestralArcs,
            C::DuplicateSources,
        ],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "`/Parent2` ranks `/PS` before `/PIS`: specializes reached through inherits are not moved after the inherited classes",
    },
    Known {
        fixture: "SpecializesAndAncestralArcs5_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "the specializes target `Cloth_C_Weave_09` ranks before `payload.usd`'s `Cloth_C_Weave_09_Satin`",
    },
    Known {
        fixture: "SpecializesAndAncestralArcs_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "specializes of `/Ref/Child` rank after every class instead of inside the ancestral reference",
    },
    Known {
        fixture: "SpecializesAndVariants2_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 4,
        props: 6,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/element` ranks `/implementation{testVariantSet=testVariant}` before `/referencedMiddleman`",
    },
    Known {
        fixture: "SpecializesAndVariants3_root",
        causes: &[
            C::VariantSpecs,
            C::SpecializesPlacement,
            C::DuplicateSources,
        ],
        prims: 3,
        props: 6,
        values: 3,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "`/implementation` misses its own `{testVariantSet=testVariant}` branch, selected by the specialized class, so `variantAttr` has no value",
    },
    Known {
        fixture: "SpecializesAndVariants4_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 1,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "the specializes introduced in `/_class_/render`'s variant is not implied onto `/A`, so `/A/render.test` resolves from `/_class_/defaultImplementation`",
    },
    Known {
        fixture: "SpecializesAndVariants_root",
        causes: &[C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/B` repeats `root.usd /A{nestedVariantSet=nestedVariant}`",
    },
    Known {
        fixture: "SubrootInheritsAndVariants_root",
        causes: &[C::AncestralArcs, C::DuplicateSources],
        prims: 4,
        props: 3,
        values: 1,
        diffs: &[D::MissingSite, D::ExtraSite, D::ExtraRepeat, D::Order],
        reason: "the subroot inherit of `/Root/Child` uses `{v=x}`, not the `{v=z}` selected on its ancestor `/Group`, so `a` is `v_x`",
    },
    Known {
        fixture: "SubrootReferenceAndClasses_root",
        causes: &[C::AncestralArcs],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite],
        reason: "the subroot reference to `/Set/Model` misses classes inherited and specialized through its ancestor `/Set`",
    },
    Known {
        fixture: "SubrootReferenceAndRelocates_root",
        causes: &[C::Relocates],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite],
        reason: "ignores `</Groups/CharGroup/Char>` -> `</Groups/CrowdGroup/Char>` authored in `groups.usd`",
    },
    Known {
        fixture: "SubrootReferenceAndVariants2_root",
        causes: &[C::AncestralArcs],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite],
        reason: "the subroot reference to `/CHARGROUP/CHARACTER` misses its ancestor's variant and class sites",
    },
    Known {
        fixture: "SubrootReferenceNonCycle_root",
        causes: &[C::AncestralArcs],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite],
        reason: "`/ImplNoCycle/A/D` misses `/ImplNoCycle/A/B`, reached through a subroot reference to an ancestor",
    },
    Known {
        fixture: "TrickyClassHierarchy_root",
        causes: &[C::ImpliedClasses],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::Order],
        reason: "the implied class chain `root.usd /_Class_Sullivan` ranks after `Sullivan.usd /Sullivan`",
    },
    Known {
        fixture: "TrickyConnectionToRelocatedAttribute_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 10,
        props: 0,
        values: 4,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</FaceRig/rig/LEyeRig/Anim>` -> `</FaceRig/Anim/LEye>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocates2_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat],
        reason: "ignores `</Group/Char>` -> `</Group/Char_Named>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocates3_root",
        causes: &[C::Relocates],
        prims: 8,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite],
        reason: "ignores `</GuitarRig/Rig/StringsRig/String1Rig/String>` -> `</GuitarRig/Anim/Strings/String1>` authored in `rig.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocates4_root",
        causes: &[C::Relocates],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::Order],
        reason: "ignores `</CurveTrackRig/rig/ConstRig/Anim/Const>` -> `</CurveTrackRig/Anim/Curve/Const>` authored in `CurveTrackRig.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocates5_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 19,
        props: 0,
        values: 5,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</TentacleRig/TentacleInterface/Knot03Rig/Anim>` -> `</TentacleRig/Tentacle/Knot03>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocatesToNewRootPrim_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 14,
        props: 1,
        values: 3,
        diffs: &[
            D::MissingPrim,
            D::ExtraPrim,
            D::MissingSite,
            D::ExtraSite,
            D::ExtraRepeat,
        ],
        reason: "ignores `</Group/Model>` -> `</Model_Renamed>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyInheritsAndRelocates_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 14,
        props: 1,
        values: 3,
        diffs: &[
            D::MissingPrim,
            D::ExtraPrim,
            D::MissingSite,
            D::ExtraSite,
            D::ExtraRepeat,
        ],
        reason: "ignores `</Group/Model>` -> `</Group/Model_Renamed>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyInheritsInVariants2_root",
        causes: &[C::VariantSpecs, C::DuplicateSources, C::NestedArcDepth],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "USDA ingestion keys variant descendants by namespace path, so the `tidscene` branch's `/Sarah/FaceRig/EyesRig` replaces the selected `full` branch's; `LEyeRig`'s inherit `Sarah_rig.usd /Sarah/FaceRig/EyesRig/SymEyeRig` outranks its own target",
    },
    Known {
        fixture: "TrickyInheritsInVariants_root",
        causes: &[C::DuplicateSources, C::NestedArcDepth],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/Model` repeats `model.usd /Model{complexity=high}`; `/Model/Scope` interleaves its class and variant sites",
    },
    Known {
        fixture: "TrickyLocalClassHierarchyWithRelocates_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 12,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "ignores `</C/ArmsRig/LArmRig/ArmRegion/Region>` -> `</C/CollisionRig/Body/CollBody/SimRegions/LArm>` authored in `Sullivan_masterrig.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocations2_root",
        causes: &[C::Relocates],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite],
        reason: "ignores `</ModelGroup/Model>` -> `</ModelGroup/Model_2>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocations3_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite, D::ExtraRepeat],
        reason: "ignores `</Group/Subgroup/Char>` -> `</Group/Subgroup/Char_Renamed>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocations4_root",
        causes: &[C::Relocates],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite],
        reason: "ignores `</RearLegRig/Knee_bone/Ankle_bone>` -> `</RearLegRig/Knee_bone/Ankle_bone_phrbv>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocations5_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraRepeat],
        reason: "ignores `</Group/CHARACTER>` -> `</Group/Character>` authored in `variant.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocationsAndClasses2_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 8,
        props: 0,
        values: 1,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</LegsRig/LLegRig/TentacleRig/Tentacle>` -> `</LegsRig/Legs/LHip>` authored in `LegsRig.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocationsAndClasses_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 20,
        props: 0,
        values: 4,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</HumanRig/rig/Face/Anim/Face>` -> `</HumanRig/Face>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyMultipleRelocations_root",
        causes: &[C::Relocates],
        prims: 15,
        props: 0,
        values: 2,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite],
        reason: "ignores `</CharRig/Rig/SubRig/Anim/AnimScope>` -> `</CharRig/Anim/AnimScope>` authored in `rig.usd`",
    },
    Known {
        fixture: "TrickyNestedClasses2_root",
        causes: &[C::ImpliedClasses, C::NestedArcDepth, C::DuplicateSources],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /_class_ClothRig` ranks after `rig.usd /KiltRig`",
    },
    Known {
        fixture: "TrickyNestedClasses3_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 11,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /Rig/_class_SubRig` ranks after `rig.usd /Rig/SymRig/SubRig1`",
    },
    Known {
        fixture: "TrickyNestedClasses4_root",
        causes: &[C::AncestralArcs, C::ImpliedClasses, C::DuplicateSources],
        prims: 10,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "local classes nested inside inherited classes miss their ancestral class sites",
    },
    Known {
        fixture: "TrickyNestedClasses_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 7,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd .../_Class_FingerRig` ranks after `HandsRig.usd .../IndexRig`",
    },
    Known {
        fixture: "TrickyNestedSpecializes_root",
        causes: &[C::SpecializesPlacement],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::Order],
        reason: "`ref3.usd /Ref3`, referenced by a specializes target, ranks before `ref2.usd /Ref2/Nested`",
    },
    Known {
        fixture: "TrickyNestedVariants_root",
        causes: &[C::VariantSpecs],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::Order],
        reason: "variant sets nested in `/A{v1=x}B` miss `{v2=y}` and the arcs authored inside it",
    },
    Known {
        fixture: "TrickyNonLocalVariantSelection_root",
        causes: &[C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/Model` repeats `model.usd /Model{costume=basicCostume}`",
    },
    Known {
        fixture: "TrickyRelocatedTargetInVariant_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 9,
        props: 1,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::MissingSite, D::ExtraRepeat],
        reason: "ignores `</Root/Child>` -> `</Root/Anim/Child>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyRelocationOfPrimFromPayload_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 15,
        props: 2,
        values: 2,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat, D::Order],
        reason: "ignores `</Model/Rig/LRig/Anim>` -> `</Model/Anim/LRig>` authored in `model_payload.usd`",
    },
    Known {
        fixture: "TrickyRelocationOfPrimFromVariant_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::ExtraPrim, D::ExtraRepeat],
        reason: "ignores `</CharRig/TailRig/Tail>` -> `</CharRig/Anim/Tail>` authored in `CharRig.usd`",
    },
    Known {
        fixture: "TrickyRelocationSquatter_root",
        causes: &[C::Relocates],
        prims: 4,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraSite],
        reason: "ignores `</X/A>` -> `</X/A2>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickySpecializesAndInherits2_root",
        causes: &[C::SpecializesPlacement],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::Order],
        reason: "classes of specializes targets rank before `ref2.usd /Ref2/Nested`",
    },
    Known {
        fixture: "TrickySpecializesAndInherits3_root",
        causes: &[C::ImpliedClasses, C::DuplicateSources],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /SetClass` ranks after `package.usd /SetPackage`",
    },
    Known {
        fixture: "TrickySpecializesAndInherits_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/Model/A` ranks `ref.usd` specializes targets before `root.usd`'s",
    },
    Known {
        fixture: "TrickySpecializesAndRelocates_root",
        causes: &[C::Relocates],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite],
        reason: "ignores `</Model/Brass>` -> `</Model/Brass_2>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickySpookyInheritsInSymmetricArmRig_root",
        causes: &[C::Relocates],
        prims: 3,
        props: 1,
        values: 1,
        diffs: &[D::MissingPrim, D::MissingSite],
        reason: "ignores `</HumanRig/Rig/LArm/Anim>` -> `</HumanRig/Anim/LArm>` authored in `humanRig.usd`",
    },
    Known {
        fixture: "TrickySpookyInheritsInSymmetricBrowRig_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 8,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "ignores `</BrowRig/LBrow/Anim/Brow>` -> `</BrowRig/Anim/LBrow>` authored in `BrowRig.usd`",
    },
    Known {
        fixture: "TrickySpookyInherits_root",
        causes: &[C::Relocates],
        prims: 5,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraSite, D::Order],
        reason: "ignores `</Model/Rig/LRig>` -> `</Model/Anim/LAnim>` authored in `model.usd`",
    },
    Known {
        fixture: "TrickySpookyVariantSelectionInClass_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 8,
        props: 2,
        values: 3,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "ignores `</CharRig/Rig/LeftLegRig/Anim>` -> `</CharRig/Anim/LeftLeg>` authored in `CharRig.usd`",
    },
    Known {
        fixture: "TrickySpookyVariantSelection_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 3,
        props: 2,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraRepeat],
        reason: "ignores `</FaceRig/Rig/LipRig/Anim>` -> `</FaceRig/Anim/Lip>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyVariantAncestralSelection_root",
        causes: &[C::ImpliedClasses, C::AncestralArcs, C::DuplicateSources],
        prims: 3,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "implied `root.usd /_class_A1` ranks after `ref.usd /A1`; `/Root/B/C` misses variants of ancestral sites",
    },
    Known {
        fixture: "TrickyVariantInPayload_root",
        causes: &[C::NestedArcDepth],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::Order],
        reason: "`model.usd /A/B` ranks before `model.usd /B{v=v2}`: the variant of a payload target sorts after the payload's ancestral site",
    },
    Known {
        fixture: "TrickyVariantIndependentSelection_root",
        causes: &[C::NestedArcDepth, C::VariantSpecs, C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "variant branches of three references to `ref.usd` interleave and repeat",
    },
    Known {
        fixture: "TrickyVariantOverrideOfLocalClass_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/HandRig/_Class_FingerRig` repeats its variant site",
    },
    Known {
        fixture: "TrickyVariantOverrideOfRelocatedPrim_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 5,
        props: 1,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite, D::ExtraRepeat],
        reason: "ignores `</Model/UnrelocatedSphere>` -> `</Model/RelocatedSphere>` authored in `root.usd`",
    },
    Known {
        fixture: "TrickyVariantSelectionInVariant2_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/Ref/Model` repeats `root.usd /Ref{v2=b}Model`",
    },
    Known {
        fixture: "TrickyVariantSelectionInVariant_root",
        causes: &[C::DuplicateSources, C::NestedArcDepth],
        prims: 3,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat, D::Order],
        reason: "`/SlugJ` repeats payload sites and interleaves variant branches",
    },
    Known {
        fixture: "TrickyVariantWeakerSelection3_root",
        causes: &[C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/Model` repeats `geo.usd /Model{K_hierarchy=full}`",
    },
    Known {
        fixture: "TrickyVariantWeakerSelection4_root",
        causes: &[C::DuplicateSources],
        prims: 2,
        props: 0,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/bob/geom` repeats `root.usd /bob{geotype=cube}geom`",
    },
    Known {
        fixture: "TrickyVariantWeakerSelection_root",
        causes: &[C::DuplicateSources],
        prims: 1,
        props: 1,
        values: 0,
        diffs: &[D::ExtraRepeat],
        reason: "`/A` repeats `model.usd /A{v=v2}`",
    },
    Known {
        fixture: "TypicalReferenceToChargroupWithRename_root",
        causes: &[C::Relocates],
        prims: 6,
        props: 0,
        values: 0,
        diffs: &[D::ExtraPrim, D::MissingSite],
        reason: "ignores `</Group_1/Model>` -> `</Group_1/Model_1>` authored in `root.usd`",
    },
    Known {
        fixture: "TypicalReferenceToChargroup_root",
        causes: &[C::VariantSpecs, C::DuplicateSources],
        prims: 2,
        props: 2,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat],
        reason: "`/Group/Model` misses `group.usd /Group{standin=sim}Model`, a child spec in the selected branch of a referenced layer",
    },
    Known {
        fixture: "TypicalReferenceToRiggedModel_root",
        causes: &[C::ImpliedClasses, C::FallbackVariants, C::DuplicateSources],
        prims: 2,
        props: 2,
        values: 1,
        diffs: &[
            D::MissingPrim,
            D::MissingSite,
            D::ExtraSite,
            D::ExtraRepeat,
            D::Order,
        ],
        reason: "implied `root.usd /Class` selects `pin=latest` in OpenUSD; here `mcat.usd`'s `pin=stable` wins",
    },
    Known {
        fixture: "VariantSpecializesAndReferenceSurprisingBehavior_root",
        causes: &[C::SpecializesPlacement],
        prims: 3,
        props: 2,
        values: 2,
        diffs: &[D::MissingSite, D::ExtraRepeat, D::Order],
        reason: "`/Model` ranks `/Model_defaultShadingVariant` before `/New_Shading_Variant`, so `Material.myInt` is 0, not 1",
    },
    Known {
        fixture: "VariantSpecializesAndReference_root",
        causes: &[C::SpecializesPlacement, C::DuplicateSources],
        prims: 2,
        props: 2,
        values: 0,
        diffs: &[D::MissingSite, D::ExtraRepeat],
        reason: "`/Model/Material_Child` misses `/Model_defaultShadingVariant/Material`, so `myInt` is 1, not 0",
    },
    Known {
        fixture: "bug69932_root",
        causes: &[C::Relocates, C::DuplicateSources],
        prims: 21,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraSite, D::ExtraRepeat],
        reason: "ignores `</Pigeon/Rig/ToesRig/LToesRig/ThumbToeLOCALRig/Toe>` -> `</Pigeon/Anim/Legs/LToes/Thumb>` authored in `Pigeon_bodyrig.usd`",
    },
    Known {
        fixture: "bug74847_root",
        causes: &[C::AncestralArcs, C::DuplicateSources],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::ExtraSite, D::ExtraRepeat],
        reason: "`/A/B/B` gains `ref.usd /A`, the parent of the reference target, and repeats its variant spec",
    },
    Known {
        fixture: "bug92827_root",
        causes: &[C::Relocates],
        prims: 1,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim],
        reason: "ignores `</Rig/Other/A/B>` -> `</Rig/B>` authored in `root.usd`",
    },
    Known {
        fixture: "case1_root",
        causes: &[C::FallbackVariants],
        prims: 11,
        props: 0,
        values: 0,
        diffs: &[D::MissingPrim, D::MissingSite, D::ExtraRepeat],
        reason: "`standin=render` comes from the test harness's variant fallbacks, not from scene description",
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
