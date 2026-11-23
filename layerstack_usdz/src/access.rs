// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package selection shared by complete snapshots and explicit range readers.
use crate::{
    UsdzError,
    range::{RangeError, RangePackage, RangeSource},
    snapshot::{PackageMember, PackageSnapshot},
};
use alloc::{string::String, vec::Vec};
use core::fmt::Display;
use layerstack::asset::PackagePath;

/// Package names leading to a physical member, from outermost to innermost.
pub(crate) fn containers(member: &str) -> Option<Vec<String>> {
    let path = PackagePath::parse(member, 32).ok()?;
    let Some((_, parents)) = path.members().split_last() else {
        return Some(Vec::new());
    };
    let mut names = alloc::vec![String::from(path.outer())];
    names.extend_from_slice(parents);
    Some(names)
}

pub(crate) trait PackageAccess {
    type Error: From<UsdzError> + Display;
    fn root_member(&self) -> Option<&str>;
    fn layer(&mut self, path: &str) -> Result<PackageMember, Self::Error>;
    fn search_root(&mut self, member: &str) -> Result<Option<String>, Self::Error>;
    fn is_missing(error: &Self::Error) -> bool;
}
impl PackageAccess for &PackageSnapshot {
    type Error = UsdzError;
    fn root_member(&self) -> Option<&str> {
        PackageSnapshot::root_member(self)
    }
    fn layer(&mut self, path: &str) -> Result<PackageMember, Self::Error> {
        PackageSnapshot::layer(self, path)
    }
    fn search_root(&mut self, member: &str) -> Result<Option<String>, Self::Error> {
        Ok(PackageSnapshot::containing_root(self, member))
    }
    fn is_missing(error: &Self::Error) -> bool {
        matches!(error, UsdzError::MissingMember { .. })
    }
}
impl<S: RangeSource> PackageAccess for RangePackage<S>
where
    S::Error: Display,
{
    type Error = RangeError<S::Error>;
    fn root_member(&self) -> Option<&str> {
        self.root_member()
    }
    fn layer(&mut self, path: &str) -> Result<PackageMember, Self::Error> {
        self.layer(path)
    }
    fn search_root(&mut self, member: &str) -> Result<Option<String>, Self::Error> {
        Self::search_root(self, member)
    }
    fn is_missing(error: &Self::Error) -> bool {
        matches!(error, RangeError::Format(UsdzError::MissingMember { .. }))
    }
}
