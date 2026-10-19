// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Experimental native storage, independent of USDC.
//!
//! A directory borrows named payload blocks without expanding paths or values.
//! Text columns borrow a contiguous UTF-8 dictionary and little-endian row
//! indices. There are no per-string allocations, casts, alignment requirements,
//! implicit caches or I/O. Hosts own backing bytes, schema interpretation and
//! compression. This is a version-zero experiment, not a stable interchange
//! format or an implementation of USD scene composition. Native row semantics
//! remain those of [`crate::table::TableView`]. See `docs/native-tables.md`.

use alloc::{collections::BTreeSet, vec::Vec};

use super::{BytesColumn, TextColumn};

const MAGIC: &[u8; 8] = b"LSTABLE\0";
const HEADER: usize = 24;

/// A malformed or unsupported experimental storage encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageError(pub &'static str);

impl core::fmt::Display for StorageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}
impl core::error::Error for StorageError {}

/// Encoding of a directory block. Unknown encodings are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Opaque bytes interpreted and validated by the host.
    Bytes,
    /// A dictionary/index text column read by [`TextArray`].
    Text,
    /// Offset-indexed binary records read by [`BytesArray`].
    Records,
}

/// A named encoded payload, borrowed from an input file or a writer's buffers.
#[derive(Clone, Copy, Debug)]
pub struct Block<'a> {
    /// Nonempty UTF-8 name, unique within the directory.
    pub name: &'a str,
    /// The payload encoding.
    pub kind: BlockKind,
    /// Encoded payload. A directory open does not validate its contents.
    pub data: &'a [u8],
}

/// A validated borrowed directory. Opening allocates nothing and visits only
/// the header and directory; payload validation happens explicitly on access.
#[derive(Clone, Copy, Debug)]
pub struct NativeFile<'a> {
    data: &'a [u8],
    directory: &'a [u8],
    blocks: usize,
}

impl<'a> NativeFile<'a> {
    /// Validate version, directory names, lengths and disjoint payload extents.
    ///
    /// # Errors
    /// Unsupported version, truncation, overflow, invalid UTF-8 names, unordered
    /// or duplicate names, unknown block kinds, gaps/overlaps or trailing bytes.
    /// Success does not validate any block's payload, schema or business rules.
    pub fn open(data: &'a [u8]) -> Result<Self, StorageError> {
        if data.get(..8) != Some(MAGIC.as_slice()) || u32_at(data, 8)? != 0 {
            return Err(StorageError("unsupported native table header/version"));
        }
        let blocks = usize::try_from(u32_at(data, 12)?)
            .map_err(|_| StorageError("block count exceeds address space"))?;
        let end = add(HEADER, usize_at(data, 16)?)?;
        let directory = data
            .get(HEADER..end)
            .ok_or(StorageError("truncated directory"))?;
        if blocks > directory.len() / 21 {
            return Err(StorageError("impossible directory count"));
        }
        let mut remaining = directory;
        let mut previous = "";
        let mut expected = end;
        for _ in 0..blocks {
            let (name, _, offset, len, rest) = entry(remaining)?;
            if name.is_empty() || name <= previous || offset != expected {
                return Err(StorageError("unordered names or noncontiguous blocks"));
            }
            expected = add(offset, len)?;
            if expected > data.len() {
                return Err(StorageError("block extends beyond file"));
            }
            previous = name;
            remaining = rest;
        }
        if !remaining.is_empty() || expected != data.len() {
            return Err(StorageError("directory count or file length mismatch"));
        }
        Ok(Self {
            data,
            directory,
            blocks,
        })
    }

    /// Number of blocks. Lookup currently scans this small directory.
    #[must_use]
    pub fn len(self) -> usize {
        self.blocks
    }

    /// Whether the file has no blocks.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.blocks == 0
    }

    /// Borrow every block in directory order without reading its payload.
    pub fn blocks(self) -> impl ExactSizeIterator<Item = Block<'a>> {
        let mut remaining = self.directory;
        (0..self.blocks).map(move |_| {
            let (name, kind, offset, len, rest) = entry(remaining).expect("validated directory");
            remaining = rest;
            Block {
                name,
                kind,
                data: &self.data[offset..offset + len],
            }
        })
    }

    /// Borrow a named block without reading other payloads or allocating.
    #[must_use]
    pub fn block(self, wanted: &str) -> Option<Block<'a>> {
        self.blocks()
            .take_while(|block| block.name <= wanted)
            .find(|block| block.name == wanted)
    }

    /// Borrow and validate one complete text column; unrelated blocks stay unread.
    ///
    /// # Errors
    /// Missing block, wrong kind or malformed dictionary/index payload.
    pub fn text(self, name: &str) -> Result<TextArray<'a>, StorageError> {
        let block = self.block(name).ok_or(StorageError("missing text block"))?;
        if block.kind != BlockKind::Text {
            return Err(StorageError("block is not text"));
        }
        TextArray::open(block.data)
    }

    /// Borrow and validate record boundaries without interpreting record contents.
    ///
    /// # Errors
    /// Missing block, wrong kind or malformed offsets/counts.
    pub fn records(self, name: &str) -> Result<BytesArray<'a>, StorageError> {
        let block = self
            .block(name)
            .ok_or(StorageError("missing record block"))?;
        if block.kind != BlockKind::Records {
            return Err(StorageError("block is not records"));
        }
        BytesArray::open(block.data)
    }
}

/// A contiguous binary-record buffer with borrowed, unaligned-safe offsets.
#[derive(Clone, Copy, Debug)]
pub struct BytesArray<'a> {
    offsets: &'a [u8],
    data: &'a [u8],
}
impl<'a> BytesArray<'a> {
    /// Validate count and monotone offsets. Record bytes remain opaque.
    ///
    /// # Errors
    /// Overflow, truncation or offsets outside the record buffer.
    pub fn open(bytes: &'a [u8]) -> Result<Self, StorageError> {
        let count = usize_at(bytes, 0)?;
        let end = add(8, mul(add(count, 1)?, 8)?)?;
        let offsets = bytes
            .get(8..end)
            .ok_or(StorageError("truncated record offsets"))?;
        let data = bytes.get(end..).ok_or(StorageError("truncated records"))?;
        if usize_at(offsets, 0)? != 0 || usize_at(offsets, count * 8)? != data.len() {
            return Err(StorageError("record extent mismatch"));
        }
        let mut previous = 0;
        for index in 0..count {
            let next = usize_at(offsets, (index + 1) * 8)?;
            if next < previous || next > data.len() {
                return Err(StorageError("invalid record offsets"));
            }
            previous = next;
        }
        Ok(Self { offsets, data })
    }

    /// Borrow a record without allocating or decoding its payload.
    #[must_use]
    pub fn get(self, index: usize) -> Option<&'a [u8]> {
        if index >= self.offsets.len() / 8 - 1 {
            return None;
        }
        let start = usize_at(self.offsets, index * 8).ok()?;
        let end = usize_at(self.offsets, (index + 1) * 8).ok()?;
        self.data.get(start..end)
    }
}
impl BytesColumn for BytesArray<'_> {
    fn len(&self) -> usize {
        self.offsets.len() / 8 - 1
    }
    fn get(&self, index: usize) -> Option<&[u8]> {
        (*self).get(index)
    }
}

/// Encode contiguous binary records without interpreting their payloads.
///
/// # Errors
/// Lengths/counts exceed the address space or `u64` encoding.
pub fn write_records(records: &[&[u8]]) -> Result<Vec<u8>, StorageError> {
    let length = records
        .iter()
        .try_fold(0, |sum, record| add(sum, record.len()))?;
    let mut out = Vec::with_capacity(add(add(8, mul(add(records.len(), 1)?, 8)?)?, length)?);
    push_size(&mut out, records.len())?;
    push_size(&mut out, 0)?;
    let mut offset = 0;
    for record in records {
        offset = add(offset, record.len())?;
        push_size(&mut out, offset)?;
    }
    for record in records {
        out.extend_from_slice(record);
    }
    Ok(out)
}

/// A validated dictionary/index column borrowing all storage from its block.
/// Implements [`TextColumn`] directly for native keyed-table composition.
#[derive(Clone, Copy, Debug)]
pub struct TextArray<'a> {
    rows: &'a [u8],
    offsets: &'a [u8],
    text: &'a str,
}

impl<'a> TextArray<'a> {
    /// Validate all row indices, dictionary order, offsets and UTF-8 boundaries.
    /// Work is linear in this column's rows and encoded dictionary bytes;
    /// memory allocation is zero. No other column is visited.
    ///
    /// # Errors
    /// Counts overflow, truncated data, invalid indices/offsets/UTF-8, or an
    /// unordered/duplicate dictionary. Zero rows and an empty dictionary are valid.
    pub fn open(data: &'a [u8]) -> Result<Self, StorageError> {
        let rows = usize_at(data, 0)?;
        let dictionary = usize_at(data, 8)?;
        let row_end = add(16, mul(rows, 4)?)?;
        let text_start = add(row_end, mul(add(dictionary, 1)?, 8)?)?;
        let row_data = data
            .get(16..row_end)
            .ok_or(StorageError("truncated row indices"))?;
        let offsets = data
            .get(row_end..text_start)
            .ok_or(StorageError("truncated offsets"))?;
        let text = core::str::from_utf8(
            data.get(text_start..)
                .ok_or(StorageError("truncated text"))?,
        )
        .map_err(|_| StorageError("invalid dictionary UTF-8"))?;
        if usize_at(offsets, 0)? != 0 || usize_at(offsets, dictionary * 8)? != text.len() {
            return Err(StorageError("dictionary extent mismatch"));
        }
        let mut previous = None;
        let mut start = 0;
        for index in 0..dictionary {
            let end = usize_at(offsets, (index + 1) * 8)?;
            let value = text
                .get(start..end)
                .ok_or(StorageError("invalid text offsets"))?;
            if previous.is_some_and(|before| before >= value) {
                return Err(StorageError("dictionary must be unique and sorted"));
            }
            previous = Some(value);
            start = end;
        }
        for row in row_data.as_chunks::<4>().0 {
            if usize::try_from(u32_at(row, 0)?).map_or(true, |id| id >= dictionary) {
                return Err(StorageError("dictionary index out of range"));
            }
        }
        Ok(Self {
            rows: row_data,
            offsets,
            text,
        })
    }

    /// Borrow one row, with no allocations or implicit cache.
    #[must_use]
    pub fn get(self, index: usize) -> Option<&'a str> {
        if index >= self.rows.len() / 4 {
            return None;
        }
        let id = usize::try_from(u32_at(self.rows, index * 4).ok()?).ok()?;
        let start = usize_at(self.offsets, id * 8).ok()?;
        let end = usize_at(self.offsets, (id + 1) * 8).ok()?;
        self.text.get(start..end)
    }
}

impl TextColumn for TextArray<'_> {
    fn len(&self) -> usize {
        self.rows.len() / 4
    }
    fn get(&self, index: usize) -> Option<&str> {
        (*self).get(index)
    }
}

/// Encode a text column deterministically, deduplicating its dictionary.
/// This owned writer is for preparation; read-side access needs no allocation.
///
/// # Errors
/// More than `u32::MAX` distinct strings or an address-space size overflow.
pub fn write_text(values: &[&str]) -> Result<Vec<u8>, StorageError> {
    let dictionary: Vec<_> = values
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let text_size = dictionary
        .iter()
        .try_fold(0, |sum, value| add(sum, value.len()))?;
    let capacity = add(
        add(16, mul(values.len(), 4)?)?,
        add(mul(add(dictionary.len(), 1)?, 8)?, text_size)?,
    )?;
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(
        &u64::try_from(values.len())
            .map_err(|_| StorageError("too many rows"))?
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u64::try_from(dictionary.len())
            .map_err(|_| StorageError("too many strings"))?
            .to_le_bytes(),
    );
    for value in values {
        let id = dictionary
            .binary_search(value)
            .expect("dictionary contains every input");
        out.extend_from_slice(
            &u32::try_from(id)
                .map_err(|_| StorageError("dictionary exceeds u32"))?
                .to_le_bytes(),
        );
    }
    let mut offset = 0_u64;
    out.extend_from_slice(&offset.to_le_bytes());
    for value in &dictionary {
        offset += u64::try_from(value.len()).map_err(|_| StorageError("text too long"))?;
        out.extend_from_slice(&offset.to_le_bytes());
    }
    for value in dictionary {
        out.extend_from_slice(value.as_bytes());
    }
    Ok(out)
}

/// Write a version-zero directory and encoded blocks in deterministic name order.
/// Payload validation remains the encoder/host's responsibility.
///
/// # Errors
/// Empty/duplicate names or lengths/counts exceeding the encoding/address space.
pub fn write_file(blocks: &[Block<'_>]) -> Result<Vec<u8>, StorageError> {
    let mut ordered: Vec<_> = blocks.iter().collect();
    ordered.sort_unstable_by_key(|block| block.name);
    let mut previous = "";
    let mut directory = 0;
    let mut payload = 0;
    for block in &ordered {
        if block.name.is_empty() || block.name == previous {
            return Err(StorageError("block names must be nonempty and unique"));
        }
        u32::try_from(block.name.len()).map_err(|_| StorageError("block name too long"))?;
        directory = add(directory, add(21, block.name.len())?)?;
        payload = add(payload, block.data.len())?;
        previous = block.name;
    }
    let mut offset = add(HEADER, directory)?;
    let mut out = Vec::with_capacity(add(offset, payload)?);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(ordered.len())
            .map_err(|_| StorageError("too many blocks"))?
            .to_le_bytes(),
    );
    push_size(&mut out, directory)?;
    for block in &ordered {
        out.extend_from_slice(
            &u32::try_from(block.name.len())
                .map_err(|_| StorageError("block name too long"))?
                .to_le_bytes(),
        );
        out.extend_from_slice(block.name.as_bytes());
        out.push(match block.kind {
            BlockKind::Bytes => 0,
            BlockKind::Text => 1,
            BlockKind::Records => 2,
        });
        push_size(&mut out, offset)?;
        push_size(&mut out, block.data.len())?;
        offset = add(offset, block.data.len())?;
    }
    for block in ordered {
        out.extend_from_slice(block.data);
    }
    Ok(out)
}

fn push_size(out: &mut Vec<u8>, value: usize) -> Result<(), StorageError> {
    out.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| StorageError("length exceeds u64"))?
            .to_le_bytes(),
    );
    Ok(())
}
fn add(a: usize, b: usize) -> Result<usize, StorageError> {
    a.checked_add(b).ok_or(StorageError("length overflow"))
}
fn mul(a: usize, b: usize) -> Result<usize, StorageError> {
    a.checked_mul(b).ok_or(StorageError("length overflow"))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, StorageError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..add(offset, 4)?)
            .ok_or(StorageError("truncated u32"))?
            .try_into()
            .map_err(|_| StorageError("invalid u32"))?,
    ))
}
fn usize_at(bytes: &[u8], offset: usize) -> Result<usize, StorageError> {
    let raw = u64::from_le_bytes(
        bytes
            .get(offset..add(offset, 8)?)
            .ok_or(StorageError("truncated u64"))?
            .try_into()
            .map_err(|_| StorageError("invalid u64"))?,
    );
    usize::try_from(raw).map_err(|_| StorageError("length exceeds address space"))
}
type DirectoryEntry<'a> = (&'a str, BlockKind, usize, usize, &'a [u8]);
fn entry(bytes: &[u8]) -> Result<DirectoryEntry<'_>, StorageError> {
    let name_len = usize::try_from(u32_at(bytes, 0)?).map_err(|_| StorageError("name too long"))?;
    let end = add(4, name_len)?;
    let name = core::str::from_utf8(
        bytes
            .get(4..end)
            .ok_or(StorageError("truncated block name"))?,
    )
    .map_err(|_| StorageError("invalid block name"))?;
    let kind = match bytes.get(end) {
        Some(0) => BlockKind::Bytes,
        Some(1) => BlockKind::Text,
        Some(2) => BlockKind::Records,
        _ => return Err(StorageError("unknown block kind")),
    };
    let offset = usize_at(bytes, add(end, 1)?)?;
    let len = usize_at(bytes, add(end, 9)?)?;
    let rest = bytes
        .get(add(end, 17)?..)
        .ok_or(StorageError("truncated entry"))?;
    Ok((name, kind, offset, len, rest))
}

#[cfg(test)]
mod tests;
