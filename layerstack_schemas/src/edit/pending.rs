// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, vec::Vec};
use hashbrown::HashMap;
#[cfg(feature = "usd-geom")]
use layerstack::FieldValue;
use layerstack::{PathId, PropertyType, Specifier, TargetPath, TokenId, Value};

use super::PropertyMetadataEdit;
#[cfg(feature = "usd-geom")]
use super::TimeSampleEdit;

/// Pending opinions on one prim, kept apart from the transaction's replay
/// order. Queries visit the addressed prim even during a large edit.
#[derive(Clone, Default)]
pub(super) struct PrimEdits {
    pub(super) type_name: Option<TokenId>,
    pub(super) specifier: Option<Specifier>,
    pub(super) applied: Vec<TokenId>,
    pub(super) created: Vec<(TokenId, Option<PropertyType>)>,
    pub(super) defaults: Vec<(TokenId, Value)>,
    other: Option<Box<OtherEdits>>,
}

static EMPTY_PRIM: PrimEdits = PrimEdits {
    type_name: None,
    specifier: None,
    applied: Vec::new(),
    created: Vec::new(),
    defaults: Vec::new(),
    other: None,
};

#[derive(Clone, Default)]
pub(super) struct OtherEdits {
    #[cfg(feature = "usd-geom")]
    pub(super) samples: Vec<TimeSampleEdit>,
    #[cfg(feature = "usd-geom")]
    pub(super) metadata: Vec<(TokenId, FieldValue)>,
    pub(super) property_metadata: Vec<PropertyMetadataEdit>,
    pub(super) removed: Vec<TokenId>,
    #[cfg(feature = "usd-ui")]
    pub(super) ui_hints: Vec<(TargetPath, Value)>,
    pub(super) dictionaries: Vec<(TargetPath, TokenId, Value)>,
}

static EMPTY_OTHER: OtherEdits = OtherEdits {
    #[cfg(feature = "usd-geom")]
    samples: Vec::new(),
    #[cfg(feature = "usd-geom")]
    metadata: Vec::new(),
    property_metadata: Vec::new(),
    removed: Vec::new(),
    #[cfg(feature = "usd-ui")]
    ui_hints: Vec::new(),
    dictionaries: Vec::new(),
};

impl PrimEdits {
    pub(super) fn is_defined(&self) -> bool {
        matches!(self.specifier, Some(Specifier::Def | Specifier::Class))
    }

    pub(super) fn other(&self) -> &OtherEdits {
        self.other.as_deref().unwrap_or(&EMPTY_OTHER)
    }

    pub(super) fn other_mut(&mut self) -> &mut OtherEdits {
        self.other.get_or_insert_with(Box::default)
    }
}

/// Each child group borrows the parent's pending opinions and owns only the
/// prims it changes. Committing transfers those prims to the parent. Dropping
/// the child leaves the parent intact, including when the closure unwinds.
#[derive(Default)]
pub(super) struct PendingEdits<'a> {
    parent: Option<&'a Self>,
    pub(super) local: LocalEdits,
}

#[derive(Default)]
pub(super) struct LocalEdits {
    prims: HashMap<PathId, PrimEdits>,
    // Child queries preserve the authored order of each kind across groups.
    #[cfg(feature = "usd-geom")]
    defined: Vec<PathId>,
    #[cfg(feature = "usd-geom")]
    overridden: Vec<PathId>,
}

impl<'a> PendingEdits<'a> {
    pub(super) fn child(&self) -> PendingEdits<'_> {
        PendingEdits {
            parent: Some(self),
            local: LocalEdits::default(),
        }
    }

    pub(super) fn prim(&self, path: PathId) -> &PrimEdits {
        self.local
            .prims
            .get(&path)
            .or_else(|| self.parent.map(|parent| parent.prim(path)))
            .unwrap_or(&EMPTY_PRIM)
    }

    pub(super) fn prim_mut(&mut self, path: PathId) -> &mut PrimEdits {
        let parent = self.parent;
        self.local.prims.entry(path).or_insert_with(|| {
            parent.map_or_else(PrimEdits::default, |parent| parent.prim(path).clone())
        })
    }

    pub(super) fn mark_defining(&mut self, path: PathId, specifier: Specifier) {
        self.prim_mut(path).specifier = Some(specifier);
        #[cfg(feature = "usd-geom")]
        self.local.defined.push(path);
    }

    pub(super) fn mark_override(&mut self, path: PathId) {
        self.prim_mut(path).specifier = Some(Specifier::Over);
        #[cfg(feature = "usd-geom")]
        self.local.overridden.push(path);
    }

    pub(super) fn absorb(&mut self, local: LocalEdits) {
        self.local.prims.extend(local.prims);
        #[cfg(feature = "usd-geom")]
        {
            self.local.defined.extend(local.defined);
            self.local.overridden.extend(local.overridden);
        }
    }

    #[cfg(feature = "usd-geom")]
    pub(super) fn for_each_placed(&self, f: &mut impl FnMut(PathId)) {
        self.for_each_ordered(f, |edits| &edits.defined);
        self.for_each_ordered(f, |edits| &edits.overridden);
    }

    #[cfg(feature = "usd-geom")]
    fn for_each_ordered(&self, f: &mut impl FnMut(PathId), paths: fn(&LocalEdits) -> &[PathId]) {
        if let Some(parent) = self.parent {
            parent.for_each_ordered(f, paths);
        }
        paths(&self.local).iter().copied().for_each(f);
    }
}
