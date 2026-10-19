// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native keyed tables: borrowed columnar bases and sparse row edits.
//!
//! This is a Layerstack extension, not USD composition. A row is identified
//! by its stable text key, never by a positional handle. Keys are strictly
//! sorted; columns are typed and row-aligned. A host may store columns in
//! ordinary USD arrays, but stock USD does not apply these patch semantics.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

/// Read-only string storage, including dictionary/index-backed arrays.
/// Implementations must return one element for every index below `len`, and
/// keep their length and text stable while a table borrows them.
pub trait TextColumn: core::fmt::Debug + Sync {
    /// Number of text elements.
    fn len(&self) -> usize;
    /// Borrow one element without allocating or caching an owned string.
    fn get(&self, index: usize) -> Option<&str>;
    /// Whether there are no elements.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl TextColumn for [Arc<str>] {
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&self, index: usize) -> Option<&str> {
        self.get(index).map(AsRef::as_ref)
    }
}
impl TextColumn for Vec<Arc<str>> {
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&self, index: usize) -> Option<&str> {
        self.as_slice().get(index).map(AsRef::as_ref)
    }
}

/// The scalar kind of one native table column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// Exact text. Decimal quantities can use a separately declared text codec.
    Text,
    /// A boolean.
    Bool,
    /// A signed 64-bit integer.
    Int64,
    /// An unsigned 64-bit integer.
    UInt64,
    /// A binary 64-bit float; not an exact decimal or money type.
    Double,
}

/// A cell borrowed from a column or sparse edit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cell<'a> {
    /// Borrowed text.
    Text(&'a str),
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int64(i64),
    /// An unsigned integer.
    UInt64(u64),
    /// A binary float.
    Double(f64),
}
impl Cell<'_> {
    /// The cell's scalar kind.
    #[must_use]
    pub fn kind(self) -> CellKind {
        match self {
            Self::Text(_) => CellKind::Text,
            Self::Bool(_) => CellKind::Bool,
            Self::Int64(_) => CellKind::Int64,
            Self::UInt64(_) => CellKind::UInt64,
            Self::Double(_) => CellKind::Double,
        }
    }
}

/// An owned sparse cell. Only edited/inserted cells need this representation.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedCell {
    /// Shared exact text.
    Text(Arc<str>),
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int64(i64),
    /// An unsigned integer.
    UInt64(u64),
    /// A binary float.
    Double(f64),
}
impl OwnedCell {
    /// Borrow the edited cell.
    #[must_use]
    pub fn as_cell(&self) -> Cell<'_> {
        match self {
            Self::Text(v) => Cell::Text(v),
            Self::Bool(v) => Cell::Bool(*v),
            Self::Int64(v) => Cell::Int64(*v),
            Self::UInt64(v) => Cell::UInt64(*v),
            Self::Double(v) => Cell::Double(*v),
        }
    }
}

/// A typed, borrowed base column. No cell objects are stored per base row.
#[derive(Clone, Copy, Debug)]
pub enum ColumnData<'a> {
    /// Text, possibly backed by a compact dictionary and index array.
    Text(&'a dyn TextColumn),
    /// Booleans.
    Bool(&'a [bool]),
    /// Signed integers.
    Int64(&'a [i64]),
    /// Unsigned integers.
    UInt64(&'a [u64]),
    /// Binary floats.
    Double(&'a [f64]),
}
impl<'a> ColumnData<'a> {
    /// Column length.
    #[must_use]
    pub fn len(self) -> usize {
        match self {
            Self::Text(v) => v.len(),
            Self::Bool(v) => v.len(),
            Self::Int64(v) => v.len(),
            Self::UInt64(v) => v.len(),
            Self::Double(v) => v.len(),
        }
    }
    /// Whether the column is empty.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    /// Its scalar kind.
    #[must_use]
    pub fn kind(self) -> CellKind {
        match self {
            Self::Text(_) => CellKind::Text,
            Self::Bool(_) => CellKind::Bool,
            Self::Int64(_) => CellKind::Int64,
            Self::UInt64(_) => CellKind::UInt64,
            Self::Double(_) => CellKind::Double,
        }
    }
    fn get(self, index: usize) -> Option<Cell<'a>> {
        match self {
            Self::Text(v) => v.get(index).map(Cell::Text),
            Self::Bool(v) => v.get(index).copied().map(Cell::Bool),
            Self::Int64(v) => v.get(index).copied().map(Cell::Int64),
            Self::UInt64(v) => v.get(index).copied().map(Cell::UInt64),
            Self::Double(v) => v.get(index).copied().map(Cell::Double),
        }
    }
}

/// One named column of a keyed table.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    /// Stable field name, independent of its column position.
    pub name: &'a str,
    /// Borrowed typed values.
    pub data: ColumnData<'a>,
}

/// A refused base table or patch. Positions are in the caller's arrays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableError {
    /// Keys are absent, empty, duplicate or not strictly sorted at this row.
    KeyOrder(usize),
    /// Duplicate/empty column name or a column with the wrong row count.
    ColumnShape(usize),
    /// An insert names a row which already exists.
    ExistingRow(Arc<str>),
    /// An update or delete names a row which does not exist.
    MissingRow(Arc<str>),
    /// An insert does not provide exactly one value per column.
    InsertShape(Arc<str>),
    /// An edit names an unknown column or has the wrong scalar kind.
    CellType {
        /// Stable row key.
        row: Arc<str>,
        /// Stable column name.
        column: Arc<str>,
    },
}

impl core::fmt::Display for TableError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::KeyOrder(row) => write!(
                f,
                "row {row}: keys must be present, non-empty, unique and sorted"
            ),
            Self::ColumnShape(column) => write!(
                f,
                "column {column}: duplicate/empty name or row count mismatch"
            ),
            Self::ExistingRow(row) => write!(f, "cannot insert existing row {row:?}"),
            Self::MissingRow(row) => write!(
                f,
                "cannot update/delete absent row {row:?}; insertion must be explicit"
            ),
            Self::InsertShape(row) => write!(
                f,
                "insert {row:?} requires a non-empty key and a complete row"
            ),
            Self::CellType { row, column } => write!(
                f,
                "row {row:?}, column {column:?}: unknown column or incompatible scalar kind"
            ),
        }
    }
}
impl core::error::Error for TableError {}

/// An immutable base table borrowing every key and cell from its owner.
/// Construction validates shape and key order without building a row index.
#[derive(Debug)]
pub struct KeyedTable<'a> {
    keys: &'a dyn TextColumn,
    columns: &'a [Column<'a>],
}
impl<'a> KeyedTable<'a> {
    /// Validate a columnar base. The number of allocations is zero; validation
    /// scans keys and column descriptors, not each data cell.
    ///
    /// # Errors
    /// Empty/unsorted/duplicate keys or malformed columns.
    pub fn new(keys: &'a dyn TextColumn, columns: &'a [Column<'a>]) -> Result<Self, TableError> {
        let mut previous = None;
        for index in 0..keys.len() {
            let key = keys
                .get(index)
                .filter(|key| !key.is_empty())
                .ok_or(TableError::KeyOrder(index))?;
            if previous.is_some_and(|p| p >= key) {
                return Err(TableError::KeyOrder(index));
            }
            previous = Some(key);
        }
        for (index, column) in columns.iter().enumerate() {
            if column.name.is_empty()
                || column.data.len() != keys.len()
                || columns[..index]
                    .iter()
                    .any(|other| other.name == column.name)
            {
                return Err(TableError::ColumnShape(index));
            }
        }
        Ok(Self { keys, columns })
    }
    /// Number of base rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    /// Whether the base has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    /// Find a stable key without allocating a hash-map entry per row.
    #[must_use]
    pub fn row(&self, key: &str) -> Option<usize> {
        let mut low = 0;
        let mut high = self.len();
        while low < high {
            let mid = low + (high - low) / 2;
            match self.keys.get(mid)?.cmp(key) {
                core::cmp::Ordering::Less => low = mid + 1,
                core::cmp::Ordering::Greater => high = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
    /// Borrow one cell by stable row key and column name.
    #[must_use]
    pub fn get(&self, key: &str, column: &str) -> Option<Cell<'a>> {
        self.column(column)?.data.get(self.row(key)?)
    }
    fn column(&self, name: &str) -> Option<&'a Column<'a>> {
        self.columns.iter().find(|column| column.name == name)
    }
}

/// An explicit row operation. Updates never implicitly insert or resurrect.
#[derive(Clone, Debug, PartialEq)]
pub enum RowEdit {
    /// Complete row in the base schema's column order.
    Insert(Vec<OwnedCell>),
    /// Sparse replacements by stable column name.
    Update(BTreeMap<Arc<str>, OwnedCell>),
    /// Delete this row.
    Delete,
}
/// Sparse row edits. `insert` replaces the operation for that key within this
/// patch; distinct layers remain separate and are validated when composed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TablePatch {
    /// One operation per stable row key.
    pub rows: BTreeMap<Arc<str>, RowEdit>,
}

/// A validated native table composition. Patches are **weakest to strongest**.
/// The base is borrowed; storage scales with patch count, not base row count.
/// There is no clock, cache, file I/O or implicit write-back.
#[derive(Debug)]
pub struct TableView<'a> {
    base: &'a KeyedTable<'a>,
    patches: &'a [TablePatch],
    rows: usize,
}
impl<'a> TableView<'a> {
    /// Validate every sparse operation before exposing the composed view.
    /// A deletion can be followed by an explicit insertion; a plain update
    /// after deletion is refused. No base or patch is mutated.
    ///
    /// # Errors
    /// Missing/existing rows, incomplete inserts or incompatible cells.
    pub fn compose(
        base: &'a KeyedTable<'a>,
        patches: &'a [TablePatch],
    ) -> Result<Self, TableError> {
        let mut view = Self {
            base,
            patches: &[],
            rows: base.len(),
        };
        for (index, patch) in patches.iter().enumerate() {
            for (key, edit) in &patch.rows {
                let exists = view.contains(key);
                match edit {
                    RowEdit::Insert(cells) => {
                        if exists {
                            return Err(TableError::ExistingRow(key.clone()));
                        }
                        if key.is_empty() || cells.len() != base.columns.len() {
                            return Err(TableError::InsertShape(key.clone()));
                        }
                        for (column, cell) in base.columns.iter().zip(cells) {
                            if column.data.kind() != cell.as_cell().kind() {
                                return Err(TableError::CellType {
                                    row: key.clone(),
                                    column: Arc::from(column.name),
                                });
                            }
                        }
                        view.rows += 1;
                    }
                    RowEdit::Update(cells) => {
                        if !exists {
                            return Err(TableError::MissingRow(key.clone()));
                        }
                        for (column, cell) in cells {
                            if base
                                .column(column)
                                .is_none_or(|c| c.data.kind() != cell.as_cell().kind())
                            {
                                return Err(TableError::CellType {
                                    row: key.clone(),
                                    column: column.clone(),
                                });
                            }
                        }
                    }
                    RowEdit::Delete => {
                        if !exists {
                            return Err(TableError::MissingRow(key.clone()));
                        }
                        view.rows -= 1;
                    }
                }
            }
            view.patches = &patches[..=index];
        }
        Ok(view)
    }
    /// Number of live rows after inserts/deletes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows
    }
    /// Whether every row is absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }
    /// Whether the composed row exists.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        for patch in self.patches.iter().rev() {
            match patch.rows.get(key) {
                Some(RowEdit::Insert(_)) => return true,
                Some(RowEdit::Delete) => return false,
                _ => {}
            }
        }
        self.base.row(key).is_some()
    }
    /// Borrow the strongest cell, falling back to the immutable base.
    #[must_use]
    pub fn get(&self, key: &str, column: &str) -> Option<Cell<'a>> {
        let index = self.base.columns.iter().position(|c| c.name == column)?;
        for patch in self.patches.iter().rev() {
            match patch.rows.get(key) {
                Some(RowEdit::Insert(cells)) => return cells.get(index).map(OwnedCell::as_cell),
                Some(RowEdit::Delete) => return None,
                Some(RowEdit::Update(cells)) => {
                    if let Some(cell) = cells.get(column) {
                        return Some(cell.as_cell());
                    }
                }
                None => {}
            }
        }
        self.base.get(key, column)
    }
}

#[cfg(test)]
mod tests;
