// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Borrowed inputs for comparing authored property layouts in both writers.
use layerstack::{
    ListOp, PathId, PrimSpec, PropertyKind, PropertySpec, PropertyType, TargetPath, TokenId, Value,
    Variability,
};

/// Source of ordered property declarations on ordinary prims.
pub trait PropertySource {
    /// Borrows all properties of the prim in authored order.
    fn properties<'a>(
        &'a self,
        path: PathId,
        prim: &'a PrimSpec,
    ) -> impl Iterator<Item = PropertyView<'a>>;
}

/// Properties stored on the prim itself.
#[derive(Debug)]
pub struct NativeProperties;

impl PropertySource for NativeProperties {
    fn properties<'a>(
        &'a self,
        _: PathId,
        prim: &'a PrimSpec,
    ) -> impl Iterator<Item = PropertyView<'a>> {
        prim.properties
            .iter()
            .map(|e| PropertyView::native(e.name, &e.spec))
    }
}

/// Borrowed declarations, defaults and optional independent opinions.
#[derive(Debug)]
pub struct PropertyView<'a> {
    /// Authored property name.
    pub name: TokenId,
    /// Attribute or relationship.
    pub kind: PropertyKind,
    /// Authored custom flag.
    pub custom: bool,
    /// Authored variability.
    pub variability: Variability,
    /// Attribute type declaration.
    pub type_name: Option<&'a PropertyType>,
    /// Default opinion, including a block.
    pub default: Option<&'a Value>,
    /// Connection or relationship lists.
    pub targets: Option<Targets<'a>>,
    /// Owner of metadata, time samples and spline when present.
    pub extra: Option<&'a PropertySpec>,
}

impl<'a> PropertyView<'a> {
    /// Borrows an ordinary property record without copying its opinions.
    pub fn native(name: TokenId, spec: &'a PropertySpec) -> Self {
        Self {
            name,
            kind: spec.kind,
            custom: spec.custom,
            variability: spec.variability,
            type_name: spec.type_name.as_ref(),
            default: spec.default.as_ref(),
            targets: spec.targets.as_deref().map(Targets::Native),
            extra: Some(spec),
        }
    }
}

/// Target lists held in a native record or borrowed from alternative storage.
#[derive(Debug)]
pub enum Targets<'a> {
    /// Authored record.
    Native(&'a ListOp<TargetPath>),
    /// Explicit targets including an explicitly empty list.
    Explicit(&'a [TargetPath]),
    /// Edited lists in prepend, append, delete, add and reorder order.
    Lists(Option<&'a [TargetPath]>, [&'a [TargetPath]; 5]),
}
impl<'a> Targets<'a> {
    pub(super) fn slices(self) -> (Option<&'a [TargetPath]>, [&'a [TargetPath]; 5]) {
        match self {
            Self::Native(op) => (
                op.explicit.as_deref(),
                [&op.prepend, &op.append, &op.delete, &op.add, &op.reorder],
            ),
            Self::Explicit(items) => (Some(items), [&[]; 5]),
            Self::Lists(explicit, edits) => (explicit, edits),
        }
    }
    pub(crate) fn explicit(self) -> Option<&'a [TargetPath]> {
        let (explicit, edits) = self.slices();
        explicit.filter(|_| edits.iter().all(|list| list.is_empty()))
    }
}
