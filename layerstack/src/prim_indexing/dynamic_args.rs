// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The walk a [`DynamicFormatContext`](crate::dynamic_format::DynamicFormatContext)
//! composes opinions with, over the prim index being built.
//!
//! The opinions for a payload's arguments are those of the prim index as it
//! stands when the payload is added: starting at the payload's parent node,
//! its path is translated to the root of the graph (or as close as it maps),
//! and, while that graph is being built recursively for an arc of another
//! index, across that arc to the graph it will join, until the outermost
//! graph's root. The opinions are then read root first, strongest first:
//! each node's layer stack at the path, then its children in strength
//! order, the path mapped into each. Among the children of the payload's
//! parent, the walk stops at the first one weaker than the payload, so
//! arcs that will be weaker than the payload contribute nothing.
//!
//! OpenUSD: `PcpDynamicFileFormatContext::_ComposeValueHelper`
//! (`_ComposeOpinionFromAncestors`, `_ComposeOpinionInSubtree`) in
//! `pxr/usd/pcp/dynamicFileFormatContext.cpp`, and
//! `PcpCompareSiblingPayloadNodeStrength` in `pxr/usd/pcp/strengthOrdering.cpp`.
//! AOUSD Core §10.4 (strength ordering).

use alloc::{collections::BTreeSet, vec::Vec};

use crate::{
    doc::{FieldValue, LayerId, LayerStore, Value, get_field},
    interner::TokenId,
    path::PathId,
    prim_index::ArcKind,
    schema::{MetadataDefinition, SchemaRegistry},
    spec_path::SpecPath,
};

use super::{
    graph::{Graph, NodeIndex},
    indexer::Frame,
    layer_stacks::LayerStacks,
    site, site_paths,
};

/// Where a payload's arguments are composed: its parent node in the graph
/// being built, the path the payload is introduced at, its arc number, and
/// the frames of the recursive indexing in progress.
pub(crate) struct Walk<'a> {
    pub(crate) store: &'a dyn LayerStore,
    pub(crate) stacks: &'a LayerStacks,
    pub(crate) schemas: Option<&'a SchemaRegistry>,
    pub(crate) graph: &'a Graph,
    pub(crate) parent: NodeIndex,
    pub(crate) path_in_node: SpecPath,
    pub(crate) arc_num: u16,
    pub(crate) previous_frame: Option<&'a Frame<'a>>,
}

impl Walk<'_> {
    /// The plugin metadata declaration of `field`, if registered.
    pub(crate) fn registered_metadata(&self, field: &str) -> Option<&MetadataDefinition> {
        let name = self.store.tokens().lookup(field)?;
        self.schemas?.metadata(name)
    }

    /// Passes the opinions of the field `field` (or, with `property`, of
    /// that attribute's default) to `found`, strongest first, only the
    /// strongest when `strongest_only`. Records in `sites` every spec read.
    pub(crate) fn compose(
        &self,
        property: Option<&str>,
        field: &str,
        strongest_only: bool,
        sites: &mut BTreeSet<(LayerId, PathId)>,
        found: &mut dyn FnMut(Value),
    ) {
        let tokens = self.store.tokens();
        let key = match property {
            Some(name) => Key::Default(tokens.lookup(name)),
            None => Key::Field(tokens.lookup(field)),
        };
        let mut state = State {
            walk: self,
            key,
            strongest_only,
            sites,
            found,
            frame: self.previous_frame,
        };
        state.compose_from_ancestors(self.graph, self.parent, self.path_in_node.clone());
    }
}

/// What is read at each spec: a metadata field, or an attribute's default.
/// `None` for a name the store never interned, which no spec can author.
#[derive(Clone, Copy)]
enum Key {
    Field(Option<TokenId>),
    Default(Option<TokenId>),
}

struct State<'w, 'a> {
    walk: &'w Walk<'a>,
    key: Key,
    strongest_only: bool,
    sites: &'w mut BTreeSet<(LayerId, PathId)>,
    found: &'w mut dyn FnMut(Value),
    /// The frame to cross once the walk reaches its graph's root.
    /// OpenUSD: the `PcpPrimIndex_StackFrameIterator`.
    frame: Option<&'a Frame<'a>>,
}

impl<'a> State<'_, 'a> {
    /// Composes the opinions from `node`'s ancestors and their subtrees,
    /// across frames; `true` once composition should stop.
    /// OpenUSD: `_ComposeOpinionFromAncestors`.
    fn compose_from_ancestors(
        &mut self,
        graph: &'a Graph,
        node: NodeIndex,
        path: SpecPath,
    ) -> bool {
        let paths = self.walk.store.paths();
        let (rootmost_path, rootmost) = translate_to_root_or_closest(paths, graph, node, &path);
        if rootmost == 0
            && let Some(frame) = self.frame
        {
            let parent_path = frame
                .arc_map_to_parent
                .map_source_to_target(paths, rootmost_path.prim_path());
            self.frame = frame.previous;
            // A path the arc does not map has no opinions in the parent
            // graph, nor beyond it (OpenUSD walks on with the empty path).
            if let Some(parent_path) = parent_path
                && self.compose_from_ancestors(
                    frame.parent_graph,
                    frame.parent_node,
                    SpecPath::from_prim_path(parent_path),
                )
            {
                return true;
            }
        }
        self.compose_in_subtree(graph, rootmost, &rootmost_path)
    }

    /// Composes the opinions of `node`, at `path`, and of its subtree;
    /// `true` once composition should stop.
    /// OpenUSD: `_ComposeOpinionInSubtree`.
    fn compose_in_subtree(&mut self, graph: &'a Graph, node: NodeIndex, path: &SpecPath) -> bool {
        let store = self.walk.store;
        let paths = store.paths();
        let entry = graph.node(node);
        let stack = self.walk.stacks.get(entry.site.stack);
        for (_, layer, spec) in site::specs(store, stack, path) {
            self.sites.insert((layer, path.prim_path()));
            let value = match self.key {
                Key::Field(Some(field)) => match get_field(spec.fields(), &field) {
                    Some(FieldValue::Value(value)) => Some(value.clone()),
                    _ => None,
                },
                Key::Default(Some(name)) => spec
                    .properties()
                    .iter()
                    .find(|property| property.name == name)
                    .and_then(|property| property.spec.default.as_ref())
                    .map(|value| value.materialized().clone()),
                Key::Field(None) | Key::Default(None) => None,
            };
            if let Some(value) = value {
                (self.found)(evaluate_assets(store, stack, value));
                if self.strongest_only {
                    return true;
                }
            }
        }
        for child in graph.children(node) {
            // Among the payload's future siblings, the first weaker than it
            // ends composition: it and the arcs after it will be weaker.
            if core::ptr::eq(graph, self.walk.graph)
                && node == self.walk.parent
                && payload_is_stronger(graph, node, self.walk.arc_num, child)
            {
                return true;
            }
            let child_entry = graph.node(child);
            let Some(in_child) = child_entry
                .map_to_parent
                .map_target_to_source(paths, path.prim_path())
            else {
                continue;
            };
            let at_intro = graph.path_at_introduction(paths, child);
            let in_child = site_paths::apply_selections_of(paths, in_child, &at_intro);
            if self.compose_in_subtree(graph, child, &in_child) {
                return true;
            }
        }
        false
    }
}

/// `path`, a site of `node`, translated to the root node of `graph`, or to
/// the node closest to it that the path maps to, with that node's variant
/// selections applied. OpenUSD: `Pcp_TranslatePathFromNodeToRootOrClosestNode`.
pub(crate) fn translate_to_root_or_closest(
    paths: &crate::path::PathInterner,
    graph: &Graph,
    node: NodeIndex,
    path: &SpecPath,
) -> (SpecPath, NodeIndex) {
    if node == 0 {
        return (path.clone(), 0);
    }
    let mut current = node;
    let mut current_path = path.prim_path();
    if let Some(in_root) = graph
        .node(node)
        .map_to_root
        .map_source_to_target(paths, current_path)
    {
        current = 0;
        current_path = in_root;
    } else {
        while current != 0 {
            let entry = graph.node(current);
            let Some(in_parent) = entry
                .map_to_parent
                .map_source_to_target(paths, current_path)
            else {
                break;
            };
            current = entry.parent;
            current_path = in_parent;
        }
    }
    let at_intro = graph.path_at_introduction(paths, current);
    (
        site_paths::apply_selections_of(paths, current_path, &at_intro),
        current,
    )
}

/// Whether a payload of `parent`, its `arc_num`th, will be stronger than
/// `parent`'s child `sibling`. OpenUSD: `PcpCompareSiblingPayloadNodeStrength`
/// returning `-1`.
pub(crate) fn payload_is_stronger(
    graph: &Graph,
    parent: NodeIndex,
    arc_num: u16,
    sibling: NodeIndex,
) -> bool {
    let sibling = graph.node(sibling);
    let (payload, other) = (
        ArcKind::Payloads.strength_rank(),
        sibling.arc_kind.strength_rank(),
    );
    if payload != other {
        return payload < other;
    }
    // Deeper opinions are stronger.
    let depth = graph.node(parent).namespace_depth;
    if depth != sibling.namespace_depth {
        return depth > sibling.namespace_depth;
    }
    arc_num < sibling.sibling_num
}

/// `value` with each asset path that is a variable expression evaluated
/// with the expression variables of `stack`, which authors it.
///
/// OpenUSD resolves asset-valued opinions too (`SdfResolveAssetPaths`);
/// composition here has no asset resolver, so the format anchors them.
fn evaluate_assets(
    store: &dyn LayerStore,
    stack: &super::layer_stacks::ComputedStack,
    value: Value,
) -> Value {
    let evaluate = |asset: alloc::sync::Arc<str>| {
        if !crate::variable_expression::is_expression(&asset) {
            return Value::Asset(asset);
        }
        let mut chain: Vec<_> = stack.chain.to_vec();
        if chain.last() != Some(&stack.identifier) {
            chain.push(stack.identifier);
        }
        match crate::expression_variables::evaluate(store, &chain, &asset, None) {
            Ok(path) => Value::Asset(path.unwrap_or_default().into()),
            Err(_) => Value::Asset(asset),
        }
    };
    match value {
        Value::Asset(asset) => evaluate(asset),
        Value::Array(items) if items.iter().any(|item| matches!(item, Value::Asset(_))) => {
            Value::Array(
                items
                    .into_iter()
                    .map(|item| match item {
                        Value::Asset(asset) => evaluate(asset),
                        other => other,
                    })
                    .collect(),
            )
        }
        other => other,
    }
}
