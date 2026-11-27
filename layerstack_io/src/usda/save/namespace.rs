// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Destinations for the authored namespace walk. Documents retain the tree.
//! A crate consumes each spec's fields and child names as it is visited.

use super::*;
use crate::usda::writer::borrowed::Order;

pub(super) trait Sink<'a> {
    /// Consume an ordinary prim directly when this destination can represent
    /// all of its opinions. Other prims follow the general lowering below.
    fn borrow_prim(
        &mut self,
        _: &'a PrimSpec,
        _: PathId,
        _: TokenId,
        _: &'a TokenInterner,
        _: &'a PathInterner,
    ) -> bool {
        false
    }

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

impl Sink<'_> for DocumentBuilder {
    fn root(&mut self, mut root: Document, children: &[TokenId]) {
        root.prims.reserve_exact(children.len());
        self.document = root;
    }

    fn body(
        &mut self,
        _: &str,
        mut prim: Prim,
        _: bool,
        _: bool,
        children: &[TokenId],
        sets: &[(&TokenId, &VariantSetSpec)],
    ) {
        prim.children.reserve_exact(children.len());
        prim.variant_sets.reserve_exact(sets.len());
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
/// in namespace order. This is an implementation interface between the format modules.
#[derive(Debug)]
pub(crate) enum LayerSpec<'a> {
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
    /// Plain prim fields and checked declarations in authored order.
    Declarations {
        /// Interned namespace path of this prim.
        path: PathId,
        /// The prim supplying the header and child names.
        prim: &'a PrimSpec,
        /// Exact-typed properties with validated names and target paths.
        properties: &'a [Declaration<'a>],
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
pub(crate) fn visit_layer_specs(
    layer: &Layer,
    tokens: &TokenInterner,
    paths: &PathInterner,
    visit: impl FnMut(LayerSpec<'_>),
) -> Result<(), LayerSaveError> {
    let mut sink = CheckedSpecs {
        names: crate::usda::writer::CheckedNames::default(),
        declarations: Vec::new(),
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
    sink.validation.map_err(LayerSaveError::Document)
}

struct CheckedSpecs<'a, F> {
    names: crate::usda::writer::CheckedNames,
    declarations: Vec<Declaration<'a>>,
    tokens: &'a TokenInterner,
    visit: F,
    validation: Result<(), WriteError>,
}

impl<'a, F: FnMut(LayerSpec<'_>)> Sink<'a> for CheckedSpecs<'a, F> {
    fn borrow_prim(
        &mut self,
        spec: &'a PrimSpec,
        id: PathId,
        name: TokenId,
        tokens: &'a TokenInterner,
        paths: &'a PathInterner,
    ) -> bool {
        self.declarations.clear();
        if self
            .names
            .borrow_prim(
                spec,
                name,
                tokens,
                paths,
                &mut self.declarations,
                Order::Authored,
            )
            .is_none()
        {
            return false;
        }
        if self.validation.is_err() {
            return true;
        }
        (self.visit)(LayerSpec::Declarations {
            path: id,
            prim: spec,
            properties: &self.declarations,
        });
        true
    }

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
            crate::usda::writer::validate_variant(&prim, path, &[])
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
        self.validation = crate::usda::writer::validate_set_name(name, &path);
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

pub(super) struct TextBuilder<O> {
    names: crate::usda::writer::CheckedNames,
    open: Vec<bool>,
    after_sibling: bool,
    output: O,
    shared: Result<(), WriteError>,
    text: Result<(), WriteError>,
    minor: u8,
}

impl<O: Default> TextBuilder<O> {
    pub(super) fn new() -> Self {
        Self {
            output: O::default(),
            names: crate::usda::writer::CheckedNames::default(),
            open: Vec::new(),
            after_sibling: false,
            shared: Ok(()),
            text: Ok(()),
            minor: 0,
        }
    }

    fn open_body(&mut self, variant: bool, children: bool) -> crate::usda::writer::Layout {
        let depth = self.open.len();
        let layout = crate::usda::writer::Layout {
            depth,
            variant,
            blank_before: !variant && (depth == 0 || self.after_sibling),
            children,
        };
        self.open.push(variant);
        self.after_sibling = false;
        layout
    }

    pub(super) fn finish<'a>(self) -> Result<O::Finished, LayerSaveError>
    where
        O: crate::usda::writer::TextOutput<'a>,
    {
        self.shared
            .and(self.text)
            .map_err(LayerSaveError::Document)?;
        Ok(self.output.finish(self.minor))
    }
}

impl<'a, O: crate::usda::writer::TextOutput<'a>> Sink<'a> for TextBuilder<O> {
    fn borrow_prim(
        &mut self,
        spec: &'a PrimSpec,
        _: PathId,
        name: TokenId,
        tokens: &'a TokenInterner,
        paths: &'a PathInterner,
    ) -> bool {
        if self.shared.is_err() || self.text.is_err() {
            return false;
        }
        let Some(body) = self.names.borrow_prim(
            spec,
            name,
            tokens,
            paths,
            self.output.declarations(),
            Order::Text,
        ) else {
            return false;
        };
        let layout = self.open_body(false, !spec.authored_children.is_empty());
        self.output.borrowed(body, layout);
        true
    }

    fn root(&mut self, root: Document, _: &[TokenId]) {
        self.shared = root.validate();
        self.text = crate::usda::writer::text_metadata(&root.metadata, "/");
        self.minor = crate::usda::writer::value_text_version::metadata(&root.metadata);
        if self.shared.is_ok() && self.text.is_ok() {
            self.output.root(root);
        }
    }

    fn body(
        &mut self,
        path: &str,
        prim: Prim,
        variant: bool,
        duplicate: bool,
        children: &[TokenId],
        _: &[(&TokenId, &VariantSetSpec)],
    ) {
        if self.shared.is_ok() {
            self.shared = if variant {
                crate::usda::writer::validate_variant(&prim, path, &[])
            } else {
                prim.validate(path, duplicate)
            };
        }
        if self.text.is_ok() {
            self.text = crate::usda::writer::validate_text_body(&prim, path);
        }
        if self.shared.is_err() || self.text.is_err() {
            return;
        }
        self.minor = self
            .minor
            .max(crate::usda::writer::body_text_version(&prim));
        let layout = self.open_body(variant, !children.is_empty());
        self.output.body(prim, layout);
    }

    fn set(&mut self, owner: &str, name: &str, _: &[(TokenId, &VariantSpec)]) {
        if self.shared.is_ok() {
            self.shared =
                crate::usda::writer::validate_set_name(name, &branch_display(owner, name, ""));
        }
        if self.shared.is_err() || self.text.is_err() {
            return;
        }
        self.output.set(name, self.open.len());
        self.open.push(false);
        self.after_sibling = false;
    }

    fn leave(&mut self) {
        if self.shared.is_err() || self.text.is_err() {
            return;
        }
        let variant = self.open.pop().expect("paired with an opening spec");
        self.output.leave(self.open.len(), variant);
        self.after_sibling = true;
    }
}
