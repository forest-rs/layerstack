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
    specifier: layerstack::doc::Specifier,
    attrs: Vec<String>,
    /// TimeSamples: (attr_name, sorted samples).
    time_samples: Vec<(String, Vec<(f64, String)>)>,
    references: ReferencesDef,
    inherits: InheritsDef,
    specializes: SpecializesDef,
    payloads: PayloadsDef,
    targets: TargetsDef,
    declares_targets: bool,
    /// Named relationships: (rel_name, op, target_paths).
    named_rels: Vec<(String, TargetOp, Vec<String>)>,
    /// Attribute connections: (attr_name, op, target_paths).
    connections: Vec<(String, TargetOp, Vec<String>)>,
    prim_order: Option<Vec<String>>,
    variant_selections: Vec<(String, String)>,
    variant_set_names: Vec<String>,
    /// If this prim is inside a variant branch: (parent_path, set_name, branch_name).
    variant_parent: Option<(String, String, String)>,
    /// Fields defined inside variant branches of this prim: (set_name, branch_name, attr_name).
    variant_fields: Vec<(String, String, String)>,
    /// Composition arcs on child prims inside variant branches of this prim.
    /// (set_name, branch_name, child_name, references, inherits, specializes, payloads).
    variant_child_arcs: Vec<VariantChildArc>,
    /// Composition arcs on variant branch headers (e.g. `"full" (add references = ...) {}`).
    /// These apply to the prim itself when the variant is selected.
    variant_branch_arcs: Vec<VariantBranchArc>,
}

#[derive(Clone, Debug)]
struct VariantChildArc {
    set_name: String,
    branch_name: String,
    child_name: String,
    references: ReferencesDef,
    inherits: InheritsDef,
    specializes: SpecializesDef,
    payloads: PayloadsDef,
}

#[derive(Clone, Debug)]
struct VariantBranchArc {
    set_name: String,
    branch_name: String,
    references: ReferencesDef,
    inherits: InheritsDef,
    specializes: SpecializesDef,
    payloads: PayloadsDef,
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
struct SpecializesDef {
    explicit: Option<Vec<String>>,
    prepend: Vec<String>,
    append: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct PayloadsDef {
    explicit: Option<Vec<ReferenceSpec>>,
    prepend: Vec<ReferenceSpec>,
    append: Vec<ReferenceSpec>,
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
    specifier: layerstack::doc::Specifier,
    references: ReferencesDef,
    inherits: InheritsDef,
    specializes: SpecializesDef,
    payloads: PayloadsDef,
    variant_selections: Vec<(String, String)>,
    variant_set_names: Vec<String>,
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

/// Brace-level context for tracking what each `{` ... `}` block represents.
#[derive(Clone, Debug)]
enum BraceKind {
    /// A prim scope (def/over/class).
    Prim,
    /// A `variantSet "name" = { ... }` block.
    VariantSet(String),
    /// A variant branch `"name" { ... }` inside a variant set.
    VariantBranch(String, String), // (set_name, branch_name)
    /// Any other brace scope (e.g., `variants = { ... }`).
    Other,
}

fn parse_prim_defs(text: &str) -> Vec<PrimDef> {
    let mut out: Vec<PrimDef> = Vec::new();

    let mut scope: Vec<String> = Vec::new();
    let mut brace_stack: Vec<BraceKind> = Vec::new();
    let mut pending: Option<PendingPrim> = None;

    // Current variant context: if we're inside a variant branch, this tracks
    // the owning prim path, set name, and branch name.
    fn current_variant_context(brace_stack: &[BraceKind]) -> Option<(String, String)> {
        for kind in brace_stack.iter().rev() {
            if let BraceKind::VariantBranch(set, branch) = kind {
                return Some((set.clone(), branch.clone()));
            }
        }
        None
    }

    // Find the owning prim path for the current scope (skip variant set/branch braces).
    fn owning_prim_path(scope: &[String]) -> String {
        if scope.is_empty() {
            String::new()
        } else {
            format!("/{}", scope.join("/"))
        }
    }

    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim();

        // Check if we're inside a variant branch — attrs at the variant-owning
        // prim's level should be routed to that prim's variant_fields, not its
        // regular fields. But attrs on child prims nested within a variant
        // branch are regular attrs of those child prims.
        let variant_ctx = current_variant_context(&brace_stack);

        // Count how many Prim-level braces sit above the innermost VariantBranch.
        let prim_depth_in_variant = {
            let mut depth = 0_u32;
            let mut found_branch = false;
            for kind in brace_stack.iter().rev() {
                match kind {
                    BraceKind::VariantBranch(..) => {
                        found_branch = true;
                        break;
                    }
                    BraceKind::Prim => depth += 1,
                    _ => {}
                }
            }
            if found_branch { depth } else { 0 }
        };

        if let Some((set_name, branch_name)) = variant_ctx.as_ref()
            && prim_depth_in_variant == 0
        {
            // Directly inside a variant branch (no child prim nesting).
            let parent_path = owning_prim_path(&scope);
            if let Some(attr) = parse_any_attr(line)
                && let Some(parent_def) = out.iter_mut().rev().find(|d| d.path == parent_path)
            {
                parent_def.variant_fields.push((
                    set_name.clone(),
                    branch_name.clone(),
                    attr,
                ));
            }
        } else {
            // Regular prim scope (or a child prim nested inside a variant branch).
            if let Some(attr) = parse_any_attr(line)
                && let Some(last) = out.last_mut()
                && !scope.is_empty()
            {
                let current_path = format!("/{}", scope.join("/"));
                if last.path == current_path && !last.attrs.contains(&attr) {
                    last.attrs.push(attr);
                }
            }
        }

        // Parse timeSamples: `<type> <name>.timeSamples = {`
        if line.contains(".timeSamples")
            && let Some(attr_name) = parse_time_samples_attr(line)
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            // Accumulate time sample entries until closing `}`.
            let mut samples: Vec<(f64, String)> = Vec::new();

            // Check if entries are on the same line as the opening brace.
            if let Some(brace_pos) = line.find('{') {
                let inline = line[brace_pos + 1..].trim();
                if !inline.is_empty() && !inline.starts_with('}') {
                    for entry in inline.trim_end_matches('}').split(',') {
                        if let Some(sample) = parse_time_sample_entry(entry.trim()) {
                            samples.push(sample);
                        }
                    }
                }
            }

            // Read subsequent lines if the block wasn't closed inline.
            if !line.contains('}') || line.rfind('{') > line.rfind('}') {
                while let Some(sample_line) = lines.peek() {
                    let sample_line = sample_line.trim();
                    if sample_line.starts_with('}') {
                        lines.next();
                        break;
                    }
                    let consumed = lines.next().unwrap().trim().to_string();
                    for entry in consumed.split(',') {
                        let entry = entry.trim();
                        if !entry.is_empty()
                            && let Some(sample) = parse_time_sample_entry(entry)
                        {
                            samples.push(sample);
                        }
                    }
                }
            }

            // Sort by time and add to current prim.
            samples.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
            if let Some(def) = out.iter_mut().rev().find(|d| d.path == current_path) {
                // Also register the attr name so it shows up as a known field.
                if !def.attrs.contains(&attr_name) {
                    def.attrs.push(attr_name.clone());
                }
                def.time_samples.push((attr_name, samples));
            }
        }

        if (line.starts_with("def ") || line.starts_with("over ") || line.starts_with("class "))
            && let Some(name) = parse_prim_name(line)
        {
            let specifier = if line.starts_with("def ") {
                layerstack::doc::Specifier::Def
            } else if line.starts_with("class ") {
                layerstack::doc::Specifier::Class
            } else {
                layerstack::doc::Specifier::Over
            };
            pending = Some(PendingPrim {
                name: name.to_string(),
                specifier,
                references: ReferencesDef::default(),
                inherits: InheritsDef::default(),
                specializes: SpecializesDef::default(),
                payloads: PayloadsDef::default(),
                variant_selections: Vec::new(),
                variant_set_names: Vec::new(),
            });
        }

        if line.contains('(')
            && let Some(pending) = pending.as_mut()
        {
            let mut meta_lines: Vec<String> = Vec::new();
            if let (Some(open), Some(close)) = (line.find('('), line.rfind(')')) {
                if close > open {
                    let inline = line[open + 1..close].trim();
                    if !inline.is_empty() {
                        meta_lines.push(inline.to_string());
                    }
                }
            } else {
                let mut accumulator: Option<String> = None;
                while let Some(spec_line) = lines.peek() {
                    let spec_line = spec_line.trim();
                    if spec_line.starts_with(')') {
                        break;
                    }
                    let consumed = lines.next().unwrap().trim().to_string();

                    if let Some(ref mut acc) = accumulator {
                        acc.push(' ');
                        acc.push_str(&consumed);
                        if consumed.contains(']') || consumed.contains('}') {
                            meta_lines.push(acc.clone());
                            accumulator = None;
                        }
                    } else if (consumed.contains('[') && !consumed.contains(']'))
                        || (consumed.starts_with("variants") && consumed.contains('{') && !consumed.contains('}'))
                    {
                        accumulator = Some(consumed);
                    } else {
                        meta_lines.push(consumed);
                    }
                }
                if let Some(acc) = accumulator {
                    meta_lines.push(acc);
                }
            }

            for spec_line in &meta_lines {
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
                if let Some((op, specs)) = parse_specializes_line(spec_line) {
                    match op {
                        InheritOp::Explicit => pending.specializes.explicit = Some(specs),
                        InheritOp::Prepend => pending.specializes.prepend.extend(specs),
                        InheritOp::Append => pending.specializes.append.extend(specs),
                    }
                }
                if let Some((op, specs)) = parse_payloads_line(spec_line) {
                    match op {
                        RefOp::Explicit => pending.payloads.explicit = Some(specs),
                        RefOp::Prepend => pending.payloads.prepend.extend(specs),
                        RefOp::Append => pending.payloads.append.extend(specs),
                    }
                }
                // Parse variant selections: `variants = { string v1 = "C", string v2 = "Z" }`
                if let Some(sels) = parse_variant_selections_line(spec_line) {
                    pending.variant_selections.extend(sels);
                }
                // Parse variant set names: `variantSets = ["v1", "v2"]`
                // or `add variantSets = "costume"`
                if let Some(names) = parse_variant_set_names_line(spec_line) {
                    pending.variant_set_names.extend(names);
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

        // Parse named relationship lines: `add rel controls = </path>`
        if let Some((name, op, specs)) = parse_named_rel_line(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                if !last.attrs.contains(&name) {
                    last.attrs.push(name.clone());
                }
                last.named_rels.push((name, op, specs));
            }
        }

        // Parse connection lines: `add double focalLength.connect = </path>`
        if let Some((name, op, specs)) = parse_connection_line(line)
            && let Some(last) = out.last_mut()
            && !scope.is_empty()
        {
            let current_path = format!("/{}", scope.join("/"));
            if last.path == current_path {
                if !last.attrs.contains(&name) {
                    last.attrs.push(name.clone());
                }
                last.connections.push((name, op, specs));
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

        // Detect `variantSet "name" = {` before generic brace handling.
        let variant_set_name = parse_variant_set_start(line);
        // Detect `"branchName" {` or `"branchName" (metadata) {` inside a variant set.
        let variant_branch_name = if variant_set_name.is_none()
            && pending.is_none()
            && brace_stack.last().map_or(false, |k| matches!(k, BraceKind::VariantSet(_)))
        {
            parse_variant_branch_name(line)
        } else {
            None
        };

        // Parse variant branch header metadata: `"branchName" (add references = ...) {`
        if let Some(ref branch_name) = variant_branch_name
            && line.contains('(')
        {
            let set_name = match brace_stack.last() {
                Some(BraceKind::VariantSet(s)) => s.clone(),
                _ => String::new(),
            };
            let owning_path = format!("/{}", scope.join("/"));
            let mut meta_lines: Vec<String> = Vec::new();

            // Check for inline metadata `"branch" (... metadata ...) {`
            if let (Some(open), Some(close)) = (line.find('('), line.rfind(')')) {
                if close > open {
                    let inline = line[open + 1..close].trim();
                    if !inline.is_empty() {
                        meta_lines.push(inline.to_string());
                    }
                }
            } else {
                // Multi-line metadata: read until `)`
                let mut accumulator: Option<String> = None;
                while let Some(spec_line) = lines.peek() {
                    let spec_line = spec_line.trim();
                    if spec_line.starts_with(')') {
                        lines.next();
                        break;
                    }
                    let consumed = lines.next().unwrap().trim().to_string();
                    if let Some(ref mut acc) = accumulator {
                        acc.push(' ');
                        acc.push_str(&consumed);
                        if consumed.contains(']') || consumed.contains('}') {
                            meta_lines.push(acc.clone());
                            accumulator = None;
                        }
                    } else if (consumed.contains('[') && !consumed.contains(']'))
                        || (consumed.starts_with("variants")
                            && consumed.contains('{')
                            && !consumed.contains('}'))
                    {
                        accumulator = Some(consumed);
                    } else {
                        meta_lines.push(consumed);
                    }
                }
                if let Some(acc) = accumulator {
                    meta_lines.push(acc);
                }
            }

            let mut branch_refs = ReferencesDef::default();
            let mut branch_inherits = InheritsDef::default();
            let mut branch_specializes = SpecializesDef::default();
            let mut branch_payloads = PayloadsDef::default();

            for meta_line in &meta_lines {
                if let Some((op, specs)) = parse_references_line(meta_line) {
                    match op {
                        RefOp::Explicit => branch_refs.explicit = Some(specs),
                        RefOp::Prepend => branch_refs.prepend.extend(specs),
                        RefOp::Append => branch_refs.append.extend(specs),
                    }
                }
                if let Some((op, specs)) = parse_inherits_line(meta_line) {
                    match op {
                        InheritOp::Explicit => branch_inherits.explicit = Some(specs),
                        InheritOp::Prepend => branch_inherits.prepend.extend(specs),
                        InheritOp::Append => branch_inherits.append.extend(specs),
                    }
                }
                if let Some((op, specs)) = parse_specializes_line(meta_line) {
                    match op {
                        InheritOp::Explicit => branch_specializes.explicit = Some(specs),
                        InheritOp::Prepend => branch_specializes.prepend.extend(specs),
                        InheritOp::Append => branch_specializes.append.extend(specs),
                    }
                }
                if let Some((op, specs)) = parse_payloads_line(meta_line) {
                    match op {
                        RefOp::Explicit => branch_payloads.explicit = Some(specs),
                        RefOp::Prepend => branch_payloads.prepend.extend(specs),
                        RefOp::Append => branch_payloads.append.extend(specs),
                    }
                }
            }

            let has_arcs = branch_refs.explicit.is_some()
                || !branch_refs.prepend.is_empty()
                || !branch_refs.append.is_empty()
                || branch_inherits.explicit.is_some()
                || !branch_inherits.prepend.is_empty()
                || !branch_inherits.append.is_empty()
                || branch_specializes.explicit.is_some()
                || !branch_specializes.prepend.is_empty()
                || !branch_specializes.append.is_empty()
                || branch_payloads.explicit.is_some()
                || !branch_payloads.prepend.is_empty()
                || !branch_payloads.append.is_empty();

            if has_arcs {
                // Find the owning prim's PrimDef and add the variant branch arcs.
                if let Some(owner_def) = out.iter_mut().rev().find(|d| d.path == owning_path) {
                    owner_def.variant_branch_arcs.push(VariantBranchArc {
                        set_name,
                        branch_name: branch_name.clone(),
                        references: branch_refs,
                        inherits: branch_inherits,
                        specializes: branch_specializes,
                        payloads: branch_payloads,
                    });
                }
            }
        }

        for ch in line.chars() {
            match ch {
                '{' => {
                    if let Some(pending) = pending.take() {
                        let variant_parent = current_variant_context(&brace_stack)
                            .map(|(set, branch)| (owning_prim_path(&scope), set, branch));
                        scope.push(pending.name.clone());
                        brace_stack.push(BraceKind::Prim);
                        out.push(PrimDef {
                            path: format!("/{}", scope.join("/")),
                            specifier: pending.specifier,
                            attrs: Vec::new(),
                            time_samples: Vec::new(),
                            references: pending.references,
                            inherits: pending.inherits,
                            specializes: pending.specializes,
                            payloads: pending.payloads,
                            targets: TargetsDef::default(),
                            declares_targets: false,
                            named_rels: Vec::new(),
                            connections: Vec::new(),
                            prim_order: None,
                            variant_selections: pending.variant_selections,
                            variant_set_names: pending.variant_set_names,
                            variant_parent,
                            variant_fields: Vec::new(),
                            variant_child_arcs: Vec::new(),
                            variant_branch_arcs: Vec::new(),
                        });
                    } else if let Some(ref set_name) = variant_set_name {
                        brace_stack.push(BraceKind::VariantSet(set_name.clone()));
                    } else if let Some(ref branch) = variant_branch_name {
                        let set_name = match brace_stack.last() {
                            Some(BraceKind::VariantSet(s)) => s.clone(),
                            _ => String::new(),
                        };
                        brace_stack.push(BraceKind::VariantBranch(set_name, branch.clone()));
                    } else {
                        brace_stack.push(BraceKind::Other);
                    }
                }
                '}' => {
                    if let Some(kind) = brace_stack.pop()
                        && matches!(kind, BraceKind::Prim)
                    {
                        let _ = scope.pop();
                    }
                }
                _ => {}
            }
        }
    }

    out
}

/// Parses `variants = { string v1 = "C", string v2 = "Z" }` from a metadata line.
/// Handles both comma-separated and space-separated entries (multi-line metadata
/// gets joined with spaces by the accumulator).
fn parse_variant_selections_line(line: &str) -> Option<Vec<(String, String)>> {
    let line = line.trim();
    let rest = line.strip_prefix("variants")?;
    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let rhs = rhs.trim();
    let lb = rhs.find('{')?;
    let rb = rhs.rfind('}')?;
    let inside = &rhs[lb + 1..rb];

    let mut selections = Vec::new();
    // Split on `string ` boundaries to handle both comma and space separation.
    for part in inside.split("string ") {
        let part = part.trim().trim_end_matches(',').trim();
        if part.is_empty() {
            continue;
        }
        // Parse `setName = "value"`
        let (name, val_part) = part.split_once('=')?;
        let name = name.trim();
        let val = val_part.trim().trim_matches('"');
        if !name.is_empty() && !val.is_empty() {
            selections.push((name.to_string(), val.to_string()));
        }
    }
    Some(selections)
}

/// Parses `variantSets = ["v1", "v2"]` or `add variantSets = "costume"` from a metadata line.
fn parse_variant_set_names_line(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    let rest = line
        .strip_prefix("add variantSets")
        .or_else(|| line.strip_prefix("prepend variantSets"))
        .or_else(|| line.strip_prefix("variantSets"))?;
    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let rhs = rhs.trim();
    if rhs.starts_with('[') {
        parse_string_list_rhs(rhs)
    } else {
        // Single unquoted or quoted value: `"costume"`
        let val = rhs.trim_matches('"');
        if val.is_empty() {
            None
        } else {
            Some(vec![val.to_string()])
        }
    }
}

/// Detects `variantSet "name" = {` and returns the set name.
fn parse_variant_set_start(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("variantSet ")?;
    let first_quote = rest.find('"')?;
    let after = &rest[first_quote + 1..];
    let second_quote = after.find('"')?;
    let name = &after[..second_quote];
    // Verify there's an `=` and `{` after the name
    let remaining = &after[second_quote + 1..];
    if remaining.contains('=') {
        Some(name.to_string())
    } else {
        None
    }
}

/// Detects `"branchName" {` inside a variant set.
fn parse_variant_branch_name(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with('"') {
        return None;
    }
    let rest = &line[1..];
    let end = rest.find('"')?;
    let name = &rest[..end];
    let after = rest[end + 1..].trim();
    // Should be followed by `{`, `(` (metadata), or be at end of line.
    if after.starts_with('{') || after.starts_with('(') || after.is_empty() {
        Some(name.to_string())
    } else {
        None
    }
}

/// Recognised USDA attribute type keywords (including `custom`/`uniform` prefixes).
const ATTR_TYPE_PREFIXES: &[&str] = &[
    "custom uniform token ",
    "custom uniform string ",
    "custom uniform int ",
    "custom uniform double ",
    "custom uniform float ",
    "custom uniform bool ",
    "uniform token ",
    "uniform string ",
    "uniform int ",
    "uniform double ",
    "uniform float ",
    "uniform bool ",
    "custom double ",
    "custom float ",
    "custom int ",
    "custom string ",
    "custom token ",
    "custom bool ",
    "double ",
    "float ",
    "int ",
    "string ",
    "token ",
    "bool ",
];

fn parse_any_attr(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = ATTR_TYPE_PREFIXES
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))?
        .trim();

    // Handle quoted attribute names like `string "v" = "ref"`.
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        let name = rest[..end].trim();
        return (!name.is_empty()).then(|| name.to_string());
    }

    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '=' || ch == ';' || ch == '.')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Parse an attribute name from a `.timeSamples = {` line.
/// e.g. `int root.timeSamples = {` → `"root"`.
fn parse_time_samples_attr(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = ATTR_TYPE_PREFIXES
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))?
        .trim();
    let dot_pos = rest.find(".timeSamples")?;
    let name = rest[..dot_pos].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Parse a single time sample entry like `-10:100` or `0:200`.
fn parse_time_sample_entry(entry: &str) -> Option<(f64, String)> {
    let entry = entry.trim().trim_end_matches(',');
    let colon = entry.find(':')?;
    let time_str = entry[..colon].trim();
    let value_str = entry[colon + 1..].trim();
    let time: f64 = time_str.parse().ok()?;
    Some((time, value_str.to_string()))
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

    // Try asset reference first: @asset@</Path>
    if let Some(first_at) = spec.find('@') {
        let rest = &spec[first_at + 1..];
        let second_at = rest.find('@')?;
        let asset = &rest[..second_at];

        let rest = &rest[second_at + 1..];
        let lt = rest.find('<')?;
        let gt = rest.find('>')?;
        let prim_path = rest[lt + 1..gt].trim();

        return Some(ReferenceSpec {
            asset: asset.to_string(),
            prim_path: prim_path.to_string(),
        });
    }

    // Internal reference: </Path> (no asset)
    let lt = spec.find('<')?;
    let gt = spec.find('>')?;
    let prim_path = spec[lt + 1..gt].trim();
    Some(ReferenceSpec {
        asset: String::new(),
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

/// Parses named relationship lines like `add rel controls = </path>`.
fn parse_named_rel_line(line: &str) -> Option<(String, TargetOp, Vec<String>)> {
    let line = line.trim().trim_end_matches(',').trim();
    // Must contain "rel " but NOT be a "rel targets" line.
    let (op, rest) = if let Some(rest) = line.strip_prefix("add rel ") {
        (TargetOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("prepend rel ") {
        (TargetOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append rel ") {
        (TargetOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("custom rel ") {
        // `custom rel name` without `=` just declares the rel.
        if !rest.contains('=') {
            return None;
        }
        (TargetOp::Explicit, rest)
    } else if let Some(rest) = line.strip_prefix("rel ") {
        (TargetOp::Explicit, rest)
    } else {
        return None;
    };
    // Skip "targets" — handled by parse_rel_targets_line.
    if rest.starts_with("targets") {
        return None;
    }
    let (name, rhs) = rest.split_once('=')?;
    let name = name.trim().to_string();
    let specs = parse_path_list_rhs(rhs.trim())?;
    Some((name, op, specs))
}

/// Parses connection lines like `add double focalLength.connect = </path>`.
fn parse_connection_line(line: &str) -> Option<(String, TargetOp, Vec<String>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if !line.contains(".connect") {
        return None;
    }
    // Strip optional list-op prefix.
    let (op, rest) = if let Some(rest) = line.strip_prefix("add ") {
        (TargetOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("prepend ") {
        (TargetOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append ") {
        (TargetOp::Append, rest)
    } else {
        (TargetOp::Explicit, line)
    };
    // Strip optional type prefix.
    let rest = ATTR_TYPE_PREFIXES
        .iter()
        .find_map(|prefix| rest.strip_prefix(prefix))
        .unwrap_or(rest)
        .trim();
    // Find "name.connect".
    let dot_pos = rest.find(".connect")?;
    let name = rest[..dot_pos].trim().to_string();
    let after = &rest[dot_pos + ".connect".len()..];
    let rhs = after.split_once('=')?.1.trim();
    let specs = parse_path_list_rhs(rhs)?;
    Some((name, op, specs))
}

fn parse_specializes_line(line: &str) -> Option<(InheritOp, Vec<String>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if line.is_empty() {
        return None;
    }

    let (op, rest) = if let Some(rest) = line.strip_prefix("prepend specializes") {
        (InheritOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append specializes") {
        (InheritOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("add specializes") {
        (InheritOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("specializes") {
        (InheritOp::Explicit, rest)
    } else {
        return None;
    };

    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let specs = parse_path_list_rhs(rhs)?;
    Some((op, specs))
}

fn parse_payloads_line(line: &str) -> Option<(RefOp, Vec<ReferenceSpec>)> {
    let line = line.trim().trim_end_matches(',').trim();
    if line.is_empty() {
        return None;
    }

    let (op, rest) = if let Some(rest) = line.strip_prefix("prepend payload") {
        (RefOp::Prepend, rest)
    } else if let Some(rest) = line.strip_prefix("append payload") {
        (RefOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("add payload") {
        (RefOp::Append, rest)
    } else if let Some(rest) = line.strip_prefix("payload") {
        (RefOp::Explicit, rest)
    } else {
        return None;
    };

    // Strip trailing 's' from "payloads" if present (keyword may be "payload" or "payloads").
    let rest = rest.strip_prefix('s').unwrap_or(rest);

    let rhs = rest.split_once('=').map(|(_, rhs)| rhs.trim())?;
    let specs = parse_reference_rhs(rhs)?;
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
    current_layer_id: LayerId,
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
                        current_layer_id,
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
                    current_layer_id,
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
                    current_layer_id,
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
    prim_path: &str,
) -> ListOp<layerstack::PathId> {
    // Resolve relative paths (e.g. `../SymEyeRig`) relative to the prim path.
    let resolved = InheritsDef {
        explicit: list
            .explicit
            .as_ref()
            .map(|v| v.iter().map(|p| resolve_relative_path(p, prim_path)).collect()),
        prepend: list
            .prepend
            .iter()
            .map(|p| resolve_relative_path(p, prim_path))
            .collect(),
        append: list
            .append
            .iter()
            .map(|p| resolve_relative_path(p, prim_path))
            .collect(),
    };
    let targets = TargetsDef {
        explicit: resolved.explicit,
        prepend: resolved.prepend,
        append: resolved.append,
    };
    resolve_path_listop(&targets, store)
}

fn resolve_specializes_listop(
    list: &SpecializesDef,
    store: &mut InMemoryStore,
    prim_path: &str,
) -> ListOp<layerstack::PathId> {
    let resolved = SpecializesDef {
        explicit: list
            .explicit
            .as_ref()
            .map(|v| v.iter().map(|p| resolve_relative_path(p, prim_path)).collect()),
        prepend: list
            .prepend
            .iter()
            .map(|p| resolve_relative_path(p, prim_path))
            .collect(),
        append: list
            .append
            .iter()
            .map(|p| resolve_relative_path(p, prim_path))
            .collect(),
    };
    let targets = TargetsDef {
        explicit: resolved.explicit,
        prepend: resolved.prepend,
        append: resolved.append,
    };
    resolve_path_listop(&targets, store)
}

/// Resolves a potentially relative path (e.g. `../Foo`) against a prim path.
fn resolve_relative_path(path: &str, base_prim_path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    // Split base prim path into segments, go up for each `..`
    let mut segments: Vec<&str> = base_prim_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    for component in path.split('/') {
        match component {
            ".." => {
                segments.pop();
            }
            "." | "" => {}
            name => segments.push(name),
        }
    }
    format!("/{}", segments.join("/"))
}

fn resolve_reference_spec(
    spec: &ReferenceSpec,
    base_dir: &Path,
    root_dir: &Path,
    current_layer_id: LayerId,
    store: &mut InMemoryStore,
    next_layer_id: &mut u64,
    by_path: &mut std::collections::BTreeMap<PathBuf, LayerId>,
    layer_names: &mut std::collections::BTreeMap<LayerId, String>,
) -> Reference {
    let asset = spec.asset.trim();

    let prim =
        LsPath::parse_absolute(spec.prim_path.trim(), &mut store.tokens).expect("reference path");
    let prim_path = store.paths.intern(prim);

    // Internal reference (no asset path) — same layer.
    if asset.is_empty() {
        return Reference {
            layer: current_layer_id,
            prim_path,
            asset: None,
        };
    }

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

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            // Missing referenced layer file — create an empty layer.
            let layer = Layer {
                id,
                sublayers: Vec::new(),
                prims: layerstack::HashMap::new(),
            };
            store.insert_layer(layer);
            return id;
        }
    };
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

    // Track which children are variant children so we exclude them from
    // the parent's authored_children (they go in VariantSpec::authored_children instead).
    let mut variant_child_names: std::collections::HashMap<
        String, // parent path
        std::collections::HashSet<String>, // child names that are variant children
    > = std::collections::HashMap::new();
    for prim in &prim_defs {
        if let Some((parent_path, _set, _branch)) = &prim.variant_parent {
            let leaf = prim.path.rsplit('/').next().unwrap_or("");
            variant_child_names
                .entry(parent_path.clone())
                .or_default()
                .insert(leaf.to_string());
        }
    }

    // Track which paths have non-variant definitions so we can detect when a
    // variant-scoped child prim overlaps with an existing definition.
    let mut non_variant_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for prim in &prim_defs {
        if prim.variant_parent.is_none() {
            non_variant_paths.insert(prim.path.clone());
        }
    }

    for prim in prim_defs {
        let prim_path = {
            let path = LsPath::parse_absolute(&prim.path, &mut store.tokens).expect("path");
            store.paths.intern(path)
        };

        // Only add to authored_children if NOT a variant child.
        let is_variant_child = prim.variant_parent.is_some();
        if !is_variant_child
            && let Some(parent) = store.paths.resolve(prim_path).parent()
        {
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

    // Collect variant children info for building VariantSpecs.
    // Key: (parent_path, set_name, branch_name) → Vec<child_name>
    let mut variant_children_map: std::collections::HashMap<
        (String, String, String),
        Vec<String>,
    > = std::collections::HashMap::new();
    // Grandchild authored children: variant-scoped prims nested deeper than
    // the direct variant branch children (e.g. `over "Class" { def "Grandchild" {} }`
    // inside a variant). These go to the variant set owner's
    // VariantSpec.child_authored_children.
    // Key: (variant_set_owner_path, set_name, branch_name, child_leaf) → Vec<grandchild_leaf>
    let mut variant_grandchild_map: std::collections::HashMap<
        (String, String, String, String),
        Vec<String>,
    > = std::collections::HashMap::new();
    // Build a map of paths → variant set names from their variant_parent.
    // A path may appear multiple times (e.g. `over "Class"` in both "high"
    // and "low" branches), so we collect all variant set names.
    let mut variant_sets_by_path: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (_, p) in &prim_defs_with_ids {
        if let Some((_, set, _)) = &p.variant_parent {
            variant_sets_by_path
                .entry(p.path.clone())
                .or_default()
                .insert(set.clone());
        }
    }
    for (_, prim) in &prim_defs_with_ids {
        if let Some((parent_path, set, branch)) = &prim.variant_parent {
            let leaf = prim.path.rsplit('/').next().unwrap_or("");
            // A prim is a "grandchild" when its parent_path is itself a
            // variant-scoped prim in the SAME variant set. If the variant
            // sets differ, the prim is a direct child of its parent's OWN
            // variant set (e.g. a nested variant set defined on a prim that
            // itself lives inside an outer variant branch).
            let is_grandchild = variant_sets_by_path
                .get(parent_path)
                .map_or(false, |parent_sets| parent_sets.contains(set));
            if is_grandchild {
                // parent_path is itself a variant-scoped prim with the same
                // variant context → this is a grandchild.
                let child_leaf = parent_path.rsplit('/').next().unwrap_or("");
                // Find the variant set owner: the parent of parent_path
                let owner_path = if let Some(idx) = parent_path.rfind('/') {
                    &parent_path[..idx]
                } else {
                    ""
                };
                // Ensure owner_path is non-empty (root-level)
                let owner_path = if owner_path.is_empty() {
                    "/".to_string()
                } else {
                    owner_path.to_string()
                };
                variant_grandchild_map
                    .entry((owner_path, set.clone(), branch.clone(), child_leaf.to_string()))
                    .or_default()
                    .push(leaf.to_string());
            } else {
                // Direct child of the variant branch.
                variant_children_map
                    .entry((parent_path.clone(), set.clone(), branch.clone()))
                    .or_default()
                    .push(leaf.to_string());
            }
        }
    }

    // Collect variant child arcs: when a child prim inside a variant branch
    // has composition arcs AND a non-variant definition exists at the same path,
    // the arcs should be stored on the parent's VariantSpec, not on the child's PrimSpec.
    // Key: parent_path → Vec<VariantChildArc>
    let mut variant_child_arcs_by_parent: std::collections::HashMap<
        String,
        Vec<VariantChildArc>,
    > = std::collections::HashMap::new();

    // Variant child fields: when a child prim inside a variant branch has
    // fields AND a non-variant definition exists at the same path, the fields
    // are stored on the parent's VariantSpec.child_fields.
    // Key: (parent_path, set_name, branch_name) → Vec<(child_name, attrs, time_samples)>
    #[allow(clippy::type_complexity)]
    let mut variant_child_fields_by_parent: std::collections::HashMap<
        (String, String, String),
        Vec<(String, Vec<String>, Vec<(String, Vec<(f64, String)>)>)>,
    > = std::collections::HashMap::new();

    // Separate PrimDefs into non-variant and variant-scoped.
    let mut non_variant_defs: Vec<(layerstack::PathId, PrimDef)> = Vec::new();
    let mut variant_defs: Vec<(layerstack::PathId, PrimDef)> = Vec::new();
    for (prim_path, prim) in prim_defs_with_ids {
        if prim.variant_parent.is_some() {
            variant_defs.push((prim_path, prim));
        } else {
            non_variant_defs.push((prim_path, prim));
        }
    }

    // For variant-scoped PrimDefs that overlap with non-variant definitions,
    // route their composition arcs to the parent's VariantSpec.
    let mut variant_only_defs: Vec<(layerstack::PathId, PrimDef)> = Vec::new();
    for (prim_path, prim) in variant_defs {
        let has_non_variant_counterpart = non_variant_paths.contains(&prim.path);
        if has_non_variant_counterpart {
            // Route composition arcs to parent's VariantSpec.
            let (parent_path, set, branch) = prim.variant_parent.as_ref().unwrap();
            let child_name = prim.path.rsplit('/').next().unwrap_or("").to_string();
            let has_arcs = !prim.references.prepend.is_empty()
                || !prim.references.append.is_empty()
                || prim.references.explicit.is_some()
                || !prim.inherits.prepend.is_empty()
                || !prim.inherits.append.is_empty()
                || prim.inherits.explicit.is_some()
                || !prim.specializes.prepend.is_empty()
                || !prim.specializes.append.is_empty()
                || prim.specializes.explicit.is_some()
                || !prim.payloads.prepend.is_empty()
                || !prim.payloads.append.is_empty()
                || prim.payloads.explicit.is_some();
            if has_arcs {
                variant_child_arcs_by_parent
                    .entry(parent_path.clone())
                    .or_default()
                    .push(VariantChildArc {
                        set_name: set.clone(),
                        branch_name: branch.clone(),
                        child_name: child_name.clone(),
                        references: prim.references,
                        inherits: prim.inherits,
                        specializes: prim.specializes,
                        payloads: prim.payloads,
                    });
            }
            // Route variant-scoped fields to the parent's VariantSpec.child_fields.
            if !prim.attrs.is_empty() || !prim.time_samples.is_empty() {
                variant_child_fields_by_parent
                    .entry((parent_path.clone(), set.clone(), branch.clone()))
                    .or_default()
                    .push((child_name, prim.attrs, prim.time_samples));
            }
            // Don't create a PrimSpec for this variant-scoped prim.
        } else {
            // New child introduced by variant — create PrimSpec normally.
            variant_only_defs.push((prim_path, prim));
        }
    }

    // Process all PrimDefs: non-variant first, then variant-only.
    let all_defs = non_variant_defs.into_iter().chain(variant_only_defs.into_iter());
    for (prim_path, prim) in all_defs {
        let mut spec = PrimSpec {
            specifier: Some(prim.specifier),
            authored_children: children_by_parent.remove(&prim_path).unwrap_or_default(),
            ..PrimSpec::default()
        };
        for attr in prim.attrs {
            let tok = store.tokens.intern(attr);
            spec.fields
                .entry(tok)
                .or_insert(FieldValue::Value(Value::Null));
        }
        for (attr_name, samples) in prim.time_samples {
            let tok = store.tokens.intern(attr_name);
            let ts: Vec<(f64, Value)> = samples
                .into_iter()
                .map(|(t, v)| {
                    // Try parsing as int, then float, then string.
                    let value = if let Ok(i) = v.parse::<i64>() {
                        Value::Int(i)
                    } else if let Ok(f) = v.parse::<f64>() {
                        Value::Float(f)
                    } else {
                        Value::String(v.into())
                    };
                    (t, value)
                })
                .collect();
            spec.fields.insert(tok, FieldValue::TimeSamples(ts));
        }
        spec.references = resolve_references(
            &prim.references,
            path.parent().unwrap_or(Path::new(".")),
            root_dir,
            id,
            store,
            next_layer_id,
            by_path,
            layer_names,
        );
        spec.inherits = resolve_inherits_listop(&prim.inherits, store, &prim.path);
        spec.specializes = resolve_specializes_listop(&prim.specializes, store, &prim.path);
        spec.payloads = resolve_references(
            &ReferencesDef {
                explicit: prim.payloads.explicit,
                prepend: prim.payloads.prepend,
                append: prim.payloads.append,
            },
            path.parent().unwrap_or(Path::new(".")),
            root_dir,
            id,
            store,
            next_layer_id,
            by_path,
            layer_names,
        );
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

        // Process named relationships (e.g. `add rel controls = </path>`).
        for (name, op, specs) in &prim.named_rels {
            let targets = match op {
                TargetOp::Explicit => TargetsDef {
                    explicit: Some(specs.clone()),
                    ..Default::default()
                },
                TargetOp::Prepend => TargetsDef {
                    prepend: specs.clone(),
                    ..Default::default()
                },
                TargetOp::Append => TargetsDef {
                    append: specs.clone(),
                    ..Default::default()
                },
            };
            let tok = store.tokens.intern(name);
            let list_op = resolve_path_listop(&targets, store);
            // Merge with existing if present.
            if let Some(FieldValue::PathListOp(existing)) = spec.fields.get_mut(&tok) {
                existing.prepend.extend(list_op.prepend);
                existing.append.extend(list_op.append);
                if list_op.explicit.is_some() {
                    existing.explicit = list_op.explicit;
                }
            } else {
                spec.fields.insert(tok, FieldValue::PathListOp(list_op));
            }
        }

        // Process attribute connections (e.g. `add double focalLength.connect = </path>`).
        for (name, op, specs) in &prim.connections {
            let targets = match op {
                TargetOp::Explicit => TargetsDef {
                    explicit: Some(specs.clone()),
                    ..Default::default()
                },
                TargetOp::Prepend => TargetsDef {
                    prepend: specs.clone(),
                    ..Default::default()
                },
                TargetOp::Append => TargetsDef {
                    append: specs.clone(),
                    ..Default::default()
                },
            };
            let tok = store.tokens.intern(name);
            let list_op = resolve_path_listop(&targets, store);
            if let Some(FieldValue::PathListOp(existing)) = spec.fields.get_mut(&tok) {
                existing.prepend.extend(list_op.prepend);
                existing.append.extend(list_op.append);
                if list_op.explicit.is_some() {
                    existing.explicit = list_op.explicit;
                }
            } else {
                spec.fields.insert(tok, FieldValue::PathListOp(list_op));
            }
        }

        // Process variant selections.
        for (set_name, selected) in &prim.variant_selections {
            let set_tok = store.tokens.intern(set_name);
            let sel_tok = store.tokens.intern(selected);
            spec.variant_selections.insert(set_tok, sel_tok);
        }

        // Process variant set names and build VariantSetSpec/VariantSpec entries.
        // Collect all variant set names (from metadata + variant children + variant fields + child arcs).
        let mut all_set_names: Vec<String> = prim.variant_set_names.clone();
        for (parent, set, _branch) in variant_children_map.keys() {
            if *parent == prim.path && !all_set_names.contains(set) {
                all_set_names.push(set.clone());
            }
        }
        for (set, _branch, _attr) in &prim.variant_fields {
            if !all_set_names.contains(set) {
                all_set_names.push(set.clone());
            }
        }
        if let Some(child_arcs) = variant_child_arcs_by_parent.get(&prim.path) {
            for arc in child_arcs {
                if !all_set_names.contains(&arc.set_name) {
                    all_set_names.push(arc.set_name.clone());
                }
            }
        }
        for ((parent, set, _branch), _) in &variant_child_fields_by_parent {
            if *parent == prim.path && !all_set_names.contains(set) {
                all_set_names.push(set.clone());
            }
        }
        for arc in &prim.variant_branch_arcs {
            if !all_set_names.contains(&arc.set_name) {
                all_set_names.push(arc.set_name.clone());
            }
        }

        for set_name in &all_set_names {
            let set_tok = store.tokens.intern(set_name);
            let set_spec = spec.variant_sets.entry(set_tok).or_default();

            // Find all branches for this set from variant_children_map.
            for ((parent, s, branch), children) in &variant_children_map {
                if *parent == prim.path && s == set_name {
                    let branch_tok = store.tokens.intern(branch);
                    let variant_spec = set_spec.variants.entry(branch_tok).or_default();
                    for child_name in children {
                        let child_tok = store.tokens.intern(child_name);
                        if !variant_spec.authored_children.contains(&child_tok) {
                            variant_spec.authored_children.push(child_tok);
                        }
                    }
                }
            }

            // Add variant branch fields.
            for (s, branch, attr) in &prim.variant_fields {
                if s == set_name {
                    let branch_tok = store.tokens.intern(branch);
                    let variant_spec = set_spec.variants.entry(branch_tok).or_default();
                    let attr_tok = store.tokens.intern(attr);
                    variant_spec
                        .fields
                        .entry(attr_tok)
                        .or_insert(FieldValue::Value(Value::Null));
                }
            }

            // Add variant child arcs (composition arcs on child prims within variant branches).
            if let Some(child_arcs) = variant_child_arcs_by_parent.get(&prim.path) {
                for arc in child_arcs {
                    if arc.set_name == *set_name {
                        let branch_tok = store.tokens.intern(&arc.branch_name);
                        let variant_spec = set_spec.variants.entry(branch_tok).or_default();
                        let child_tok = store.tokens.intern(&arc.child_name);

                        let child_refs = resolve_references(
                            &arc.references,
                            path.parent().unwrap_or(Path::new(".")),
                            root_dir,
                            id,
                            store,
                            next_layer_id,
                            by_path,
                            layer_names,
                        );
                        if !child_refs.prepend.is_empty()
                            || !child_refs.append.is_empty()
                            || child_refs.explicit.is_some()
                        {
                            variant_spec.child_references.insert(child_tok, child_refs);
                        }

                        let child_prim_path =
                            format!("{}/{}", prim.path, arc.child_name);
                        let child_inherits =
                            resolve_inherits_listop(&arc.inherits, store, &child_prim_path);
                        if !child_inherits.prepend.is_empty()
                            || !child_inherits.append.is_empty()
                            || child_inherits.explicit.is_some()
                        {
                            variant_spec.child_inherits.insert(child_tok, child_inherits);
                        }

                        let child_specializes =
                            resolve_specializes_listop(&arc.specializes, store, &child_prim_path);
                        if !child_specializes.prepend.is_empty()
                            || !child_specializes.append.is_empty()
                            || child_specializes.explicit.is_some()
                        {
                            variant_spec.child_specializes.insert(child_tok, child_specializes);
                        }

                        let child_payloads = resolve_references(
                            &ReferencesDef {
                                explicit: arc.payloads.explicit.clone(),
                                prepend: arc.payloads.prepend.clone(),
                                append: arc.payloads.append.clone(),
                            },
                            path.parent().unwrap_or(Path::new(".")),
                            root_dir,
                            id,
                            store,
                            next_layer_id,
                            by_path,
                            layer_names,
                        );
                        if !child_payloads.prepend.is_empty()
                            || !child_payloads.append.is_empty()
                            || child_payloads.explicit.is_some()
                        {
                            variant_spec.child_payloads.insert(child_tok, child_payloads);
                        }
                    }
                }
            }

            // Add grandchild authored children (children of child prims within variant branches).
            for ((owner, s, branch, child_leaf), grandchildren) in &variant_grandchild_map {
                if *owner == prim.path && s == set_name {
                    let branch_tok = store.tokens.intern(branch);
                    let variant_spec = set_spec.variants.entry(branch_tok).or_default();
                    let child_tok = store.tokens.intern(child_leaf);
                    let entry = variant_spec
                        .child_authored_children
                        .entry(child_tok)
                        .or_default();
                    for gc in grandchildren {
                        let gc_tok = store.tokens.intern(gc);
                        if !entry.contains(&gc_tok) {
                            entry.push(gc_tok);
                        }
                    }
                }
            }

            // Add variant-scoped child prim fields.
            for ((parent, s, branch), child_entries) in &variant_child_fields_by_parent {
                if *parent == prim.path && s == set_name {
                    let branch_tok = store.tokens.intern(branch);
                    let variant_spec = set_spec.variants.entry(branch_tok).or_default();
                    for (child_name, attrs, time_samples) in child_entries {
                        let child_tok = store.tokens.intern(child_name);
                        let child_fields = variant_spec
                            .child_fields
                            .entry(child_tok)
                            .or_default();
                        for attr_name in attrs {
                            // attrs are already parsed attribute names.
                            let key = store.tokens.intern(attr_name);
                            child_fields
                                .entry(key)
                                .or_insert(FieldValue::Value(Value::Null));
                        }
                        for (attr_name, samples) in time_samples {
                            let key = store.tokens.intern(attr_name);
                            let ts: Vec<(f64, Value)> = samples
                                .iter()
                                .map(|(t, v)| {
                                    let value = if let Ok(i) = v.parse::<i64>() {
                                        Value::Int(i)
                                    } else if let Ok(f) = v.parse::<f64>() {
                                        Value::Float(f)
                                    } else {
                                        Value::String(v.clone().into())
                                    };
                                    (*t, value)
                                })
                                .collect();
                            if !ts.is_empty() {
                                child_fields
                                    .entry(key)
                                    .or_insert(FieldValue::TimeSamples(ts));
                            }
                        }
                    }
                }
            }

            // Add variant branch-level composition arcs (arcs on the variant
            // branch header itself, e.g. `"full" (add references = ...) {}`).
            for arc in &prim.variant_branch_arcs {
                if arc.set_name == *set_name {
                    let branch_tok = store.tokens.intern(&arc.branch_name);
                    let variant_spec = set_spec.variants.entry(branch_tok).or_default();

                    variant_spec.references = resolve_references(
                        &arc.references,
                        path.parent().unwrap_or(Path::new(".")),
                        root_dir,
                        id,
                        store,
                        next_layer_id,
                        by_path,
                        layer_names,
                    );
                    variant_spec.inherits =
                        resolve_inherits_listop(&arc.inherits, store, &prim.path);
                    variant_spec.specializes =
                        resolve_specializes_listop(&arc.specializes, store, &prim.path);
                    variant_spec.payloads = resolve_references(
                        &ReferencesDef {
                            explicit: arc.payloads.explicit.clone(),
                            prepend: arc.payloads.prepend.clone(),
                            append: arc.payloads.append.clone(),
                        },
                        path.parent().unwrap_or(Path::new(".")),
                        root_dir,
                        id,
                        store,
                        next_layer_id,
                        by_path,
                        layer_names,
                    );
                }
            }
        }

        // Set variant set order.
        spec.variant_set_order = all_set_names
            .iter()
            .map(|n| store.tokens.intern(n))
            .collect();

        layer.prims.insert(prim_path, spec);
    }

    store.insert_layer(layer);
    id
}
