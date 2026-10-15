// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package selection shared by complete snapshots and explicit range readers.
use crate::{
    UsdzError,
    range::{RangeError, RangePackage, RangeSource},
    snapshot::{PackageMember, PackageSnapshot},
};
use alloc::string::String;
use core::fmt::Display;

pub(crate) trait PackageAccess {
    type Error: From<UsdzError> + Display;
    fn root_member(&self) -> Option<&str>;
    fn layer(&mut self, path: &str) -> Result<PackageMember, Self::Error>;
    fn containing_root(&self, member: &str) -> Option<String>;
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
    fn containing_root(&self, member: &str) -> Option<String> {
        PackageSnapshot::containing_root(self, member)
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
    fn containing_root(&self, member: &str) -> Option<String> {
        self.containing_root(member)
    }
    fn is_missing(error: &Self::Error) -> bool {
        matches!(error, RangeError::Format(UsdzError::MissingMember { .. }))
    }
}
