// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{
    query::{Arguments, Record},
    stage::{self, QueryError},
};
use layerstack::{InMemoryStore, PropertyPath, StageOptions};
use layerstack_io::OpenOptions;
use layerstack_schemas::{
    BindingOptions, MaterialPurpose, PrimView, Scene, shading::ShadingIssue, usd_shade::Material,
};

pub(super) const HELP: &str = "\
Usage: layerstack material [--json] [--purpose PURPOSE] [--context CONTEXT] STAGE /Prim

Trace the binding that wins for a prim, then the connections from its material's
surface output to a terminal shader. Repeat --context in preference order.
Defaults to all-purpose bindings and the universal surface output.
";

pub(super) fn query(args: Arguments) -> Result<Vec<Record>, QueryError> {
    let [root, path] = args.positional.as_slice() else {
        return Err(QueryError(
            "usage",
            "expected STAGE and /Prim. See 'material --help'".into(),
        ));
    };
    let mut purpose = MaterialPurpose::All;
    let mut contexts = Vec::new();
    for (name, value) in &args.options {
        if name == "--context" {
            contexts.push(value.as_str());
        } else {
            purpose = MaterialPurpose::from_token(value);
        }
    }
    let store = InMemoryStore::default();
    let schemas = layerstack_schemas::openusd(store.tokens());
    let doc = stage::open_with(
        root,
        OpenOptions {
            stage: StageOptions {
                schemas: Some(schemas.into()),
                ..Default::default()
            },
            store: Some(store),
            ..Default::default()
        },
    )?;
    let scene = Scene::new(doc.stage().stage(), doc.store());
    let prim = doc.view().prim(path).ok_or_else(|| {
        QueryError(
            "missing_prim",
            format!("{root}: no composed prim at {path}"),
        )
    })?;
    let store = doc.store();
    let display = |p: PropertyPath| p.display(store.paths(), store.tokens());
    let display_prim = |p| store.paths().display(p, store.tokens());
    let bound = PrimView::new(scene, prim.path())
        .compute_bound_material(&purpose, BindingOptions::default());
    let mut records = vec![];
    let Some(binding) = bound.binding else {
        return Ok(vec![Record {
            kind: "unbound",
            fields: vec![("prim", path.clone())],
        }]);
    };
    records.push(Record {
        kind: "binding",
        fields: vec![
            ("prim", path.clone()),
            ("relationship", display(binding.relationship)),
            ("target", display_prim(binding.target)),
            ("purpose", binding.purpose.as_str().into()),
        ],
    });
    let Some(material) = bound.material.and_then(|p| Material::get(&scene, p)) else {
        records.push(Record {
            kind: "invalidMaterial",
            fields: vec![("target", display_prim(binding.target))],
        });
        return Ok(records);
    };
    let source = material.compute_surface_source(&contexts);
    if let Some(shader) = source.shader {
        records.push(Record {
            kind: "surfaceShader",
            fields: vec![
                ("prim", display_prim(shader)),
                ("context", source.context.unwrap_or_default()),
            ],
        });
    } else {
        records.push(Record {
            kind: "noSurfaceShader",
            fields: vec![("material", display_prim(binding.target))],
        });
    }
    for candidate in source.trace.sources {
        for edge in candidate.chain.windows(2) {
            records.push(Record {
                kind: "connection",
                fields: vec![("from", display(edge[0])), ("to", display(edge[1]))],
            });
        }
    }
    for issue in source.trace.issues {
        let (kind, fields) = match issue {
            ShadingIssue::InvalidTarget { from, target } => (
                "invalidTarget",
                vec![
                    ("from", display(from)),
                    ("target", target.display(store.paths(), store.tokens())),
                ],
            ),
            ShadingIssue::Cycle(p) => ("cycle", vec![("property", display(p))]),
            ShadingIssue::NonContainerInput(p) => {
                ("nonContainerInput", vec![("property", display(p))])
            }
            _ => ("unrecognizedShadingIssue", vec![]),
        };
        records.push(Record { kind, fields });
    }
    Ok(records)
}
