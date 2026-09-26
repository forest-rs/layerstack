// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Expression variables in composition.
//!
//! A layer stack's expression variables are the `expressionVariables`
//! authored on its root layer. A layer stack reached through a reference or
//! payload composes them beneath those of the layer stack that reaches it:
//! a variable the referencing layer stack sets wins. Composition evaluates
//! the variable expressions it meets ([`crate::variable_expression`]) with
//! the variables of the layer stack that authors them:
//!
//! - a sublayer asset path, with the variables composed along the chain of
//!   arcs that reaches the layer stack (`LayerStack::gather_recording`), so
//!   one root layer reached from two contexts may gather two stacks;
//! - a reference or payload asset path, per authoring layer, as its list op
//!   is read and before list ops compose, with the variables composed along
//!   the chain of arcs that reaches the authoring layer stack
//!   ([`ArcAnchor`], `anchor_internal_arcs`), so list editing compares the
//!   evaluated, anchored arcs;
//! - a variant selection, where composition reads it, with the variables
//!   of the chain of arcs through which it reads that site
//!   ([`site_selections`], [`SiteContext`]), so one layer reached from two
//!   contexts may select two variants; it is an error or a dependency only
//!   where composition reads it ([`read_selections`]).
//!
//! An asset path expression that evaluates to no value or to an empty
//! string drops the sublayer or arc without an error; any expression that
//! fails to evaluate is dropped and reported as a
//! [`VariableExpressionError`]. A non-empty asset path is resolved relative
//! to the layer that authors it ([`LayerStore::asset_layer`]). The
//! variables each evaluation reads are recorded ([`VariableReads`]), so a
//! live stage recomposes when one of them changes.
//!
//! Spec: AOUSD Core §7.6.1.7 reserves the `expressionVariables` layer field
//! as out of scope; §10.3.1 (sublayers), §10.3.2.1 (references),
//! §10.3.2.2 (payloads) and §10.3.2.5.1 (variant selections) define where
//! expressions are evaluated. OpenUSD: `PcpExpressionVariables::Compute`
//! (`pxr/usd/pcp/expressionVariables.cpp`), `_BuildLayerStack`
//! (`pxr/usd/pcp/layerStack.cpp`), `_PcpComposeSiteReferencesOrPayloads`
//! and `PcpComposeSiteVariantSelection` (`pxr/usd/pcp/composeSite.cpp`),
//! `_EvalRefOrPayloadArcs` (`pxr/usd/pcp/primIndex.cpp`) and
//! `Pcp_EvaluateVariableExpression` (`pxr/usd/pcp/utils.cpp`).

use alloc::{borrow::Cow, collections::BTreeSet, string::String, vec::Vec};
use core::cell::{OnceCell, RefCell};

use hashbrown::{HashMap, HashSet};

use crate::{
    asset::ExpressionAssetPath,
    composition_error::{ExpressionContext, VariableExpressionError},
    doc::{FieldValue, Layer, LayerId, LayerStore, Reference, Value},
    interner::{TokenId, TokenInterner},
    layer_stack::LayerStack,
    listop::ListOp,
    prim_index::{OpinionKey, PrimIndex},
    prim_index_graph::{NodeId, PrimIndexGraph, PrimNode},
    variable_expression::{
        ExpressionValue, ExpressionVariables, VariableExpression, VariableValue, is_expression,
    },
};

/// The layer metadata field holding a layer's expression variables.
pub(crate) const EXPRESSION_VARIABLES: &str = "expressionVariables";

/// The `expressionVariables` authored on `layer`; empty when it authors
/// none.
///
/// Each value converts as OpenUSD's supports it: strings, booleans,
/// integers (`int` and `int64`), and arrays of one of those; `None` for an
/// authored `None`. A value of any other type is kept as
/// [`VariableValue::Unsupported`], which is an error where it is used.
#[must_use]
pub(crate) fn layer_expression_variables(
    layer: &Layer,
    tokens: &TokenInterner,
) -> ExpressionVariables {
    let mut variables = ExpressionVariables::new();
    let Some(field) = tokens.lookup(EXPRESSION_VARIABLES) else {
        return variables;
    };
    if let Some(FieldValue::Value(Value::Dictionary(entries))) = layer.metadata(field) {
        for (name, value) in entries {
            variables.insert(&**name, variable_value(value));
        }
    }
    variables
}

/// Converts an authored dictionary value to an expression variable.
///
/// OpenUSD: `SdfVariableExpression::IsValidVariableType` and
/// `CoerceIfUnsupportedValueType` (`int` becomes `int64`).
fn variable_value(value: &Value) -> VariableValue {
    let scalar = |value: &Value| match value {
        Value::String(s) => Some(ExpressionValue::String(String::from(&**s))),
        Value::Int(n) => Some(ExpressionValue::Int(i64::from(*n))),
        Value::Int64(n) => Some(ExpressionValue::Int(*n)),
        Value::Bool(b) => Some(ExpressionValue::Bool(*b)),
        _ => None,
    };
    match value {
        Value::Blocked | Value::Null => VariableValue::None,
        Value::Array(items) => {
            let items: Option<Vec<ExpressionValue>> = items.iter().map(scalar).collect();
            match items.as_deref() {
                Some([]) => VariableValue::Value(ExpressionValue::EmptyList),
                Some(items) => list(items).map_or_else(
                    || VariableValue::Unsupported(String::from("array")),
                    VariableValue::Value,
                ),
                None => VariableValue::Unsupported(String::from("array")),
            }
        }
        other => scalar(other).map_or_else(
            || VariableValue::Unsupported(String::from(value_type_name(other))),
            VariableValue::Value,
        ),
    }
}

/// A list of scalars of one type, or `None` for mixed types.
fn list(items: &[ExpressionValue]) -> Option<ExpressionValue> {
    let strings: Option<Vec<String>> = items.iter().map(|v| v.as_str().map(String::from)).collect();
    if let Some(strings) = strings {
        return Some(ExpressionValue::StringList(strings));
    }
    let ints: Option<Vec<i64>> = items
        .iter()
        .map(|v| match v {
            ExpressionValue::Int(n) => Some(*n),
            _ => None,
        })
        .collect();
    if let Some(ints) = ints {
        return Some(ExpressionValue::IntList(ints));
    }
    let bools: Option<Vec<bool>> = items
        .iter()
        .map(|v| match v {
            ExpressionValue::Bool(b) => Some(*b),
            _ => None,
        })
        .collect();
    bools.map(ExpressionValue::BoolList)
}

/// A short type name for a value expressions do not support.
fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::UChar(_) => "uchar",
        Value::UInt(_) => "uint",
        Value::UInt64(_) => "uint64",
        Value::Half(_) => "half",
        Value::Float(_) => "float",
        Value::Double(_) => "double",
        Value::Token(_) => "token",
        Value::Asset(_) => "asset",
        Value::TimeCode(_) => "timecode",
        Value::Dictionary(_) => "dictionary",
        _ => "value",
    }
}

/// The expression variables of the layer stack reached through the chain
/// of layer stacks `stacks`, by root layer, outermost first: each layer
/// stack's variables composed beneath those of the layer stacks before it.
///
/// OpenUSD: `PcpExpressionVariables::Compute`, whose override source is the
/// layer stack that introduces the next.
pub(crate) fn composed_variables(
    store: &dyn LayerStore,
    stacks: &[LayerId],
) -> ExpressionVariables {
    let mut variables = ExpressionVariables::new();
    if store.tokens().lookup(EXPRESSION_VARIABLES).is_none() {
        // No layer authors `expressionVariables`.
        return variables;
    }
    for root in stacks {
        if let Some(layer) = store.layer(*root) {
            variables.compose_over(&layer_expression_variables(layer, store.tokens()));
        }
    }
    variables
}

/// The expression variables composition read, for dependency tracking:
/// for each root layer and variable name looked up there, the value found
/// (`None` when the layer does not set it).
///
/// A variable is looked up in each layer stack of the chain, outermost
/// first, until one sets it, so authoring it on a stronger layer stack is a
/// change too. OpenUSD records the variable names per layer stack
/// (`PcpExpressionVariablesDependencyData`).
#[derive(Clone, Debug, Default)]
pub(crate) struct VariableReads {
    reads: HashMap<(LayerId, String), Option<VariableValue>>,
}

impl VariableReads {
    /// Records the lookups of `used` along `stacks`.
    fn record(&mut self, store: &dyn LayerStore, stacks: &[LayerId], used: &BTreeSet<String>) {
        for name in used {
            for root in stacks {
                let value = store.layer(*root).and_then(|layer| {
                    layer_expression_variables(layer, store.tokens())
                        .get(name)
                        .cloned()
                });
                let found = value.is_some();
                self.reads.insert((*root, name.clone()), value);
                if found {
                    break;
                }
            }
        }
    }

    /// Adds the reads of `other`.
    pub(crate) fn extend(&mut self, other: Self) {
        self.reads.extend(other.reads);
    }

    /// Returns `true` when a variable read from `layer` no longer has the
    /// value composition found.
    pub(crate) fn changed(&self, store: &dyn LayerStore, layer: LayerId) -> bool {
        let mut current = None;
        self.reads
            .iter()
            .filter(|((read, _), _)| *read == layer)
            .any(|((_, name), value)| {
                let now = current.get_or_insert_with(|| {
                    store
                        .layer(layer)
                        .map(|l| layer_expression_variables(l, store.tokens()))
                        .unwrap_or_default()
                });
                now.get(name) != value.as_ref()
            })
    }
}

/// Evaluates `expression`, an asset path or variant selection, with the
/// variables of the chain of layer stacks `stacks` (see
/// [`composed_variables`]), recording the variables it reads.
///
/// Returns the string it evaluates to, `None` for no value or an empty
/// string, or the errors joined.
pub(crate) fn evaluate(
    store: &dyn LayerStore,
    stacks: &[LayerId],
    expression: &str,
    reads: Option<&mut VariableReads>,
) -> Result<Option<String>, String> {
    let variables = composed_variables(store, stacks);
    let evaluation = VariableExpression::parse(expression).evaluate(&variables);
    if let Some(reads) = reads {
        reads.record(store, stacks, &evaluation.used_variables);
    }
    Ok(evaluation.into_string()?.filter(|s| !s.is_empty()))
}

/// The expression variables of one layer stack, reached through a chain of
/// arcs, for evaluating the asset paths of the arcs its layers author (see
/// [`ArcAnchor`]), with what those evaluations found.
///
/// The variables are composed when an expression first needs them.
pub(crate) struct ExpressionScope {
    /// The root layers of the layer stacks on the chain, outermost first,
    /// ending with the one whose arcs are read.
    stacks: Vec<LayerId>,
    variables: OnceCell<ExpressionVariables>,
    findings: RefCell<ScopeFindings>,
}

/// What evaluating arc asset paths in an [`ExpressionScope`] found.
#[derive(Default)]
pub(crate) struct ScopeFindings {
    /// The variables read.
    pub(crate) reads: VariableReads,
    /// The expressions that failed: context, authoring layer, expression
    /// and error.
    pub(crate) errors: Vec<(ExpressionContext, LayerId, String, String)>,
}

impl ExpressionScope {
    /// A scope for the chain of layer stacks `stacks` (see
    /// [`composed_variables`]).
    pub(crate) fn new(stacks: Vec<LayerId>) -> Self {
        Self {
            stacks,
            variables: OnceCell::new(),
            findings: RefCell::new(ScopeFindings::default()),
        }
    }

    /// What the evaluations in this scope found.
    pub(crate) fn into_findings(self) -> ScopeFindings {
        self.findings.into_inner()
    }
}

/// The layer stack whose arcs are being read: its root layer, which
/// internal arcs target, and the [`ExpressionScope`] their asset path
/// expressions are evaluated in, if any.
///
/// Without a scope an expression arc is kept unevaluated, and targets
/// nothing ([`Reference::is_unresolved`]).
#[derive(Clone, Copy)]
pub(crate) struct ArcAnchor<'a> {
    /// The root layer of the layer stack.
    pub(crate) layer: LayerId,
    scope: Option<&'a ExpressionScope>,
    /// Whether the arcs read are payloads, for error reports.
    payloads: bool,
}

impl<'a> ArcAnchor<'a> {
    /// The anchor of the layer stack rooted at `layer`, evaluating in
    /// `scope`.
    pub(crate) fn new(layer: LayerId, scope: Option<&'a ExpressionScope>) -> Self {
        Self {
            layer,
            scope,
            payloads: false,
        }
    }

    /// This anchor, for reading payloads.
    #[must_use]
    pub(crate) fn payloads(self) -> Self {
        Self {
            payloads: true,
            ..self
        }
    }

    /// The reference or payload `arc`, authored in `layer` with a variable
    /// expression as its asset path, evaluated in this anchor's scope and
    /// resolved relative to `layer` ([`LayerStore::asset_layer`]); an
    /// evaluated path that does not resolve leaves the arc unresolved,
    /// which composition reports when it follows the arc.
    ///
    /// Returns `None` for an expression that evaluates to nothing, and for
    /// one that fails, which the scope records. Without a scope, `arc` is
    /// returned unevaluated.
    ///
    /// OpenUSD evaluates in the list-op callback of each layer, then
    /// anchors the result with `SdfComputeAssetPathRelativeToLayer`
    /// (`_PcpComposeSiteReferencesOrPayloads` in
    /// `pxr/usd/pcp/composeSite.cpp`).
    pub(crate) fn evaluate(
        self,
        store: &dyn LayerStore,
        arc: &Reference,
        layer: LayerId,
    ) -> Option<Reference> {
        let Some(scope) = self.scope else {
            return Some(arc.clone());
        };
        let expression = arc.asset.as_deref().unwrap_or_default();
        let variables = scope
            .variables
            .get_or_init(|| composed_variables(store, &scope.stacks));
        let evaluation = VariableExpression::parse(expression).evaluate(variables);
        let mut findings = scope.findings.borrow_mut();
        findings
            .reads
            .record(store, &scope.stacks, &evaluation.used_variables);
        match evaluation.into_string() {
            Ok(Some(path)) if !path.is_empty() => Some(Reference {
                layer: store
                    .asset_layer(layer, &path)
                    .unwrap_or(LayerId::UNRESOLVED),
                asset: Some(path),
                ..arc.clone()
            }),
            Ok(_) => None,
            Err(error) => {
                let context = if self.payloads {
                    ExpressionContext::Payload
                } else {
                    ExpressionContext::Reference
                };
                findings
                    .errors
                    .push((context, layer, String::from(expression), error));
                None
            }
        }
    }
}

/// The error for `expression`, authored in `layer`, that failed to
/// evaluate with `error`.
pub(crate) fn expression_error(
    context: ExpressionContext,
    layer: LayerId,
    prim: Option<crate::path::PathId>,
    expression: &str,
    error: String,
) -> crate::CompositionError {
    crate::CompositionError::VariableExpressionError(VariableExpressionError {
        prim,
        context,
        layer,
        expression: String::from(expression),
        error,
    })
}

/// The layer stacks reachable from `root` through sublayers, references and
/// payloads, each with the expression variables composed for it; see
/// [`walk`].
pub(crate) struct Walk {
    /// Each layer stack reached, in the order reached. A layer stack reached
    /// with different variables is listed once for each.
    pub(crate) stacks: Vec<WalkedStack>,
    /// The asset paths variable expressions evaluate to that `store` does
    /// not resolve, sorted.
    pub(crate) unresolved: Vec<ExpressionAssetPath>,
}

/// Walks the layer stacks reachable from the layer stack rooted at `root`
/// through the references and payloads their layers author, in every
/// variant branch, breadth first, evaluating the asset path expressions
/// with each layer stack's variables.
pub(crate) fn walk(store: &dyn LayerStore, root: LayerId) -> Walk {
    let mut seen: HashSet<(LayerId, ExpressionVariables)> = HashSet::new();
    let mut unresolved: BTreeSet<ExpressionAssetPath> = BTreeSet::new();
    let mut stacks = Vec::new();
    let mut queue = alloc::collections::VecDeque::new();
    queue.push_back(alloc::vec![root]);
    while let Some(chain) = queue.pop_front() {
        let stack_root = *chain.last().expect("a chain ends at its layer stack");
        let variables = composed_variables(store, &chain);
        if !seen.insert((stack_root, variables.clone())) {
            continue;
        }
        let stack = LayerStack::gather_recording(store, &chain, &mut Vec::new(), None);
        for layer in stack.layers.iter().filter_map(|id| store.layer(*id)) {
            for sublayer in &layer.sublayers {
                let Some(asset) = sublayer.asset.as_deref().filter(|a| is_expression(a)) else {
                    continue;
                };
                let evaluated = VariableExpression::parse(asset)
                    .evaluate(&variables)
                    .into_string();
                if let Ok(Some(path)) = evaluated
                    && !path.is_empty()
                    && store.asset_layer(layer.id, &path).is_none()
                {
                    unresolved.insert(ExpressionAssetPath {
                        anchor: layer.id,
                        asset_path: path,
                    });
                }
            }
            for arc in layer_arcs(layer) {
                let target = if arc.is_expression() {
                    let expression = arc.asset.as_deref().unwrap_or_default();
                    let evaluated = VariableExpression::parse(expression)
                        .evaluate(&variables)
                        .into_string();
                    let Ok(Some(path)) = evaluated else {
                        continue;
                    };
                    if path.is_empty() {
                        continue;
                    }
                    match store.asset_layer(arc.layer, &path) {
                        Some(target) => target,
                        None => {
                            unresolved.insert(ExpressionAssetPath {
                                anchor: arc.layer,
                                asset_path: path,
                            });
                            continue;
                        }
                    }
                } else if arc.is_unresolved() || arc.asset.is_none() {
                    // Unresolved, or internal: its target is this layer
                    // stack.
                    continue;
                } else {
                    arc.layer
                };
                let mut target_chain = chain.clone();
                target_chain.push(target);
                queue.push_back(target_chain);
            }
        }
        stacks.push(WalkedStack { stack });
    }
    Walk {
        stacks,
        unresolved: unresolved.into_iter().collect(),
    }
}

/// A layer stack [`walk`] reached.
pub(crate) struct WalkedStack {
    /// The layer stack, gathered with the variables of the chain that
    /// reaches it.
    pub(crate) stack: LayerStack,
}

/// Where a site's variant selections are read: the context their variable
/// expressions evaluate in.
///
/// OpenUSD evaluates a selection with the expression variables of the layer
/// stack of the node it is read through (`PcpComposeSiteVariantSelection`
/// in `pxr/usd/pcp/composeSite.cpp`), composed along the arcs that reach it,
/// so one layer reached from two contexts may select two variants.
#[derive(Clone, Copy)]
pub(crate) enum SiteContext<'a> {
    /// A site of a layer stack reached through the chain of layer stacks,
    /// outermost first (see [`LayerStack::chain_of`]).
    Chain(&'a [LayerId]),
    /// A site of the layer stack of `node` in a prim index's graph.
    Node(&'a PrimIndexGraph, NodeId),
}

impl SiteContext<'_> {
    /// The context of `source`, a site of the prim index `index`.
    pub(crate) fn of_source<'a>(index: &'a PrimIndex, source: &OpinionKey) -> SiteContext<'a> {
        SiteContext::Node(&index.graph, source.node)
    }

    /// The root layers of the layer stacks whose variables apply,
    /// outermost first.
    fn chain(self) -> Vec<LayerId> {
        match self {
            Self::Chain(chain) => chain.to_vec(),
            Self::Node(graph, node) => node_chain(graph, node),
        }
    }
}

/// The root layers of the layer stacks on the arcs from `graph`'s root to
/// `node`, outermost first, up to the first node of `node`'s layer stack:
/// the chain whose variables `node`'s layer stack composes (see
/// [`composed_variables`]).
fn node_chain(graph: &PrimIndexGraph, node: NodeId) -> Vec<LayerId> {
    let mut stacks = Vec::new();
    let mut cursor = Some(node);
    while let Some(id) = cursor {
        let Some(current) = graph.node(id) else {
            break;
        };
        stacks.push(current.layer_stack());
        cursor = current.parent();
    }
    stacks.reverse();
    let own = graph.node(node).map(PrimNode::layer_stack);
    if let Some(end) = stacks.iter().position(|stack| Some(*stack) == own) {
        stacks.truncate(end + 1);
    }
    stacks.dedup();
    stacks
}

/// Whether the nodes `a` and `b` of `graph` read their layer stacks with
/// the same expression variables ([`node_chain`], [`composed_variables`]):
/// two nodes of one root layer are sites of one layer stack only then.
///
/// OpenUSD: `PcpLayerStackIdentifier::expressionVariablesOverrideSource`.
pub(crate) fn same_context(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    a: NodeId,
    b: NodeId,
) -> bool {
    a == b || node_variables(store, graph, a) == node_variables(store, graph, b)
}

/// The expression variables `node` of `graph` reads its layer stack with:
/// those composed along its chain ([`node_chain`]).
pub(crate) fn node_variables(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    node: NodeId,
) -> ExpressionVariables {
    if store.tokens().lookup(EXPRESSION_VARIABLES).is_none() {
        return ExpressionVariables::new();
    }
    composed_variables(store, &node_chain(graph, node))
}

/// The variant selections `selections`, authored at a site read in
/// `context`, with those authored as variable expressions evaluated there:
/// one that fails to evaluate is left out, so a weaker selection applies;
/// an evaluated name no layer uses selects the expression's own token,
/// which names no variant either. Borrowed unless a selection is an
/// expression.
///
/// OpenUSD: `PcpComposeSiteVariantSelection` and
/// `PcpComposeSiteVariantSelections` in `pxr/usd/pcp/composeSite.cpp`.
pub(crate) fn site_selections<'m>(
    store: &dyn LayerStore,
    selections: &'m HashMap<TokenId, TokenId>,
    context: SiteContext<'_>,
) -> Cow<'m, HashMap<TokenId, TokenId>> {
    let tokens = store.tokens();
    if !tokens.has_expressions()
        || !selections
            .values()
            .any(|variant| is_expression(tokens.resolve(*variant)))
    {
        return Cow::Borrowed(selections);
    }
    let variables = composed_variables(store, &context.chain());
    let mut evaluated = selections.clone();
    for (set, variant) in selections {
        let expression = tokens.resolve(*variant);
        if !is_expression(expression) {
            continue;
        }
        match VariableExpression::parse(expression)
            .evaluate(&variables)
            .into_string()
        {
            Ok(name) => {
                let name = name.unwrap_or_default();
                evaluated.insert(*set, tokens.lookup(&name).unwrap_or(*variant));
            }
            Err(_) => {
                evaluated.remove(set);
            }
        }
    }
    Cow::Owned(evaluated)
}

/// The variant selections authored at `source`: those of its prim spec, or
/// of the variant spec its spec path ends in (`/P{a=x}{b=y}` for a set
/// nested in a branch), as authored.
fn authored_source_selections<'s>(
    store: &'s dyn LayerStore,
    source: &OpinionKey,
) -> Option<&'s HashMap<TokenId, TokenId>> {
    let spec = store.layer(source.layer_id).and_then(|layer| {
        layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
    })?;
    let chain = source.spec_path.variant_chain();
    if chain.is_empty() {
        Some(&spec.variant_selections)
    } else {
        spec.variant_spec(&chain)
            .map(|branch| &branch.variant_selections)
    }
}

/// The variable expression errors and variable reads of the variant
/// selections composition reads for the composed prims `prims`.
///
/// For each variant set some node of a prim declares
/// ([`declared_variant_sets`]), the prim's sources are searched strongest
/// first until one selects the set: each selection authored as an
/// expression on the way is evaluated in its source's context
/// ([`SiteContext::Node`]), recording the variables it reads; one that
/// fails is an error and the search goes on. A selection in an unselected
/// branch, beneath a stronger one, or for a set no node declares is
/// neither an error nor a dependency.
///
/// OpenUSD: `_EvalNodeVariantSets` and `_ComposeVariantSelectionForNode`
/// in `pxr/usd/pcp/primIndex.cpp`, `PcpComposeSiteVariantSelection` in
/// `pxr/usd/pcp/composeSite.cpp`.
pub(crate) fn read_selections(
    store: &dyn LayerStore,
    prims: &HashMap<crate::path::PathId, PrimIndex>,
) -> (Vec<crate::CompositionError>, VariableReads) {
    let mut errors = Vec::new();
    let mut reads = VariableReads::default();
    if !store.tokens().has_expressions() {
        return (errors, reads);
    }
    let tokens = store.tokens();
    for index in prims.values() {
        let declared = declared_variant_sets(store, index);
        let mut decided: HashSet<TokenId> = HashSet::new();
        for source in &index.sources {
            let Some(selections) = authored_source_selections(store, source) else {
                continue;
            };
            for (set, variant) in selections {
                if !declared.contains(set) || decided.contains(set) {
                    continue;
                }
                let expression = tokens.resolve(*variant);
                if !is_expression(expression) {
                    decided.insert(*set);
                    continue;
                }
                let chain = node_chain(&index.graph, source.node);
                let variables = composed_variables(store, &chain);
                let evaluation = VariableExpression::parse(expression).evaluate(&variables);
                reads.record(store, &chain, &evaluation.used_variables);
                match evaluation.into_string() {
                    Ok(_) => {
                        decided.insert(*set);
                    }
                    Err(error) => errors.push(expression_error(
                        ExpressionContext::VariantSelection,
                        source.layer_id,
                        None,
                        expression,
                        error,
                    )),
                }
            }
        }
    }
    (errors, reads)
}

/// The variant sets some node of `index` declares: each node's `variantSets`
/// list op composed over its sources, weakest first, so a stronger spec's
/// `delete` removes a weaker one's declaration in that node. A variant
/// node's sources declare the sets nested in its branch
/// ([`crate::doc::PrimSpec::variant_sets_in`]).
///
/// OpenUSD: `PcpComposeSiteVariantSets` in `pxr/usd/pcp/composeSite.cpp`,
/// called for each node by `_EvalNodeVariantSets` in
/// `pxr/usd/pcp/primIndex.cpp`.
pub(crate) fn declared_variant_sets(store: &dyn LayerStore, index: &PrimIndex) -> HashSet<TokenId> {
    let mut per_node: HashMap<NodeId, Vec<TokenId>> = HashMap::new();
    for source in index.sources.iter().rev() {
        let Some(spec) = store.layer(source.layer_id).and_then(|layer| {
            layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
        }) else {
            continue;
        };
        let chain = source.spec_path.variant_chain();
        let Some((_, order)) = spec.variant_sets_in(&chain) else {
            continue;
        };
        let names = per_node.entry(source.node).or_default();
        if chain.is_empty() {
            names.retain(|name| !spec.deleted_variant_sets.contains(name));
        }
        for name in order {
            if !names.contains(name) {
                names.push(*name);
            }
        }
    }
    per_node.into_values().flatten().collect()
}

/// Every reference and payload `layer` authors, in any spec and variant
/// branch, as authored, deletions included: a deleted arc is compared by
/// the asset it resolves to, so its asset must be known.
fn layer_arcs(layer: &Layer) -> impl Iterator<Item = &Reference> {
    fn items(list: &ListOp<Reference>) -> impl Iterator<Item = &Reference> {
        list.explicit
            .iter()
            .flatten()
            .chain(&list.prepend)
            .chain(&list.append)
            .chain(&list.delete)
    }
    layer
        .prims
        .values()
        .chain(layer.variant_prims.values().flatten())
        .flat_map(|spec| {
            let branches = spec.variant_branches().flat_map(|branch| {
                items(&branch.spec.references).chain(items(&branch.spec.payloads))
            });
            items(&spec.references)
                .chain(items(&spec.payloads))
                .chain(branches)
        })
}
