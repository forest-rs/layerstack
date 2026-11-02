// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a document knows of the version of each source it read, so that a
//! reload reads only the sources that changed.

use alloc::sync::Arc;

use crate::Storage;

/// A version of a source, as storage reports it ([`Storage::version`]): a
/// value that changes whenever the source's content does, such as a
/// file's modification time and length, an HTTP entity tag or a host's
/// revision counter. Only equality is read: a reload skips a clean layer
/// whose source reports the version it reported when the layer was read.
///
/// OpenUSD: the modification timestamp `ArResolver::GetModificationTimestamp`
/// gives (`ArTimestamp`), which `SdfLayer::Reload` compares
/// (`pxr/usd/sdf/layer.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[doc(alias = "ArTimestamp")]
pub struct SourceVersion(Arc<str>);

impl SourceVersion {
    /// A version spelled `version`; two versions are equal when their
    /// spellings are.
    pub fn new(version: impl Into<Arc<str>>) -> Self {
        Self(version.into())
    }

    /// A version naming `bytes` themselves, by their length and a 64-bit
    /// digest: for storage that holds sources in memory and has no other
    /// version to report. Equal bytes give equal versions, so a host that
    /// supplies a source again unchanged makes no reload read it.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self::new(alloc::format!(
            "{}:{:016x}",
            bytes.len(),
            content_digest(bytes)
        ))
    }

    /// The version's spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a document tells whether a source changed since it read a layer
/// from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// The version storage reported just before the read.
    Version(SourceVersion),
    /// Storage reports no versions: the length and digest of the bytes
    /// read ([`content_digest`]), which a reload compares with the bytes
    /// it reads.
    Digest { len: u64, digest: u64 },
}

impl Freshness {
    /// What to record for a read of `bytes`, given the version `version`
    /// storage reported before the read: the version, or with none the
    /// bytes' digest.
    pub(crate) fn of(version: Option<SourceVersion>, bytes: &[u8]) -> Self {
        match version {
            Some(version) => Self::Version(version),
            None => Self::Digest {
                len: bytes.len() as u64,
                digest: content_digest(bytes),
            },
        }
    }
}

/// The version `storage` reports for `identifier`, `None` when it reports
/// none or fails: a failure to stat a source makes the reload read it,
/// which reports the failure.
pub(crate) fn version_of<B: Storage + ?Sized>(
    storage: &B,
    identifier: &str,
) -> Option<SourceVersion> {
    storage.version(identifier).ok().flatten()
}

/// Whether the source `identifier`, read when storage reported the
/// version `recorded`, has changed since: storage now reports another
/// version, or none.
pub(crate) fn version_changed<B: Storage + ?Sized>(
    storage: &B,
    identifier: &str,
    recorded: &SourceVersion,
) -> bool {
    version_of(storage, identifier).as_ref() != Some(recorded)
}

/// A 64-bit digest of `bytes`, for telling a changed source from an
/// unchanged one when storage reports no versions. Not cryptographic:
/// accidental changes are what it detects.
pub(crate) fn content_digest(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let step = |hash: u64, word: u64| (hash ^ word).wrapping_mul(K).rotate_left(29);
    let mut hash = (bytes.len() as u64) ^ K;
    let (words, rest) = bytes.as_chunks::<8>();
    for word in words {
        hash = step(hash, u64::from_le_bytes(*word));
    }
    for &byte in rest {
        hash = step(hash, u64::from(byte));
    }
    hash ^ (hash >> 32)
}

#[cfg(test)]
mod tests {
    use super::content_digest;

    #[test]
    fn digests_tell_changed_bytes_apart() {
        let a = content_digest(b"#usda 1.0\ndef \"B\" { double y = 2 }\n");
        let b = content_digest(b"#usda 1.0\ndef \"B\" { double y = 20 }\n");
        let c = content_digest(b"#usda 1.0\ndef \"B\" { double y = 3 }\n");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(
            a,
            content_digest(b"#usda 1.0\ndef \"B\" { double y = 2 }\n")
        );
        assert_ne!(content_digest(b""), content_digest(b"\0"));
    }
}
