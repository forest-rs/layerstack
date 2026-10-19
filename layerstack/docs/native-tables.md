# Experimental native table storage

`table::storage` is a version-zero physical-storage experiment for native
`KeyedTable` text and binary-record columns. It is independent of USDC and does
not change USD composition, array replacement, path semantics, or `Stage`. Applications own
domain schemas and the interpretation of opaque blocks. The format is not
stable or suitable for a durable production archive yet.

## Ownership and access

`NativeFile::open` borrows input bytes and validates the directory only. It
allocates nothing and reads no payload. `text(name)` validates that column's
dictionary, UTF-8, offsets and row indices without allocating. The resulting
`TextArray` implements `TextColumn`; use it directly in a `KeyedTable` with
ordinary `TablePatch` and `TableView` edits. Unchanged text borrows the file;
only edited cells need owned storage. Hosts retain the backing bytes and own
file I/O, decompression, caches, and lifetime management.

Successful directory open is not full payload validation. A malformed unread
column can coexist with a valid readable column; requesting the malformed
column returns `StorageError`. Native keys still require `KeyedTable::new`
validation for sorted, nonempty, unique identities and aligned columns.
Validation work is bounded by the encoded input sizes; no decoded length
causes an allocation on the read path. No unsafe code or alignment-dependent
typed casts are used.

## Version-zero layout

All integers are little-endian. There is no padding, compression or checksum.

- Header: eight bytes `LSTABLE\0`, `u32` version (0), `u32` block count,
  `u64` directory byte length.
- Each directory entry: `u32` UTF-8 name length, name bytes, `u8` kind
  (0 opaque bytes, 1 text column, 2 binary-record column), `u64` file offset,
  `u64` payload length.
- Names are nonempty, unique, strictly bytewise sorted. Payloads follow the
  directory contiguously in directory order; gaps, overlaps and trailing bytes
  are refused. Lookup scans the directory; this design expects a small number
  of blocks, not one directory entry per data row.
- A text column: `u64` row count, `u64` dictionary count, one `u32` dictionary
  index per row, dictionary count plus one `u64` text offsets, then contiguous
  UTF-8 text. Dictionary strings are strictly sorted and unique; offsets start
  at zero and end at the text length. Empty strings and zero-row columns are
  supported. There is no implicit nullable text; a domain schema must encode
  presence explicitly.
- A binary-record column: `u64` row count, row count plus one `u64` offsets,
  then contiguous record bytes. Offsets start at zero, are nondecreasing and
  end at the data length. Empty records are valid. `BytesArray` validates
  boundaries without interpreting the records; it implements `BytesColumn`
  for `ColumnData::Bytes`. Domain decoding stays with the host.

`write_text` deduplicates strings and `write_file` sorts directory entries.
Writer preparation owns temporary buffers; this is not a streaming exporter.
An opaque block may hold a domain adapter's record, but embedding a blob does
not give its contents typed views or composition semantics automatically.

## Scope and migration

The storage module and `BytesColumn` are additions. `CellKind`, `Cell`,
`OwnedCell`, and `ColumnData` gain a `Bytes` variant: callers with exhaustive
matches must handle binary records explicitly. Existing USDC, `LayerStore`,
schema-generated scene views, `Layer`, and `Stage` callers are unchanged.
This experiment composes native keyed tables only. It is not an immutable
backend for arbitrary USD layer stacks, references, variants or relationships.
No production data should depend on version-zero wire compatibility.

Numeric physical columns, persisted patches, random-access compression,
integrity/content identity, range-source I/O and direct scene/schema integration
are not implemented. A compiled cache is a host concern, not a storage truth.
