// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The table behind [`crate::TokenInterner`] and [`crate::PathInterner`]:
//! values interned through a shared reference, from any number of threads.
//!
//! Each value gets the next id of one counter, so ids stay dense (dependency
//! tables index by them, [`invalidation::DenseKey`]) and number values in the
//! order they are first interned; values interned by several threads at once
//! take ids in whichever order the threads get there. Lookups by value hash
//! into one of [`SHARDS`] separately locked tables, which hold only ids;
//! lookups by id read append-only storage without locking.
//!
//! Without the `std` feature the locks are
//! [`RefCell`](core::cell::RefCell)s and the table is not [`Sync`].
//!
//! Built with `--cfg layerstack_reverse_ids`, ids count down instead within
//! each run of 1,024, so a result ordered by id rather than by
//! value comes out reversed: a diagnostic for code that would order its
//! results differently when threads intern in a different order.
//!
//! OpenUSD interns tokens and paths into global tables split the same way,
//! each part behind its own lock (`Tf_TokenRegistry` in
//! `pxr/base/tf/token.cpp`, `_PrimTable` in `pxr/usd/sdf/pathNode.cpp`).

use alloc::boxed::Box;
use core::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    sync::atomic::{AtomicU32, Ordering},
};

use hashbrown::{DefaultHashBuilder, HashTable};

#[cfg(not(feature = "std"))]
use core::cell::{OnceCell as Once, RefCell};
#[cfg(feature = "std")]
use std::sync::{Mutex, OnceLock as Once};

/// The number of separately locked tables; a power of two.
const SHARDS: usize = 64;

/// The hash bits that pick a value's table: above those the tables index
/// buckets with, below those they tag entries with.
const SHARD_SHIFT: u32 = 40;

/// The number of values in the first storage chunk, as a power of two; each
/// later chunk is twice the size of the one before.
const FIRST_CHUNK_BITS: u32 = 10;

/// The number of storage chunks, enough for every `u32` id.
const CHUNKS: usize = 33 - FIRST_CHUNK_BITS as usize;

/// Under `--cfg layerstack_reverse_ids`, ids are reversed within each run
/// of this many, so they stay dense.
#[cfg(layerstack_reverse_ids)]
const REVERSED_RUN: u32 = 1 << FIRST_CHUNK_BITS;

/// The id of the `n`th value interned.
fn id_of(n: u32) -> u32 {
    #[cfg(layerstack_reverse_ids)]
    let id = n ^ (REVERSED_RUN - 1);
    #[cfg(not(layerstack_reverse_ids))]
    let id = n;
    id
}

/// The position in interning order of `id`, the inverse of [`id_of`].
fn position_of(id: u32) -> u32 {
    id_of(id)
}

/// A value behind a lock: a `Mutex` with the `std` feature, a `RefCell`
/// without.
struct Lock<T>(
    #[cfg(feature = "std")] Mutex<T>,
    #[cfg(not(feature = "std"))] RefCell<T>,
);

impl<T> Lock<T> {
    fn new(value: T) -> Self {
        #[cfg(feature = "std")]
        let lock = Mutex::new(value);
        #[cfg(not(feature = "std"))]
        let lock = RefCell::new(value);
        Self(lock)
    }

    /// Runs `f` on the value while holding the lock.
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        #[cfg(feature = "std")]
        let mut value = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(not(feature = "std"))]
        let mut value = self.0.borrow_mut();
        f(&mut value)
    }
}

/// Values interned by id, from any number of threads.
pub(crate) struct Interned<T> {
    hasher: DefaultHashBuilder,
    shards: Box<[Lock<HashTable<u32>>]>,
    /// Chunk `k` holds the values interned `(2^k - 1) << FIRST_CHUNK_BITS`th
    /// up to the next chunk's first. A chunk, once allocated, never moves,
    /// and a value, once set, never changes.
    chunks: [Once<Box<[Once<T>]>>; CHUNKS],
    len: AtomicU32,
}

impl<T> Default for Interned<T> {
    fn default() -> Self {
        Self {
            hasher: DefaultHashBuilder::default(),
            shards: (0..SHARDS).map(|_| Lock::new(HashTable::new())).collect(),
            chunks: core::array::from_fn(|_| Once::new()),
            len: AtomicU32::new(0),
        }
    }
}

/// Returns the chunk holding the `n`th value interned and its place in
/// that chunk.
fn chunk_of(n: u32) -> (usize, usize) {
    let biased = (n >> FIRST_CHUNK_BITS) + 1;
    let chunk = 31 - biased.leading_zeros();
    let first = ((1_u32 << chunk) - 1) << FIRST_CHUNK_BITS;
    (chunk as usize, (n - first) as usize)
}

impl<T: Hash + Eq> Interned<T> {
    /// Returns the number of values interned.
    pub(crate) fn len(&self) -> u32 {
        self.len.load(Ordering::Acquire)
    }

    /// Returns the value of `id`, or `None` for an id this table has not
    /// handed out.
    pub(crate) fn get(&self, id: u32) -> Option<&T> {
        let (chunk, slot) = chunk_of(position_of(id));
        self.chunks[chunk].get()?[slot].get()
    }

    fn shard(&self, hash: u64) -> &Lock<HashTable<u32>> {
        &self.shards[(hash >> SHARD_SHIFT) as usize & (SHARDS - 1)]
    }

    fn rehash(&self) -> impl Fn(&u32) -> u64 + '_ {
        |&id| self.hasher.hash_one(self.get(id).expect("interned id"))
    }

    /// Returns the id of the value equal to `key`, if one is interned.
    pub(crate) fn find<Q>(&self, key: &Q) -> Option<u32>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        self.shard(hash).with(|table| {
            table
                .find(hash, |&id| self.get(id).is_some_and(|v| v.borrow() == key))
                .copied()
        })
    }

    /// Returns the id of the value equal to `key`, interning `make()`, a
    /// value equal to it, first if there is none, and whether it did.
    pub(crate) fn intern<Q>(&self, key: &Q, make: impl FnOnce() -> T) -> (u32, bool)
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        self.shard(hash).with(|table| {
            if let Some(&id) =
                table.find(hash, |&id| self.get(id).is_some_and(|v| v.borrow() == key))
            {
                return (id, false);
            }
            let n = self.len.fetch_add(1, Ordering::AcqRel);
            assert!(n != u32::MAX, "interner overflow");
            let id = id_of(n);
            self.store(n, make());
            table.insert_unique(hash, id, self.rehash());
            (id, true)
        })
    }

    /// Sets the `n`th value interned.
    fn store(&self, n: u32, value: T) {
        let (chunk, slot) = chunk_of(n);
        let chunk = self.chunks[chunk].get_or_init(|| {
            // The last chunks hold more values than 32-bit memory can.
            let len = usize::try_from(1_u64 << (FIRST_CHUNK_BITS as usize + chunk))
                .expect("interner capacity");
            (0..len).map(|_| Once::new()).collect()
        });
        if chunk[slot].set(value).is_err() {
            unreachable!("each id is handed out once");
        }
    }
}

impl<T: Hash + Eq + Clone> Interned<T> {
    /// Returns a copy of this table: the same values under the same ids.
    pub(crate) fn snapshot(&self) -> Self {
        let copy = Self::default();
        let len = self.len();
        // Each id below `len` was handed out under its table's lock, and
        // has its value once that lock is released.
        for shard in &self.shards {
            shard.with(|_| ());
        }
        for n in 0..len {
            let id = id_of(n);
            let value = self.get(id).expect("interned id").clone();
            let hash = copy.hasher.hash_one(&value);
            copy.store(n, value);
            copy.shard(hash).with(|table| {
                table.insert_unique(hash, id, copy.rehash());
            });
        }
        copy.len.store(len, Ordering::Release);
        copy
    }
}

impl<T> core::fmt::Debug for Interned<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Interned")
            .field("len", &self.len.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::String, vec::Vec};

    #[test]
    fn chunks_cover_ids_without_gaps() {
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
    fn ids_number_values_in_first_interned_order() {
        let table: Interned<String> = Interned::default();
        let words: Vec<String> = (0..5000).map(|i| alloc::format!("w{i}")).collect();
        for (n, word) in (0..).zip(&words) {
            assert_eq!(
                table.intern(word.as_str(), || word.clone()),
                (id_of(n), true)
            );
        }
        for (n, word) in (0..).zip(&words) {
            let id = id_of(n);
            assert_eq!(table.intern(word.as_str(), || word.clone()), (id, false));
            assert_eq!(table.find(word.as_str()), Some(id));
            assert_eq!(table.get(id), Some(word));
        }
        assert_eq!(table.find("absent"), None);
        assert_eq!(table.get(id_of(5000)), None);
        let copy = table.snapshot();
        assert_eq!(copy.len(), 5000);
        assert_eq!(copy.find("w4321"), Some(id_of(4321)));
        let new = copy.intern("new", || String::from("new"));
        assert_eq!(new, (id_of(5000), true));
    }

    #[cfg(feature = "std")]
    #[test]
    fn threads_interning_the_same_values_agree_on_their_ids() {
        let table: Interned<String> = Interned::default();
        let ids: Option<Vec<Vec<u32>>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    std::thread::Builder::new().spawn_scoped(scope, || {
                        (0..2000)
                            .map(|i| {
                                let value = alloc::format!("v{i}");
                                table.intern(value.as_str(), || value.clone()).0
                            })
                            .collect::<Vec<u32>>()
                    })
                })
                .collect::<Result<_, _>>()
                .ok()?;
            Some(workers.into_iter().map(|w| w.join().unwrap()).collect())
        });
        // Some targets with `std` cannot start threads (`wasm32-wasip1`).
        let Some(ids) = ids else {
            return;
        };
        assert!(ids.iter().all(|thread| thread == &ids[0]));
        assert_eq!(table.len(), 2000);
        for (i, &id) in ids[0].iter().enumerate() {
            assert_eq!(table.get(id), Some(&alloc::format!("v{i}")));
        }
    }
}
