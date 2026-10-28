// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A walk through a document's specs that keeps its place on the heap.
//!
//! A namespace has no depth limit (OpenUSD writes and reads any depth), so
//! nothing that visits a [`Document`](super::Document)'s specs recurses through it: the
//! writer, its validation and the version it requests all go through
//! [`walk()`], whose stack is a `Vec`.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Prim, VariantSet};

/// The order [`walk()`] visits a variant set's variants in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VariantOrder {
    /// The document's order.
    Authored,
    /// By name in byte order, as OpenUSD writes them
    /// (`Sdf_WriteVariantSet`).
    ByName,
}

/// One step of a [`walk()`]: a spec entered or left.
///
/// A prim spec's or variant's contents are entered in this order: its
/// child prims, then its variant sets, each set's variants in the
/// [`VariantOrder`] asked for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Step<'a> {
    /// The prim spec `prim`, the `index`th of `siblings`: the root prims,
    /// or the children of the spec entered last and not yet left.
    Prim {
        prim: &'a Prim,
        siblings: &'a [Prim],
        index: usize,
    },
    /// The variant set `set`, the `index`th of `siblings`, the variant sets
    /// of the spec entered last and not yet left.
    Set {
        set: &'a VariantSet,
        siblings: &'a [VariantSet],
        index: usize,
    },
    /// The variant `variant` of the set entered last, the `index`th of
    /// its variants in the order they are visited.
    Variant {
        variant: &'a Prim,
        set: &'a VariantSet,
        index: usize,
    },
    /// Leaving the prim spec entered at the same depth.
    LeavePrim,
    /// Leaving the variant set entered at the same depth.
    LeaveSet,
    /// Leaving the variant entered at the same depth.
    LeaveVariant,
}

/// A spec [`walk()`] has entered and not yet left.
enum Frame<'a> {
    /// A prim spec or a variant, and the next of its children and sets.
    Spec {
        spec: &'a Prim,
        variant: bool,
        depth: usize,
        path_len: usize,
        child: usize,
        set: usize,
    },
    /// A variant set and the next of its variants.
    Set {
        set: &'a VariantSet,
        variants: Vec<&'a Prim>,
        depth: usize,
        owner_len: usize,
        next: usize,
    },
}

/// Visits every spec below the pseudo-root holding `prims`, depth first.
///
/// `visit` is given each [`Step`] with the path of the spec (`/A/B`, a
/// variant set as `/A{v=}`, a variant as `/A{v=x}`, a prim in it as
/// `/A{v=x}B`) and its depth: 0 for a root prim, and one more than its
/// owner's for a child prim, a variant set or a variant. A leave step has
/// the path and depth of its enter step. The first error `visit` returns
/// ends the walk.
pub(crate) fn walk<'a, E>(
    prims: &'a [Prim],
    order: VariantOrder,
    mut visit: impl FnMut(Step<'a>, &str, usize) -> Result<(), E>,
) -> Result<(), E> {
    let mut path = String::new();
    let mut stack: Vec<Frame<'a>> = Vec::new();
    let mut roots = 0;
    loop {
        let Some(top) = stack.last_mut() else {
            let Some(prim) = prims.get(roots) else {
                return Ok(());
            };
            enter_prim(&mut path, &mut stack, prim, prims, roots, 0, &mut visit)?;
            roots += 1;
            continue;
        };
        match top {
            Frame::Spec {
                spec,
                variant,
                depth,
                path_len,
                child,
                set,
            } => {
                let (spec, variant, depth, path_len) = (*spec, *variant, *depth, *path_len);
                if let Some(prim) = spec.children.get(*child) {
                    let index = *child;
                    *child += 1;
                    enter_prim(
                        &mut path,
                        &mut stack,
                        prim,
                        &spec.children,
                        index,
                        depth + 1,
                        &mut visit,
                    )?;
                } else if let Some(variant_set) = spec.variant_sets.get(*set) {
                    let index = *set;
                    *set += 1;
                    let owner_len = path.len();
                    push_set_path(&mut path, &variant_set.name);
                    visit(
                        Step::Set {
                            set: variant_set,
                            siblings: &spec.variant_sets,
                            index,
                        },
                        &path,
                        depth + 1,
                    )?;
                    let mut variants: Vec<&Prim> = variant_set.variants.iter().collect();
                    if order == VariantOrder::ByName {
                        // Names are unique: the order is total.
                        variants.sort_unstable_by(|a, b| a.name.cmp(&b.name));
                    }
                    stack.push(Frame::Set {
                        set: variant_set,
                        variants,
                        depth: depth + 1,
                        owner_len,
                        next: 0,
                    });
                } else {
                    stack.pop();
                    let step = if variant {
                        Step::LeaveVariant
                    } else {
                        Step::LeavePrim
                    };
                    visit(step, &path, depth)?;
                    path.truncate(path_len);
                }
            }
            Frame::Set {
                set,
                variants,
                depth,
                owner_len,
                next,
            } => {
                let (set, depth, owner_len) = (*set, *depth, *owner_len);
                if let Some(&variant) = variants.get(*next) {
                    let index = *next;
                    *next += 1;
                    // The variant's path replaces its set's (`/A{v=}`):
                    // `/A{v=x}`.
                    path.truncate(owner_len);
                    path.push('{');
                    path.push_str(&set.name);
                    path.push('=');
                    path.push_str(&variant.name);
                    path.push('}');
                    visit(
                        Step::Variant {
                            variant,
                            set,
                            index,
                        },
                        &path,
                        depth + 1,
                    )?;
                    stack.push(Frame::Spec {
                        spec: variant,
                        variant: true,
                        depth: depth + 1,
                        path_len: owner_len,
                        child: 0,
                        set: 0,
                    });
                } else {
                    stack.pop();
                    visit(Step::LeaveSet, &path, depth)?;
                    path.truncate(owner_len);
                }
            }
        }
        // A variant left (its path truncated to its owner's) restores its
        // set's path.
        if let Some(Frame::Set { set, owner_len, .. }) = stack.last()
            && path.len() == *owner_len
        {
            push_set_path(&mut path, &set.name);
        }
    }
}

/// Enters `prim`, the `index`th of `siblings`, at `depth`.
fn enter_prim<'a, E>(
    path: &mut String,
    stack: &mut Vec<Frame<'a>>,
    prim: &'a Prim,
    siblings: &'a [Prim],
    index: usize,
    depth: usize,
    visit: &mut impl FnMut(Step<'a>, &str, usize) -> Result<(), E>,
) -> Result<(), E> {
    let path_len = path.len();
    // A prim in a variant (`/P{v=x}C`) follows its variant's selection
    // directly.
    if !path.ends_with('}') {
        path.push('/');
    }
    path.push_str(&prim.name);
    visit(
        Step::Prim {
            prim,
            siblings,
            index,
        },
        path,
        depth,
    )?;
    stack.push(Frame::Spec {
        spec: prim,
        variant: false,
        depth,
        path_len,
        child: 0,
        set: 0,
    });
    Ok(())
}

/// Appends a variant set's part of a path: `{v=}`.
fn push_set_path(path: &mut String, name: &str) {
    path.push('{');
    path.push_str(name);
    path.push_str("=}");
}
