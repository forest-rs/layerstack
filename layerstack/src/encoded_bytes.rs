// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Immutable encoded storage shared safely through checked byte ranges.
use alloc::{sync::Arc, vec::Vec};
use core::ops::{Deref, Range};

#[derive(Clone)]
enum Owner {
    Slice(Arc<[u8]>),
    Vector(Arc<Vec<u8>>),
}
impl Owner {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Slice(bytes) => bytes,
            Self::Vector(bytes) => bytes,
        }
    }
    fn capacity(&self) -> usize {
        match self {
            Self::Slice(bytes) => bytes.len(),
            Self::Vector(bytes) => bytes.capacity(),
        }
    }
}

/// An immutable view that owns its backing allocation. Slicing never copies.
/// A small view can retain a large allocation; use [`Self::detached`] when that
/// tradeoff is undesirable. Offsets are relative to this view, never raw pointers.
/// Moving a `Vec<u8>` into this type retains its payload allocation and capacity;
/// an `Arc<[u8]>` or `Arc<Vec<u8>>` is shared as-is. No owner exposes mutable bytes.
#[derive(Clone)]
pub struct EncodedBytes {
    owner: Owner,
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
    /// Payload capacity retained by the complete owner, including bytes outside
    /// this view and spare vector capacity. Shared-owner control blocks are excluded.
    #[must_use]
    pub fn backing_bytes(&self) -> usize {
        self.owner.capacity()
    }
    /// Whether two views retain the same immutable allocation.
    #[must_use]
    pub fn shares_backing(&self, other: &Self) -> bool {
        match (&self.owner, &other.owner) {
            (Owner::Slice(a), Owner::Slice(b)) => Arc::ptr_eq(a, b),
            (Owner::Vector(a), Owner::Vector(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl From<Arc<[u8]>> for EncodedBytes {
    fn from(owner: Arc<[u8]>) -> Self {
        let end = owner.len();
        Self {
            owner: Owner::Slice(owner),
            range: 0..end,
        }
    }
}
/// Transfers the vector's existing payload allocation. Only the small shared
/// owner is allocated; payload bytes and spare capacity are not copied.
impl From<Vec<u8>> for EncodedBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Arc::new(bytes).into()
    }
}
/// Shares a vector owner without copying its payload or allocating another owner.
impl From<Arc<Vec<u8>>> for EncodedBytes {
    fn from(owner: Arc<Vec<u8>>) -> Self {
        let end = owner.len();
        Self {
            owner: Owner::Vector(owner),
            range: 0..end,
        }
    }
}
impl AsRef<[u8]> for EncodedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.owner.bytes()[self.range.clone()]
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
    #[test]
    fn vector_handoff_retains_allocation_capacity_and_snapshot_lifetimes() {
        let mut input = Vec::with_capacity(1024);
        input.extend_from_slice(b"abcdef");
        let pointer = input.as_ptr();
        let bytes = EncodedBytes::from(input);
        assert_eq!(
            bytes.as_ptr(),
            pointer,
            "handoff must preserve the payload allocation"
        );
        assert_eq!(
            bytes.backing_bytes(),
            1024,
            "spare vector capacity remains accounted"
        );
        let view = bytes.slice(1..5).unwrap().slice(1..3).unwrap();
        assert_eq!(
            view.as_ptr(),
            pointer.wrapping_add(2),
            "nested views retain original allocation offsets"
        );
        assert!(view.shares_backing(&bytes), "views share a vector owner");
        let detached = view.detached();
        assert_eq!(
            detached.as_ref(),
            b"cd",
            "detachment preserves only the selected bytes"
        );
        assert_eq!(
            detached.backing_bytes(),
            2,
            "detachment releases spare backing capacity"
        );
        assert!(
            !detached.shares_backing(&view),
            "detachment creates a different owner"
        );
        drop(bytes);
        assert_eq!(view.as_ref(), b"cd", "view outlives the original wrapper");
    }
    #[test]
    fn shared_vector_handoff_prevents_mutation_while_views_exist() {
        let mut input = Arc::new(alloc::vec![1, 2, 3]);
        let bytes = EncodedBytes::from(input.clone());
        assert_eq!(
            bytes.as_ptr(),
            input.as_ptr(),
            "shared handoff preserves allocation"
        );
        assert!(
            Arc::get_mut(&mut input).is_none(),
            "safe mutation requires all encoded holders to be gone"
        );
        drop(bytes);
        assert!(
            Arc::get_mut(&mut input).is_some(),
            "exclusive ownership returns after the last view drops"
        );
        assert!(
            EncodedBytes::from(Vec::new()).slice(0..0).is_some(),
            "empty owned buffers support empty checked views"
        );
    }
}
