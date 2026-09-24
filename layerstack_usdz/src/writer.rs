// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDZ package writer.
//!
//! Produces the constrained ZIP layout that USDZ requires (AOUSD Core
//! §16.4.1; `OpenUSD` USDZ specification, "Layout",
//! <https://openusd.org/dev/spec_usdz.html#layout>):
//!
//! - every entry is stored (compression method 0), unencrypted, with its
//!   CRC-32 and sizes in the local header (no data descriptors);
//! - every entry's data starts at a multiple of 64 bytes. Alignment is
//!   achieved by padding the local header's extra field with a single
//!   zero-filled record (header ID `0x1986`, as `usdzip` writes), so there
//!   are no gaps between entries;
//! - the root layer is the first entry, both in the file and in the
//!   central directory (§16.4.1.2);
//! - every member is a USD layer, image or audio file
//!   ([`MEMBER_EXTENSIONS`], §16.4.1.5);
//! - 32-bit ZIP only, and the End of Central Directory record has no
//!   comment and ends the file (§16.4.1.4).
//!
//! Output is deterministic: timestamps are fixed at the DOS epoch
//! (1980-01-01 00:00) and no host-specific attributes are recorded.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::crc32::crc32;

/// Extra-field header ID used for alignment padding (matches `usdzip`).
const PADDING_EXTRA_ID: u16 = 0x1986;
/// Required data alignment.
const ALIGNMENT: usize = 64;
/// Local File Header fixed size.
const LFH_FIXED_SIZE: usize = 30;
/// Version 1.0: stored entries need no later ZIP features.
const VERSION_NEEDED: u16 = 10;
/// DOS date for 1980-01-01 (day 1, month 1, year 0).
const DOS_EPOCH_DATE: u16 = (1 << 5) | 1;
/// General-purpose flag bit 11: file name is UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

/// One file to place in a package.
#[derive(Clone, Copy, Debug)]
pub struct PackageFile<'a> {
    /// Path inside the package, relative, `/`-separated (e.g.
    /// `textures/albedo.png`). Layers refer to it by this same path.
    pub path: &'a str,
    /// File contents, stored verbatim.
    pub data: &'a [u8],
}

impl<'a> PackageFile<'a> {
    /// Creates a package entry.
    pub fn new(path: &'a str, data: &'a [u8]) -> Self {
        Self { path, data }
    }
}

/// Why a package could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsdzWriteError {
    /// No files were given.
    Empty,
    /// The first file is not a USD layer (`.usd`, `.usda`, `.usdc`).
    ///
    /// Spec: AOUSD Core §16.4.1.2 (the first file is the root layer).
    RootNotLayer {
        /// Path of the first file.
        path: String,
    },
    /// A path is empty, absolute, contains `\`, NUL, an empty, `.` or `..`
    /// segment, or is longer than a ZIP name field allows.
    InvalidPath {
        /// The rejected path.
        path: String,
    },
    /// A file's extension is not one of the USD, image or audio formats a
    /// USDZ package may contain ([`MEMBER_EXTENSIONS`]).
    UnsupportedMemberType {
        /// The rejected path.
        path: String,
    },
    /// Two files share a path.
    DuplicatePath {
        /// The repeated path.
        path: String,
    },
    /// The package would need Zip64 (too many entries, or a size or offset
    /// beyond 32 bits), which USDZ forbids (§16.4.1.1).
    TooLarge,
}

impl fmt::Display for UsdzWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "a USDZ package needs at least a root layer"),
            Self::RootNotLayer { path } => {
                write!(f, "first package file {path:?} is not a USD layer")
            }
            Self::InvalidPath { path } => write!(f, "invalid package path {path:?}"),
            Self::UnsupportedMemberType { path } => {
                write!(f, "{path:?} is not a USD, image or audio file")
            }
            Self::DuplicatePath { path } => write!(f, "duplicate package path {path:?}"),
            Self::TooLarge => write!(f, "package exceeds 32-bit ZIP limits"),
        }
    }
}

impl core::error::Error for UsdzWriteError {}

/// Writes a USDZ package containing `files`, the first of which is the
/// root layer.
///
/// Entries are written in the given order. Asset paths inside the layers
/// should use the same relative paths as [`PackageFile::path`]; the reader
/// resolves them inside the package (§9.7, packaged resource resolution).
///
/// # Errors
///
/// See [`UsdzWriteError`]. Nothing is produced on error.
///
/// # Example
///
/// ```
/// use layerstack_usdz::writer::{PackageFile, write_usdz};
///
/// let layer = b"#usda 1.0\n";
/// let bytes = write_usdz(&[PackageFile::new("scene.usda", layer)])?;
/// let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
/// assert_eq!(archive.entries()[0].data_offset % 64, 0);
/// # Ok::<(), layerstack_usdz::writer::UsdzWriteError>(())
/// ```
pub fn write_usdz(files: &[PackageFile<'_>]) -> Result<Vec<u8>, UsdzWriteError> {
    validate(files)?;
    // 0xFFFF entries is the Zip64 escape value.
    let count = u16::try_from(files.len())
        .ok()
        .filter(|&n| n != u16::MAX)
        .ok_or(UsdzWriteError::TooLarge)?;

    let mut out = Vec::new();
    let mut records = Vec::with_capacity(files.len());
    for file in files {
        let header_offset = out.len();
        let name = file.path.as_bytes();
        let pad = padding_for(header_offset + LFH_FIXED_SIZE + name.len());
        let crc = crc32(file.data);
        let size = u32_field(file.data.len())?;
        let name_len = u16::try_from(name.len()).map_err(|_| UsdzWriteError::TooLarge)?;
        let extra_len = u16::try_from(pad).map_err(|_| UsdzWriteError::TooLarge)?;
        let flags = if file.path.is_ascii() { 0 } else { FLAG_UTF8 };

        put_u32(&mut out, 0x0403_4b50);
        put_u16(&mut out, VERSION_NEEDED);
        put_u16(&mut out, flags);
        put_u16(&mut out, 0); // method: stored
        put_u16(&mut out, 0); // time: 00:00:00
        put_u16(&mut out, DOS_EPOCH_DATE);
        put_u32(&mut out, crc);
        put_u32(&mut out, size); // compressed
        put_u32(&mut out, size); // uncompressed
        put_u16(&mut out, name_len);
        put_u16(&mut out, extra_len);
        out.extend_from_slice(name);
        if pad > 0 {
            put_u16(&mut out, PADDING_EXTRA_ID);
            put_u16(&mut out, extra_len - 4);
            out.resize(out.len() + pad - 4, 0);
        }
        debug_assert_eq!(out.len() % ALIGNMENT, 0, "entry data must be aligned");
        out.extend_from_slice(file.data);

        records.push((header_offset, crc, size, name_len, flags));
    }

    let cd_offset = out.len();
    for (file, &(header_offset, crc, size, name_len, flags)) in files.iter().zip(&records) {
        put_u32(&mut out, 0x0201_4b50);
        put_u16(&mut out, VERSION_NEEDED); // version made by (MS-DOS, 1.0)
        put_u16(&mut out, VERSION_NEEDED);
        put_u16(&mut out, flags);
        put_u16(&mut out, 0); // method
        put_u16(&mut out, 0); // time
        put_u16(&mut out, DOS_EPOCH_DATE);
        put_u32(&mut out, crc);
        put_u32(&mut out, size);
        put_u32(&mut out, size);
        put_u16(&mut out, name_len);
        put_u16(&mut out, 0); // extra
        put_u16(&mut out, 0); // comment
        put_u16(&mut out, 0); // disk number start
        put_u16(&mut out, 0); // internal attributes
        put_u32(&mut out, 0); // external attributes
        put_u32(&mut out, u32_field(header_offset)?);
        out.extend_from_slice(file.path.as_bytes());
    }
    let cd_size = u32_field(out.len() - cd_offset)?;
    let cd_offset = u32_field(cd_offset)?;

    // End of Central Directory, no comment, last bytes of the file (§16.4.1.4).
    put_u32(&mut out, 0x0605_4b50);
    put_u16(&mut out, 0); // this disk
    put_u16(&mut out, 0); // disk with central directory
    put_u16(&mut out, count);
    put_u16(&mut out, count);
    put_u32(&mut out, cd_size);
    put_u32(&mut out, cd_offset);
    put_u16(&mut out, 0); // comment length
    Ok(out)
}

/// Extra-field bytes needed so data starting at `unpadded` lands on a
/// 64-byte boundary. A non-empty extra field holds at least one 4-byte
/// record header, so gaps of 1–3 bytes grow by a full block.
fn padding_for(unpadded: usize) -> usize {
    let pad = (ALIGNMENT - unpadded % ALIGNMENT) % ALIGNMENT;
    if pad != 0 && pad < 4 {
        pad + ALIGNMENT
    } else {
        pad
    }
}

/// A 32-bit size/offset field; `0xFFFFFFFF` is the Zip64 escape value.
fn u32_field(v: usize) -> Result<u32, UsdzWriteError> {
    u32::try_from(v)
        .ok()
        .filter(|&n| n != u32::MAX)
        .ok_or(UsdzWriteError::TooLarge)
}

fn validate(files: &[PackageFile<'_>]) -> Result<(), UsdzWriteError> {
    let Some(root) = files.first() else {
        return Err(UsdzWriteError::Empty);
    };
    for (i, file) in files.iter().enumerate() {
        let path = file.path;
        let bad_segment = path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
        if bad_segment || path.contains('\\') || path.contains('\0') || path.len() > 0xFFFF {
            return Err(UsdzWriteError::InvalidPath { path: path.into() });
        }
        if files[..i].iter().any(|f| f.path == path) {
            return Err(UsdzWriteError::DuplicatePath { path: path.into() });
        }
        if !is_member_type(path) {
            return Err(UsdzWriteError::UnsupportedMemberType { path: path.into() });
        }
    }
    if !crate::is_usd_extension(root.path) {
        return Err(UsdzWriteError::RootNotLayer {
            path: root.path.into(),
        });
    }
    Ok(())
}

/// File extensions a USDZ package may contain, exactly as the `OpenUSD`
/// USDZ specification lists them (`docs/spec_usdz.rst`, "Usdz
/// Specification" table and "File Types"): USD layers, PNG/JPEG/OpenEXR/AVIF
/// images, and M4A/MP3/WAV audio. AOUSD Core §16.4.1.5 recommends the same
/// set. Nested `.usdz` packages are not in the specification's list and are
/// not written. Extensions must be lowercase.
pub const MEMBER_EXTENSIONS: &[&str] = &[
    "usda", "usdc", "usd", "png", "jpg", "jpeg", "exr", "avif", "m4a", "mp3", "wav",
];

fn is_member_type(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    file_name
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && MEMBER_EXTENSIONS.contains(&ext))
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::ZipArchive;

    const LAYER: &[u8] =
        b"#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\ndef Xform \"Root\"\n{\n}\n";

    #[test]
    fn layout_is_aligned_and_ordered() {
        let png = [0x89_u8, b'P', b'N', b'G', 1, 2, 3];
        let files = [
            PackageFile::new("scene.usda", LAYER),
            PackageFile::new("textures/a.png", &png),
            PackageFile::new("tex/ü.png", b""),
        ];
        let bytes = write_usdz(&files).unwrap();
        let archive = ZipArchive::parse(&bytes).expect("reader accepts writer output");
        let entries = archive.entries();
        assert_eq!(entries.len(), 3, "entry count");
        for (entry, file) in entries.iter().zip(&files) {
            assert_eq!(&*entry.name, file.path, "central directory order");
            assert_eq!(entry.data_offset % 64, 0, "{} data aligned", file.path);
            assert_eq!(archive.entry_data(entry), file.data, "{} data", file.path);
            assert_eq!(entry.crc32, crc32(file.data), "{} crc", file.path);
        }
        assert_eq!(
            entries[0].data_offset, 64,
            "root layer data follows first header"
        );
        assert_eq!(
            &bytes[bytes.len() - 2..],
            &[0, 0],
            "EOCD comment length ends the file"
        );
        assert_eq!(bytes, write_usdz(&files).unwrap(), "deterministic");
    }

    #[test]
    fn padding_never_leaves_room_for_less_than_a_record_header() {
        for unpadded in 0..256 {
            let pad = padding_for(unpadded);
            assert_eq!((unpadded + pad) % 64, 0, "aligned for {unpadded}");
            assert!(pad == 0 || pad >= 4, "pad {pad} for {unpadded}");
        }
    }

    #[test]
    fn rejects_invalid_packages() {
        assert_eq!(write_usdz(&[]), Err(UsdzWriteError::Empty), "empty");
        assert_eq!(
            write_usdz(&[PackageFile::new("a.png", b"")]),
            Err(UsdzWriteError::RootNotLayer {
                path: "a.png".into()
            }),
            "root must be a layer"
        );
        for bad in [
            "",
            "/abs.usda",
            "a//b.usda",
            "../up.usda",
            "./x.usda",
            "a\\b.usda",
        ] {
            assert_eq!(
                write_usdz(&[PackageFile::new(bad, LAYER)]),
                Err(UsdzWriteError::InvalidPath { path: bad.into() }),
                "path {bad:?}"
            );
        }
        assert_eq!(
            write_usdz(&[
                PackageFile::new("a.usda", LAYER),
                PackageFile::new("a.usda", LAYER)
            ]),
            Err(UsdzWriteError::DuplicatePath {
                path: "a.usda".into()
            }),
            "duplicate"
        );
    }

    #[test]
    fn member_types_follow_the_usdz_specification() {
        for path in [
            "b.usda", "b.usdc", "b.usd", "t/a.png", "t/a.jpg", "t/a.jpeg", "t/a.exr", "t/a.avif",
            "s/a.m4a", "s/a.mp3", "s/a.wav",
        ] {
            let files = [
                PackageFile::new("scene.usda", LAYER),
                PackageFile::new(path, b"x"),
            ];
            assert!(write_usdz(&files).is_ok(), "{path} accepted");
        }
        for path in [
            "data.json",
            "blob.bin",
            "noextension",
            "t/.png",
            "nested.usdz",
            "a.PNG",
            "png/readme",
        ] {
            let files = [
                PackageFile::new("scene.usda", LAYER),
                PackageFile::new(path, b"x"),
            ];
            assert_eq!(
                write_usdz(&files),
                Err(UsdzWriteError::UnsupportedMemberType { path: path.into() }),
                "{path} rejected"
            );
        }
    }
}
