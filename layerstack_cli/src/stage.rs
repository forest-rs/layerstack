// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage opening shared by the read-only composition commands.

use layerstack::{
    CompositionError, FlattenRequirements, InMemoryStore, PropertyPath, StageOptions, Time,
    stage::flatten::Instancing,
    value_clips::{ClipAssetRole, ClipAssetStatus, ClipAssetUnavailable},
};
use layerstack_io::diff::{ComparableLayer, DiffSide};
use layerstack_io::usda::{save::usdc_document, writer};
use layerstack_io::{Filesystem, IoErrorKind, OpenOptions, StageDocument};
use std::{collections::BTreeSet, path::Path};

pub(super) type Document = StageDocument<Filesystem>;

#[derive(Debug)]
pub(super) struct QueryError(pub(super) &'static str, pub(super) String);

pub(super) fn schema_options() -> OpenOptions {
    let store = InMemoryStore::default();
    let schemas = layerstack_schemas::openusd(store.tokens());
    OpenOptions {
        stage: StageOptions {
            schemas: Some(schemas.into()),
            ..Default::default()
        },
        store: Some(store),
        ..Default::default()
    }
}

pub(super) fn open(root: &str) -> Result<Document, QueryError> {
    open_with(root, OpenOptions::default())
}

pub(super) fn open_with(root: &str, options: OpenOptions) -> Result<Document, QueryError> {
    let fs = Filesystem::new(".", []).map_err(|e| QueryError("open", e.to_string()))?;
    let doc = StageDocument::open(fs, root, options)
        .map_err(|e| QueryError("open", format!("{root}: {e}")))?;
    if let Some(failure) = doc.load_report().asset_failures.first() {
        let anchor = failure
            .anchor
            .and_then(|id| doc.identifier(id))
            .unwrap_or(root);
        return Err(QueryError(
            "incomplete_stage",
            format!("{anchor}: {failure}"),
        ));
    }
    check_composition(&doc)?;
    Ok(doc)
}

fn check_composition(doc: &Document) -> Result<(), QueryError> {
    // These drops have defined results. Target opinions explain them individually.
    if let Some(error) = doc
        .stage()
        .stage()
        .composition_errors()
        .iter()
        .find(|error| {
            !matches!(
                error,
                CompositionError::InvalidExternalTargetPath(_)
                    | CompositionError::InvalidInstanceTargetPath(_)
            )
        })
    {
        return Err(QueryError(
            "incomplete_stage",
            error.display(doc.store()).to_string(),
        ));
    }
    Ok(())
}

/// Prepare the clip inputs selected by numeric value resolution (AOUSD Core §12.3, §12.5).
pub(super) fn prepare_clip_value(
    doc: &mut Document,
    property: PropertyPath,
    time: Time,
) -> Result<(), String> {
    let prepared = doc.prepare_clip_query(property, time, usize::MAX, usize::MAX);
    if let Some(failure) = prepared.failures.first() {
        return Err(format!("{}: {}", failure.request.identifier, failure.error));
    }
    if let Some(error) = prepared.plan.error {
        return Err(error.to_string());
    }
    if let Some(request) = prepared.plan.demands.iter().map(|d| &d.request).find(|r| {
        matches!(r.status, ClipAssetStatus::Unavailable(_))
            && r.role != ClipAssetRole::TemplateCandidate
    }) {
        return Err(format!("unavailable clip source {}", request.identifier));
    }
    if !prepared.plan.is_complete() {
        return Err("could not resolve clip sources".into());
    }
    Ok(())
}

pub(super) fn diff<R>(
    before: &Path,
    after: &Path,
    compare: impl FnOnce(&ComparableLayer, &ComparableLayer) -> R,
) -> Result<R, (DiffSide, QueryError)> {
    let left = snapshot(before).map_err(|e| (DiffSide::Before, e))?;
    let right = snapshot(after).map_err(|e| (DiffSide::After, e))?;
    Ok(compare(
        &ComparableLayer::from_document(left),
        &ComparableLayer::from_document(right),
    ))
}

/// Compare active composition in stage namespace, retaining all animation in
/// stage time (AOUSD Core §11, §12). Release the source stage before opening
/// the other input. Neither a temporary layer file nor an encoded buffer is needed.
fn snapshot(root: &Path) -> Result<writer::Document, QueryError> {
    let root = root.to_str().ok_or_else(|| {
        QueryError(
            "invalid_identifier",
            "stage identifier must be UTF-8".into(),
        )
    })?;
    let mut doc = open(root)?;
    load_clip_sources(&mut doc)?;
    let flat = doc
        .flatten(&FlattenRequirements {
            instancing: Instancing::Expand,
            ..FlattenRequirements::default()
        })
        .map_err(|e| QueryError("incomplete_stage", e.to_string()))?;
    usdc_document(&flat.layer, doc.store().tokens(), doc.store().paths())
        .map_err(|e| QueryError("unsupported_content", e.to_string()))
}

fn load_clip_sources(doc: &mut Document) -> Result<(), QueryError> {
    let mut attempted = BTreeSet::new();
    loop {
        // Loading a manifest or confirming a template candidate can change the
        // request inventory. Only attempt a given anchored identifier once.
        let request = doc
            .stage()
            .stage()
            .clip_asset_requests()
            .iter()
            .find(|r| {
                !matches!(
                    r.status,
                    ClipAssetStatus::Loaded(_)
                        | ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
                ) && !attempted.contains(&(r.anchor, r.identifier.clone()))
            })
            .cloned();
        let Some(request) = request else { break };
        attempted.insert((request.anchor, request.identifier.clone()));
        match doc.load_asset(&request.identifier, Some(request.anchor)) {
            Ok((_, report)) => {
                if let Some(failure) = report.asset_failures.first() {
                    return Err(QueryError("incomplete_stage", failure.to_string()));
                }
            }
            Err(error)
                if error.kind == IoErrorKind::NotFound
                    && request.role == ClipAssetRole::TemplateCandidate =>
            {
                doc.store_mut()
                    .mark_asset_missing(request.anchor, &request.identifier);
                doc.synchronize();
            }
            Err(error) => {
                return Err(QueryError(
                    "incomplete_stage",
                    format!("clip source {}: {error}", request.identifier),
                ));
            }
        }
    }
    check_composition(doc)
}
