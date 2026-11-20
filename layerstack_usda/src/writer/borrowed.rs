// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text views of unadorned declarations. The exact type matches below need
//! neither coercion nor a newer text version. Anything else, including an
//! invalid name or a mixed list op, goes through normal lowering so that it
//! keeps its diagnostics and their precedence. AOUSD Core §§7.6, 16.2.16.

use super::{Layout, Specifier, Writer, dictionary_cmp, is_identifier, is_property_name};
use crate::save::PropertyView;
use alloc::vec::Vec;
use hashbrown::HashSet;
use layerstack::{
    PathId, PathInterner, PrimSpec, PropertyKind, TargetPath, TokenId, TokenInterner, TypedArray,
    Value, Variability,
};

#[derive(Clone, Copy)]
pub(crate) enum Order {
    Authored,
    Text,
}

#[derive(Default)]
pub(crate) struct CheckedNames {
    prim_names: HashSet<TokenId>,
    property_names: HashSet<TokenId>,
    target_prims: HashSet<PathId>,
}

fn checked(set: &mut HashSet<TokenId>, id: TokenId, text: &str, valid: fn(&str) -> bool) -> bool {
    set.contains(&id) || valid(text) && set.insert(id)
}

impl CheckedNames {
    pub(crate) fn borrow_prim<'a>(
        &mut self,
        spec: &'a PrimSpec,
        name: TokenId,
        tokens: &'a TokenInterner,
        paths: &'a PathInterner,
        declarations: &mut Vec<Declaration<'a>>,
        (order, entries): (Order, impl Iterator<Item = PropertyView<'a>>),
    ) -> Option<BorrowedPrim<'a>> {
        if spec.rare().is_some()
            || !spec.fields.is_empty()
            || spec.active.is_some()
            || spec.instanceable.is_some()
            || spec.references.is_authored()
            || spec.payloads.is_authored()
        {
            return None;
        }
        if !checked(
            &mut self.prim_names,
            name,
            tokens.resolve(name),
            is_identifier,
        ) || spec
            .type_name
            .is_some_and(|id| !checked(&mut self.prim_names, id, tokens.resolve(id), is_identifier))
        {
            return None;
        }
        let start = declarations.len();
        for entry in entries {
            let name = tokens.resolve(entry.name);
            let property = checked(&mut self.property_names, entry.name, name, is_property_name)
                .then(|| self.property(entry, name, tokens, paths))
                .flatten();
            let Some(property) = property else {
                declarations.truncate(start);
                return None;
            };
            declarations.push(property);
        }
        let properties = &mut declarations[start..];
        let duplicate = if properties.is_sorted_by(|a, b| dictionary_cmp(a.name, b.name).is_le()) {
            properties
                .windows(2)
                .any(|pair| pair[0].name == pair[1].name)
        } else {
            match order {
                Order::Text => {
                    layerstack::sort::sort_unstable_by(properties, |a, b| {
                        dictionary_cmp(a.name, b.name)
                    });
                    properties
                        .windows(2)
                        .any(|pair| pair[0].name == pair[1].name)
                }
                Order::Authored => {
                    let mut names: smallvec::SmallVec<[&str; 8]> =
                        properties.iter().map(|p| p.name).collect();
                    layerstack::sort::sort_unstable_by(&mut names, |a, b| a.cmp(b));
                    names.windows(2).any(|pair| pair[0] == pair[1])
                }
            }
        };
        if duplicate {
            declarations.truncate(start);
            return None;
        }
        Some(BorrowedPrim {
            name: tokens.resolve(name),
            type_name: spec.type_name.map(|t| tokens.resolve(t)),
            specifier: spec.specifier,
            properties: start..declarations.len(),
            tokens,
            paths,
        })
    }

    fn property<'a>(
        &mut self,
        spec: PropertyView<'a>,
        name: &'a str,
        tokens: &'a TokenInterner,
        paths: &'a PathInterner,
    ) -> Option<Declaration<'a>> {
        if spec.extra.is_some_and(|s| {
            !s.metadata.is_empty() || s.time_samples.is_authored() || s.spline.is_some()
        }) {
            return None;
        }
        let (ty, payload) = match spec.kind {
            PropertyKind::Attribute => {
                if spec.targets.is_some() {
                    return None;
                }
                let ty = spec.type_name?.type_name();
                (ty, Payload::default_value(ty, spec.default?, tokens)?)
            }
            PropertyKind::Relationship => {
                if spec.type_name.is_some() || spec.default.is_some() {
                    return None;
                }
                let targets = match spec.targets {
                    None => None,
                    Some(op) => Some(op.explicit()?),
                };
                for t in targets.unwrap_or_default() {
                    let id = t.prim_path();
                    if !self.target_prims.contains(&id) {
                        let parts = paths.resolve(id).segments();
                        if parts.is_empty()
                            || !parts.iter().all(|&p| {
                                checked(&mut self.prim_names, p, tokens.resolve(p), is_identifier)
                            })
                        {
                            return None;
                        }
                        self.target_prims.insert(id);
                    }
                    if let Some(p) = t.property_path()
                        && !checked(
                            &mut self.property_names,
                            p.property(),
                            tokens.resolve(p.property()),
                            is_property_name,
                        )
                    {
                        return None;
                    }
                }
                ("rel", Payload::Targets(targets))
            }
        };
        Some(Declaration {
            name,
            ty,
            payload,
            custom: spec.custom,
            variability: spec.variability,
        })
    }
}

pub(crate) struct BorrowedPrim<'a> {
    name: &'a str,
    type_name: Option<&'a str>,
    specifier: Option<Specifier>,
    properties: core::ops::Range<usize>,
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
}

/// A checked property declaration borrowed from an authored prim.
#[derive(Debug)]
pub struct Declaration<'a> {
    /// Property name, including any namespace.
    pub name: &'a str,
    /// Declared attribute type or `rel`.
    pub ty: &'a str,
    /// Authored default or relationship targets.
    pub payload: Payload<'a>,
    /// Whether the declaration is custom.
    pub custom: bool,
    /// Authored variability.
    pub variability: Variability,
}

/// Native values that can be written directly at their declared type.
#[derive(Debug)]
pub enum Payload<'a> {
    /// String default.
    String(&'a str),
    /// Resolved token default.
    Token(&'a str),
    /// Boolean default.
    Bool(bool),
    /// Signed 32-bit default.
    Int(i32),
    /// Signed 64-bit default.
    Int64(i64),
    /// Single-precision default.
    Float(f32),
    /// Double-precision default.
    Double(f64),
    /// Signed 64-bit array.
    Ints(&'a [i64]),
    /// Single-precision array.
    Floats(&'a [f32]),
    /// Double-precision array.
    Doubles(&'a [f64]),
    /// Interned token array.
    Tokens(&'a [TokenId]),
    /// String array.
    Strings(&'a layerstack::StringArray),
    /// The relationship's explicit targets, or no authored target field.
    Targets(Option<&'a [TargetPath]>),
}

impl<'a> Payload<'a> {
    fn default_value(ty: &str, v: &'a Value, tokens: &'a TokenInterner) -> Option<Self> {
        Some(match (ty, v) {
            ("string", Value::String(s)) if !s.contains('\0') => Self::String(s),
            ("token", Value::Token(id)) => Self::Token(tokens.resolve(*id)),
            ("bool", Value::Bool(v)) => Self::Bool(*v),
            ("int", Value::Int(v)) => Self::Int(*v),
            ("int64", Value::Int64(v)) => Self::Int64(*v),
            ("float", Value::Float(v)) => Self::Float(*v),
            ("double", Value::Double(v)) => Self::Double(*v),
            ("int64[]", Value::TypedArray(TypedArray::Int64(v))) => Self::Ints(v),
            ("float[]", Value::TypedArray(TypedArray::Float(v))) => Self::Floats(v),
            ("double[]", Value::TypedArray(TypedArray::Double(v))) => Self::Doubles(v),
            ("token[]", Value::TypedArray(TypedArray::Token(v))) => Self::Tokens(v),
            ("string[]", Value::TypedArray(TypedArray::String(v)))
                if v.iter().all(|s| !s.contains('\0')) =>
            {
                Self::Strings(v)
            }
            _ => return None,
        })
    }
}

impl BorrowedPrim<'_> {
    pub(super) fn write(
        &self,
        w: &mut Writer<'_>,
        layout: Layout,
        declarations: &[Declaration<'_>],
    ) {
        w.open_header(self.name, self.type_name, self.specifier, None, layout);
        let depth = layout.depth + 1;
        for p in &declarations[self.properties.clone()] {
            w.indent(depth);
            if p.custom {
                w.out.push_str("custom ");
            }
            match (&p.payload, p.variability) {
                (Payload::Targets(_), Variability::Varying) => w.out.push_str("varying "),
                (Payload::Targets(_), _) => {}
                (_, Variability::Uniform) => w.out.push_str("uniform "),
                _ => {}
            }
            w.text(p.ty);
            w.out.push(' ');
            w.text(p.name);
            if !matches!(p.payload, Payload::Targets(None)) {
                w.out.push_str(" = ");
            }
            match &p.payload {
                Payload::String(s) | Payload::Token(s) => w.string(s),
                Payload::Bool(v) => w.out.push(if *v { '1' } else { '0' }),
                Payload::Int(v) => w.display(v),
                Payload::Int64(v) => w.display(v),
                Payload::Float(v) => w.f32(*v),
                Payload::Double(v) => w.f64(*v),
                Payload::Ints(v) => w.array(v, |w, v| w.display(v)),
                Payload::Floats(v) => w.array(v, |w, v| w.f32(*v)),
                Payload::Doubles(v) => w.array(v, |w, v| w.f64(*v)),
                Payload::Tokens(v) => w.array(v, |w, t| w.string(self.tokens.resolve(*t))),
                Payload::Strings(v) => w.array_items(v.iter(), |w, s| w.string(s)),
                Payload::Targets(None) => {}
                Payload::Targets(Some(items)) => w.target_list(items, depth, |w, t| {
                    w.out.push('<');
                    for &name in self.paths.resolve(t.prim_path()).segments() {
                        w.out.push('/');
                        w.text(self.tokens.resolve(name));
                    }
                    if let Some(p) = t.property_path() {
                        w.out.push('.');
                        w.text(self.tokens.resolve(p.property()));
                    }
                    w.out.push('>');
                }),
            }
            w.out.push('\n');
            w.spill();
        }
        if layout.children && !self.properties.is_empty() {
            w.out.push('\n');
        }
    }
}
