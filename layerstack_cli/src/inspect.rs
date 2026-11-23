// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{
    query::{Arguments, Record},
    stage::{self, QueryError},
};

pub(super) const HELP: &str = "\
Usage: layerstack inspect [--json] [--limit N] STAGE [ /Prim ]

Browse the stage root or one composed prim. Show direct children and authored
properties, with full paths for a subsequent inspect, explain or material query.
The default limit is 25 children and 25 properties. The header gives totals.
";

pub(super) fn query(args: Arguments) -> Result<Vec<Record>, QueryError> {
    let (root, path) = match args.positional.as_slice() {
        [root] => (root, "/"),
        [root, path] => (root, path.as_str()),
        _ => {
            return Err(QueryError(
                "usage",
                "expected STAGE and an optional /Prim. See 'inspect --help'".into(),
            ));
        }
    };
    let limit = args
        .options
        .last()
        .map_or(Ok(25), |(_, v)| v.parse::<usize>())
        .map_err(|_| {
            QueryError(
                "usage",
                "--limit expects a non-negative whole number".into(),
            )
        })?;
    let doc = stage::open(root)?;
    let store = doc.store();
    let id = store.try_path(path).map_err(|e| {
        QueryError(
            "invalid_path",
            format!("{path}: {e}. Use 'explain' for a property path"),
        )
    })?;
    let prim = doc.view().prim(id).ok_or_else(|| {
        QueryError(
            "missing_prim",
            format!("{root}: no composed prim at {path}"),
        )
    })?;
    let display = |id| store.paths().display(id, store.tokens());
    let children = prim.all_children();
    let properties = prim.authored_properties();
    let mut fields = vec![
        ("stage", root.clone()),
        ("path", path.into()),
        ("children", children.len().to_string()),
        ("properties", properties.len().to_string()),
    ];
    if let Some(ty) = prim.type_name() {
        fields.push(("type", ty.into()));
    }
    if path == "/"
        && let Some(default) = doc.view().default_prim()
    {
        fields.push(("defaultPrim", display(default.path())));
    }
    let mut records = vec![Record {
        kind: if path == "/" { "stage" } else { "prim" },
        fields,
    }];
    for child in children.take(limit) {
        let mut fields = vec![("path", display(child.path()))];
        if let Some(ty) = child.type_name() {
            fields.push(("type", ty.into()));
        }
        if !child.is_active() {
            fields.push(("active", "false".into()));
        }
        records.push(Record {
            kind: "child",
            fields,
        });
    }
    for property in properties.into_iter().take(limit) {
        let mut fields = vec![(
            "path",
            property.path().display(store.paths(), store.tokens()),
        )];
        let kind = if let Some(attr) = property.as_attribute() {
            if let Some(ty) = attr.type_name() {
                fields.push(("type", ty.into()));
            }
            "attribute"
        } else {
            "relationship"
        };
        records.push(Record { kind, fields });
    }
    Ok(records)
}
