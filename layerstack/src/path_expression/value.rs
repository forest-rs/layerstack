// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition of `pathExpression` values.
//!
//! Composition handles a path expression value in two steps:
//!
//! - Each opinion is anchored and mapped into the stage namespace: relative
//!   patterns are made absolute at the prim that authors them, and every
//!   pattern prefix and reference path is mapped through the arcs to the
//!   composed prim. A pattern outside an arc's domain matches nothing,
//!   except one with a leading stretch (`//`), which matches anywhere.
//!   OpenUSD: `PcpMapFunction::MapSourceToTarget(SdfPathExpression)` in
//!   `pxr/usd/pcp/mapFunction.cpp`, applied by `UsdStage` value resolution.
//! - `%_` names the next weaker opinion's expression, spliced in by
//!   [`fold`] at the default time and at numeric times alike; once no
//!   weaker opinion is left, it matches nothing. OpenUSD:
//!   `SdfPathExpression::ComposeOver`.
//!
//! Composed values are written as `SdfPathExpression::GetText` writes
//! them, except that predicate arguments keep their types
//! ([`PathExpression::lossless_text`]: OpenUSD composes expression objects,
//! not text); text that does not parse is left as authored.
//!
//! Spec: AOUSD Core §10 (composition arcs map namespace), §12.3 (attribute
//! value resolution).

use alloc::{
    borrow::{Cow, ToOwned},
    string::String,
    sync::Arc,
    vec::Vec,
};

use hashbrown::HashMap;

use super::{Expr, PathExpression, RefPath};
use crate::{
    doc::{FieldValue, InterpolationType, LayerStore, Value},
    path::PathId,
    prim_index::{ArcKind, Opinion, OpinionValue, PrimIndex},
    prim_index_graph::{NodeId, PrimIndexGraph},
    spec_path::{SpecComponent, SpecPath},
    stage::value_at_time,
};

/// One arc's map from its target's namespace to the namespace it is
/// authored in: `source` maps to `target`, and with `root_identity`, every
/// other path maps to itself.
///
/// OpenUSD: `PcpMapFunction`.
#[derive(Clone, Debug)]
pub(crate) struct ArcMap {
    source: Vec<String>,
    target: Vec<String>,
    root_identity: bool,
}

impl ArcMap {
    /// Maps an absolute prim path; `None` outside the map's domain.
    ///
    /// The most specific pair applies; a result that falls under a more
    /// specific pair's target is blocked, since that pair's inverse would
    /// not map it back. OpenUSD: `_Map` in `pxr/usd/pcp/mapFunction.cpp`.
    fn map(&self, path: &[String]) -> Option<Vec<String>> {
        if path.starts_with(&self.source) {
            let mut mapped = self.target.clone();
            mapped.extend_from_slice(&path[self.source.len()..]);
            return Some(mapped);
        }
        if !self.root_identity {
            return None;
        }
        // The root identity matched, with no elements: the pair's target is
        // more specific.
        (!path.starts_with(&self.target) || self.target.is_empty()).then(|| path.to_vec())
    }
}

/// Maps an absolute prim path through `maps`, innermost arc first.
fn map_path(maps: &[ArcMap], path: &[String]) -> Option<Vec<String>> {
    let mut path = path.to_vec();
    for map in maps {
        path = map.map(&path)?;
    }
    Some(path)
}

/// Anchors `expr` at the prim `anchor` and maps it into the stage
/// namespace through `maps`, innermost arc first.
///
/// A pattern whose prefix is the absolute root and has more after it (`//`,
/// `//Name`) is kept as is; any other pattern, and any reference path,
/// outside the maps' domain matches nothing.
///
/// OpenUSD: `SdfPathExpression::MakeAbsolute` and
/// `PcpMapFunction::MapSourceToTarget` (`_MapPathExpressionImpl` in
/// `pxr/usd/pcp/mapFunction.cpp`).
pub(crate) fn anchor_and_map(
    expr: PathExpression,
    anchor: &[String],
    maps: &[ArcMap],
) -> PathExpression {
    expr.rebuild(&mut |atom| match atom {
        Expr::Pattern(mut pattern) => {
            if !pattern.prefix.absolute {
                let relative = core::mem::take(&mut pattern.prefix.prims);
                let Some(prims) = anchor_names(anchor, relative) else {
                    return PathExpression::nothing();
                };
                pattern.prefix.prims = prims;
                pattern.prefix.absolute = true;
            }
            // A leading stretch (`//...`) matches anywhere and maps to
            // itself; any other prefix, the root included, maps through
            // the arcs. OpenUSD: `HasLeadingStretch`.
            if pattern.has_leading_stretch() {
                return PathExpression::pattern(pattern);
            }
            match map_path(maps, &pattern.prefix.prims) {
                Some(prims) => {
                    pattern.prefix.prims = prims;
                    PathExpression::pattern(pattern)
                }
                None => PathExpression::nothing(),
            }
        }
        Expr::Reference(mut reference) => {
            let Some(path) = reference.path.take() else {
                return PathExpression::reference(reference);
            };
            let prims = if path.absolute {
                Some(path.prims)
            } else {
                anchor_names(anchor, path.prims)
            };
            match prims.and_then(|prims| map_path(maps, &prims)) {
                Some(prims) => {
                    reference.path = Some(RefPath {
                        absolute: true,
                        prims,
                    });
                    PathExpression::reference(reference)
                }
                None => PathExpression::nothing(),
            }
        }
        atom => PathExpression { root: Some(atom) },
    })
}

/// Makes relative prim names (`.`, `..` and names) absolute at `anchor`;
/// `None` when `..` climbs above the root.
fn anchor_names(anchor: &[String], relative: Vec<String>) -> Option<Vec<String>> {
    let mut prims: Vec<String> = anchor.to_vec();
    for name in relative {
        match name.as_str() {
            "." => {}
            ".." => {
                prims.pop()?;
            }
            _ => prims.push(name),
        }
    }
    Some(prims)
}

/// A node's site prim, which anchors its relative expressions, and the
/// maps from its namespace to the stage namespace.
type NodeNamespace = (Vec<String>, Vec<ArcMap>);

/// The prim names of `site`, without its variant selections.
fn prim_names(store: &dyn LayerStore, site: &SpecPath) -> Vec<String> {
    site.components()
        .iter()
        .filter_map(|component| match component {
            SpecComponent::Prim(name) => Some(store.tokens().resolve(*name).to_owned()),
            SpecComponent::VariantSelection { .. } => None,
        })
        .collect()
}

/// The maps of the arcs from `node` up to the root of `graph`, innermost
/// first, for a composed prim at namespace depth `prim_depth`; `None` when
/// a site does not line up with the arc that reaches it.
///
/// Each arc maps the site it targets to the site it is authored at, at the
/// namespace depth it is authored on; a variant arc maps every path to
/// itself. Class arcs (inherits, specializes) and references or payloads
/// within one layer stack also map every other path to itself. OpenUSD:
/// `_CreateMapExpressionForArc` in `pxr/usd/pcp/primIndex.cpp`.
fn node_maps(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    node: NodeId,
    prim_depth: usize,
) -> Option<Vec<ArcMap>> {
    let mut maps = Vec::new();
    let mut cursor = graph.node(node)?;
    while let Some(parent_id) = cursor.parent() {
        let parent = graph.node(parent_id)?;
        if cursor.arc_kind() != ArcKind::Variants {
            let below = prim_depth.checked_sub(usize::from(cursor.namespace_depth()))?;
            let mut source = prim_names(store, cursor.site());
            let mut target = prim_names(store, parent.site());
            source.truncate(source.len().checked_sub(below)?);
            target.truncate(target.len().checked_sub(below)?);
            let root_identity =
                matches!(cursor.arc_kind(), ArcKind::Inherits | ArcKind::Specializes)
                    || cursor.layer_stack() == parent.layer_stack();
            maps.push(ArcMap {
                source,
                target,
                root_identity,
            });
        }
        cursor = parent;
    }
    Some(maps)
}

/// Parses `text`; `None` for text that does not parse, which composition
/// leaves as authored.
fn parse(text: &str) -> Option<PathExpression> {
    PathExpression::parse(text).ok()
}

/// Whether this value has expressions the anchoring pass can transform.
fn has_path_expression(value: &Value) -> bool {
    match value {
        Value::PathExpression(_) => true,
        Value::Array(items) => items
            .iter()
            .any(|item| matches!(item, Value::PathExpression(_))),
        _ => false,
    }
}

/// Anchors expressions in place, leaving other values and unsupported
/// expression text untouched. Arrays keep their already owned storage.
fn anchor_value(value: &mut Value, anchor: &[String], maps: &[ArcMap]) {
    match value {
        Value::PathExpression(text) => {
            if let Some(expression) = parse(text) {
                *text = Arc::from(anchor_and_map(expression, anchor, maps).lossless_text());
            }
        }
        Value::Array(items) if items.iter().any(|v| matches!(v, Value::PathExpression(_))) => {
            for item in items {
                anchor_value(item, anchor, maps);
            }
        }
        _ => {}
    }
}

/// Anchors every path expression an opinion of `prims` authors at the prim
/// that authors it, and maps it through the arcs into the stage namespace,
/// so value resolution composes them in one namespace.
///
/// Spec: AOUSD Core §10 (composition arcs map namespace). OpenUSD:
/// `PcpMapFunction::MapSourceToTarget(SdfPathExpression)`, applied to each
/// node's opinions by `UsdStage` value resolution.
pub(crate) fn anchor_opinions(store: &dyn LayerStore, prims: &mut HashMap<PathId, PrimIndex>) {
    for (path, index) in prims.iter_mut() {
        let prim_depth = store.paths().resolve(*path).depth();
        let graph = &index.graph;
        let mut maps: HashMap<NodeId, Option<NodeNamespace>> = HashMap::new();
        for opinions in index.opinions_by_field.values_mut() {
            for opinion in opinions.iter_mut() {
                // The default, and each time sample, of a path expression.
                let (default, samples) = match &mut opinion.value {
                    OpinionValue::Field(FieldValue::Value(value)) => (Some(value), None),
                    OpinionValue::Property(spec) => {
                        (spec.default.as_mut(), spec.time_samples.as_mut())
                    }
                    OpinionValue::Field(_) => continue,
                };
                let mut authored = default
                    .into_iter()
                    .chain(samples.into_iter().flatten().map(|(_, value)| value))
                    .filter(|value| has_path_expression(value))
                    .peekable();
                if authored.peek().is_none() {
                    continue;
                }
                let node = opinion.key.node;
                let Some((anchor, node_maps)) = maps
                    .entry(node)
                    .or_insert_with(|| {
                        let anchor = prim_names(store, graph.node(node)?.site());
                        Some((anchor, node_maps(store, graph, node, prim_depth)?))
                    })
                    .as_ref()
                else {
                    continue;
                };
                for value in authored {
                    anchor_value(value, anchor, node_maps);
                }
            }
        }
    }
}

/// Maps freshly authored value slots using an existing opinion's node. Values
/// already held by the stage must not pass through this mapping a second time.
pub(crate) fn anchor_fresh_values<'a>(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    prim: PathId,
    node: NodeId,
    values: impl Iterator<Item = &'a mut Value>,
) {
    let mut values = values.filter(|v| has_path_expression(v)).peekable();
    if values.peek().is_none() {
        return;
    }
    let Some(site) = graph.node(node) else {
        return;
    };
    let anchor = prim_names(store, site.site());
    let Some(maps) = node_maps(store, graph, node, store.paths().resolve(prim).depth()) else {
        return;
    };
    for value in values {
        anchor_value(value, &anchor, &maps);
    }
}

/// The values a path expression query reads, strongest first: each
/// answering opinion's position (`None` for a schema fallback) and its
/// value, `None` for a block in effect.
pub(crate) type ChainValues<'v> = Vec<(Option<usize>, Option<Cow<'v, Value>>)>;

/// How a chain of path expressions composed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fold {
    /// The composed expression; `None` when a block resolved the query to
    /// no value.
    pub(crate) value: Option<Value>,
    /// The positions of the values that contributed, strongest first:
    /// the strongest one and each one a `%_` spliced in.
    pub(crate) contributors: Vec<Option<usize>>,
    /// Where the fold ended before its `%_` were all filled, when it did:
    /// at a block, or at a value that is not a path expression.
    pub(crate) stopped_at: Option<(Option<usize>, Stop)>,
}

/// Why a [`Fold`] ended early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Block,
    Incompatible,
}

/// Composes a path expression query over `values` (see [`ChainValues`]):
/// each `%_` of the strongest expression splices in the next weaker one,
/// until none is left, and a `%_` that nothing fills matches nothing.
///
/// A `%_` that reaches a block ends the chain. At a numeric time (`at_time`)
/// the `%_` then matches nothing; at the default time the query resolves to
/// no value, as in OpenUSD 26.08.
///
/// Returns `None` when the strongest value is not a path expression: the
/// query is not a path expression query.
///
/// Spec: AOUSD Core §12.3 (attribute value resolution). OpenUSD:
/// `SdfPathExpression::ComposeOver`, applied by `UsdStage` value
/// resolution.
pub(crate) fn fold(values: ChainValues<'_>, at_time: bool) -> Option<Fold> {
    let mut values = values.into_iter();
    let (position, strongest) = values.next()?;
    let Some(Value::PathExpression(text)) = strongest.as_deref() else {
        return None;
    };
    let mut fold = Fold {
        value: None,
        contributors: Vec::from([position]),
        stopped_at: None,
    };
    let Some(mut expr) = parse(text) else {
        fold.value = Some(Value::PathExpression(text.clone()));
        return Some(fold);
    };
    while expr.contains_weaker_reference() {
        let Some((position, weaker)) = values.next() else {
            break;
        };
        let Some(weaker) = weaker else {
            fold.stopped_at = Some((position, Stop::Block));
            if !at_time {
                return Some(fold);
            }
            break;
        };
        let Some(weaker) = (match weaker.as_ref() {
            Value::PathExpression(text) => parse(text),
            _ => None,
        }) else {
            fold.stopped_at = Some((position, Stop::Incompatible));
            break;
        };
        fold.contributors.push(position);
        expr = expr.compose_over(&weaker);
    }
    let expr = expr.compose_over(&PathExpression::nothing());
    fold.value = Some(Value::PathExpression(Arc::from(expr.lossless_text())));
    Some(fold)
}

/// Composes the default-time value of `opinions` over `fallback` when it
/// is a path expression (see [`fold`]); `None` otherwise.
pub(crate) fn fold_default(opinions: &[Opinion], fallback: Option<&Value>) -> Option<Fold> {
    let strongest = opinions
        .iter()
        .find_map(|opinion| opinion.value.default_value())
        .or(fallback)?;
    if !matches!(strongest, Value::PathExpression(_)) {
        return None;
    }
    let values = opinions
        .iter()
        .enumerate()
        .filter_map(|(position, opinion)| Some((position, opinion.value.default_value()?)))
        .map(|(position, value)| {
            let value = (*value != Value::Blocked).then_some(Cow::Borrowed(value));
            (Some(position), value)
        })
        .chain(fallback.map(|value| (None, Some(Cow::Borrowed(value)))))
        .collect();
    fold(values, false)
}

/// Composes the value of `opinions` at stage time `time` over `fallback`
/// when it is a path expression (see [`fold`]); `None` otherwise.
///
/// Each opinion answers with its time samples, else its spline, else its
/// default, as for any other value.
///
/// Spec: AOUSD Core §12.3.2.
pub(crate) fn fold_at_time(
    opinions: &[Opinion],
    time: f64,
    interp: InterpolationType,
    fallback: Option<&Value>,
) -> Option<Fold> {
    // Decide from the strongest answering opinion's authored kind, without
    // sampling or cloning anything, whether this is a path expression query.
    let strongest_is_expression = opinions
        .iter()
        .find_map(answers_with_expression)
        .unwrap_or(matches!(fallback, Some(Value::PathExpression(_))));
    if !strongest_is_expression {
        return None;
    }
    let values = opinions
        .iter()
        .enumerate()
        .filter_map(|(position, opinion)| {
            Some((
                Some(position),
                value_at_time(opinion, time, interp)?.map(Cow::Owned),
            ))
        })
        .chain(fallback.map(|value| (None, Some(Cow::Borrowed(value)))))
        .collect();
    fold(values, true)
}

/// Whether `opinion` answers a numeric-time query with a path expression:
/// `None` when it authors no time samples, spline or default, so a weaker
/// opinion answers. Looks at the authored values only; nothing is sampled.
fn answers_with_expression(opinion: &Opinion) -> Option<bool> {
    let is_expression = |value: &Value| matches!(value, Value::PathExpression(_));
    if let Some(samples) = opinion
        .value
        .time_samples()
        .filter(|samples| !samples.is_empty())
    {
        return Some(samples.iter().any(|(_, value)| is_expression(value)));
    }
    if opinion.value.spline().is_some() {
        return Some(false);
    }
    opinion.value.default_value().map(is_expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) -> String {
        parse(text).expect("parses").text()
    }

    fn names(path: &str) -> Vec<String> {
        path.split('/')
            .filter(|n| !n.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Text is written as `SdfPathExpression::GetText` writes it (checked
    /// against OpenUSD 26.08).
    #[test]
    fn text_matches_openusd() {
        for (text, expected) in [
            ("/A /B", "/A /B"),
            ("/A    /B", "/A /B"),
            ("/A+/B", "/A + /B"),
            ("/A&/B", "/A & /B"),
            ("/A-/B", "/A - /B"),
            ("~ /A", "~/A"),
            ("(/A /B) & /C", "/A /B & /C"),
            ("/A (/B & /C)", "/A (/B & /C)"),
            ("((/A))", "/A"),
            ("(/A - /B) - /C", "/A - /B - /C"),
            ("/A - (/B - /C)", "/A - (/B - /C)"),
            ("~(/A /B)", "~(/A /B)"),
            ("~(~/A)", "/A"),
            ("(/A + /B) & /C", "/A + /B & /C"),
            ("(/A - /B) & /C", "(/A - /B) & /C"),
            ("/A - (/B + /C)", "/A - /B + /C"),
            ("(/A + /B) /C", "(/A + /B) /C"),
            ("/A & (/B /C)", "/A & /B /C"),
            ("/A + (/B + /C)", "/A + (/B + /C)"),
            ("%_ /A", "%_ /A"),
            ("%:heroes /B", "%:heroes /B"),
            ("%/Sets:heroes", "%/Sets:heroes"),
            (".//", ".//"),
            ("../B", "../B"),
            ("/World//*{isa:\"Mesh\"}", "/World//*{isa:\"Mesh\"}"),
            ("/A/B.x:y*", "/A/B.x:y*"),
            ("", ""),
        ] {
            assert_eq!(round_trip(text), expected, "{text:?}");
        }
    }

    /// `%_` splices in the weaker expression, and resolves to nothing once
    /// no weaker expression is left.
    #[test]
    fn weaker_references_compose_as_in_openusd() {
        let compose = |strong: &str, weak: &str| {
            parse(strong)
                .unwrap()
                .compose_over(&parse(weak).unwrap())
                .text()
        };
        assert_eq!(compose("/A %_", "/B /C"), "/A (/B /C)");
        assert_eq!(compose("/A & %_", "/B /C"), "/A & /B /C");
        assert_eq!(compose("/A - %_", "/B - /C"), "/A - (/B - /C)");
        assert_eq!(compose("/A %_", ""), "/A");
        assert_eq!(compose("/A %_", "%_ /D"), "/A (%_ /D)");
        assert_eq!(compose("%_", "/D"), "/D");
        assert_eq!(compose("%_ - %_", ""), "");
        assert_eq!(compose("~%_", ""), "//");
        assert_eq!(compose("/X", "/B"), "/X");
    }

    /// Relative patterns anchor at the authoring prim; patterns and
    /// reference paths map through the arcs, and drop out of the domain of
    /// a map without a root identity (checked against OpenUSD 26.08).
    #[test]
    fn anchoring_and_mapping_match_openusd() {
        let reference = [ArcMap {
            source: names("/Gear"),
            target: names("/Part"),
            root_identity: false,
        }];
        let map = |text: &str, anchor: &str| {
            anchor_and_map(parse(text).unwrap(), &names(anchor), &reference).text()
        };
        assert_eq!(map(".//", "/Gear"), "/Part//");
        assert_eq!(map(".", "/Gear"), "/Part");
        assert_eq!(
            map("Tip .. ../Hub", "/Gear/Tooth"),
            "/Part/Tooth/Tip /Part /Part/Hub"
        );
        assert_eq!(map("/Gear/Tooth //Tooth", "/Gear"), "/Part/Tooth //Tooth");
        assert_eq!(
            map("/Gear/Tooth /Elsewhere ../Elsewhere", "/Gear"),
            "/Part/Tooth"
        );
        assert_eq!(map("/Elsewhere", "/Gear"), "");
        assert_eq!(map("/", "/Gear"), "");
        assert_eq!(map("//", "/Gear"), "//");
        assert_eq!(map("~/Elsewhere", "/Gear"), "//");
        assert_eq!(map("/Elsewhere - /Gear/T", "/Gear"), "");
        assert_eq!(map("/Gear/T - /Elsewhere", "/Gear"), "/Part/T");
        assert_eq!(map("~/Elsewhere - /Gear/T", "/Gear"), "~/Part/T");
        assert_eq!(
            map("/Gear/T.x:y /Gear.size", "/Gear"),
            "/Part/T.x:y /Part.size"
        );
        assert_eq!(map("/Gear/T* /Gear//T", "/Gear"), "/Part/T* /Part//T");
        assert_eq!(map("%/Gear:x %/Else:y %:z %_", "/Gear"), "%/Part:x %:z %_");

        let internal = [ArcMap {
            source: names("/Local"),
            target: names("/Copy"),
            root_identity: true,
        }];
        let map =
            |text: &str| anchor_and_map(parse(text).unwrap(), &names("/Local"), &internal).text();
        assert_eq!(
            map("/Local/Pin /Stray Pin ../Stray /Copy/X"),
            "/Copy/Pin /Stray /Copy/Pin /Stray"
        );
        assert_eq!(map("/"), "/");
    }

    /// Character classes, root globs and relative references authored on
    /// `/Model`, mapped across an external reference and an internal one
    /// to `/Copy` (checked against OpenUSD 26.08 `MakeAbsolute` and
    /// `PcpMapFunction::MapSourceToTarget`).
    #[test]
    fn globs_and_references_map_as_in_openusd() {
        let external = [ArcMap {
            source: names("/Model"),
            target: names("/Copy"),
            root_identity: false,
        }];
        let internal = [ArcMap {
            root_identity: true,
            ..external[0].clone()
        }];
        let map = |text: &str, maps: &[ArcMap]| {
            anchor_and_map(parse(text).unwrap(), &names("/Model"), maps).text()
        };
        for (text, over_external, over_internal) in [
            ("/Model/A[a-z]", "/Copy/A[a-z]", "/Copy/A[a-z]"),
            ("/Model/A[!a-z]", "/Copy/A[!a-z]", "/Copy/A[!a-z]"),
            (
                "/Model/A[a-z0-9_]*",
                "/Copy/A[a-z0-9_]*",
                "/Copy/A[a-z0-9_]*",
            ),
            ("A[a-z]", "/Copy/A[a-z]", "/Copy/A[a-z]"),
            (
                "A[-a] A[a-]",
                "/Copy/A[-a] /Copy/A[a-]",
                "/Copy/A[-a] /Copy/A[a-]",
            ),
            (
                "/Model/A[a-z] - /Model/B",
                "/Copy/A[a-z] - /Copy/B",
                "/Copy/A[a-z] - /Copy/B",
            ),
            (
                "/Model/A? /Model/B*",
                "/Copy/A? /Copy/B*",
                "/Copy/A? /Copy/B*",
            ),
            ("/A*", "", "/A*"),
            ("/*", "", "/*"),
            ("/A*/B", "", "/A*/B"),
            ("//A*", "//A*", "//A*"),
            ("//", "//", "//"),
            ("*", "/Copy/*", "/Copy/*"),
            ("../*", "", "/*"),
            ("..//A", "//A", "//A"),
            ("../Other//", "", "/Other//"),
            ("/Model/A*//B", "/Copy/A*//B", "/Copy/A*//B"),
            ("%../Model:foo", "%/Copy:foo", "%/Copy:foo"),
            ("%..:foo", "", "%/:foo"),
            ("%:foo", "%:foo", "%:foo"),
            ("%/Model/Sub:foo", "%/Copy/Sub:foo", "%/Copy/Sub:foo"),
            ("%/Other:foo", "", "%/Other:foo"),
        ] {
            assert_eq!(map(text, &external), over_external, "{text:?} external");
            assert_eq!(map(text, &internal), over_internal, "{text:?} internal");
        }
    }

    /// Only a chain whose strongest answering opinion authors a path
    /// expression takes the expression fold; any other chain, a dense array
    /// or a spline over a weaker expression included, is left to ordinary
    /// resolution.
    #[test]
    fn only_expression_chains_fold() {
        use crate::{
            doc::LayerOffset,
            interner::TokenInterner,
            path::PathInterner,
            property::PropertySpec,
            spline::{CurveType, Extrapolation, SplineData, SplineDataType},
        };
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path = SpecPath::parse("/A", &mut tokens, &mut paths).expect("spec path");
        let lookup_path = paths.intern(crate::path::Path::root());
        let field = tokens.intern("x");
        let opinion = |strength: u16, spec: PropertySpec| Opinion {
            key: crate::prim_index::OpinionKey {
                node: NodeId::ROOT,
                layer_strength: strength,
                layer_id: crate::doc::LayerId(1),
                lookup_path,
                spec_path: spec_path.clone(),
            },
            field,
            value: spec.into(),
            layer_offset: LayerOffset::IDENTITY,
        };
        let expression = |text: &str| Value::PathExpression(text.into());
        let weaker = opinion(1, PropertySpec::attribute().with_default(expression("/W")));
        let chain = |strongest: PropertySpec| [opinion(0, strongest), weaker.clone()];
        let at_time = |opinions: &[Opinion]| {
            fold_at_time(opinions, 1.0, InterpolationType::Held, None).and_then(|fold| fold.value)
        };

        let array =
            chain(PropertySpec::attribute().with_default(Value::Array(Vec::from([Value::Int(1)]))));
        assert_eq!(fold_default(&array, None), None);
        assert_eq!(at_time(&array), None);
        let spline = chain(PropertySpec::attribute().with_spline(SplineData {
            data_type: SplineDataType::Double,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Held,
            post_extrapolation: Extrapolation::Held,
            loop_params: None,
            knots: Vec::new(),
        }));
        assert_eq!(at_time(&spline), None);

        let sampled = chain(
            PropertySpec::attribute().with_time_samples(Vec::from([(0.0, expression("/S %_"))])),
        );
        assert_eq!(at_time(&sampled), Some(expression("/S /W")));
        // No opinion answers: the schema fallback decides.
        assert_eq!(
            fold_at_time(&[], 1.0, InterpolationType::Held, Some(&expression("/F")))
                .and_then(|fold| fold.value),
            Some(expression("/F"))
        );
    }

    #[test]
    fn anchoring_arrays_keeps_other_values_and_unsupported_text() {
        let expression = |text: &str| Value::PathExpression(Arc::from(text));
        let mut value = Value::Array(Vec::from([
            expression("Child"),
            Value::Int(7),
            Value::Array(Vec::from([expression("Leaf")])),
            expression("(/A"),
        ]));
        anchor_value(&mut value, &names("/Root"), &[]);
        assert_eq!(
            value,
            Value::Array(Vec::from([
                expression("/Root/Child"),
                Value::Int(7),
                Value::Array(Vec::from([expression("/Root/Leaf")])),
                expression("(/A"),
            ]))
        );
    }

    #[test]
    fn unsupported_text_is_not_parsed() {
        assert_eq!(parse("(/A"), None);
        assert_eq!(parse("/A{isa:Mesh"), None);
        assert_eq!(parse("/A[a-z"), None);
        assert_eq!(parse("/A -"), None);
        assert_eq!(parse("%Sub:foo"), None);
        assert!(parse("/A /B").is_some());
        // OpenUSD stops reading at a line end.
        assert_eq!(parse("/A\n/B").map(|e| e.text()), Some("/A".into()));
    }
}
