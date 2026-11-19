// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Destinations for the authored namespace walk. Text retains the tree.
//! A crate consumes each spec's fields and child names as it is visited.

use super::*;

pub(super) trait Sink {
    fn root(&mut self, root: Document, children: &[TokenId]);
    fn body(
        &mut self,
        path: &str,
        prim: Prim,
        variant: bool,
        duplicate: bool,
        children: &[TokenId],
        sets: &[(&TokenId, &VariantSetSpec)],
    );
    fn set(&mut self, owner: &str, name: &str, variants: &[(TokenId, &VariantSpec)]);
    fn leave(&mut self);
}

enum Open {
    Prim(Box<Prim>),
    Set(VariantSet),
}

#[derive(Default)]
pub(super) struct DocumentBuilder {
    pub(super) document: Document,
    open: Vec<Open>,
}

impl Sink for DocumentBuilder {
    fn root(&mut self, root: Document, _: &[TokenId]) {
        self.document = root;
    }

    fn body(
        &mut self,
        _: &str,
        prim: Prim,
        _: bool,
        _: bool,
        _: &[TokenId],
        _: &[(&TokenId, &VariantSetSpec)],
    ) {
        self.open.push(Open::Prim(Box::new(prim)));
    }

    fn set(&mut self, _: &str, name: &str, variants: &[(TokenId, &VariantSpec)]) {
        self.open.push(Open::Set(VariantSet {
            name: name.into(),
            variants: Vec::with_capacity(variants.len()),
        }));
    }

    fn leave(&mut self) {
        match (self.open.pop().expect("an open spec"), self.open.last_mut()) {
            (Open::Prim(p), Some(Open::Prim(parent))) => parent.children.push(*p),
            (Open::Prim(p), Some(Open::Set(set))) => set.variants.push(*p),
            (Open::Prim(p), None) => self.document.prims.push(*p),
            (Open::Set(set), Some(Open::Prim(p))) => p.variant_sets.push(set),
            (Open::Set(_), _) => unreachable!("a variant set belongs to a prim or variant"),
        }
    }
}

/// A single authored spec for the binary writer. Nested specs arrive separately,
/// in namespace order. This is an implementation interface between format crates.
#[derive(Debug)]
pub enum LayerSpec<'a> {
    /// Layer fields and root prim names.
    Root(Document, Vec<String>),
    /// Prim or variant fields, followed separately by the specs of its children.
    Body {
        /// Absolute, variant-qualified spec path.
        path: &'a str,
        /// The fields and properties belonging to this spec.
        prim: &'a mut Prim,
        /// Whether this is a variant body.
        variant: bool,
        /// Authored prim children in field order.
        children: Vec<String>,
        /// Nonempty variant sets in field order.
        sets: Vec<String>,
    },
    /// Variant set path and its variant children.
    Set(String, Vec<String>),
}

/// Visits the layer header and one validated spec body at a time. A visitor
/// stages its result until the walk succeeds. Source lowering errors take
/// precedence over shared document validation, as in [`usdc_document`].
///
/// # Errors
///
/// See [`usdc_document`].
pub fn visit_layer_specs(
    layer: &Layer,
    tokens: &TokenInterner,
    paths: &PathInterner,
    visit: impl FnMut(LayerSpec<'_>),
) -> Result<(), SaveError> {
    let mut sink = CheckedSpecs {
        tokens,
        visit,
        validation: Ok(()),
    };
    Lowering {
        tokens,
        paths,
        skip_empty_entries: false,
    }
    .layer(layer, &mut sink)?;
    sink.validation.map_err(SaveError::Document)
}

struct CheckedSpecs<'a, F> {
    tokens: &'a TokenInterner,
    visit: F,
    validation: Result<(), WriteError>,
}

impl<F: FnMut(LayerSpec<'_>)> Sink for CheckedSpecs<'_, F> {
    fn root(&mut self, root: Document, children: &[TokenId]) {
        self.validation = root.validate();
        if self.validation.is_ok() {
            (self.visit)(LayerSpec::Root(
                root,
                children
                    .iter()
                    .map(|&t| self.tokens.resolve(t).into())
                    .collect(),
            ));
        }
    }

    fn body(
        &mut self,
        path: &str,
        mut prim: Prim,
        variant: bool,
        duplicate: bool,
        children: &[TokenId],
        sets: &[(&TokenId, &VariantSetSpec)],
    ) {
        if self.validation.is_err() {
            return;
        }
        self.validation = if variant {
            // Variant names are keys of the authored set.
            crate::writer::validate_variant(&prim, path, &[])
        } else {
            prim.validate(path, duplicate)
        };
        if self.validation.is_ok() {
            (self.visit)(LayerSpec::Body {
                path,
                prim: &mut prim,
                variant,
                children: children
                    .iter()
                    .map(|&t| self.tokens.resolve(t).into())
                    .collect(),
                sets: sets
                    .iter()
                    .filter(|(_, s)| !s.variants.is_empty())
                    .map(|(t, _)| self.tokens.resolve(**t).into())
                    .collect(),
            });
        }
    }

    fn set(&mut self, owner: &str, name: &str, variants: &[(TokenId, &VariantSpec)]) {
        if self.validation.is_err() {
            return;
        }
        let path = branch_display(owner, name, "");
        self.validation = crate::writer::validate_set_name(name, &path);
        if self.validation.is_ok() {
            (self.visit)(LayerSpec::Set(
                path,
                variants
                    .iter()
                    .map(|(t, _)| self.tokens.resolve(*t).into())
                    .collect(),
            ));
        }
    }

    fn leave(&mut self) {}
}
