// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! UDIM texture tile patterns: one asset path naming a set of tile files.
//!
//! A texture authored as `textures/color.<UDIM>.png` names one file per
//! tile of the UDIM grid, `textures/color.1001.png` through
//! `textures/color.1100.png`; only some of them need exist. This module is
//! the spelling alone: recognising the pattern, writing a tile's path and
//! reading a tile number back. Finding which tiles exist is transport work
//! for the host's storage (see `layerstack_io`'s `StageDocument::resolve_udim_tiles`).
//!
//! The AOUSD Core specification does not define UDIM patterns; asset paths
//! carrying one resolve tile by tile as any other asset path (§9.4). The
//! spelling and range follow OpenUSD's `UsdShadeUdimUtils`
//! (`pxr/usd/usdShade/udimUtils.cpp`).

use alloc::string::String;
use core::fmt;
use core::ops::RangeInclusive;

/// One tile of the UDIM grid: 1001 through 1100, ten columns by ten rows.
///
/// OpenUSD probes exactly this range (`UDIM_START_TILE`, `UDIM_END_TILE`),
/// so a file spelled with a tile outside it, such as `color.1101.png`, is
/// not part of a pattern's set. [`fmt::Display`] writes the four digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UdimTile(u16);

impl UdimTile {
    /// The first tile, 1001.
    pub const FIRST: Self = Self(1001);
    /// The last tile, 1100.
    pub const LAST: Self = Self(1100);
    /// The tile numbers a pattern may name, in the order OpenUSD probes them.
    pub const RANGE: RangeInclusive<u16> = Self::FIRST.0..=Self::LAST.0;

    /// The tile `number`, or `None` outside [`Self::RANGE`].
    #[must_use]
    pub const fn new(number: u16) -> Option<Self> {
        if number >= Self::FIRST.0 && number <= Self::LAST.0 {
            Some(Self(number))
        } else {
            None
        }
    }

    /// The tile number, such as 1011.
    #[must_use]
    pub const fn number(self) -> u16 {
        self.0
    }

    /// Every tile in ascending order, the order OpenUSD returns them in.
    pub fn all() -> impl DoubleEndedIterator<Item = Self> + ExactSizeIterator {
        Self::RANGE.map(Self)
    }
}

impl fmt::Display for UdimTile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An asset path containing the `<UDIM>` marker, split around it.
///
/// [`Self::parse`] accepts what OpenUSD's
/// `UsdShadeUdimUtils::IsUdimIdentifier` accepts: the first `<UDIM>` (case
/// sensitive) is the marker, and the path must have text before or after
/// it, so a bare `<UDIM>` is not a pattern. A later `<UDIM>` is kept as
/// text in the suffix, as `ReplaceUdimPattern` keeps it.
///
/// The pattern is pure spelling: it does not anchor, normalize or resolve.
/// A variable expression containing `<UDIM>` parses too, as it does in
/// OpenUSD; evaluate it before treating it as a path.
///
/// ```
/// use layerstack::asset::{UdimPattern, UdimTile};
///
/// let pattern = UdimPattern::parse("textures/color.<UDIM>.png").unwrap();
/// let tile = UdimTile::new(1011).unwrap();
/// assert_eq!(pattern.tile_path(tile), "textures/color.1011.png");
/// assert_eq!(pattern.tile_of("textures/color.1011.png"), Some(tile));
/// assert_eq!(pattern.tile_of("textures/color.1101.png"), None);
/// assert!(UdimPattern::parse("<UDIM>").is_none());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UdimPattern<'a> {
    path: &'a str,
    marker: usize,
}

impl<'a> UdimPattern<'a> {
    /// The marker a tile number replaces.
    pub const MARKER: &'static str = "<UDIM>";

    /// Splits `path` around its first `<UDIM>`, or `None` when it has none
    /// or nothing but the marker (`UsdShadeUdimUtils::IsUdimIdentifier`).
    #[must_use]
    pub fn parse(path: &'a str) -> Option<Self> {
        let marker = path.find(Self::MARKER)?;
        (path.len() > Self::MARKER.len()).then_some(Self { path, marker })
    }

    /// The whole pattern, as parsed.
    #[must_use]
    pub fn as_str(&self) -> &'a str {
        self.path
    }

    /// The text before the marker.
    #[must_use]
    pub fn prefix(&self) -> &'a str {
        &self.path[..self.marker]
    }

    /// The text after the marker.
    #[must_use]
    pub fn suffix(&self) -> &'a str {
        &self.path[self.marker + Self::MARKER.len()..]
    }

    /// The path of one tile: the marker replaced by its number
    /// (`UsdShadeUdimUtils::ReplaceUdimPattern`).
    #[must_use]
    pub fn tile_path(&self, tile: UdimTile) -> String {
        self.replace(&alloc::format!("{tile}"))
    }

    /// The pattern with the marker replaced by `text`, such as another
    /// pattern's marker or a host's own tile token.
    #[must_use]
    pub fn replace(&self, text: &str) -> String {
        let mut path = String::with_capacity(self.path.len() - Self::MARKER.len() + text.len());
        path.push_str(self.prefix());
        path.push_str(text);
        path.push_str(self.suffix());
        path
    }

    /// The tile `path` spells, when it is this pattern with four digits of
    /// a tile in [`UdimTile::RANGE`] in place of the marker.
    #[must_use]
    pub fn tile_of(&self, path: &str) -> Option<UdimTile> {
        let digits = path
            .strip_prefix(self.prefix())?
            .strip_suffix(self.suffix())?;
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        UdimTile::new(digits.parse().ok()?)
    }
}

impl fmt::Display for UdimPattern<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_parse_as_openusd_identifies_them() {
        // `UsdShadeUdimUtils::IsUdimIdentifier` and `ReplaceUdimPattern`
        // in OpenUSD 26.08, as `layerstack_conformance`'s UDIM oracle records.
        for (path, replaced) in [
            ("textures/color.<UDIM>.png", Some("textures/color.1011.png")),
            ("a<UDIM>", Some("a1011")),
            ("<UDIM>b", Some("1011b")),
            ("x.<UDIM>.<UDIM>.png", Some("x.1011.<UDIM>.png")),
            ("./tiles/<UDIM>/color.png", Some("./tiles/1011/color.png")),
            ("<UDIM>", None),
            ("<udim>.png", None),
            ("color.png", None),
            ("", None),
        ] {
            let pattern = UdimPattern::parse(path);
            assert_eq!(
                pattern.map(|p| p.tile_path(UdimTile::new(1011).unwrap())),
                replaced.map(String::from),
                "{path}"
            );
            if let Some(pattern) = pattern {
                assert_eq!(pattern.as_str(), path);
                assert_eq!(pattern.replace(UdimPattern::MARKER), path);
            }
        }
    }

    #[test]
    fn tiles_cover_the_range_openusd_probes() {
        assert_eq!(UdimTile::all().len(), 100);
        assert_eq!(UdimTile::all().next(), Some(UdimTile::FIRST));
        assert_eq!(UdimTile::all().next_back(), Some(UdimTile::LAST));
        assert_eq!(UdimTile::new(1000), None);
        assert_eq!(UdimTile::new(1101), None);
        assert_eq!(UdimTile::new(1050).map(UdimTile::number), Some(1050));
        assert_eq!(alloc::format!("{}", UdimTile::FIRST), "1001");
    }

    #[test]
    fn tile_numbers_read_back_only_from_the_pattern_spelling() {
        let pattern = UdimPattern::parse("t/c.<UDIM>.png").unwrap();
        assert_eq!(pattern.prefix(), "t/c.");
        assert_eq!(pattern.suffix(), ".png");
        assert_eq!(pattern.tile_of("t/c.1100.png"), UdimTile::new(1100));
        for other in [
            "t/c.1000.png",
            "t/c.1101.png",
            "t/c.101.png",
            "t/c.01001.png",
            "t/c.+101.png",
            "t/d.1001.png",
            "t/c.1001.jpg",
            "t/c.png",
        ] {
            assert_eq!(pattern.tile_of(other), None, "{other}");
        }
        // The prefix and suffix may not overlap inside a short path.
        let pattern = UdimPattern::parse("aa<UDIM>aa").unwrap();
        assert_eq!(pattern.tile_of("aaa"), None);
        assert_eq!(pattern.tile_of("aa1001aa"), UdimTile::new(1001));
    }
}
