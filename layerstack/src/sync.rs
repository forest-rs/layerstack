// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared-state building blocks for tables several threads fill at once.
//!
//! With the `std` feature a [`ReadLock`] is a `RwLock` and [`Slots`] are
//! `OnceLock`s, so the tables built from them are [`Sync`]; without it they
//! are a `RefCell` and `OnceCell`s, for one thread.

use alloc::boxed::Box;

#[cfg(not(feature = "std"))]
use core::cell::{OnceCell as Once, RefCell};
#[cfg(feature = "std")]
use std::sync::{OnceLock as Once, RwLock};

/// A value many threads read at once and one writes at a time: a `RwLock`
/// with the `std` feature, a `RefCell` without.
pub(crate) struct ReadLock<T>(
    #[cfg(feature = "std")] RwLock<T>,
    #[cfg(not(feature = "std"))] RefCell<T>,
);

impl<T> ReadLock<T> {
    pub(crate) fn new(value: T) -> Self {
        #[cfg(feature = "std")]
        let lock = RwLock::new(value);
        #[cfg(not(feature = "std"))]
        let lock = RefCell::new(value);
        Self(lock)
    }

    /// Runs `f` on the value, sharing it with other readers.
    pub(crate) fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        #[cfg(feature = "std")]
        let value = self
            .0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(not(feature = "std"))]
        let value = self.0.borrow();
        f(&value)
    }

    /// Runs `f` on the value while no one else reads or writes it.
    pub(crate) fn write<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        #[cfg(feature = "std")]
        let mut value = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(not(feature = "std"))]
        let mut value = self.0.borrow_mut();
        f(&mut value)
    }
}

impl<T: Default> Default for ReadLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// The number of slots in the first chunk of [`Slots`], as a power of two;
/// each later chunk is twice the size of the one before.
pub(crate) const FIRST_CHUNK_BITS: u32 = 10;

/// The number of chunks of [`Slots`], enough for every `u32` position.
const CHUNKS: usize = 33 - FIRST_CHUNK_BITS as usize;

/// Values by position, each set once, read through a shared reference
/// without locking while others are set.
///
/// Chunk `k` holds positions `(2^k - 1) << FIRST_CHUNK_BITS` up to the next
/// chunk's first. A chunk, once allocated, never moves, and a value, once
/// set, never changes, so a reference to one lives as long as the slots.
pub(crate) struct Slots<T> {
    chunks: [Once<Box<[Once<T>]>>; CHUNKS],
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self {
            chunks: core::array::from_fn(|_| Once::new()),
        }
    }
}

/// Returns the chunk holding position `n` and its place in that chunk.
fn chunk_of(n: u32) -> (usize, usize) {
    let biased = (n >> FIRST_CHUNK_BITS) + 1;
    let chunk = 31 - biased.leading_zeros();
    let first = ((1_u32 << chunk) - 1) << FIRST_CHUNK_BITS;
    (chunk as usize, (n - first) as usize)
}

impl<T> Slots<T> {
    /// Returns the value at position `n`, or `None` before it is set.
    pub(crate) fn get(&self, n: u32) -> Option<&T> {
        let (chunk, slot) = chunk_of(n);
        self.chunks[chunk].get()?[slot].get()
    }

    /// Sets the value at position `n`, which must not be set yet.
    pub(crate) fn set(&self, n: u32, value: T) {
        let (chunk, slot) = chunk_of(n);
        let chunk = self.chunks[chunk].get_or_init(|| {
            // The last chunks hold more positions than 32-bit memory can.
            let len = usize::try_from(1_u64 << (FIRST_CHUNK_BITS as usize + chunk))
                .expect("slot capacity");
            (0..len).map(|_| Once::new()).collect()
        });
        if chunk[slot].set(value).is_err() {
            unreachable!("each position is set once");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_positions_without_gaps() {
        assert_eq!(chunk_of(0), (0, 0));
        assert_eq!(chunk_of(1023), (0, 1023));
        assert_eq!(chunk_of(1024), (1, 0));
        assert_eq!(chunk_of(3071), (1, 2047));
        assert_eq!(chunk_of(3072), (2, 0));
        let (last, slot) = chunk_of(u32::MAX);
        assert_eq!(last, CHUNKS - 1);
        assert!((slot as u64) < 1_u64 << (FIRST_CHUNK_BITS as usize + last));
    }

    #[test]
    fn slots_read_what_was_set() {
        let slots: Slots<u32> = Slots::default();
        for n in [0, 5, 1023, 1024, 70_000] {
            assert_eq!(slots.get(n), None);
            slots.set(n, n * 2);
            assert_eq!(slots.get(n), Some(&(n * 2)));
        }
        assert_eq!(slots.get(6), None);
    }
}
