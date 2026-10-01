// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared authored sequences with explicit copy-on-write mutation.

use alloc::{sync::Arc, vec::Vec};
use core::{fmt, ops::Deref, slice};

/// An authored sequence whose clones share immutable storage.
///
/// New empty sequences allocate nothing. Reading borrows a slice; [`Self::make_mut`]
/// detaches a shared buffer before editing it and retains vector capacity when
/// uniquely owned. Property samples and metadata use this to keep snapshot
/// copies and default-only edits independent of untouched sequence sizes.
pub struct SharedVec<T>(Option<Arc<Vec<T>>>);

impl<T> SharedVec<T> {
    /// Borrows the sequence as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// Creates an empty sequence without allocating.
    #[must_use]
    pub const fn new() -> Self {
        Self(None)
    }
}

impl<T: Clone> SharedVec<T> {
    /// Returns a mutable vector, copying elements only when storage is shared.
    pub fn make_mut(&mut self) -> &mut Vec<T> {
        Arc::make_mut(self.0.get_or_insert_with(|| Arc::new(Vec::new())))
    }

    /// Takes the vector, copying elements only when storage is shared.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.0.map_or_else(Vec::new, Arc::unwrap_or_clone)
    }
}

impl<T> Default for SharedVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for SharedVec<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> From<Vec<T>> for SharedVec<T> {
    fn from(value: Vec<T>) -> Self {
        Self((!value.is_empty()).then(|| Arc::new(value)))
    }
}

impl<T> FromIterator<T> for SharedVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}

impl<T> Deref for SharedVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }
}

impl<T: fmt::Debug> fmt::Debug for SharedVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<T: PartialEq> PartialEq for SharedVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl<T: Eq> Eq for SharedVec<T> {}

impl<'a, T> IntoIterator for &'a SharedVec<T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T: Clone> IntoIterator for SharedVec<T> {
    type Item = T;
    type IntoIter = alloc::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::SharedVec;
    use alloc::vec;

    #[test]
    fn clones_share_until_explicit_mutation() {
        let first: SharedVec<_> = vec![1, 2, 3].into();
        let mut second = first.clone();
        assert_eq!(first.as_ptr(), second.as_ptr());
        second.make_mut().push(4);
        assert_ne!(first.as_ptr(), second.as_ptr());
        assert_eq!(&*first, &[1, 2, 3]);
        assert_eq!(&*second, &[1, 2, 3, 4]);
    }

    #[test]
    fn unique_storage_retains_capacity_and_empty_sequences_compare_equal() {
        let mut buffer = vec![1, 2];
        buffer.reserve(16);
        let pointer = buffer.as_ptr();
        let capacity = buffer.capacity();
        let mut shared: SharedVec<_> = buffer.into();
        assert_eq!(shared.make_mut().as_ptr(), pointer);
        assert_eq!(shared.make_mut().capacity(), capacity);
        shared.make_mut().clear();
        assert_eq!(shared, SharedVec::new());
    }
}
