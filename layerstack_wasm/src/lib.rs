// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebAssembly bindings for Layerstack: the native side of the TypeScript
//! package `@forest-rs/layerstack` (`packages/layerstack`).
//!
//! The module opens a stage from the bytes of its root layer and never
//! fetches. The host runs the loop: the stage lists the assets it asks for
//! that the host has not supplied (`unavailable`), the host fetches them
//! however it likes, supplies their bytes ([`HostStorage::supply`], or
//! [`HostStorage::confirm_absent`] for one that does not exist), and the
//! stage reads them (`loadSupplied`), until none is unavailable. A USDZ package is one supplied source. The stage then
//! answers prim, child, attribute and value queries and explains where a
//! value comes from (`explain`).
//!
//! Edits are authored through [`Stage::apply`]: the wrapper fills an
//! [`EditBatch`] from its plain-data edits, and the batch is lowered to one
//! atomic [`layerstack::Transaction`] on an explicit target layer of the
//! stage's layer stack. The [`EditReceipt`] it returns holds the guarded
//! inverse, which [`Stage::undo`] applies, returning the receipt that
//! redoes. The stage keeps one [`layerstack::ChangeCursor`], which the
//! wrapper drains after each call that changes the stage to call its
//! change listeners. A root that cannot be saved (a USDZ package) is edited
//! through a session layer opened in front of it ([`Stage::open`]).
//!
//! Saving writes the dirty layers to the [`HostStorage`], which keeps the
//! bytes for the host to persist ([`HostStorage::take_written`]); exports
//! return bytes and touch nothing: a layer in USDA or USDC
//! ([`layerstack_io::StageDocument::encode_layer`]), the flattened stage,
//! or a USDZ package of the root's dependencies.
//!
//! Composed numeric arrays cross as [`ArrayHandle`]s: the stage's shared
//! buffer, pinned, which the wrapper views in place in the module's memory.
//!
//! Failures are [`Error`]s with a stable dotted code (`io.unavailable`,
//! `path.invalid`); the codes of the core crates' errors come from their
//! `code()` methods ([`layerstack_io::IoErrorKind::code`],
//! [`layerstack::CompositionError::code`]).
//!
//! The exported classes and functions are an ABI for the TypeScript
//! wrapper, which decodes their positional records into typed objects; the
//! wrapper is the JavaScript API. [`native_version`] reports [`ABI`], and
//! the wrapper refuses a module whose ABI it does not speak.
//!
//! A downstream crate can link this crate as an `rlib` into a module of its
//! own, exporting functions that take `&Stage` and reach the document
//! through [`Stage::document`].
//!
//! The bindings are single-threaded: opening and composition run on the
//! calling thread (`StageOptions::threads` is 0).
//!
//! The package's module is built without `std` and with the `standalone`
//! feature, which supplies the global allocator (dlmalloc, as `std` does on
//! `wasm32-unknown-unknown`) and a panic handler that traps: a panic traps
//! the instance as it does with `std`, without the formatting of a message
//! nothing on that target reads. A crate that links this one into a module
//! of its own keeps `std`, which supplies both.
#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod array;
mod edit;
mod error;
mod receipt;
mod stage;
mod storage;
mod value;
mod wire;

/// The allocator and panic handler of a module built without `std`.
#[cfg(all(feature = "standalone", not(feature = "std"), target_arch = "wasm32"))]
mod standalone {
    /// The allocator `std` uses on `wasm32-unknown-unknown`.
    #[global_allocator]
    static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

    /// Traps, as `std`'s panic runtime does on this target once it has
    /// formatted a message it has nowhere to write.
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }
}

pub use array::ArrayHandle;
pub use edit::EditBatch;
pub use error::Error;
pub use receipt::EditReceipt;
pub use stage::{BROWSER_LIMITS, Stage};
pub use storage::HostStorage;

use alloc::vec;

use wasm_bindgen::prelude::*;

/// The version of the records the module exchanges with the TypeScript
/// wrapper: currently 2. It changes whenever an export's arguments or a
/// record's layout change incompatibly; the wrapper
/// refuses a module whose ABI differs from its own (`module.abi`).
pub const ABI: u32 = 2;

/// The crate version, the source revision the module was built from
/// (`LAYERSTACK_REVISION` at build time, `unknown` when unset) and [`ABI`],
/// as `[crate, revision, abi]`.
#[wasm_bindgen(js_name = nativeVersion)]
#[must_use]
pub fn native_version() -> JsValue {
    wire::Wire::List(vec![
        wire::Wire::text(env!("CARGO_PKG_VERSION")),
        wire::Wire::text(option_env!("LAYERSTACK_REVISION").unwrap_or("unknown")),
        wire::Wire::Number(f64::from(ABI)),
    ])
    .into()
}
