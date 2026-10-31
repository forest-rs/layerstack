// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What an applied edit leaves the host: its inverse, and its report.

use alloc::sync::Arc;

use layerstack::{LayerId, Transaction};
use wasm_bindgen::prelude::*;

use crate::{error::Error, wire::Wire};

/// An applied batch of edits: the guarded inverse that undoes it, and
/// what it did. The native object behind the TypeScript `EditReceipt`.
///
/// `Stage.undo` applies the inverse and returns the receipt of the undo,
/// whose own inverse redoes the edits. Undoing consumes the inverse: a
/// receipt undoes once. Freeing a receipt releases the inverse; the
/// engine keeps no history. [`EditReceipt::copy`] and
/// [`EditReceipt::absorb`] combine several receipts' inverses into one
/// receipt, which undoes them all as one atomic transaction.
///
/// Spec: AOUSD Core §7 (scene description); OpenUSD has no inverse, see
/// [`Transaction`].
#[wasm_bindgen]
#[derive(Debug)]
pub struct EditReceipt {
    /// The identity of the stage that made it.
    pub(crate) stage: Arc<()>,
    /// The layer the edits went to.
    pub(crate) target: LayerId,
    /// The record of the edit target they went through
    /// ([`crate::target`]), which the receipt of an undo names too.
    pub(crate) target_record: Wire,
    /// The transaction that undoes the edits; `None` once undone.
    pub(crate) inverse: Option<Transaction>,
    /// `[target, changes, loaded, failures, unavailable, layers,
    /// unrepaired]`, as `Stage::receipt` documents it.
    pub(crate) record: Wire,
}

impl EditReceipt {
    /// The inverse that undoes the edits, `None` once undone.
    #[must_use]
    pub fn inverse(&self) -> Option<&Transaction> {
        self.inverse.as_ref()
    }

    /// The layer the edits went to.
    #[must_use]
    pub fn target(&self) -> LayerId {
        self.target
    }

    /// A receipt holding a copy of this one's inverse and report, which
    /// undoes the same edits; this one stays as it is.
    ///
    /// # Errors
    ///
    /// `receipt.released` when this receipt was undone or released.
    pub fn copy(&self) -> Result<Self, Error> {
        let inverse = self.inverse.clone().ok_or_else(released)?;
        Ok(Self {
            stage: Arc::clone(&self.stage),
            target: self.target,
            target_record: self.target_record.clone(),
            inverse: Some(inverse),
            record: self.record.clone(),
        })
    }

    /// Appends a copy of `older`'s inverse to this receipt's own, so this
    /// receipt undoes both as one atomic transaction: its own edits first,
    /// then `older`'s ([`Transaction::append`]). `older` names edits
    /// applied before this receipt's, and stays as it is. The report is
    /// this receipt's.
    ///
    /// # Errors
    ///
    /// `receipt.affinity` when `older` belongs to another stage;
    /// `receipt.released` when either was undone or released. Nothing
    /// changes.
    pub fn absorb(&mut self, older: &Self) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.stage, &older.stage) {
            return Err(Error::new(
                "receipt.affinity",
                "the receipts belong to different stages",
            ));
        }
        let addition = older.inverse.clone().ok_or_else(released)?;
        self.inverse.as_mut().ok_or_else(released)?.append(addition);
        Ok(())
    }
}

fn released() -> Error {
    Error::new("receipt.released", "the receipt was undone or released")
}

#[wasm_bindgen]
impl EditReceipt {
    /// The receipt's record, as `Stage::receipt` documents it.
    #[wasm_bindgen(js_name = record)]
    pub fn js_record(&self) -> JsValue {
        self.record.clone().into()
    }

    /// Whether the inverse is still held: the receipt can undo.
    #[wasm_bindgen(getter, js_name = undoable)]
    #[must_use]
    pub fn undoable(&self) -> bool {
        self.inverse.is_some()
    }

    /// A copy of the receipt ([`Self::copy`]).
    #[wasm_bindgen(js_name = copy)]
    pub fn js_copy(&self) -> Result<Self, JsValue> {
        Ok(self.copy()?)
    }

    /// Appends a copy of `older`'s inverse ([`Self::absorb`]).
    #[wasm_bindgen(js_name = absorb)]
    pub fn js_absorb(&mut self, older: &Self) -> Result<(), JsValue> {
        Ok(self.absorb(older)?)
    }
}
