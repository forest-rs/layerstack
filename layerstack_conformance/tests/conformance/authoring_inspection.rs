// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for whether a prospective write would show, and where
//! the value comes from before it.
//!
//! `fixtures/authoring_inspection/oracle.json` records, for writes of a
//! default, a time sample or a spline through edit targets at local layers,
//! reference, variant, inherit and relocation nodes, a culled site, an
//! instance proxy's prototype and an unselected variant, what OpenUSD
//! 26.08's `UsdAttribute::GetResolveInfo(time)` reports before and after the
//! write at the default time and at time codes, and whether the written
//! value is read afterwards (`scripts/authoring_inspection_oracle.py`, which
//! also writes the layers). The attributes cover defaults, samples, value
//! blocks, blocked samples, animation blocks, splines, schema fallbacks,
//! and value clips with and without a manifest, before and after their
//! clips' activations.
//!
//! `Stage::inspect_property_authoring` must answer, at each time, that the
//! write shows exactly when OpenUSD reads it; its current source must be
//! OpenUSD's before the write; and OpenUSD's source after the write must be
//! the written slot when it shows, and otherwise the source the inspection
//! says masks it. Inspected over every time code at once, each recorded
//! time's span must give the answer the single time does.
//!
//! Spec: AOUSD Core §10.4 (strength ordering), §12.3 (value resolution),
//! §12.3.7 (value clips); OpenUSD `UsdStage::_GetResolveInfo`,
//! `UsdResolveInfo`, `UsdPrim::MakeResolveTargetStrongerThanEditTarget`.

use std::sync::Arc;

use layerstack::{
    ArcKind, AuthoringInspection, AuthoringQuery, AuthoringSourceKind, AuthoringTimes,
    AuthoringVerdict, AuthoringWrite, CurrentSource, InterpolationType, LayerId, OpinionKey,
    PathId, PropertyPath, SpecPath, Stage, StageOptions, edit::EditTarget,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/authoring_inspection/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    property: String,
    target: Target,
    write: String,
    times: Vec<Recorded>,
}

#[derive(Deserialize)]
struct Target {
    layer: String,
    holder: Option<String>,
    arc: Option<String>,
    site: Option<String>,
    variant: Option<String>,
}

#[derive(Deserialize)]
struct Recorded {
    time: serde_json::Value,
    before: Resolve,
    after: Resolve,
    shows: bool,
}

/// What `GetResolveInfo` reported, as the oracle records it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Resolve {
    source: String,
    blocked: bool,
    layer: Option<String>,
    node: Option<Node>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Node {
    arc: String,
    site: Option<String>,
    stack: String,
}

/// The parts of a resolve record layerstack compares: the source, whether
/// it is blocked, its layer, and its node's arc and site.
#[derive(Debug, PartialEq)]
struct Seen {
    source: String,
    blocked: bool,
    layer: Option<String>,
    arc: Option<String>,
    site: Option<String>,
}

impl From<&Resolve> for Seen {
    fn from(resolve: &Resolve) -> Self {
        Self {
            // A block reads the fallback or nothing; `/Ball.radius` tells
            // the two apart without a block.
            source: if resolve.blocked {
                "blocked".into()
            } else {
                resolve.source.clone()
            },
            blocked: resolve.blocked,
            layer: resolve.layer.clone(),
            arc: resolve.node.as_ref().map(|node| node.arc.clone()),
            site: resolve.node.as_ref().and_then(|node| node.site.clone()),
        }
    }
}

struct Fixture {
    loaded: LoadedStage,
    stage: Stage,
}

impl Fixture {
    fn layer(&self, name: &str) -> LayerId {
        *self
            .loaded
            .layer_names
            .iter()
            .find(|(_, path)| path.rsplit('/').next() == Some(name))
            .unwrap_or_else(|| panic!("layer {name}"))
            .0
    }

    fn layer_name(&self, id: LayerId) -> String {
        let path = &self.loaded.layer_names[&id];
        path.rsplit('/').next().unwrap().to_string()
    }

    /// The prim spec path of `spec`, variant selections included.
    fn display(&self, spec: &SpecPath) -> String {
        let text = spec.display(&self.loaded.store.paths, &self.loaded.store.tokens);
        match text.rfind('.') {
            Some(dot) if !text[dot..].contains(['/', '}']) => text[..dot].to_string(),
            _ => text,
        }
    }

    fn edit_target(&mut self, target: &Target) -> EditTarget {
        let layer = self.layer(&target.layer);
        let store = &mut self.loaded.store;
        if let Some(holder) = &target.holder {
            let holder = store.path(holder);
            let site = target.site.as_deref().unwrap();
            let arc = target.arc.as_deref().unwrap();
            let graph = self.stage.prim(holder, store).unwrap().composition_graph();
            let node = graph
                .nodes()
                .find(|(_, node)| {
                    arc_name(node.arc_kind()) == arc
                        && node.site().display(&store.paths, &store.tokens) == site
                })
                .map(|(id, _)| id)
                .unwrap_or_else(|| panic!("{arc} node at {site}"));
            return EditTarget::for_node_layer(&self.stage, store, holder, node, layer)
                .expect("edit target");
        }
        if let Some(variant) = &target.variant {
            let spec = SpecPath::parse(variant, &store.tokens, &store.paths).unwrap();
            return EditTarget::for_local_variant(layer, &spec);
        }
        EditTarget::for_layer(layer)
    }

    /// `key`, an opinion of `prim`, and `kind` as `GetResolveInfo` reports
    /// them.
    fn seen(&self, prim: PathId, key: &OpinionKey, kind: &AuthoringSourceKind) -> Seen {
        let source = match kind {
            AuthoringSourceKind::Default => "default",
            AuthoringSourceKind::TimeSamples => "time_samples",
            AuthoringSourceKind::Spline => "spline",
            AuthoringSourceKind::ValueClips { .. } => "value_clips",
            AuthoringSourceKind::Block => {
                return Seen {
                    source: "blocked".into(),
                    blocked: true,
                    layer: None,
                    arc: None,
                    site: None,
                };
            }
            AuthoringSourceKind::AnimationBlock => panic!("an animation block supplies no value"),
        };
        let graph = self
            .stage
            .prim(prim, &self.loaded.store)
            .map(|prim| prim.composition_graph());
        Seen {
            source: source.into(),
            blocked: false,
            layer: Some(self.layer_name(key.layer_id)),
            arc: graph
                .and_then(|graph| graph.node(key.node))
                .map(|node| arc_name(node.arc_kind()).into()),
            site: Some(self.display(&key.spec_path)),
        }
    }
}

fn arc_name(arc: ArcKind) -> &'static str {
    match arc {
        ArcKind::Local => "root",
        ArcKind::Inherits => "inherit",
        ArcKind::Variants => "variant",
        ArcKind::Relocates => "relocate",
        ArcKind::References => "reference",
        ArcKind::Payloads => "payload",
        ArcKind::Specializes => "specialize",
    }
}

fn write_kind(write: &str) -> AuthoringWrite {
    match write {
        "default" => AuthoringWrite::Default,
        "sample" => AuthoringWrite::TimeSample,
        "spline" => AuthoringWrite::Spline,
        other => panic!("write {other}"),
    }
}

fn shows(verdict: AuthoringVerdict) -> bool {
    matches!(
        verdict,
        AuthoringVerdict::Strongest | AuthoringVerdict::ComposedOver
    )
}

#[test]
fn inspection_answers_as_openusd_resolves_before_and_after_each_write() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/authoring_inspection")
            .join(&oracle.root),
    );
    let schemas = Arc::new(layerstack_schemas::openusd(&loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut fixture = Fixture { loaded, stage };
    for case in &oracle.cases {
        let target = fixture.edit_target(&case.target);
        let property: PropertyPath = fixture.loaded.store.property_path(&case.property);
        let write = write_kind(&case.write);
        let inspect = |fixture: &mut Fixture, times| {
            fixture
                .stage
                .inspect_property_authoring(
                    &mut fixture.loaded.store,
                    property,
                    &target,
                    AuthoringQuery {
                        write,
                        times,
                        interpolation: InterpolationType::Linear,
                    },
                )
                .unwrap_or_else(|error| panic!("{}: {error}", case.name))
        };
        let everywhere = (write != AuthoringWrite::Default).then(|| {
            inspect(
                &mut fixture,
                AuthoringTimes::Range {
                    start: f64::NEG_INFINITY,
                    end: f64::INFINITY,
                },
            )
        });
        for recorded in &case.times {
            let (times, code) = match &recorded.time {
                serde_json::Value::String(_) => (AuthoringTimes::Default, None),
                time => {
                    let code = time.as_f64().unwrap();
                    (AuthoringTimes::At(code), Some(code))
                }
            };
            let context = format!("{} at {}", case.name, recorded.time);
            let inspection = inspect(&mut fixture, times);
            check(
                &fixture,
                property.prim_path(),
                &inspection,
                recorded,
                &context,
            );
            if let (Some(everywhere), Some(code)) = (&everywhere, code) {
                let span = everywhere.span_at(code).expect("inspected span");
                assert_eq!(
                    span.verdict, inspection.spans[0].verdict,
                    "{context}: over every time code"
                );
            }
        }
    }
}

fn check(
    fixture: &Fixture,
    prim: PathId,
    inspection: &AuthoringInspection,
    recorded: &Recorded,
    context: &str,
) {
    let [span] = inspection.spans.as_slice() else {
        panic!("{context}: {:?}", inspection.spans);
    };
    assert_eq!(
        shows(span.verdict),
        recorded.shows,
        "{context}: {inspection:?}"
    );
    let before = match &span.current {
        CurrentSource::Fallback { schema } => Seen {
            source: if *schema { "fallback" } else { "none" }.into(),
            blocked: false,
            layer: None,
            arc: None,
            site: None,
        },
        CurrentSource::Authored { key, kind, .. } => fixture.seen(prim, key, kind),
    };
    let mut expected = Seen::from(&recorded.before);
    if before.site.is_some() && expected.site.is_none() {
        // OpenUSD names a prototype's root node by its prototype path.
        expected.site.clone_from(&before.site);
    }
    assert_eq!(before, expected, "{context}: current source");
    let after = Seen::from(&recorded.after);
    match span.verdict {
        AuthoringVerdict::Inactive => {
            assert!(inspection.target.is_none(), "{context}");
            assert_eq!(
                recorded.after, recorded.before,
                "{context}: OpenUSD ignores it"
            );
        }
        AuthoringVerdict::Strongest | AuthoringVerdict::ComposedOver => {
            let site = inspection.target.as_ref().expect("placed target");
            let written = match inspection.query.write {
                AuthoringWrite::Default => "default",
                AuthoringWrite::TimeSample => "time_samples",
                AuthoringWrite::Spline => "spline",
            };
            assert_eq!(after.source, written, "{context}");
            assert_eq!(
                after.layer,
                Some(fixture.layer_name(site.layer_id)),
                "{context}"
            );
            assert_eq!(
                after.site,
                Some(fixture.display(&site.spec_path)),
                "{context}"
            );
        }
        AuthoringVerdict::Masked { by } => {
            let source = &inspection.stronger_sources[by];
            if source.kind == AuthoringSourceKind::AnimationBlock {
                // The written animation is skipped: nothing changes.
                assert_eq!(recorded.after, recorded.before, "{context}");
            } else {
                assert_eq!(
                    after,
                    fixture.seen(prim, &source.key, &source.kind),
                    "{context}"
                );
            }
            if let AuthoringSourceKind::ValueClips { pending, .. } = &source.kind {
                assert!(
                    pending.is_empty(),
                    "{context}: resident metadata settles clips"
                );
            }
        }
        AuthoringVerdict::Unknown { .. } => panic!("{context}: resident metadata settles it"),
        verdict => panic!("{context}: unexpected verdict {verdict:?}"),
    }
}
