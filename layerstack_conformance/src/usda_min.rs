//! Minimal USDA loader for a small subset of the supplemental composition fixtures.
//!
//! This is intentionally not a general USDA parser. It supports only the small
//! patterns needed to exercise early conformance tests:
//! - `subLayers = [@./X.usd@, ...]`
//! - top-level prim defs (`def ... "Name" { ... }`)
//! - basic attribute declarations like `custom double attrName`
//!
//! Spec: AOUSD Core file format semantics are out of scope for the core crate.

use std::path::{Path, PathBuf};

use layerstack::{
    FieldValue, InMemoryStore, Layer, LayerId, ListOp, Path as LsPath, PrimSpec, Reference, Value,
};

#[derive(Debug)]
pub struct LoadedStage {
    pub store: InMemoryStore,
    pub root_layer: LayerId,
    pub layer_names: std::collections::BTreeMap<LayerId, String>,
}

pub fn load_entry_usda(entry: &Path) -> LoadedStage {
    let mut store = InMemoryStore::default();
    let mut next_layer_id = 1_u64;
    let mut by_path: std::collections::BTreeMap<PathBuf, LayerId> =
        std::collections::BTreeMap::new();
    let mut layer_names: std::collections::BTreeMap<LayerId, String> =
        std::collections::BTreeMap::new();
    let root_dir = entry.parent().unwrap_or(Path::new(".")).to_path_buf();

    let root_layer = load_layer_with_prims(
        entry,
        &root_dir,
        &mut store,
        &mut next_layer_id,
        &mut by_path,
        &mut layer_names,
    );

    LoadedStage {
        store,
        root_layer,
        layer_names,
    }
}

/// Loads only the layer stack structure (sublayers), ignoring prim contents.
///
/// This is useful for running `pcp.json` layer-stack expectation tests for
/// fixtures that exercise composition features we have not implemented yet.
pub fn load_entry_usda_sublayers_only(entry: &Path) -> LoadedStage {
    let mut store = InMemoryStore::default();
    let mut next_layer_id = 1_u64;
    let mut by_path: std::collections::BTreeMap<PathBuf, LayerId> =
        std::collections::BTreeMap::new();
    let mut layer_names: std::collections::BTreeMap<LayerId, String> =
        std::collections::BTreeMap::new();
    let root_dir = entry.parent().unwrap_or(Path::new(".")).to_path_buf();

    fn load_layer_sublayers_only(
        path: &Path,
        root_dir: &Path,
        store: &mut InMemoryStore,
        next_layer_id: &mut u64,
        by_path: &mut std::collections::BTreeMap<PathBuf, LayerId>,
        layer_names: &mut std::collections::BTreeMap<LayerId, String>,
    ) -> LayerId {
        let canonical = path.to_path_buf();
        if let Some(id) = by_path.get(&canonical) {
            return *id;
        }

        let id = LayerId(*next_layer_id);
        *next_layer_id += 1;
        by_path.insert(canonical.clone(), id);

        let relative = canonical
            .strip_prefix(root_dir)
            .unwrap_or(canonical.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        layer_names.insert(id, relative);

        let text = std::fs::read_to_string(path).expect("read usda");
        let sublayers = parse_sublayers(&text, path.parent().unwrap_or(Path::new(".")));
        let mut layer = Layer {
            id,
            sublayers: Vec::new(),
            prims: layerstack::HashMap::new(),
        };

        for sub in sublayers {
            let sub_id = load_layer_sublayers_only(
                &sub,
                root_dir,
                store,
                next_layer_id,
                by_path,
                layer_names,
            );
            layer.sublayers.push(sub_id);
        }

        store.insert_layer(layer);
        id
    }

    let root_layer = load_layer_sublayers_only(
        entry,
        &root_dir,
        &mut store,
        &mut next_layer_id,
        &mut by_path,
        &mut layer_names,
    );

    LoadedStage {
        store,
        root_layer,
        layer_names,
    }
}

#[derive(Debug)]
struct PrimDef {
    path: String,
    custom_double_attrs: Vec<String>,
    string_attrs: Vec<String>,
    references: ReferencesDef,
    inherits: InheritsDef,
    targets: TargetsDef,
    declares_targets: bool,
    prim_order: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
struct ReferencesDef {
    explicit: Option<Vec<ReferenceSpec>>,
    prepend: Vec<ReferenceSpec>,
    append: Vec<ReferenceSpec>,
}

#[derive(Clone, Debug)]
struct ReferenceSpec {
    asset: String,
    prim_path: String,
}

#[derive(Clone, Debug, Default)]
struct InheritsDef {
    explicit: Option<Vec<String>>,
    prepend: Vec<String>,
    append: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct TargetsDef {
    explicit: Option<Vec<String>>,
    prepend: Vec<String>,
    append: Vec<String>,
}

#[derive(Clone, Debug)]
struct PendingPrim {
    name: String,
    references: ReferencesDef,
    inherits: InheritsDef,
}

fn parse_sublayers(text: &str, base_dir: &Path) -> Vec<PathBuf> {
    let Some(idx) = text.find("subLayers") else {
        return Vec::new();
    };
    let rest = &text[idx..];
    let Some(lb) = rest.find('[') else {
        return Vec::new();
    };
    let Some(rb) = rest.find(']') else {
        return Vec::new();
    };
    let inside = &rest[lb + 1..rb];

    let mut out = Vec::new();
    for part in inside.split('@') {
        let p = part.trim();
        if p.is_empty() || p.contains("subLayers") {
            continue;
        }
        if p.ends_with(',') {
            // handled by trim below
        }
        let p = p.trim().trim_end_matches(',').trim();
        if p.is_empty() {
            continue;
        }
        let p = p.strip_prefix("./").unwrap_or(p);
        out.push(base_dir.join(p));
    }
    out
}

fn parse_prim_defs(text: &str) -> Vec<PrimDef> {
    let mut out: Vec<PrimDef> = Vec::new();

    let mut scope: Vec<String> = Vec::new();
    let mut brace_stack: Vec<bool> = Vec::new(); // true = prim scope, false = other
    let mut pending: Option<PendingPrim> = None;

    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim();

        if let Some(attr) = parse_double_attr(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                last.custom_double_attrs.push(attr);
            }
        }

        if let Some(attr) = parse_string_attr(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                last.string_attrs.push(attr);
            }
        }

        if (line.starts_with("def ") || line.starts_with("over ") || line.starts_with("class "))
            && let Some(name) = parse_prim_name(line)
        {
            pending = Some(PendingPrim {
                name: name.to_string(),
                references: ReferencesDef::default(),
                inherits: InheritsDef::default(),
            });
        }

        if line.contains('(')
            && let Some(pending) = pending.as_mut()
        {
            while let Some(spec_line) = lines.peek() {
                let spec_line = spec_line.trim();
                if spec_line.starts_with(')') {
                    // Leave the closing line (often `) {`) in the stream so
                    // the main loop can observe the `{` and open the prim scope.
                    break;
                }
                let _ = lines.next();
                if let Some((op, specs)) = parse_references_line(spec_line) {
                    match op {
                        RefOp::Explicit => pending.references.explicit = Some(specs),
                        RefOp::Prepend => pending.references.prepend.extend(specs),
                        RefOp::Append => pending.references.append.extend(specs),
                    }
                }
                if let Some((op, specs)) = parse_inherits_line(spec_line) {
                    match op {
                        InheritOp::Explicit => pending.inherits.explicit = Some(specs),
                        InheritOp::Prepend => pending.inherits.prepend.extend(specs),
                        InheritOp::Append => pending.inherits.append.extend(specs),
                    }
                }
            }
        }

        if line.starts_with("custom rel targets")
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                last.declares_targets = true;
            }
        }

        if let Some((op, specs)) = parse_rel_targets_line(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                match op {
                    TargetOp::Explicit => last.targets.explicit = Some(specs),
                    TargetOp::Prepend => last.targets.prepend.extend(specs),
                    TargetOp::Append => last.targets.append.extend(specs),
                }
            }
        }

        if let Some(order) = parse_reorder_name_children(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                last.prim_order = Some(order);
            }
        }

        for ch in line.chars() {
            match ch {
                '{' => {
                    if let Some(pending) = pending.take() {
                        scope.push(pending.name.clone());
                        brace_stack.push(true);
                        out.push(PrimDef {
                            path: format!("/{}", scope.join("/")),
                            custom_double_attrs: Vec::new(),
                            string_attrs: Vec::new(),
                            references: pending.references,
                            inherits: pending.inherits,
                            targets: TargetsDef::default(),
                            declares_targets: false,
                            prim_order: None,
                        });
                    } else {
                        brace_stack.push(false);
                    }
                }
                '}' => {
                    if brace_stack.pop().is_some_and(|is_prim| is_prim) {
                        let _ = scope.pop();
                    }
                }
                _ => {}
            }
        }
    }

    out
}

fn parse_double_attr(line: &str) -> Option<String> {
    // e.g. `double A_attr`, `custom double A_attr`, or `double radius = 1;`
    let line = line.trim();
    let rest = line
        .strip_prefix("custom double ")
        .or_else(|| line.strip_prefix("double "))?;
    let rest = rest.trim();
    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '=' || ch == ';')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn parse_string_attr(line: &str) -> Option<String> {
    // e.g. `string name`, `custom string name = "x"`, `uniform string name = "x"`,
    // or `string \"v\" = \"ref\"`.
    let line = line.trim();
    let rest = line
        .strip_prefix("custom uniform string ")
        .or_else(|| line.strip_prefix("uniform string "))
        .or_else(|| line.strip_prefix("custom string "))
        .or_else(|| line.strip_prefix("string "))?
        .trim();

    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        let name = rest[..end].trim();
        return (!name.is_empty()).then(|| name.to_string());
    }

    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '=' || ch == ';')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefOp {
    Explicit,
    Prepend,
    Append,
}

fn parse_references_line(line: &str) -> Option<(RefOp, Vec<ReferenceSpec>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if line.is_empty() {
        return None;
    }

    let (op, rest) = if let Some(rest) = line.strip_prefix("references") {
        (RefOp::Explicit, rest)
    } else if let Some(rest) = line.strip_prefix("add references") {
        (RefOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("prepend references") {
        (RefOp::Prepend, rest)
    } else {
        return None;
    };

    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let specs = parse_reference_rhs(rhs)?;
    Some((op, specs))
}

fn parse_reference_rhs(rhs: &str) -> Option<Vec<ReferenceSpec>> {
    let rhs = rhs.trim();
    if rhs.starts_with('[') {
        let lb = rhs.find('[')?;
        let rb = rhs.rfind(']')?;
        let inside = &rhs[lb + 1..rb];
        let mut out = Vec::new();
        for part in inside.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            out.push(parse_reference_spec(part)?);
        }
        Some(out)
    } else {
        Some(vec![parse_reference_spec(rhs)?])
    }
}

fn parse_reference_spec(spec: &str) -> Option<ReferenceSpec> {
    let spec = spec.trim();
    let first_at = spec.find('@')?;
    let rest = &spec[first_at + 1..];
    let second_at = rest.find('@')?;
    let asset = &rest[..second_at];

    let rest = &rest[second_at + 1..];
    let lt = rest.find('<')?;
    let gt = rest.find('>')?;
    let prim_path = rest[lt + 1..gt].trim();

    Some(ReferenceSpec {
        asset: asset.to_string(),
        prim_path: prim_path.to_string(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InheritOp {
    Explicit,
    Prepend,
    Append,
}

fn parse_inherits_line(line: &str) -> Option<(InheritOp, Vec<String>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if line.is_empty() {
        return None;
    }

    let (op, rest) = if let Some(rest) = line.strip_prefix("inherits") {
        (InheritOp::Explicit, rest)
    } else if let Some(rest) = line.strip_prefix("add inherits") {
        (InheritOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("prepend inherits") {
        (InheritOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append inherits") {
        (InheritOp::Append, rest)
    } else {
        return None;
    };

    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let specs = parse_path_list_rhs(rhs)?;
    Some((op, specs))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetOp {
    Explicit,
    Prepend,
    Append,
}

fn parse_rel_targets_line(line: &str) -> Option<(TargetOp, Vec<String>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if line.is_empty() {
        return None;
    }

    let (op, rest) = if let Some(rest) = line.strip_prefix("rel targets") {
        (TargetOp::Explicit, rest)
    } else if let Some(rest) = line.strip_prefix("add rel targets") {
        (TargetOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("prepend rel targets") {
        (TargetOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append rel targets") {
        (TargetOp::Append, rest)
    } else {
        return None;
    };

    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let specs = parse_path_list_rhs(rhs)?;
    Some((op, specs))
}

fn parse_path_list_rhs(rhs: &str) -> Option<Vec<String>> {
    let rhs = rhs.trim();
    if rhs.starts_with('[') {
        let lb = rhs.find('[')?;
        let rb = rhs.rfind(']')?;
        let inside = &rhs[lb + 1..rb];
        let mut out = Vec::new();
        for part in inside.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            out.push(parse_path_spec(part)?.to_string());
        }
        Some(out)
    } else {
        Some(vec![parse_path_spec(rhs)?.to_string()])
    }
}

fn parse_path_spec(spec: &str) -> Option<&str> {
    let spec = spec.trim();
    let lt = spec.find('<')?;
    let gt = spec.find('>')?;
    Some(spec[lt + 1..gt].trim())
}

fn parse_reorder_name_children(line: &str) -> Option<Vec<String>> {
    let line = line.trim().trim_end_matches(',').trim();
    let rest = line.strip_prefix("reorder nameChildren")?;
    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    parse_string_list_rhs(rhs)
}

fn parse_string_list_rhs(rhs: &str) -> Option<Vec<String>> {
    let rhs = rhs.trim();
    let lb = rhs.find('[')?;
    let rb = rhs.rfind(']')?;
    let inside = &rhs[lb + 1..rb];
    let mut out = Vec::new();
    for part in inside.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let unquoted = part.trim_matches('"');
        if !unquoted.is_empty() {
            out.push(unquoted.to_string());
        }
    }
    Some(out)
}

fn resolve_references(
    references: &ReferencesDef,
    base_dir: &Path,
    root_dir: &Path,
    store: &mut InMemoryStore,
    next_layer_id: &mut u64,
    by_path: &mut std::collections::BTreeMap<PathBuf, LayerId>,
    layer_names: &mut std::collections::BTreeMap<LayerId, String>,
) -> ListOp<Reference> {
    ListOp {
        explicit: references.explicit.as_ref().map(|specs| {
            specs
                .iter()
                .map(|spec| {
                    resolve_reference_spec(
                        spec,
                        base_dir,
                        root_dir,
                        store,
                        next_layer_id,
                        by_path,
                        layer_names,
                    )
                })
                .collect()
        }),
        prepend: references
            .prepend
            .iter()
            .map(|spec| {
                resolve_reference_spec(
                    spec,
                    base_dir,
                    root_dir,
                    store,
                    next_layer_id,
                    by_path,
                    layer_names,
                )
            })
            .collect(),
        append: references
            .append
            .iter()
            .map(|spec| {
                resolve_reference_spec(
                    spec,
                    base_dir,
                    root_dir,
                    store,
                    next_layer_id,
                    by_path,
                    layer_names,
                )
            })
            .collect(),
        delete: Vec::new(),
    }
}

fn resolve_path_listop(list: &TargetsDef, store: &mut InMemoryStore) -> ListOp<layerstack::PathId> {
    ListOp {
        explicit: list.explicit.as_ref().map(|specs| {
            specs
                .iter()
                .map(|p| {
                    let path = LsPath::parse_absolute(p, &mut store.tokens).expect("path");
                    store.paths.intern(path)
                })
                .collect()
        }),
        prepend: list
            .prepend
            .iter()
            .map(|p| {
                let path = LsPath::parse_absolute(p, &mut store.tokens).expect("path");
                store.paths.intern(path)
            })
            .collect(),
        append: list
            .append
            .iter()
            .map(|p| {
                let path = LsPath::parse_absolute(p, &mut store.tokens).expect("path");
                store.paths.intern(path)
            })
            .collect(),
        delete: Vec::new(),
    }
}

fn resolve_inherits_listop(
    list: &InheritsDef,
    store: &mut InMemoryStore,
) -> ListOp<layerstack::PathId> {
    let targets = TargetsDef {
        explicit: list.explicit.clone(),
        prepend: list.prepend.clone(),
        append: list.append.clone(),
    };
    resolve_path_listop(&targets, store)
}

fn resolve_reference_spec(
    spec: &ReferenceSpec,
    base_dir: &Path,
    root_dir: &Path,
    store: &mut InMemoryStore,
    next_layer_id: &mut u64,
    by_path: &mut std::collections::BTreeMap<PathBuf, LayerId>,
    layer_names: &mut std::collections::BTreeMap<LayerId, String>,
) -> Reference {
    let asset = spec.asset.trim();
    let asset_rel = asset.strip_prefix("./").unwrap_or(asset);
    let asset_path = base_dir.join(asset_rel);

    let layer = load_layer_with_prims(
        &asset_path,
        root_dir,
        store,
        next_layer_id,
        by_path,
        layer_names,
    );

    let prim =
        LsPath::parse_absolute(spec.prim_path.trim(), &mut store.tokens).expect("reference path");
    let prim_path = store.paths.intern(prim);

    Reference {
        layer,
        prim_path,
        asset: Some(asset.to_string()),
    }
}

fn parse_prim_name(line: &str) -> Option<&str> {
    let first_quote = line.find('"')?;
    let rest = &line[first_quote + 1..];
    let second_quote = rest.find('"')?;
    Some(&rest[..second_quote])
}

fn load_layer_with_prims(
    path: &Path,
    root_dir: &Path,
    store: &mut InMemoryStore,
    next_layer_id: &mut u64,
    by_path: &mut std::collections::BTreeMap<PathBuf, LayerId>,
    layer_names: &mut std::collections::BTreeMap<LayerId, String>,
) -> LayerId {
    let canonical = path.to_path_buf();
    if let Some(id) = by_path.get(&canonical) {
        return *id;
    }

    let id = LayerId(*next_layer_id);
    *next_layer_id += 1;
    by_path.insert(canonical.clone(), id);
    let relative = canonical
        .strip_prefix(root_dir)
        .unwrap_or(canonical.as_path())
        .to_string_lossy()
        .replace('\\', "/");
    layer_names.insert(id, relative);

    let text = std::fs::read_to_string(path).expect("read usda");
    let sublayers = parse_sublayers(&text, path.parent().unwrap_or(Path::new(".")));
    let mut layer = Layer {
        id,
        sublayers: Vec::new(),
        prims: layerstack::HashMap::new(),
    };

    for sub in sublayers {
        let sub_id =
            load_layer_with_prims(&sub, root_dir, store, next_layer_id, by_path, layer_names);
        layer.sublayers.push(sub_id);
    }

    let prim_defs = parse_prim_defs(&text);
    let mut prim_defs_with_ids = Vec::new();
    let mut children_by_parent: std::collections::HashMap<
        layerstack::PathId,
        Vec<layerstack::TokenId>,
    > = std::collections::HashMap::new();

    for prim in prim_defs {
        let prim_path = {
            let path = LsPath::parse_absolute(&prim.path, &mut store.tokens).expect("path");
            store.paths.intern(path)
        };

        if let Some(parent) = store.paths.resolve(prim_path).parent() {
            let parent_id = store.paths.intern(parent);
            if let Some(name) = store.paths.resolve(prim_path).leaf() {
                let list = children_by_parent.entry(parent_id).or_default();
                if !list.contains(&name) {
                    list.push(name);
                }
            }
        }

        prim_defs_with_ids.push((prim_path, prim));
    }

    for (prim_path, prim) in prim_defs_with_ids {
        let mut spec = PrimSpec {
            authored_children: children_by_parent.remove(&prim_path).unwrap_or_default(),
            ..PrimSpec::default()
        };
        for attr in prim.custom_double_attrs {
            let tok = store.tokens.intern(attr);
            spec.fields
                .insert(tok, FieldValue::Value(Value::Float(0.0)));
        }
        for attr in prim.string_attrs {
            let tok = store.tokens.intern(attr);
            spec.fields
                .insert(tok, FieldValue::Value(Value::String("".into())));
        }
        spec.references = resolve_references(
            &prim.references,
            path.parent().unwrap_or(Path::new(".")),
            root_dir,
            store,
            next_layer_id,
            by_path,
            layer_names,
        );
        spec.inherits = resolve_inherits_listop(&prim.inherits, store);
        spec.prim_order = prim
            .prim_order
            .as_ref()
            .map(|names| names.iter().map(|n| store.tokens.intern(n)).collect());

        if prim.declares_targets
            || prim.targets.explicit.is_some()
            || !prim.targets.prepend.is_empty()
            || !prim.targets.append.is_empty()
        {
            let targets_token = store.tokens.intern("targets");
            spec.fields.insert(
                targets_token,
                FieldValue::PathListOp(resolve_path_listop(&prim.targets, store)),
            );
        }
        layer.prims.insert(prim_path, spec);
    }

    store.insert_layer(layer);
    id
}
