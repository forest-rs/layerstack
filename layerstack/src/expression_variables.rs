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
//! - a sublayer asset path, with the variables of the layer stack's root
//!   layer ([`crate::LayerStack::gather`]);
//! - a reference or payload asset path, per authoring layer, as its list op
//!   is read and before list ops compose, with the variables composed along
//!   the chain of arcs that reaches the authoring layer stack
//!   ([`ArcAnchor`], `anchor_internal_arcs`), so list editing compares the
//!   evaluated, anchored arcs;
//! - a variant selection, with the variables of the first layer stack
//!   holding its layer that the arcs reach ([`selection_view`]), counted
//!   only where composition reads it ([`read_selections`]).
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

use alloc::{collections::BTreeSet, rc::Rc, string::String, vec::Vec};
use core::cell::{OnceCell, RefCell};

use hashbrown::{HashMap, HashSet};

use crate::{
    asset::ExpressionAssetPath,
    composition_error::{ExpressionContext, VariableExpressionError},
    doc::{FieldValue, Layer, LayerId, LayerStore, Reference, Value},
    interner::{TokenId, TokenInterner},
    layer_stack::LayerStack,
    listop::ListOp,
    spec_path::{SpecPath, VariantSelectionSite},
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
        let stack = LayerStack::gather(store, stack_root);
        let own = composed_variables(store, &[stack_root]);
        for layer in stack.layers.iter().filter_map(|id| store.layer(*id)) {
            for sublayer in &layer.sublayers {
                let Some(asset) = sublayer.asset.as_deref().filter(|a| is_expression(a)) else {
                    continue;
                };
                let evaluated = VariableExpression::parse(asset)
                    .evaluate(&own)
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
        stacks.push(WalkedStack { stack, chain });
    }
    Walk {
        stacks,
        unresolved: unresolved.into_iter().collect(),
    }
}

/// A layer stack [`walk`] reached.
pub(crate) struct WalkedStack {
    /// The layer stack.
    pub(crate) stack: LayerStack,
    /// The root layers of the layer stacks on the way to it, outermost
    /// first, ending with its own.
    pub(crate) chain: Vec<LayerId>,
}

/// One variant selection authored as a variable expression that
/// [`SelectionView`] evaluated, with the site authoring it.
pub(crate) struct EvaluatedSelection {
    /// The layer authoring the selection.
    layer: LayerId,
    /// The prim spec or variant branch authoring it, as a prim index names
    /// its sources.
    site: SpecPath,
    /// The variant set it selects for.
    set: TokenId,
    /// The variables the evaluation read.
    reads: VariableReads,
    /// The [`VariableExpressionError`], when it failed.
    error: Option<crate::CompositionError>,
}

/// What reading the evaluated selections composition used found.
pub(crate) type SelectionFindings = Rc<RefCell<Vec<EvaluatedSelection>>>;

/// A view of a store whose layers read their variant selections authored
/// as variable expressions evaluated, or `None` when no token of the store
/// is an expression, so no selection can be one.
///
/// A layer is evaluated when composition first reads it, with the
/// variables of the first layer stack holding it that [`walk`] reaches
/// (breadth first, through the arcs in authored order). A selection that
/// fails to evaluate is removed, so a weaker selection applies
/// (`PcpComposeSiteVariantSelection` in `pxr/usd/pcp/composeSite.cpp`). An
/// evaluated name no layer uses selects the expression's own token, which
/// names no variant either.
///
/// What each evaluation found is recorded, but only the selections
/// composition reads count ([`read_selections`]): OpenUSD evaluates a
/// selection only where it composes it.
pub(crate) fn selection_view(
    store: &mut dyn LayerStore,
    root: LayerId,
) -> Option<(SelectionView<'_>, SelectionFindings)> {
    if !store.tokens().any(is_expression) {
        return None;
    }
    let mut lazy = HashMap::new();
    for walked in walk(store, root).stacks {
        for id in &walked.stack.layers {
            lazy.entry(*id)
                .or_insert_with(|| (walked.chain.clone(), OnceCell::new()));
        }
    }
    let findings = SelectionFindings::default();
    let view = SelectionView {
        store,
        lazy,
        findings: findings.clone(),
    };
    Some((view, findings))
}

/// See [`selection_view`].
pub(crate) struct SelectionView<'s> {
    store: &'s mut dyn LayerStore,
    /// For each layer the arcs reach, the chain of layer stacks whose
    /// variables evaluate its selections, and its evaluated copy, once
    /// read: `None` when it authors no selection expression.
    lazy: HashMap<LayerId, (Vec<LayerId>, OnceCell<Option<Layer>>)>,
    findings: SelectionFindings,
}

impl SelectionView<'_> {
    /// A copy of the layer `id` with its selection expressions evaluated
    /// in the chain `chain`; `None` when it authors none.
    fn evaluate(&self, id: LayerId, chain: &[LayerId]) -> Option<Layer> {
        let store = &*self.store;
        let tokens = store.tokens();
        let source = store
            .layer(id)
            .filter(|layer| has_selection_expressions(layer, tokens))?;
        let variables = composed_variables(store, chain);
        let mut layer = source.clone();
        let mut findings = self.findings.borrow_mut();
        for (site, selections) in selection_sites(store, &mut layer) {
            let expressions: Vec<(TokenId, TokenId)> = selections
                .iter()
                .filter(|(_, variant)| is_expression(tokens.resolve(**variant)))
                .map(|(set, variant)| (*set, *variant))
                .collect();
            for (set, token) in expressions {
                let expression = tokens.resolve(token);
                let evaluation = VariableExpression::parse(expression).evaluate(&variables);
                let mut reads = VariableReads::default();
                reads.record(store, chain, &evaluation.used_variables);
                let error = match evaluation.into_string() {
                    Ok(variant) => {
                        let name = variant.as_deref().unwrap_or_default();
                        selections.insert(set, tokens.lookup(name).unwrap_or(token));
                        None
                    }
                    Err(error) => {
                        selections.remove(&set);
                        Some(expression_error(
                            ExpressionContext::VariantSelection,
                            id,
                            None,
                            expression,
                            error,
                        ))
                    }
                };
                findings.push(EvaluatedSelection {
                    layer: id,
                    site: site.clone(),
                    set,
                    reads,
                    error,
                });
            }
        }
        Some(layer)
    }
}

impl LayerStore for SelectionView<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        if let Some((chain, cell)) = self.lazy.get(&id)
            && let Some(layer) = cell.get_or_init(|| self.evaluate(id, chain))
        {
            return Some(layer);
        }
        self.store.layer(id)
    }

    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        let evaluated = self
            .lazy
            .get_mut(&id)
            .and_then(|(_, cell)| cell.get_mut())
            .is_some_and(|layer| layer.is_some());
        if evaluated {
            self.lazy
                .get_mut(&id)
                .and_then(|(_, cell)| cell.get_mut())
                .and_then(Option::as_mut)
        } else {
            self.store.layer_mut(id)
        }
    }

    fn tokens(&self) -> &TokenInterner {
        self.store.tokens()
    }

    fn tokens_mut(&mut self) -> &mut TokenInterner {
        self.store.tokens_mut()
    }

    fn paths(&self) -> &crate::path::PathInterner {
        self.store.paths()
    }

    fn paths_mut(&mut self) -> &mut crate::path::PathInterner {
        self.store.paths_mut()
    }

    fn asset_layer(&self, anchor: LayerId, asset_path: &str) -> Option<LayerId> {
        self.store.asset_layer(anchor, asset_path)
    }
}

/// The selection maps of every prim spec and variant branch of `layer`,
/// each with the spec path that names it as a source.
fn selection_sites<'l>(
    store: &dyn LayerStore,
    layer: &'l mut Layer,
) -> Vec<(SpecPath, &'l mut HashMap<TokenId, TokenId>)> {
    let specs = layer
        .prims
        .iter_mut()
        .map(|(path, spec)| (*path, spec))
        .chain(
            layer
                .variant_prims
                .iter_mut()
                .flat_map(|(path, specs)| specs.iter_mut().map(|spec| (*path, spec))),
        );
    let mut maps = Vec::new();
    for (path, spec) in specs {
        let site = if spec.outer_variant_sites.is_empty() {
            SpecPath::from_prim_path(path, store.paths())
        } else {
            SpecPath::from_variant_selection_sites(path, &spec.outer_variant_sites, store.paths())
        };
        maps.push((site, &mut spec.variant_selections));
        for (set, set_spec) in &mut spec.variant_sets {
            for (variant, branch) in &mut set_spec.variants {
                let mut sites = branch.outer_variant_sites.clone();
                sites.push(VariantSelectionSite {
                    host_path: path,
                    set: *set,
                    variant: *variant,
                });
                let site = SpecPath::from_variant_selection_sites(path, &sites, store.paths());
                maps.push((site, &mut branch.variant_selections));
            }
        }
    }
    maps
}

/// Whether a variant selection of `layer` is a variable expression.
fn has_selection_expressions(layer: &Layer, tokens: &TokenInterner) -> bool {
    let is_expression_token = |variant: &TokenId| is_expression(tokens.resolve(*variant));
    layer
        .prims
        .values()
        .chain(layer.variant_prims.values().flatten())
        .any(|spec| {
            spec.variant_selections.values().any(is_expression_token)
                || spec
                    .variant_sets
                    .values()
                    .flat_map(|set| set.variants.values())
                    .any(|variant| variant.variant_selections.values().any(is_expression_token))
        })
}

/// The errors and variable reads of the evaluated selections `findings`
/// that composition reads: those of a site among a composed prim's
/// sources, `prims`, that no stronger source of the prim selects the same
/// variant set at.
///
/// OpenUSD evaluates a selection only when it composes it
/// (`PcpComposeSiteVariantSelection` in `pxr/usd/pcp/composeSite.cpp`),
/// walking the prim index's nodes strongest first until one selects the
/// set, and records the variables it used there
/// (`_ComposeVariantSelectionForNode` in `pxr/usd/pcp/primIndex.cpp`). A
/// selection in an unselected branch, or beneath a stronger one, is neither
/// an error nor a dependency.
pub(crate) fn read_selections(
    store: &dyn LayerStore,
    prims: &HashMap<crate::path::PathId, crate::prim_index::PrimIndex>,
    findings: &SelectionFindings,
) -> (Vec<crate::CompositionError>, VariableReads) {
    let findings = core::mem::take(&mut *findings.borrow_mut());
    let mut read = alloc::vec![false; findings.len()];
    if !findings.is_empty() {
        for index in prims.values() {
            let mut decided: HashSet<TokenId> = HashSet::new();
            for source in &index.sources {
                for (i, finding) in findings.iter().enumerate() {
                    if finding.layer == source.layer_id
                        && finding.site == source.spec_path
                        && !decided.contains(&finding.set)
                    {
                        read[i] = true;
                    }
                }
                decided.extend(source_selections(store, source));
            }
        }
    }
    let mut errors = Vec::new();
    let mut reads = VariableReads::default();
    for (finding, read) in findings.into_iter().zip(read) {
        if read {
            reads.extend(finding.reads);
            errors.extend(finding.error);
        }
    }
    (errors, reads)
}

/// The variant sets `source` selects: the selections of its prim spec, or
/// of the variant branch its spec path ends in.
fn source_selections(
    store: &dyn LayerStore,
    source: &crate::prim_index::OpinionKey,
) -> Vec<TokenId> {
    use crate::spec_path::SpecComponent;
    let Some(spec) = store.layer(source.layer_id).and_then(|layer| {
        layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
    }) else {
        return Vec::new();
    };
    let selections = match source.spec_path.components().last() {
        Some(SpecComponent::VariantSelection { set, variant }) => spec
            .variant_sets
            .get(set)
            .and_then(|set_spec| set_spec.variants.get(variant))
            .map(|branch| &branch.variant_selections),
        _ => Some(&spec.variant_selections),
    };
    selections
        .map(|selections| selections.keys().copied().collect())
        .unwrap_or_default()
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
            let branches = spec
                .variant_sets
                .values()
                .flat_map(|set| set.variants.values())
                .flat_map(|variant| items(&variant.references).chain(items(&variant.payloads)));
            items(&spec.references)
                .chain(items(&spec.payloads))
                .chain(branches)
        })
}
