// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Immutable encoded storage shared safely through checked byte ranges.
use alloc::sync::Arc;
use core::ops::{Deref, Range};

/// An immutable view that owns its backing allocation. Slicing never copies.
/// A small view can retain a large allocation; use [`Self::detached`] when that
/// tradeoff is undesirable. Offsets are relative to this view, never raw pointers.
#[derive(Clone)]
pub struct EncodedBytes {
    owner: Arc<[u8]>,
    range: Range<usize>,
}
impl core::fmt::Debug for EncodedBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EncodedBytes")
            .field("range", &self.range)
            .field("backing_bytes", &self.backing_bytes())
            .finish_non_exhaustive()
    }
}
impl EncodedBytes {
    /// Selects a checked range relative to this view without copying bytes.
    #[must_use]
    pub fn slice(&self, range: Range<usize>) -> Option<Self> {
        self.get(range.clone())?;
        Some(Self {
            owner: self.owner.clone(),
            range: self.range.start + range.start..self.range.start + range.end,
        })
    }
    /// Copies this view into an independent allocation, releasing its dependency
    /// on the original owner when the old view is dropped.
    #[must_use]
    pub fn detached(&self) -> Self {
        Arc::<[u8]>::from(self.as_ref()).into()
    }
    /// Bytes in the complete retained allocation, including bytes outside the view.
    #[must_use]
    pub fn backing_bytes(&self) -> usize {
        self.owner.len()
    }
    /// Whether two views retain the same immutable allocation.
    #[must_use]
    pub fn shares_backing(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
    }
}
impl From<Arc<[u8]>> for EncodedBytes {
    fn from(owner: Arc<[u8]>) -> Self {
        let end = owner.len();
        Self {
            owner,
            range: 0..end,
        }
    }
}
impl AsRef<[u8]> for EncodedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.owner[self.range.clone()]
    }
}
impl Deref for EncodedBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_keep_the_owner_and_detach_explicitly() {
        let bytes = EncodedBytes::from(Arc::<[u8]>::from(&b"abcdef"[..]));
        let view = bytes.slice(1..5).unwrap().slice(1..3).unwrap();
        assert_eq!(view.as_ref(), b"cd");
        assert!(bytes.shares_backing(&view));
        assert_eq!(view.backing_bytes(), 6);
        assert!(view.slice(Range { start: 2, end: 1 }).is_none());
        assert!(view.slice(0..3).is_none());
        let detached = view.detached();
        assert!(!detached.shares_backing(&view));
        assert_eq!(detached.backing_bytes(), 2);
        drop(bytes);
        assert_eq!(view.as_ref(), b"cd");
    }
}
