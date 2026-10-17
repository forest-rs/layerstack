// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! File format arguments: the `key=value` pairs an asset path carries after
//! `:SDF_FORMAT_ARGS:`, as `terrain.grid:SDF_FORMAT_ARGS:rows=4&seed=7`.
//!
//! They are part of a layer's identity: the same file read with different
//! arguments is a different layer, which a host reads with them.
//!
//! OpenUSD: `SdfFileFormat::FileFormatArguments`, `Sdf_SplitIdentifier` and
//! `Sdf_CreateIdentifier` (`pxr/usd/sdf/assetPathResolver.cpp`). AOUSD Core
//! §9 (asset resolution).

use alloc::{collections::BTreeMap, string::String};
use core::fmt;

/// The separator between an asset path and its format arguments.
pub const FORMAT_ARGS_DELIMITER: &str = ":SDF_FORMAT_ARGS:";

/// Format arguments: `key=value` pairs, ordered by key.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FormatArgs(BTreeMap<String, String>);

/// An asset path whose format arguments do not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatArgsError {
    /// The asset path.
    pub asset: String,
    /// The argument without `=`.
    pub argument: String,
}

impl fmt::Display for FormatArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: format argument `{}` has no `=`",
            self.asset, self.argument
        )
    }
}

impl core::error::Error for FormatArgsError {}

impl FormatArgs {
    /// No arguments.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Splits an asset path or identifier into the path and its arguments.
    /// A later duplicate key replaces an earlier one, as in OpenUSD.
    ///
    /// # Errors
    ///
    /// When an argument has no `=`.
    pub fn split(asset: &str) -> Result<(&str, Self), FormatArgsError> {
        let Some((path, encoded)) = asset.split_once(FORMAT_ARGS_DELIMITER) else {
            return Ok((asset, Self::new()));
        };
        let mut args = Self::new();
        if !encoded.is_empty() {
            for pair in encoded.split('&') {
                let (key, value) = pair.split_once('=').ok_or_else(|| FormatArgsError {
                    asset: asset.into(),
                    argument: pair.into(),
                })?;
                args.insert(key, value);
            }
        }
        Ok((path, args))
    }

    /// The path of `asset` without its arguments, whether or not they parse.
    #[must_use]
    pub fn strip(asset: &str) -> &str {
        asset
            .split_once(FORMAT_ARGS_DELIMITER)
            .map_or(asset, |(path, _)| path)
    }

    /// Sets `key` to `value`, replacing any earlier value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    /// Sets `key` to `value`.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.insert(key, value);
        self
    }

    /// Removes `key`, returning its value.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.0.remove(key)
    }

    /// Keeps only the arguments `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(&str, &str) -> bool) {
        self.0.retain(|key, value| keep(key, value));
    }

    /// Sets every argument of `other` here, replacing values for keys both
    /// hold.
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// The value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// The arguments, ordered by key.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &str)> + '_ {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many arguments there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `path` with these arguments appended, ordered by key; `path` alone
    /// when there are none.
    #[must_use]
    pub fn identifier(&self, path: &str) -> String {
        let mut identifier = String::from(path);
        for (at, (key, value)) in self.iter().enumerate() {
            identifier.push_str(if at == 0 { FORMAT_ARGS_DELIMITER } else { "&" });
            identifier.push_str(key);
            identifier.push('=');
            identifier.push_str(value);
        }
        identifier
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for FormatArgs {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{string::ToString, vec::Vec};

    #[test]
    fn arguments_split_and_join_as_openusd_writes_them() {
        let (path, args) = FormatArgs::split("a/b.grid:SDF_FORMAT_ARGS:z=1&a=2&z=3").unwrap();
        assert_eq!(path, "a/b.grid");
        assert_eq!(args.iter().collect::<Vec<_>>(), [("a", "2"), ("z", "3")]);
        assert_eq!(args.identifier(path), "a/b.grid:SDF_FORMAT_ARGS:a=2&z=3");
        assert_eq!(FormatArgs::new().identifier(path), "a/b.grid");
        let (path, args) = FormatArgs::split("a.usda:SDF_FORMAT_ARGS:").unwrap();
        assert_eq!((path, args.is_empty()), ("a.usda", true));
        let (_, args) = FormatArgs::split("a.grid:SDF_FORMAT_ARGS:k=").unwrap();
        assert_eq!(args.get("k"), Some(""));
        let error = FormatArgs::split("a.grid:SDF_FORMAT_ARGS:k").unwrap_err();
        assert_eq!(
            error.to_string(),
            "a.grid:SDF_FORMAT_ARGS:k: format argument `k` has no `=`"
        );
        assert_eq!(FormatArgs::strip("a.grid:SDF_FORMAT_ARGS:k"), "a.grid");
        let mut args = FormatArgs::new().with("a", "1").with("b", "2");
        args.extend(FormatArgs::new().with("b", "3").with("c", "4"));
        args.retain(|key, _| key != "a");
        assert_eq!(args.identifier("x"), "x:SDF_FORMAT_ARGS:b=3&c=4");
    }
}
