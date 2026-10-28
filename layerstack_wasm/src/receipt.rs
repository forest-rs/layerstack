// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What an applied edit leaves the host: its inverse, and its report.

use alloc::sync::Arc;

use layerstack::{LayerId, Transaction};
use wasm_bindgen::prelude::*;

use crate::wire::Wire;

/// An applied batch of edits: the guarded inverse that undoes it, and
/// what it did. The native object behind the TypeScript `EditReceipt`.
///
/// `Stage.undo` applies the inverse and returns the receipt of the undo,
/// whose own inverse redoes the edits. Undoing consumes the inverse: a
/// receipt undoes once. Freeing a receipt releases the inverse; the
/// engine keeps no history.
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
    /// The transaction that undoes the edits; `None` once undone.
    pub(crate) inverse: Option<Transaction>,
    /// `[target, changes, loaded, failures, wanted]`, as
    /// `Stage::receipt` documents it.
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
}
