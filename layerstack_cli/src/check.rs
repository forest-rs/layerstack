// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{
    deps,
    query::{Arguments, Record},
    stage::{self, Document, QueryError},
};
use std::collections::BTreeSet;

use layerstack::{CompositionError, LayerId, OpinionRole, PropertyPath, SpecPath, Time};
use layerstack_schemas::{
    Scene,
    subset::MeshTopologyError,
    validation::{
        BuiltinRule, ValidationContext, ValidationProblem, ValidationProblemKind, ValidationScope,
    },
};

pub(super) const HELP: &str = "\
Usage: layerstack check [--json] STAGE

Check authored asset uses, composition errors and composed mesh topology.
Asset discovery visits all variant branches in the layers it can load.
Mesh checks use active composition and USD default time only. Numeric time
samples, interpolation and clip evaluation are outside mesh coverage.
Asset contents such as texture pixels are outside the check's scope.

Exit status: 0 no errors (unchecked uses may remain), 1 errors found,
2 invalid invocation or incomplete check.
";

pub(super) fn query(args: Arguments) -> Result<Vec<Record>, QueryError> {
    let [root] = args.positional.as_slice() else {
        return Err(QueryError(
            "usage",
            "expected STAGE. See 'check --help'".into(),
        ));
    };
    let doc = deps::open(root, stage::schema_options())?;
    let store = doc.store();
    let scene = Scene::new(doc.stage().stage(), store);
    let checks =
        ValidationContext::new([BuiltinRule::CompositionErrors, BuiltinRule::MeshTopology]);
    let validation = checks
        .validate(&scene, ValidationScope::Stage, &[Time::Default])
        .map_err(|e| QueryError("validation", e.to_string()))?;

    let mut records = deps::inventory(&doc, root, deps::Inventory::Findings)?;
    let reported: BTreeSet<_> = records
        .iter()
        .filter(|row| matches!(row.get("status"), Some("missing" | "failed")))
        .filter_map(|row| Some((row.get("layer")?, row.get("asset")?)))
        .collect();
    let mut unmatched = Vec::new();
    for failure in &doc.load_report().asset_failures {
        let layer = failure
            .anchor
            .and_then(|id| doc.identifier(id))
            .unwrap_or(root);
        if !reported.contains(&(layer, &failure.asset_path)) {
            unmatched.push(Record {
                kind: "assetLoad",
                fields: vec![
                    ("layer", layer.into()),
                    ("asset", failure.asset_path.clone()),
                    (
                        "status",
                        if failure.is_missing() {
                            "missing"
                        } else {
                            "failed"
                        }
                        .into(),
                    ),
                    ("message", failure.to_string()),
                ],
            });
        }
    }
    records.extend(unmatched);
    // Strict import accepts missing package dependencies, retaining their
    // resolver diagnostics even when the affected variant is unselected.
    for diagnostic in &doc.load_report().diagnostics {
        if diagnostic.diagnostic.is_error() {
            records.push(Record {
                kind: "assetLoad",
                fields: vec![
                    (
                        "layer",
                        doc.identifier(diagnostic.layer_id).unwrap_or(root).into(),
                    ),
                    ("member", diagnostic.member.to_string()),
                    ("status", "missing".into()),
                    ("message", format!("{:?}", diagnostic.diagnostic)),
                ],
            });
        }
    }
    records.extend(
        validation
            .problems
            .iter()
            .filter_map(|problem| finding(&doc, problem)),
    );
    records.sort_unstable_by(|a, b| a.kind.cmp(b.kind).then(a.fields.cmp(&b.fields)));
    records.dedup_by(|a, b| a.kind == b.kind && a.fields == b.fields);
    let summary = Record {
        kind: "check",
        fields: vec![
            ("stage", root.clone()),
            ("meshTime", "default".into()),
            (
                "meshEvaluations",
                validation.work.mesh_time_evaluations.to_string(),
            ),
            (
                "errors",
                count(&records, |s| matches!(s, "missing" | "error")).to_string(),
            ),
            ("incomplete", count(&records, |s| s == "failed").to_string()),
            (
                "unchecked",
                count(&records, |s| s == "unchecked").to_string(),
            ),
        ],
    };
    records.insert(0, summary);
    Ok(records)
}

fn count(records: &[Record], include: impl Fn(&str) -> bool) -> usize {
    records
        .iter()
        .filter(|row| row.get("status").is_some_and(&include))
        .count()
}

pub(super) fn status(records: &[Record]) -> u8 {
    if records
        .iter()
        .any(|row| row.get("status") == Some("failed"))
    {
        2
    } else {
        u8::from(
            records
                .iter()
                .any(|row| matches!(row.get("status"), Some("error" | "missing"))),
        )
    }
}

fn finding(doc: &Document, problem: &ValidationProblem) -> Option<Record> {
    let store = doc.store();
    let mut fields = Vec::new();
    if let Some(prim) = problem.site.prim {
        let path = problem.site.property.map_or_else(
            || store.paths().display(prim, store.tokens()),
            |property| PropertyPath::new(prim, property).display(store.paths(), store.tokens()),
        );
        fields.push(("path", path));
    }
    let kind = match &problem.kind {
        ValidationProblemKind::Composition(error) => {
            let status = if matches!(
                error,
                CompositionError::LimitExceeded(_) | CompositionError::MissingLayer(_)
            ) {
                "failed"
            } else {
                "error"
            };
            fields.push(("status", status.into()));
            fields.push(("code", error.code().into()));
            let source = match error {
                CompositionError::UnresolvedAsset(e) => {
                    Some((e.introducing_layer, Some(&e.introducing_spec)))
                }
                CompositionError::UnresolvedPrimPath(e) => e
                    .introducing_layer
                    .map(|layer| (layer, Some(&e.introducing_spec))),
                _ => problem
                    .site
                    .source
                    .as_ref()
                    .map(|source| (source.layer, source.spec.as_ref())),
            };
            if let Some((layer, spec)) = source {
                add_source(doc, &mut fields, layer, spec, ("layer", "spec"));
            }
            fields.push(("message", error.display(store).to_string()));
            "composition"
        }
        ValidationProblemKind::MeshTopology(error) => {
            fields.push((
                "status",
                if matches!(error, MeshTopologyError::Decode { .. }) {
                    "failed"
                } else {
                    "error"
                }
                .into(),
            ));
            fields.push(("time", "default".into()));
            let (code, inputs): (_, &[&Input]) = match error {
                MeshTopologyError::InvalidFaceCounts => ("invalidFaceCounts", &[&COUNTS]),
                MeshTopologyError::SizeMismatch => ("faceCountMismatch", &[&COUNTS, &INDICES]),
                MeshTopologyError::InvalidVertexIndex(index) => {
                    fields.push(("index", index.to_string()));
                    ("invalidVertexIndex", &[&INDICES, &POINTS])
                }
                MeshTopologyError::Decode { property, .. } => {
                    fields.push(("property", property.display(store.paths(), store.tokens())));
                    ("decode", &[&COUNTS, &INDICES, &POINTS])
                }
                _ => ("invalidMeshTopology", &[&COUNTS, &INDICES, &POINTS]),
            };
            fields.push(("code", code.into()));
            if let Some(prim) = problem.site.prim.and_then(|p| doc.view().prim(p)) {
                for input in inputs {
                    if let Some(opinion) = prim
                        .attribute(input.name)
                        .and_then(|attribute| attribute.explain(Time::Default))
                        .and_then(|why| {
                            why.opinions.into_iter().find(|op| {
                                matches!(op.role, OpinionRole::Contributed(_) | OpinionRole::Block)
                            })
                        })
                    {
                        add_source(
                            doc,
                            &mut fields,
                            opinion.layer(),
                            Some(&opinion.spec_path()),
                            input.source_fields,
                        );
                    }
                }
            }
            fields.push(("message", error.display(store).to_string()));
            "meshTopology"
        }
        _ => return None,
    };
    Some(Record { kind, fields })
}

struct Input {
    name: &'static str,
    source_fields: (&'static str, &'static str),
}

const COUNTS: Input = Input {
    name: "faceVertexCounts",
    source_fields: ("countsLayer", "countsSpec"),
};
const INDICES: Input = Input {
    name: "faceVertexIndices",
    source_fields: ("indicesLayer", "indicesSpec"),
};
const POINTS: Input = Input {
    name: "points",
    source_fields: ("pointsLayer", "pointsSpec"),
};

fn add_source(
    doc: &Document,
    fields: &mut Vec<(&'static str, String)>,
    layer: LayerId,
    spec: Option<&SpecPath>,
    (layer_field, spec_field): (&'static str, &'static str),
) {
    if let Some(identifier) = doc.identifier(layer) {
        fields.push((layer_field, identifier.into()));
    }
    if let Some(spec) = spec {
        fields.push((
            spec_field,
            spec.display(doc.store().paths(), doc.store().tokens()),
        ));
    }
}
