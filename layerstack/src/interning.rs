// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The table behind [`crate::TokenInterner`] and [`crate::PathInterner`]:
//! values interned through a shared reference, from any number of threads.
//!
//! Each value gets the next id of one counter, so ids stay dense (dependency
//! tables index by them, [`invalidation::DenseKey`]) and number values in the
//! order they are first interned; values interned by several threads at once
//! take ids in whichever order the threads get there. Lookups by value hash
//! into one of [`SHARDS`] separately locked tables of ids and cached hashes;
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

use crate::sync::{ReadLock, Slots};

/// The number of separately locked tables; a power of two.
const SHARDS: usize = 64;

/// The hash bits that pick a value's table: above those the tables index
/// buckets with, below those they tag entries with.
const SHARD_SHIFT: u32 = 40;

/// Under `--cfg layerstack_reverse_ids`, ids are reversed within each run
/// of this many, so they stay dense.
#[cfg(layerstack_reverse_ids)]
const REVERSED_RUN: u32 = 1 << crate::sync::FIRST_CHUNK_BITS;

/// The id of the `n`th value interned.
pub(crate) const fn id_of(n: u32) -> u32 {
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

/// A shard holds its ids with enough of their hash to relocate them during
/// growth without reading or hashing the values in the slot storage.
#[derive(Clone, Copy)]
struct Entry {
    id: u32,
    hash: u32,
}

impl Entry {
    fn table_hash(&self) -> u64 {
        table_hash(self.hash)
    }
}

/// The low hash bits used inside a shard. Shard selection uses higher bits.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the table stores only the low hash bits"
)]
fn bucket_hash(hash: u64) -> u32 {
    hash as u32
}

/// Replicate the hash so both 32-bit and 64-bit targets can extract their
/// probe position and fingerprint. Shard selection uses different bits.
fn table_hash(hash: u32) -> u64 {
    let hash = u64::from(hash);
    hash | (hash << 32)
}

/// Values interned by id, from any number of threads.
pub(crate) struct Interned<T> {
    hasher: DefaultHashBuilder,
    shards: Box<[ReadLock<HashTable<Entry>>]>,
    /// The values, in interning order.
    values: Slots<T>,
    len: AtomicU32,
}

impl<T> Default for Interned<T> {
    fn default() -> Self {
        Self {
            hasher: DefaultHashBuilder::default(),
            shards: (0..SHARDS)
                .map(|_| ReadLock::new(HashTable::new()))
                .collect(),
            values: Slots::default(),
            len: AtomicU32::new(0),
        }
    }
}

impl<T: Hash + Eq> Interned<T> {
    /// Returns the number of values interned.
    pub(crate) fn len(&self) -> u32 {
        self.len.load(Ordering::Acquire)
    }

    /// Returns the value of `id`, or `None` for an id this table has not
    /// handed out.
    pub(crate) fn get(&self, id: u32) -> Option<&T> {
        self.values.get(position_of(id))
    }

    fn shard(&self, hash: u64) -> &ReadLock<HashTable<Entry>> {
        &self.shards[(hash >> SHARD_SHIFT) as usize & (SHARDS - 1)]
    }

    /// Returns the id of the value equal to `key`, if one is interned.
    pub(crate) fn find<Q>(&self, key: &Q) -> Option<u32>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        self.find_hashed(hash, key)
    }

    fn find_hashed<Q>(&self, hash: u64, key: &Q) -> Option<u32>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let shard = self.shard(hash);
        let hash = bucket_hash(hash);
        shard.read(|table| {
            table
                .find(table_hash(hash), |entry| {
                    entry.hash == hash && self.get(entry.id).is_some_and(|v| v.borrow() == key)
                })
                .map(|entry| entry.id)
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
        // Most values are interned already: find them under a shared lock.
        if let Some(id) = self.find_hashed(hash, key) {
            return (id, false);
        }
        let shard = self.shard(hash);
        let hash = bucket_hash(hash);
        shard.write(|table| {
            if let Some(entry) = table.find(table_hash(hash), |entry| {
                entry.hash == hash && self.get(entry.id).is_some_and(|v| v.borrow() == key)
            }) {
                return (entry.id, false);
            }
            let n = self.len.fetch_add(1, Ordering::AcqRel);
            assert!(n != u32::MAX, "interner overflow");
            let id = id_of(n);
            self.values.set(n, make());
            table.insert_unique(table_hash(hash), Entry { id, hash }, Entry::table_hash);
            (id, true)
        })
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
            shard.write(|_| ());
        }
        for n in 0..len {
            let id = id_of(n);
            let value = self.get(id).expect("interned id").clone();
            let hash = copy.hasher.hash_one(&value);
            copy.values.set(n, value);
            copy.shard(hash).write(|table| {
                let hash = bucket_hash(hash);
                table.insert_unique(table_hash(hash), Entry { id, hash }, Entry::table_hash);
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

    #[test]
    fn equal_hashes_do_not_merge_unequal_values_after_growth() {
        #[derive(Clone, Copy, PartialEq, Eq)]
        struct Colliding(u32);

        impl Hash for Colliding {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                state.write_u8(0);
            }
        }

        let table = Interned::default();
        for n in 0..256 {
            let value = Colliding(n);
            assert_eq!(table.intern(&value, || value), (id_of(n), true));
        }
        for n in 0..256 {
            let value = Colliding(n);
            assert_eq!(table.find(&value), Some(id_of(n)));
            assert_eq!(table.intern(&value, || value), (id_of(n), false));
        }
        assert_eq!(table.find(&Colliding(256)), None);
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
