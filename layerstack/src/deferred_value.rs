// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored values an adapter keeps encoded until their first read.
//!
//! A format adapter may hold a large authored value — a `token[]` default, a
//! `customData` dictionary — as its encoded bytes and decode it only when it
//! is read. [`Value::Deferred`] carries such a value. Decoding is the
//! adapter's, happens at most once per source, and yields exactly the value
//! an eager read of the same bytes produces, so every query answers as it
//! would have and repeat reads cost what reading an eager value costs.
//!
//! Retained numeric arrays have their own form, [`TypedArray::Deferred`],
//! which also knows its element kind and can be retimed without decoding;
//! this one covers everything else. Both report failure as an
//! [`ArrayReadError`] and cache it.
//!
//! [`TypedArray::Deferred`]: crate::TypedArray::Deferred

use alloc::sync::Arc;
use core::fmt;

use crate::{ArrayReadError, Value};

/// Adapter-owned immutable storage for one authored value, decoded on first
/// read.
///
/// Implementations decode at most once, cache success and failure, and do no
/// file I/O; a borrowed result lives as long as the source. The decoded value
/// is what an eager read produces and is never itself
/// [`Value::Deferred`]. Sources remain usable across threads and after their
/// loader is dropped.
pub trait DeferredValueSource: fmt::Debug + Send + Sync {
    /// Decodes once and borrows the cached value or error.
    fn materialize(&self) -> Result<&Value, &ArrayReadError>;
    /// For an array, a scalar value of its element kind, known without
    /// decoding (as [`crate::TypedArray::element_kind`]); `None` for a value
    /// that is not an array, such as a dictionary.
    fn element_kind(&self) -> Option<Value>;
}

/// A value retained in encoded form until it is first read.
///
/// Clones share one source, so a value decodes once however many specs,
/// opinions or threads read it. Read it with [`Value::try_materialize`] or
/// [`Value::materialized`].
#[derive(Clone)]
pub struct DeferredValue(Arc<dyn DeferredValueSource>);

impl DeferredValue {
    /// Wraps an adapter's source.
    #[must_use]
    pub fn new(source: Arc<dyn DeferredValueSource>) -> Self {
        Self(source)
    }

    /// Decodes the value once and borrows it, or the cached failure.
    ///
    /// # Errors
    ///
    /// The source's cached decode failure, or
    /// [`ArrayReadError::RecursiveSource`] when it decoded into another
    /// deferred value.
    pub fn try_materialize(&self) -> Result<&Value, &ArrayReadError> {
        static RECURSIVE: ArrayReadError = ArrayReadError::RecursiveSource;
        match self.0.materialize()? {
            Value::Deferred(_) => Err(&RECURSIVE),
            value => Ok(value),
        }
    }

    /// For an array, a scalar value of its element kind, without decoding;
    /// `None` otherwise.
    #[must_use]
    pub fn element_kind(&self) -> Option<Value> {
        self.0.element_kind()
    }

    /// Whether both values share one source. This never decodes.
    #[must_use]
    pub fn shares_source(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for DeferredValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DeferredValue").field(&self.0).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenId;
    use alloc::vec;

    /// An already decoded value or failure.
    #[derive(Debug)]
    struct Decoded(Result<Value, ArrayReadError>);
    impl DeferredValueSource for Decoded {
        fn materialize(&self) -> Result<&Value, &ArrayReadError> {
            self.0.as_ref()
        }
        fn element_kind(&self) -> Option<Value> {
            Some(Value::Token(TokenId::EMPTY))
        }
    }

    fn deferred(value: Result<Value, ArrayReadError>) -> Value {
        Value::Deferred(DeferredValue::new(Arc::new(Decoded(value))))
    }

    #[test]
    fn deferred_values_read_as_their_decoded_value() {
        let tokens = Value::Array(vec![Value::Token(TokenId::EMPTY); 3]);
        let value = deferred(Ok(tokens.clone()));
        assert_eq!(value, tokens);
        assert_eq!(tokens, value);
        assert!(value.same_representation(&tokens));
        assert_eq!(value.array_ref().map(crate::ArrayRef::len), Some(3));
        assert!(core::ptr::eq(
            value.materialized(),
            value.try_materialize().unwrap()
        ));
        assert_eq!(alloc::format!("{value}"), alloc::format!("{tokens}"));
    }

    #[test]
    fn failures_are_cached_errors_not_values() {
        let error = ArrayReadError::InvalidData("broken token array".into());
        let value = deferred(Err(error.clone()));
        assert_eq!(value.try_materialize(), Err(&error));
        assert!(matches!(value.materialized(), Value::Deferred(_)));
        assert_ne!(value, value.clone(), "a failed value equals nothing");
        assert!(value.array_ref().is_none());
        let recursive = deferred(Ok(value));
        assert_eq!(
            recursive.try_materialize(),
            Err(&ArrayReadError::RecursiveSource)
        );
    }
}
