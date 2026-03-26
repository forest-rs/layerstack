//! Property type metadata carried alongside authored field opinions.
//!
//! This metadata preserves the declared USD property type in a form the
//! composition kernel can use without access to a token interner. Sparse array
//! edits rely on the typed default scalar value to synthesize appended elements
//! for `minsize` / `resize`.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::doc::Value;

/// Declared type information for an authored property.
///
/// `type_name` preserves the authored USD type name. `default_scalar` is the
/// zero/default value for the non-array form of the property type.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyType {
    /// Authored USD type name, such as `int`, `point3f`, or `token`.
    pub type_name: Arc<str>,
    /// Whether the declared property is array-valued.
    pub is_array: bool,
    /// Default scalar value for one element of this type.
    pub default_scalar: Value,
}

impl PropertyType {
    /// Creates property type metadata from an authored USD type name.
    #[must_use]
    pub fn new(type_name: impl Into<Arc<str>>, is_array: bool, default_scalar: Value) -> Self {
        Self {
            type_name: type_name.into(),
            is_array,
            default_scalar,
        }
    }

    /// Returns the declared default value for the whole property.
    #[must_use]
    pub fn default_property_value(&self) -> Value {
        if self.is_array {
            Value::Array(Vec::default())
        } else {
            self.default_scalar.clone()
        }
    }

    /// Returns the default scalar element for an array property.
    #[must_use]
    pub fn default_array_element(&self) -> Option<Value> {
        self.is_array.then(|| self.default_scalar.clone())
    }
}
