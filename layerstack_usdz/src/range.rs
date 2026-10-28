// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit range-based package inspection and selected-member verification.
//!
//! Directory opening reads no member payloads. Each selected member's local
//! header, bounds and CRC are checked before its bytes are returned. Unselected
//! members and enclosing nested-package payload CRCs remain unverified; use
//! [`crate::snapshot::PackageSnapshot`] when complete verification is required.
//! AOUSD Core §9.7, §16.4; ZIP APPNOTE central/local headers and stored members.
use crate::{
    UsdzError,
    snapshot::PackageMember,
    zip::{read_u16 as u16_at, read_u32 as u32_at},
};
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{fmt, ops::Range};
use layerstack::{asset::PackagePath, encoded_bytes::EncodedBytes};

/// One fixed logical byte source. Opening a source and identity/version policy
/// belong to the host. Reads return independently owned immutable byte views.
///
/// The host must keep content stable for this source's lifetime or return errors
/// on change. A source opened before atomic replacement should retain the old
/// content. Checksums do not establish a stable source version by themselves.
pub trait RangeSource {
    /// Host transport failure.
    type Error;
    /// Fixed byte length captured when this source was opened.
    fn byte_len(&self) -> u64;
    /// Returns exactly `range.end - range.start` bytes, or an error.
    /// Readers check ranges and budgets before calling this method.
    fn read_range(&self, range: Range<u64>) -> Result<EncodedBytes, Self::Error>;
}
impl RangeSource for EncodedBytes {
    type Error = UsdzError;
    fn byte_len(&self) -> u64 {
        self.len() as u64
    }
    fn read_range(&self, range: Range<u64>) -> Result<EncodedBytes, Self::Error> {
        let start = usize::try_from(range.start).map_err(|_| UsdzError::UnexpectedEof)?;
        let end = usize::try_from(range.end).map_err(|_| UsdzError::UnexpectedEof)?;
        self.slice(start..end).ok_or(UsdzError::UnexpectedEof)
    }
}
impl<T: RangeSource + ?Sized> RangeSource for Arc<T> {
    type Error = T::Error;
    fn byte_len(&self) -> u64 {
        (**self).byte_len()
    }
    fn read_range(&self, range: Range<u64>) -> Result<EncodedBytes, Self::Error> {
        (**self).read_range(range)
    }
}

/// Range inspection budgets, checked before transport or payload allocation.
///
/// Build it as a struct literal that names the fields it sets and takes
/// the rest from `..RangeLimits::default()`: a field a later release adds then
/// keeps its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeLimits {
    /// Maximum nested archive levels, at most 31.
    pub depth: usize,
    /// Maximum cumulative directory entries across opened archives.
    pub entries: usize,
    /// Maximum cumulative encoded central-directory bytes inspected by this reader.
    pub directory_bytes: u64,
    /// Maximum bytes in one selected member.
    pub member_bytes: u64,
    /// Maximum cumulative selected-member bytes verified, including repeated reads.
    pub verification_bytes: u64,
}
impl Default for RangeLimits {
    fn default() -> Self {
        Self {
            depth: 31,
            entries: 1_000_000,
            directory_bytes: 64 * 1024 * 1024,
            member_bytes: 1024 * 1024 * 1024,
            verification_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}
/// Source requests and actual successful byte delivery, excluding host-internal
/// copies/retries. Verification covers selected payloads, never the entire archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RangeStats {
    /// Number of source read attempts, including failed requests.
    pub read_attempts: usize,
    /// Bytes returned by successful source requests.
    pub bytes_read: u64,
    /// Selected payload bytes inspected by CRC, including failed CRC checks.
    pub verification_bytes: u64,
    /// Central directories successfully opened, including nested archives.
    pub index_parses: usize,
    /// Cumulative indexed entries.
    pub entries: usize,
    /// Central-directory encoded bytes inspected during index construction.
    pub directory_bytes: u64,
}
/// A package-format or host-transport failure, with its original typed cause.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RangeError<E> {
    /// Original host source failure.
    Source(E),
    /// ZIP layout, budget, member selection or checksum failure.
    Format(UsdzError),
}
impl<E> From<UsdzError> for RangeError<E> {
    fn from(error: UsdzError) -> Self {
        Self::Format(error)
    }
}
impl<E: fmt::Display> fmt::Display for RangeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(f, "package transport: {error}"),
            Self::Format(error) => error.fmt(f),
        }
    }
}
impl<E: core::error::Error + 'static> core::error::Error for RangeError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Format(error) => Some(error),
        }
    }
}
#[derive(Clone, Debug)]
struct Record {
    name: Arc<str>,
    header: u64,
    boundary: u64,
    size: u64,
    crc: u32,
    flags: u16,
}
#[derive(Clone, Debug)]
struct Index {
    start: u64,
    size: u64,
    records: BTreeMap<Arc<str>, Record>,
    root: Option<Arc<str>>,
}
/// Caller-owned range reader with shared immutable directory indexes and explicit
/// mutable work counters. Cloneable sources permit cheap index-sharing snapshots;
/// clones have independent work histories and local-header caches.
#[derive(Clone, Debug)]
pub struct RangePackage<S> {
    source: S,
    limits: RangeLimits,
    indexes: BTreeMap<String, Arc<Index>>,
    headers: BTreeMap<String, u64>,
    stats: RangeStats,
}
fn invalid(reason: &'static str) -> UsdzError {
    UsdzError::InvalidZip { reason }
}
fn constraint(reason: &'static str) -> UsdzError {
    UsdzError::ConstraintViolation { reason }
}
fn add(a: u64, b: u64) -> Result<u64, UsdzError> {
    a.checked_add(b).ok_or(UsdzError::UnexpectedEof)
}

impl<S: RangeSource> RangePackage<S> {
    /// Reads the outer central directory under explicit limits. Member payloads
    /// and local headers are untouched until selection. Source content must remain
    /// stable as required by [`RangeSource`]. AOUSD Core §9.7, §16.4.
    ///
    /// # Errors
    /// Returns typed source failures or invalid ZIP/directory/budget errors.
    pub fn open(source: S, limits: RangeLimits) -> Result<Self, RangeError<S::Error>> {
        if limits.depth > 31 {
            return Err(
                constraint("package depth limit exceeds supported identifier depth").into(),
            );
        }
        let size = source.byte_len();
        let mut reader = Self {
            source,
            limits,
            indexes: BTreeMap::new(),
            headers: BTreeMap::new(),
            stats: RangeStats::default(),
        };
        let index = reader.index(0, size)?;
        reader.indexes.insert(String::new(), Arc::new(index));
        Ok(reader)
    }
    /// Work performed by this reader. Does not imply whole-package verification.
    #[must_use]
    pub fn stats(&self) -> RangeStats {
        self.stats
    }
    /// Default outer layer, selected by the first physical member's local-header
    /// offset. A later layer never substitutes for a non-layer first member.
    #[must_use]
    pub fn root_member(&self) -> Option<&str> {
        self.indexes[""].root.as_deref()
    }
    /// Outer member names in lexical order. Performs no additional source reads.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.indexes[""].records.keys().map(AsRef::as_ref)
    }
    /// Selects and verifies an arbitrary member, supporting nested bracket paths.
    /// Local header metadata is checked once per reader; payloads are read and
    /// CRC-verified on every call. Returned bytes outlive this reader.
    ///
    /// # Errors
    /// Reports missing/invalid members, transport failures, budgets or bad CRCs.
    pub fn member(&mut self, path: &str) -> Result<PackageMember, RangeError<S::Error>> {
        self.select(path, false)
    }
    /// As [`Self::member`], selecting a nested package's default layer when the
    /// final member is itself a package. Verifies only the returned layer payload.
    ///
    /// # Errors
    /// As [`Self::member`], plus [`UsdzError::NoRootLayer`].
    pub fn layer(&mut self, path: &str) -> Result<PackageMember, RangeError<S::Error>> {
        self.select(path, true)
    }
    /// The containing archive's default-layer path after directories for `member`
    /// have been opened. This inspection performs no additional source reads.
    #[must_use]
    pub fn containing_root(&self, member: &str) -> Option<String> {
        let parsed = PackagePath::parse(member, 32).ok()?;
        let mut names = alloc::vec![parsed.outer().to_string()];
        names.extend_from_slice(parsed.members());
        let prefix = if names.len() == 1 {
            String::new()
        } else {
            PackagePath::join(&names[0], &names[1..names.len() - 1])
        };
        let root = self.indexes.get(&prefix)?.root.as_deref()?;
        if prefix.is_empty() {
            Some(root.into())
        } else {
            Some(alloc::format!("{prefix}[{root}]"))
        }
    }
    fn read(&mut self, start: u64, size: u64) -> Result<EncodedBytes, RangeError<S::Error>> {
        let end = add(start, size)?;
        if end > self.source.byte_len() {
            return Err(UsdzError::UnexpectedEof.into());
        }
        self.stats.read_attempts += 1;
        let bytes = self
            .source
            .read_range(start..end)
            .map_err(RangeError::Source)?;
        self.stats.bytes_read = add(self.stats.bytes_read, bytes.len() as u64)?;
        if bytes.len() as u64 != size {
            return Err(UsdzError::UnexpectedEof.into());
        }
        Ok(bytes)
    }
    fn index(&mut self, start: u64, size: u64) -> Result<Index, RangeError<S::Error>> {
        if size < 22 {
            return Err(invalid("file too small for EOCD").into());
        }
        let end = add(start, size)?;
        let eocd = self.read(end - 22, 22)?;
        if u32_at(&eocd, 0)? != 0x0605_4b50 {
            return Err(invalid("EOCD must end the USDZ source").into());
        }
        if u16_at(&eocd, 20)? != 0 {
            return Err(constraint("EOCD comment must be empty in USDZ").into());
        }
        if u16_at(&eocd, 4)? != 0 || u16_at(&eocd, 6)? != 0 {
            return Err(constraint("split ZIP archives are unsupported").into());
        }
        let count = usize::from(u16_at(&eocd, 10)?);
        if count == usize::from(u16::MAX) || u16_at(&eocd, 8)? as usize != count {
            return Err(constraint("split or Zip64 entry counts are unsupported").into());
        }
        if count > self.limits.entries.saturating_sub(self.stats.entries) {
            return Err(constraint("package entry budget exceeded").into());
        }
        let cd_size = u64::from(u32_at(&eocd, 12)?);
        let cd_start = u64::from(u32_at(&eocd, 16)?);
        if cd_size == u64::from(u32::MAX) || cd_start == u64::from(u32::MAX) {
            return Err(constraint("Zip64 directories are unsupported").into());
        }
        if add(cd_start, cd_size)? != size - 22 {
            return Err(invalid("central directory extent does not match EOCD").into());
        }
        if cd_size
            > self
                .limits
                .directory_bytes
                .saturating_sub(self.stats.directory_bytes)
        {
            return Err(constraint("package directory byte budget exceeded").into());
        }
        if cd_size < count as u64 * 46 {
            return Err(invalid("central directory is too short for entry count").into());
        }
        let directory = self.read(add(start, cd_start)?, cd_size)?;
        self.stats.directory_bytes += cd_size;
        let mut records = BTreeMap::new();
        let mut order = Vec::with_capacity(count);
        let mut pos = 0_usize;
        for _ in 0..count {
            let metadata =
                crate::zip::central_entry(directory.get(pos..).ok_or(UsdzError::UnexpectedEof)?)?;
            let name = metadata.name;
            let header = metadata.header as u64;
            let parsed =
                PackagePath::parse(&alloc::format!("package[{name}]"), 1).map_err(constraint)?;
            if parsed.members()[0] != name {
                return Err(constraint("noncanonical package member name").into());
            }
            let name: Arc<str> = name.into();
            if add(header, 30)? > cd_start {
                return Err(invalid("local header overlaps central directory").into());
            }
            let record = Record {
                name: name.clone(),
                header,
                boundary: cd_start,
                size: metadata.size as u64,
                crc: metadata.crc,
                flags: metadata.flags,
            };
            if records.insert(name.clone(), record).is_some() {
                return Err(constraint("duplicate package member name").into());
            }
            order.push((header, name));
            pos = pos
                .checked_add(metadata.next_offset)
                .ok_or(UsdzError::UnexpectedEof)?;
        }
        if pos != directory.len() {
            return Err(invalid("central directory byte count mismatch").into());
        }
        order.sort_unstable();
        if let Some((header, _)) = order.first()
            && *header != 0
        {
            return Err(invalid("first package member must start at byte zero").into());
        }
        for pair in order.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(invalid("overlapping local header offsets").into());
            }
            records
                .get_mut(&pair[0].1)
                .expect("indexed record")
                .boundary = pair[1].0;
        }
        let root = order
            .first()
            .filter(|(_, name)| crate::is_usd_extension(name))
            .map(|(_, name)| name.clone());
        self.stats.index_parses += 1;
        self.stats.entries += count;
        Ok(Index {
            start,
            size,
            records,
            root,
        })
    }
    fn header(
        &mut self,
        key: &str,
        index: &Index,
        record: &Record,
    ) -> Result<u64, RangeError<S::Error>> {
        if let Some(offset) = self.headers.get(key) {
            return Ok(*offset);
        }
        if add(record.header, 30)? > record.boundary {
            return Err(invalid("local header overlaps another header").into());
        }
        let header = self.read(add(index.start, record.header)?, 30)?;
        let relative = crate::zip::stored_local_offset(
            &header,
            &record.name,
            record.flags,
            record.crc,
            record.size,
        )?;
        let name_size = record.name.len() as u64;
        let name_offset = add(record.header, 30)?;
        let offset = add(record.header, relative as u64)?;
        if !offset.is_multiple_of(64) {
            return Err(constraint("entry data must be 64-byte aligned").into());
        }
        if add(offset, record.size)? > record.boundary || offset > index.size {
            return Err(invalid("member data overlaps another header or directory").into());
        }
        let name = self.read(add(index.start, name_offset)?, name_size)?;
        if name.as_ref() != record.name.as_bytes() {
            return Err(invalid("local and central member names disagree").into());
        }
        let absolute = add(index.start, offset)?;
        self.headers.insert(key.into(), absolute);
        Ok(absolute)
    }
    fn nested(
        &mut self,
        prefix: &str,
        name: &str,
        depth: usize,
    ) -> Result<String, RangeError<S::Error>> {
        let key = if prefix.is_empty() {
            name.into()
        } else {
            alloc::format!("{prefix}[{name}]")
        };
        if self.indexes.contains_key(&key) {
            return Ok(key);
        }
        if depth > self.limits.depth {
            return Err(constraint("package nesting exceeds depth budget").into());
        }
        let index = self.indexes[prefix].clone();
        let record = index
            .records
            .get(name)
            .ok_or_else(|| UsdzError::MissingMember {
                member: Arc::from(key.as_str()),
            })?;
        if !name.ends_with(".usdz") {
            return Err(constraint("nested package selection requires a .usdz member").into());
        }
        let offset = self.header(&key, &index, record)?;
        let nested = self.index(offset, record.size)?;
        self.indexes.insert(key.clone(), Arc::new(nested));
        Ok(key)
    }
    fn select(&mut self, path: &str, layer: bool) -> Result<PackageMember, RangeError<S::Error>> {
        let parsed = PackagePath::parse(path, 32).map_err(constraint)?;
        let mut names = alloc::vec![parsed.outer().to_string()];
        names.extend_from_slice(parsed.members());
        let mut prefix = String::new();
        for (depth, name) in names[..names.len() - 1].iter().enumerate() {
            prefix = self.nested(&prefix, name, depth + 1)?;
        }
        let mut name = names.last().expect("nonempty member path").clone();
        if layer && name.ends_with(".usdz") {
            prefix = self.nested(&prefix, &name, names.len())?;
            name = self.indexes[&prefix]
                .root
                .as_deref()
                .ok_or(UsdzError::NoRootLayer)?
                .into();
        }
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            alloc::format!("{prefix}[{name}]")
        };
        let index = self.indexes[&prefix].clone();
        let record = index
            .records
            .get(name.as_str())
            .ok_or_else(|| UsdzError::MissingMember {
                member: Arc::from(key.as_str()),
            })?;
        if record.size > self.limits.member_bytes {
            return Err(constraint("selected member byte budget exceeded").into());
        }
        if record.size
            > self
                .limits
                .verification_bytes
                .saturating_sub(self.stats.verification_bytes)
        {
            return Err(constraint("package verification budget exceeded").into());
        }
        let offset = self.header(&key, &index, record)?;
        let bytes = self.read(offset, record.size)?;
        self.stats.verification_bytes += record.size;
        let actual = crate::crc32::crc32(&bytes);
        if actual != record.crc {
            return Err(UsdzError::CrcMismatch {
                entry: record.name.clone(),
                expected: record.crc,
                actual,
            }
            .into());
        }
        Ok(PackageMember { path: key, bytes })
    }
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
