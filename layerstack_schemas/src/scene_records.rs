// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-owned renderer-neutral scene records with explicit incremental updates.
//!
//! [`crate::scene_records::SceneObserver::update`] synchronizes a host-owned live
//! stage, captures checked polygon meshes and point instancers, and reports component changes. Native
//! instance descendants share polygon geometry; inherited primvars, bindings,
//! transforms and appearance are queried in each occurrence's namespace.
//!
//! No triangulation, subdivision evaluation, shader execution, I/O or scheduling
//! occurs here. Numeric owners are retained where schema getters match storage;
//! legacy representation conversion, interpolation and deferred decoding can
//! materialize arrays. Routing scans retained records after changes; binding
//! resolution conservatively refreshes on every composed change to cover
//! collections, forwarding and missing targets outside the geometry namespace.
//! A failed capture leaves the last successful records and cursor intact.
//! Lost history explicitly retires all handles and reconstructs the records.
//! Changed polls clone record headers and bounded change batches while staging an
//! atomic result, reconstruct native links, and may allocate small binding/mask
//! inventories. Equal geometry from different numeric owners is compared bitwise
//! before retaining the previously validated owner; this scan is counted.

mod capture;
mod types;
mod update;

pub use types::*;
pub use update::SceneObserver;

#[cfg(test)]
mod tests;
