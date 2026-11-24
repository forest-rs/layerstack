// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{
    query::{Arguments, Record},
    stage::{Document, QueryError},
};
use layerstack::{
    LayerStore,
    asset::UdimPattern,
    asset_dependencies::{AssetDependency, AssetDependencyKind, extract_layer_dependencies},
};
use layerstack_io::{
    Filesystem, LoadOptions, OpenOptions, StageDocument, Storage, UdimDiscovery, VariantArcLoading,
};

pub(super) const HELP: &str = "\
Usage: layerstack deps [--json] STAGE

List authored asset uses across loaded layers, including variant branches.
Each row names the source layer, spec, field, authored identifier and status.
Checks loose files and UDIM tiles without reading image contents.
Missing uses stay in the inventory. Resolver expressions and opaque package
resources are marked unchecked. Exit status 2 means the inventory failed.
";

pub(super) fn query(args: Arguments) -> Result<Vec<Record>, QueryError> {
    let [root] = args.positional.as_slice() else {
        return Err(QueryError(
            "usage",
            "expected STAGE. See 'deps --help'".into(),
        ));
    };
    inventory(&open(root, OpenOptions::default())?, root, Inventory::All)
}

pub(super) fn open(root: &str, options: OpenOptions) -> Result<Document, QueryError> {
    let fs = Filesystem::new(".", []).map_err(|e| QueryError("open", e.to_string()))?;
    // Discovery visits authored arcs even when a different variant is selected.
    // A missing arc remains a useful row in the inventory (AOUSD Core §9, §10.3).
    StageDocument::open(
        fs,
        root,
        OpenOptions {
            loading: LoadOptions {
                variant_arcs: VariantArcLoading::All,
                ..Default::default()
            },
            ..options
        },
    )
    .map_err(|e| QueryError("open", format!("{root}: {e}")))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Inventory {
    All,
    Findings,
}

pub(super) fn inventory(
    doc: &Document,
    root: &str,
    mode: Inventory,
) -> Result<Vec<Record>, QueryError> {
    let mut rows = Vec::new();
    let store = doc.store();
    let mut discovery = UdimDiscovery::default();
    for id in store.layer_ids() {
        let layer = store.layer(id).expect("enumerated layer");
        for dep in extract_layer_dependencies(layer, store.paths(), store.tokens()) {
            let kind = match dep.kind {
                AssetDependencyKind::Sublayer => "sublayer",
                AssetDependencyKind::Reference => "reference",
                AssetDependencyKind::Payload => "payload",
                AssetDependencyKind::Value => "value",
                AssetDependencyKind::Identity => "identity",
                AssetDependencyKind::ClipTemplate => "clipTemplate",
                _ => "other",
            };
            let source_fields = || {
                let spec = dep
                    .spec
                    .as_ref()
                    .map_or_else(|| "/".into(), |p| p.display(store.paths(), store.tokens()));
                let mut fields = vec![
                    ("layer", doc.identifier(id).unwrap_or(root).into()),
                    ("spec", spec),
                ];
                if let Some(field) = dep.field {
                    fields.push(("field", store.tokens().resolve(field).into()));
                }
                fields.push(("asset", dep.identifier.to_string()));
                fields
            };
            match resolve(doc, &dep, &mut discovery) {
                Ok(resolutions) => {
                    for (status, resolved) in resolutions {
                        if mode == Inventory::Findings
                            && !matches!(status, "missing" | "failed" | "unchecked")
                        {
                            continue;
                        }
                        let mut fields = source_fields();
                        fields.push(("status", status.into()));
                        if let Some(path) = resolved {
                            fields.push(("resolved", path));
                        }
                        rows.push(Record { kind, fields });
                    }
                }
                Err(error) if mode == Inventory::Findings => {
                    let mut fields = source_fields();
                    fields.push(("status", "failed".into()));
                    fields.push(("message", error.1));
                    rows.push(Record { kind, fields });
                }
                Err(error) => return Err(error),
            }
        }
    }
    if mode == Inventory::All {
        rows.sort_unstable_by(|a, b| a.fields.cmp(&b.fields).then(a.kind.cmp(b.kind)));
    }
    Ok(rows)
}

type Resolution = (&'static str, Option<String>);

fn resolve(
    doc: &Document,
    dep: &AssetDependency,
    discovery: &mut UdimDiscovery,
) -> Result<Vec<Resolution>, QueryError> {
    let asset = dep.identifier.as_ref();
    let anchor = doc.identifier(dep.layer);
    let one = |status, resolved| Ok(vec![(status, resolved)]);
    if dep.kind == AssetDependencyKind::Identity {
        return one("identity", None);
    }
    if let Some(target) = doc
        .store()
        .asset_layer(dep.layer, asset)
        .and_then(|id| doc.identifier(id))
    {
        return one("loaded", Some(target.into()));
    }
    if let Some(failure) = doc
        .load_report()
        .asset_failures
        .iter()
        .find(|f| f.anchor == Some(dep.layer) && f.asset_path == asset)
    {
        return one(
            if failure.is_missing() {
                "missing"
            } else {
                "failed"
            },
            failure.error.identifier.clone(),
        );
    }
    if asset.is_empty() || asset.starts_with('`') || dep.kind == AssetDependencyKind::ClipTemplate {
        return one("unchecked", None);
    }
    if dep.kind == AssetDependencyKind::Value
        && let Some(pattern) = UdimPattern::parse(asset)
    {
        let tiles = doc
            .resolve_udim_tiles(pattern, Some(dep.layer), discovery)
            .map_err(|e| QueryError("asset", format!("{asset}: {e}")))?;
        if tiles.tiles.is_empty() {
            return one("missing", None);
        }
        return Ok(tiles
            .tiles
            .into_iter()
            .map(|tile| ("present", Some(tile.identifier)))
            .collect());
    }
    if anchor.is_some_and(|a| a.contains('[') || a.to_ascii_lowercase().ends_with(".usdz"))
        || asset.contains('[')
        || asset.contains("://")
    {
        return one("unchecked", None);
    }
    let storage = doc.storage();
    let id = storage
        .identify(asset, anchor)
        .map_err(|e| QueryError("asset", format!("{asset}: {e}")))?;
    let exists = storage
        .exists(&id)
        .map_err(|e| QueryError("asset", format!("{id}: {e}")))?;
    one(if exists { "present" } else { "missing" }, Some(id))
}
