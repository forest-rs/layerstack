// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layer fields the layer model keeps in dedicated members: `subLayers`
//! with their offsets, and `layerRelocates`.
//!
//! Each edit replaces the whole list in one step ([`Raw::Sublayers`],
//! [`Raw::LayerRelocates`]), so an inverse restores and guards it as one
//! slot. The edits validate the list they write: OpenUSD rejects a
//! duplicate or empty sublayer path when it is authored, but accepts any
//! relocates, and a sublayer cycle, and reports them only when composing.
//! A transaction rejects those too, before anything is written.

use alloc::vec::Vec;

use super::{
    apply::{Journal, Raw, layer},
    error::Rejection,
};
use crate::doc::{LayerId, LayerOffset, LayerStore, Relocate, SublayerEntry};

/// An edit of a layer's sublayers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SublayerEdit {
    Set(Vec<SublayerEntry>),
    Insert(usize, SublayerEntry),
    Remove(usize),
    Offset(usize, LayerOffset),
}

/// Applies `edit` to the sublayers of `id`, checking the result (see
/// [`check_sublayers`]). Writing the list the layer holds does nothing.
pub(crate) fn edit_sublayers(
    store: &mut dyn LayerStore,
    id: LayerId,
    edit: &SublayerEdit,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let current = layer(store, id)?.sublayers.clone();
    let mut sublayers = current.clone();
    match edit {
        SublayerEdit::Set(entries) => sublayers.clone_from(entries),
        SublayerEdit::Insert(index, entry) => {
            if *index > sublayers.len() {
                return Err(Rejection::NoSuchSublayer(*index));
            }
            sublayers.insert(*index, entry.clone());
        }
        SublayerEdit::Remove(index) => {
            if *index >= sublayers.len() {
                return Err(Rejection::NoSuchSublayer(*index));
            }
            sublayers.remove(*index);
        }
        SublayerEdit::Offset(index, offset) => {
            sublayers
                .get_mut(*index)
                .ok_or(Rejection::NoSuchSublayer(*index))?
                .offset = *offset;
        }
    }
    if sublayers == current {
        return Ok(());
    }
    check_sublayers(store, id, &sublayers)?;
    journal.run(
        store,
        Raw::Sublayers {
            layer: id,
            sublayers,
        },
    )
}

/// Rejects a sublayer list that names a layer twice (by layer or by
/// authored asset path), names an empty asset path or a layer the store
/// does not hold, or makes a cycle: a sublayer that is `id`, or whose own
/// sublayers reach it.
///
/// Spec: AOUSD Core §10.3.1 (sublayers form a layer stack). OpenUSD:
/// `SdfSubLayerTypePolicy` rejects empty and repeated paths; a cycle is
/// composed as `PcpErrorSublayerCycle`.
fn check_sublayers(
    store: &dyn LayerStore,
    id: LayerId,
    sublayers: &[SublayerEntry],
) -> Result<(), Rejection> {
    for (index, entry) in sublayers.iter().enumerate() {
        if entry.asset.as_deref() == Some("") {
            return Err(Rejection::EmptySublayerAsset(index));
        }
        let earlier = &sublayers[..index];
        let repeated = earlier.iter().any(|other| {
            (entry.layer != LayerId::UNRESOLVED && other.layer == entry.layer)
                || (entry.asset.is_some() && other.asset == entry.asset)
        });
        if repeated {
            return Err(Rejection::DuplicateSublayer(index));
        }
        if entry.layer == LayerId::UNRESOLVED {
            continue;
        }
        if store.layer(entry.layer).is_none() {
            return Err(Rejection::NoSuchLayer(entry.layer));
        }
        if reaches(store, entry.layer, id) {
            return Err(Rejection::SublayerCycle(entry.layer));
        }
    }
    Ok(())
}

/// Whether `from` is `to`, or sublayers it, directly or through its
/// sublayers.
fn reaches(store: &dyn LayerStore, from: LayerId, to: LayerId) -> bool {
    let mut pending = alloc::vec![from];
    let mut seen = Vec::new();
    while let Some(next) = pending.pop() {
        if next == to {
            return true;
        }
        if seen.contains(&next) {
            continue;
        }
        seen.push(next);
        if let Some(layer) = store.layer(next) {
            pending.extend(layer.sublayers.iter().map(|entry| entry.layer));
        }
    }
    false
}

/// Replaces the `layerRelocates` of `id` with `relocates`, or removes the
/// field for `None` (an empty list authors an empty field, as
/// `SdfLayer::SetRelocates` does), rejecting an entry that composition
/// would ignore: one invalid on its own
/// ([`InvalidRelocationReason`](crate::InvalidRelocationReason)), or one whose source another entry of the
/// list relocates too.
///
/// Spec: AOUSD Core §10.3.2.6 (relocates restrictions). OpenUSD accepts
/// such entries in `SdfLayer::SetRelocates` and reports them when
/// composing (`Pcp_IsValidRelocatesEntry`,
/// `_ValidateAndRemoveConflictingRelocates` in
/// `pxr/usd/pcp/layerStack.cpp`).
pub(crate) fn set_relocates(
    store: &mut dyn LayerStore,
    id: LayerId,
    relocates: Option<&[Relocate]>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    if layer(store, id)?.relocates.as_deref() == relocates {
        return Ok(());
    }
    let entries = relocates.unwrap_or_default();
    for (index, relocate) in entries.iter().enumerate() {
        if let Some(reason) = crate::relocates::invalid_reason(store.paths(), *relocate) {
            return Err(Rejection::InvalidRelocate {
                source: relocate.source,
                reason,
            });
        }
        if entries[..index].iter().any(|r| r.source == relocate.source) {
            return Err(Rejection::DuplicateRelocateSource(relocate.source));
        }
    }
    journal.run(
        store,
        Raw::LayerRelocates {
            layer: id,
            relocates: relocates.map(<[Relocate]>::to_vec),
        },
    )
}
