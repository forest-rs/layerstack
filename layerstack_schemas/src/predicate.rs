// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD's collection predicate library: the predicates a collection's
//! `membershipExpression` may test (`//{isa:Mesh}`,
//! `/World//{kind:component}`).
//!
//! | Predicate | Holds for |
//! |-----------|-----------|
//! | `abstract` (`isAbstract=true`) | objects on a `class` prim or below one |
//! | `defined` (`isDefined=true`) | objects on a prim that it and its ancestors define (`def` or `class`) |
//! | `model` (`isModel=true`) | model prims ([`Scene::is_model`]) |
//! | `group` (`isGroup=true`) | group prims ([`Scene::is_group`]) |
//! | `kind:k1,k2`, `kind(k, strict=true)` | prims whose `kind` is one of the kinds, or a kind of one unless strict |
//! | `specifier:def,over,class` | prims with one of the composed specifiers |
//! | `isa:T1,T2`, `isa(T, strict=true)` | prims of one of the typed schemas, or derived from one unless strict |
//! | `hasAPI:A1,A2`, `hasAPI(A, instanceName=i)` | prims with one of the applied schemas (with that instance) |
//! | `variant(set=selection, ...)` | prims selecting each variant (of a set declared on the prim or nested in a variant it selects); a selection that is not an identifier is a glob searched as a POSIX regular expression, and one using syntax POSIX leaves undefined, or a POSIX feature not supported here (equivalence classes, collating symbols), does not bind |
//!
//! Arguments bind as OpenUSD binds them, quirks included: `strict` is
//! true for `true`, or a string starting with `1`, `y` or `Y` (not for the
//! integer `1`); a colon call's arguments are all positional, so
//! `kind:component,strict=true` passes the kind `strict=true`, which is
//! not a kind and is ignored; a predicate that does not bind makes the
//! whole expression match nothing.
//!
//! OpenUSD: `UsdGetCollectionPredicateLibrary`
//! (`pxr/usd/usd/collectionPredicateLibrary.cpp`), as of 26.08.

use alloc::{format, string::String, sync::Arc, vec::Vec};

use layerstack::path_expression::{
    ArgValue, MatchResult, PredicateBindError, PredicateCall, Predicates,
};
use layerstack::{PathId, SchemaKind, Specifier, TargetPath};

use crate::regex::SelectionGlob;
use crate::view::Scene;

/// OpenUSD's collection predicates, evaluated on a scene.
///
/// Link an expression with it ([`PathExpression::matcher`]) and match
/// paths of the same scene;
/// `ExpressionEvaluator` does both.
///
/// [`PathExpression::matcher`]: layerstack::path_expression::PathExpression::matcher
#[derive(Clone, Copy, Debug)]
pub struct CollectionPredicates<'a> {
    scene: Scene<'a>,
}

/// A bound collection predicate call.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionPredicate(Bound);

#[derive(Clone, Debug, PartialEq)]
enum Bound {
    Abstract(bool),
    Defined(bool),
    Model(bool),
    Group(bool),
    Kind {
        kinds: Vec<Arc<str>>,
        sub_kinds: bool,
    },
    Specifier {
        def: bool,
        over: bool,
        class: bool,
    },
    IsA {
        types: Vec<layerstack::TokenId>,
        exact: bool,
    },
    HasApi {
        schemas: Vec<layerstack::TokenId>,
        instance: Option<Arc<str>>,
    },
    Variant {
        exact: Vec<(Arc<str>, Arc<str>)>,
        globs: Vec<(Arc<str>, SelectionGlob)>,
    },
}

impl<'a> CollectionPredicates<'a> {
    /// The predicates, reading `scene`.
    #[must_use]
    pub fn new(scene: Scene<'a>) -> Self {
        Self { scene }
    }

    /// The names of the predicates it defines.
    pub const NAMES: [&'static str; 9] = [
        "abstract",
        "defined",
        "model",
        "group",
        "kind",
        "specifier",
        "isa",
        "hasAPI",
        "variant",
    ];

    /// A schema name the scene's registry knows.
    fn schema(&self, name: &str) -> Option<layerstack::TokenId> {
        let token = self.scene.store().tokens().lookup(name)?;
        self.scene.stage().schemas()?.schema(token).map(|_| token)
    }
}

/// The value of a flag predicate's one parameter, `param`, defaulting to
/// true, as `SdfPredicateLibrary` binds a `bool` parameter with a default.
fn flag(call: &PredicateCall, param: &str) -> Result<bool, PredicateBindError> {
    let error = |reason: &str| PredicateBindError::new(&call.name, reason);
    if call.args.len() > 1 {
        return Err(error("takes at most 1 argument"));
    }
    let Some(arg) = call.args.first() else {
        return Ok(true);
    };
    match &arg.name {
        None => arg
            .value
            .as_bool()
            .ok_or_else(|| error("expects a boolean")),
        Some(name) if name == param => arg
            .value
            .as_bool()
            .ok_or_else(|| error("expects a boolean")),
        Some(_) => Ok(true),
    }
}

/// `_IsStrict`: the first `strict` keyword argument.
fn strict(call: &PredicateCall) -> bool {
    match call.keyword("strict") {
        Some(ArgValue::Bool(value)) => *value,
        Some(ArgValue::String(text)) => text.starts_with(['1', 'y', 'Y']),
        _ => false,
    }
}

/// The positional string arguments.
fn names(call: &PredicateCall) -> impl Iterator<Item = &str> {
    call.positional().filter_map(ArgValue::as_str)
}

/// Whether `text` is an ASCII identifier (`TfIsValidIdentifier`).
fn is_identifier(text: &str) -> bool {
    text.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Predicates for CollectionPredicates<'_> {
    type Call = CollectionPredicate;

    fn bind(&self, call: &PredicateCall) -> Result<CollectionPredicate, PredicateBindError> {
        let error = |reason: &str| PredicateBindError::new(&call.name, reason);
        let bound = match call.name.as_str() {
            "abstract" => Bound::Abstract(flag(call, "isAbstract")?),
            "defined" => Bound::Defined(flag(call, "isDefined")?),
            "model" => Bound::Model(flag(call, "isModel")?),
            "group" => Bound::Group(flag(call, "isGroup")?),
            "kind" => {
                let kinds: Vec<Arc<str>> = names(call)
                    .filter(|kind| self.scene.kinds().has_kind(kind))
                    .map(Arc::from)
                    .collect();
                if kinds.is_empty() {
                    return Err(error("names no known kind"));
                }
                Bound::Kind {
                    kinds,
                    sub_kinds: !strict(call),
                }
            }
            "specifier" => {
                let (mut def, mut over, mut class) = (false, false, false);
                for arg in &call.args {
                    match (&arg.name, arg.value.as_str()) {
                        (None, Some("def")) => def = true,
                        (None, Some("over")) => over = true,
                        (None, Some("class")) => class = true,
                        _ => return Err(error("expects `def`, `over` or `class`")),
                    }
                }
                Bound::Specifier { def, over, class }
            }
            "isa" => Bound::IsA {
                types: names(call).filter_map(|name| self.schema(name)).collect(),
                exact: strict(call),
            },
            "hasAPI" => {
                let instance = match call.keyword("instanceName") {
                    None => None,
                    Some(ArgValue::String(name)) => Some(Arc::from(name.as_str())),
                    Some(_) => return Err(error("expects a string `instanceName`")),
                };
                Bound::HasApi {
                    schemas: names(call).filter_map(|name| self.schema(name)).collect(),
                    instance,
                }
            }
            "variant" => {
                let (mut exact, mut globs) = (Vec::new(), Vec::new());
                for arg in &call.args {
                    let (Some(set), Some(selection)) = (&arg.name, arg.value.as_str()) else {
                        return Err(error("expects `set=selection` string arguments"));
                    };
                    if is_identifier(selection) {
                        exact.push((Arc::from(set.as_str()), Arc::from(selection)));
                    } else {
                        let glob = SelectionGlob::compile(selection).map_err(|reason| {
                            error(&format!("selection glob `{selection}` has {reason}"))
                        })?;
                        globs.push((Arc::from(set.as_str()), glob));
                    }
                }
                Bound::Variant { exact, globs }
            }
            _ => return Err(error("no such predicate")),
        };
        Ok(CollectionPredicate(bound))
    }

    fn evaluate(&self, call: &CollectionPredicate, object: TargetPath) -> MatchResult {
        let scene = &self.scene;
        let (prim, is_prim) = match object {
            TargetPath::Prim(prim) => (prim, true),
            TargetPath::Property(property) => (property.prim_path(), false),
        };
        match &call.0 {
            Bound::Abstract(wanted) => {
                let abstract_ = is_abstract(scene, prim);
                if abstract_ || !is_prim {
                    MatchResult::constant(abstract_ == *wanted)
                } else {
                    MatchResult::varying(abstract_ == *wanted)
                }
            }
            Bound::Defined(wanted) => {
                let defined = is_defined(scene, prim);
                if !defined || !is_prim {
                    MatchResult::constant(defined == *wanted)
                } else {
                    MatchResult::varying(defined == *wanted)
                }
            }
            _ if !is_prim => MatchResult::constant(false),
            Bound::Model(wanted) => {
                let model = scene.is_model(prim);
                if model {
                    MatchResult::varying(model == *wanted)
                } else {
                    MatchResult::constant(model == *wanted)
                }
            }
            Bound::Group(wanted) => {
                let group = scene.is_group(prim);
                if group {
                    MatchResult::varying(group == *wanted)
                } else {
                    MatchResult::constant(group == *wanted)
                }
            }
            Bound::Kind { kinds, sub_kinds } => {
                let Some(kind) = scene.kind(prim) else {
                    return MatchResult::varying(false);
                };
                let registry = scene.kinds();
                MatchResult::varying(kinds.iter().any(|wanted| {
                    if *sub_kinds {
                        registry.is_a(kind, wanted)
                    } else {
                        kind == &**wanted
                    }
                }))
            }
            Bound::Specifier { def, over, class } => {
                let specifier = scene.stage().resolve_specifier(prim, scene.store());
                MatchResult::varying(match specifier {
                    Some(Specifier::Def) => *def,
                    Some(Specifier::Over) => *over,
                    Some(Specifier::Class) => *class,
                    None => false,
                })
            }
            Bound::IsA { types, exact } => {
                let schema_type = concrete_type(scene, prim);
                MatchResult::varying(schema_type.is_some_and(|type_name| {
                    types.iter().any(|wanted| {
                        if *exact {
                            type_name == *wanted
                        } else {
                            scene
                                .stage()
                                .schemas()
                                .is_some_and(|registry| registry.is_a(type_name, *wanted))
                        }
                    })
                }))
            }
            Bound::HasApi { schemas, instance } => {
                let tokens = scene.store().tokens();
                MatchResult::varying(schemas.iter().any(|schema| {
                    scene.has_api(prim, tokens.resolve(*schema), instance.as_deref())
                }))
            }
            Bound::Variant { exact, globs } => {
                let selection = |set: &str| variant_selection(scene, prim, set);
                let holds = exact.iter().all(|(set, wanted)| selection(set) == **wanted)
                    && globs
                        .iter()
                        .all(|(set, glob)| glob.is_found_in(&selection(set)));
                MatchResult::varying(holds)
            }
        }
    }
}

/// The prim's type, when it names a concrete typed schema (a prim of an
/// abstract or unknown type has none, as `UsdPrimTypeInfo::GetSchemaType`
/// has none).
fn concrete_type(scene: &Scene<'_>, prim: PathId) -> Option<layerstack::TokenId> {
    let type_name = scene.stage().resolve_type_name(prim, scene.store())?;
    let registry = scene.stage().schemas()?;
    registry
        .schema(type_name)
        .is_some_and(|schema| schema.kind == SchemaKind::ConcreteTyped)
        .then_some(type_name)
}

/// The prim and its ancestors, nearest first, up to the root prim.
fn ancestry<'s>(scene: &'s Scene<'_>, prim: PathId) -> impl Iterator<Item = PathId> + 's {
    core::iter::successors(Some(prim), move |p| scene.parent(*p))
        .filter(move |p| scene.parent(*p).is_some())
}

/// Whether the prim or an ancestor is a `class`.
///
/// OpenUSD: `UsdPrim::IsAbstract`. Spec: AOUSD Core §11.5.
pub(crate) fn is_abstract(scene: &Scene<'_>, prim: PathId) -> bool {
    ancestry(scene, prim)
        .any(|p| scene.stage().resolve_specifier(p, scene.store()) == Some(Specifier::Class))
}

/// Whether the prim and every ancestor define it (`def` or `class`).
///
/// OpenUSD: `UsdPrim::IsDefined`. Spec: AOUSD Core §11.5.
pub(crate) fn is_defined(scene: &Scene<'_>, prim: PathId) -> bool {
    scene.stage().has_prim(prim)
        && ancestry(scene, prim).all(|p| {
            matches!(
                scene.stage().resolve_specifier(p, scene.store()),
                Some(Specifier::Def | Specifier::Class)
            )
        })
}

/// The variant the prim selects for `set`, one of its variant sets
/// (declared by a spec of it, or nested in a variant it selects): the
/// selection composition made, fallbacks included, whether or not that
/// variant exists; empty when there is none.
///
/// OpenUSD: `UsdVariantSet::GetVariantSelection`, which reads the variant
/// arcs of the prim index (Pcp adds one for every variant set with a
/// selection, even of a variant no layer defines).
fn variant_selection(scene: &Scene<'_>, prim: PathId, set: &str) -> String {
    let store = scene.store();
    let tokens = store.tokens();
    scene
        .stage()
        .variant_sets(prim, store)
        .into_iter()
        .find(|(name, _)| tokens.resolve(*name) == set)
        .and_then(|(_, selection)| selection)
        .map_or_else(String::new, |variant| tokens.resolve(variant).into())
}
