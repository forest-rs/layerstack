// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The ids of the layers a document names by identifier.
//!
//! A layer read from a file, a package member (`package[member]`) or a
//! dynamic format's target with its arguments gets an id derived from its
//! identifier, as the catalog keys it: the 64-bit FNV-1a hash of the
//! identifier's bytes, with its top bit set, which places it at or above
//! [`LayerId::FIRST_DERIVED`]. Ids hosts and stores hand out (anonymous
//! and session layers, a store [`StageDocument::open`] is given) stay
//! below that, as [`Catalog::allocate`] does, and [`LayerId::UNRESOLVED`]
//! is never taken. FNV-1a has fixed constants and reads bytes, so an
//! identifier hashes to the same id on every platform, toolchain and
//! process, and the threads reading a scene can stamp a layer's final id
//! into the arcs naming it the moment they find it.
//!
//! A candidate already in use, by a host's layer or another identifier
//! with the same hash, is stepped past: the next candidate hashes the
//! attempt number after the identifier, until one is free. Which of two
//! colliding identifiers keeps the earlier candidate follows the order the
//! loader records layers in, depth first as their arcs ask for them, which
//! is the same on any number of threads. A layer's id depends on more than
//! its identifier only when it steps: under a collision, which a 63-bit
//! hash makes vanishingly rare for distinct identifiers, or past a host's
//! id in the derived range.
//!
//! [`StageDocument::open`]: crate::StageDocument::open
//! [`Catalog::allocate`]: crate::Catalog::allocate

use layerstack::LayerId;

use crate::{IoError, IoErrorKind};

/// FNV-1a's 64-bit offset basis.
const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a's 64-bit prime.
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// Folds `bytes` into the FNV-1a hash `hash`.
fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Identifiers containing this hash weakly in tests: every one of them has
/// the same candidates, so tests can collide identifiers at will.
#[cfg(test)]
pub(crate) const WEAK_HASH: &str = "weak-hash";

/// The `attempt`th id the layer `identifier` names may take.
pub(crate) fn candidate(identifier: &str, attempt: u32) -> LayerId {
    #[cfg(test)]
    if identifier.contains(WEAK_HASH) {
        return LayerId(LayerId::FIRST_DERIVED.0 | u64::from(attempt));
    }
    let mut hash = fnv1a(OFFSET_BASIS, identifier.as_bytes());
    if attempt > 0 {
        hash = fnv1a(hash, &attempt.to_le_bytes());
    }
    LayerId(LayerId::FIRST_DERIVED.0 | hash)
}

/// The first candidate id of `identifier` that is not `taken`, nor
/// [`LayerId::UNRESOLVED`].
pub(crate) fn derive(
    identifier: &str,
    mut taken: impl FnMut(LayerId) -> bool,
) -> Result<LayerId, IoError> {
    for attempt in 0..=u32::MAX {
        let id = candidate(identifier, attempt);
        if id != LayerId::UNRESOLVED && !taken(id) {
            return Ok(id);
        }
    }
    Err(IoError::new(
        IoErrorKind::IdentityExhausted,
        "layer identities exhausted",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is FNV-1a: these are its published 64-bit test vectors,
    /// so a change to it, which would renumber every layer read, fails.
    #[test]
    fn identifiers_hash_with_fnv1a() {
        assert_eq!(fnv1a(OFFSET_BASIS, b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(OFFSET_BASIS, b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(OFFSET_BASIS, b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(
            candidate("a", 0),
            LayerId(0xaf63_dc4c_8601_ec8c),
            "the top bit is already set"
        );
        assert_eq!(candidate("foobar", 0), LayerId(0x8594_4171_f739_67e8));
    }

    #[test]
    fn derived_ids_stay_out_of_the_range_hosts_allocate() {
        for identifier in ["", "root.usda", "/scene/asset.usdc", "pkg.usdz[a.usda]"] {
            for attempt in 0..4 {
                assert!(candidate(identifier, attempt) >= LayerId::FIRST_DERIVED);
            }
        }
    }

    #[test]
    fn a_taken_candidate_is_stepped_past() {
        let first = candidate("root.usda", 0);
        let second = derive("root.usda", |id| id == first).unwrap();
        assert_eq!(second, candidate("root.usda", 1));
        assert_ne!(second, first);
        assert_eq!(derive("root.usda", |_| false).unwrap(), first);
    }
}
