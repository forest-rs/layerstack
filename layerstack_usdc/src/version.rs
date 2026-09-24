// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Crate file format versions and the features they introduce.
//!
//! A USDC header carries a `major.minor.patch` format version (AOUSD Core
//! §16.3.2). This reader accepts files from [`CrateVersion::OLDEST_READABLE`]
//! through [`CrateVersion::NEWEST_READABLE`] and rejects everything else with
//! [`UsdcError::UnsupportedVersion`]. Within that range, a value that needs a
//! newer version than the file declares is rejected with
//! [`UsdcError::FeatureRequiresVersion`].
//!
//! The version history below is OpenUSD's (`pxr/usd/sdf/crateFile.cpp:384`,
//! v26.08). Versions after 0.12 postdate AOUSD Core 1.0.1; the reader follows
//! the OpenUSD v26.08 encoding for them.
//!
//! | Version | Adds | Reader support |
//! |---------|------|----------------|
//! | 0.7  | 64-bit array sizes | oldest readable version |
//! | 0.8  | payload list ops, payload layer offsets | yes |
//! | 0.9  | `timecode` and `timecode[]` values | yes |
//! | 0.10 | `pathExpression` values | yes |
//! | 0.11 | relocates in layer metadata | yes |
//! | 0.12 | splines (Ts binary format 1) | yes |
//! | 0.13 | spline tangent algorithms (Ts binary format 2) | yes; the algorithms are validated and dropped, and the tangents OpenUSD stored with them are kept |
//! | 0.14 | native array edits (`VtArrayEdit`) | no: [`UsdcError::UnsupportedVersion`] |
//! | 0.15 | spline `loopBoundaryTime` and `GfTimeCode`-valued splines (Ts binary format 3) | no: [`UsdcError::UnsupportedVersion`] |
//!
//! [`UsdcError::UnsupportedVersion`]: crate::UsdcError::UnsupportedVersion
//! [`UsdcError::FeatureRequiresVersion`]: crate::UsdcError::FeatureRequiresVersion

use core::fmt;

/// A crate file format version, `major.minor.patch`.
///
/// Versions order lexicographically by component.
///
/// Spec: AOUSD Core §16.3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CrateVersion {
    /// Major version. Every readable version has major version 0.
    pub major: u8,
    /// Minor version. Minor versions add encodings; see the module docs.
    pub minor: u8,
    /// Patch version. Patch-level changes are forward compatible.
    pub patch: u8,
}

impl CrateVersion {
    /// The oldest version this reader accepts, 0.7.0.
    pub const OLDEST_READABLE: Self = Self::new(0, 7, 0);
    /// The newest version this reader accepts, 0.13 (any patch level).
    pub const NEWEST_READABLE: Self = Self::new(0, 13, 0);

    /// Version 0.12.0, which introduced splines.
    pub const SPLINES: Self = Self::new(0, 12, 0);
    /// Version 0.13.0, which introduced spline tangent algorithms.
    pub const SPLINE_TANGENT_ALGORITHMS: Self = Self::new(0, 13, 0);
    /// Version 0.14.0, which introduced native array edits.
    pub const ARRAY_EDITS: Self = Self::new(0, 14, 0);
    /// Version 0.15.0, which introduced spline `loopBoundaryTime` and
    /// `GfTimeCode`-valued splines.
    pub const SPLINE_LOOP_BOUNDARY_AND_TIMECODE: Self = Self::new(0, 15, 0);

    /// Creates a version from its components.
    #[must_use]
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Whether this reader can read a file with this version.
    ///
    /// Following OpenUSD (`SdfFileVersion::CanRead`,
    /// `pxr/usd/sdf/fileVersion.h:76`), the patch level is not compared:
    /// patch-level changes are forward compatible.
    ///
    /// ```
    /// use layerstack_usdc::version::CrateVersion;
    ///
    /// assert!(CrateVersion::new(0, 13, 0).is_readable());
    /// assert!(CrateVersion::new(0, 13, 1).is_readable());
    /// assert!(!CrateVersion::new(0, 14, 0).is_readable());
    /// assert!(!CrateVersion::new(0, 6, 0).is_readable());
    /// assert!(!CrateVersion::new(1, 0, 0).is_readable());
    /// ```
    #[must_use]
    pub const fn is_readable(self) -> bool {
        self.major == Self::NEWEST_READABLE.major
            && self.minor >= Self::OLDEST_READABLE.minor
            && self.minor <= Self::NEWEST_READABLE.minor
    }

    /// Whether a file of this version may contain a feature introduced in
    /// `introduced`.
    #[must_use]
    pub fn has(self, introduced: Self) -> bool {
        self >= introduced
    }
}

impl fmt::Display for CrateVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
