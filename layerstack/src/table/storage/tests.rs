use super::*;
use crate::table::{
    Cell, Column, ColumnData, KeyedTable, OwnedCell, RowEdit, TablePatch, TableView,
};
use alloc::{collections::BTreeMap, sync::Arc};

#[test]
fn borrowed_file_columns_compose_without_materializing_rows() {
    let keys = write_text(&["a", "b", "c"]).unwrap();
    let names = write_text(&["椅子", "", "椅子"]).unwrap();
    let blocks = [
        Block {
            name: "names",
            kind: BlockKind::Text,
            data: &names,
        },
        Block {
            name: "keys",
            kind: BlockKind::Text,
            data: &keys,
        },
    ];
    let bytes = write_file(&blocks).unwrap();
    let reversed = [blocks[1], blocks[0]];
    assert_eq!(
        bytes,
        write_file(&reversed).unwrap(),
        "input order cannot change encoding"
    );
    let file = NativeFile::open(&bytes).unwrap();
    let keys = file.text("keys").unwrap();
    let names = file.text("names").unwrap();
    let value = names.get(0).unwrap();
    assert!(
        value.as_ptr() >= bytes.as_ptr()
            && value.as_ptr() < bytes.as_ptr().wrapping_add(bytes.len()),
        "cell text must borrow the original file, not a decoded allocation"
    );
    assert_eq!(names.get(1), Some(""));
    assert_eq!(names.get(3), None);
    let columns = [Column {
        name: "name",
        data: ColumnData::Text(&names),
    }];
    let base = KeyedTable::new(&keys, &columns).unwrap();
    let patches = [TablePatch {
        rows: BTreeMap::from([(
            Arc::from("b"),
            RowEdit::Update(BTreeMap::from([(
                Arc::from("name"),
                OwnedCell::Text(Arc::from("Chair")),
            )])),
        )]),
    }];
    let view = TableView::compose(&base, &patches).unwrap();
    assert_eq!(view.get("b", "name"), Some(Cell::Text("Chair")));
    assert_eq!(
        base.get("b", "name"),
        Some(Cell::Text("")),
        "base must stay unchanged"
    );
    assert_eq!(view.get("c", "name"), Some(Cell::Text("椅子")));
}

#[test]
fn directory_open_does_not_read_unrequested_payloads() {
    let good = write_text(&["hello"]).unwrap();
    let bytes = write_file(&[
        Block {
            name: "bad",
            kind: BlockKind::Text,
            data: &[0xff; 7],
        },
        Block {
            name: "good",
            kind: BlockKind::Text,
            data: &good,
        },
    ])
    .unwrap();
    let file = NativeFile::open(&bytes).unwrap();
    assert_eq!(file.len(), 2);
    assert_eq!(file.text("good").unwrap().get(0), Some("hello"));
    assert!(
        file.text("bad").is_err(),
        "malformed requested columns must fail explicitly"
    );
    assert!(file.text("missing").is_err());
}

#[test]
fn truncation_offsets_indices_and_versions_are_checked() {
    let column = write_text(&["é", "z"]).unwrap();
    for end in 0..column.len() {
        assert!(
            TextArray::open(&column[..end]).is_err(),
            "truncation at {end}"
        );
    }
    let mut bad = column.clone();
    bad[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        TextArray::open(&bad).is_err(),
        "out-of-range dictionary index"
    );
    let mut bad = column.clone();
    // Dictionary is [z, é]; an offset inside é must be refused.
    bad[24 + 8..24 + 16].copy_from_slice(&2_u64.to_le_bytes());
    assert!(TextArray::open(&bad).is_err(), "UTF-8 boundary violation");
    let block = Block {
        name: "text",
        kind: BlockKind::Text,
        data: &column,
    };
    assert!(
        write_file(&[block, block]).is_err(),
        "duplicate names are ambiguous"
    );
    let bytes = write_file(&[block]).unwrap();
    for end in 0..bytes.len() {
        assert!(
            NativeFile::open(&bytes[..end]).is_err(),
            "file truncation at {end}"
        );
    }
    let mut bad = bytes.clone();
    bad[8] = 1;
    assert!(NativeFile::open(&bad).is_err(), "unknown version");
    let mut bad = bytes.clone();
    // First payload offset follows name length, name and kind.
    bad[HEADER + 9..HEADER + 17].copy_from_slice(&0_u64.to_le_bytes());
    assert!(
        NativeFile::open(&bad).is_err(),
        "payload overlaps directory"
    );
    let mut bad = column;
    bad[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(
        TextArray::open(&bad).is_err(),
        "count arithmetic must not overflow"
    );
    let empty = write_text(&[]).unwrap();
    assert!(TextArray::open(&empty).unwrap().is_empty());
    assert!(
        NativeFile::open(&write_file(&[]).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn binary_records_borrow_and_compose_with_explicit_kinds() {
    let encoded = write_records(&[&[0xff, 0, 3], &[], &[1]]).unwrap();
    let records = BytesArray::open(&encoded).unwrap();
    let keys = write_text(&["a", "b", "c"]).unwrap();
    let keys = TextArray::open(&keys).unwrap();
    let columns = [Column {
        name: "record",
        data: ColumnData::Bytes(&records),
    }];
    let base = KeyedTable::new(&keys, &columns).unwrap();
    assert_eq!(base.get("b", "record"), Some(Cell::Bytes(&[])));
    let patches = [TablePatch {
        rows: BTreeMap::from([(
            Arc::from("b"),
            RowEdit::Update(BTreeMap::from([(
                Arc::from("record"),
                OwnedCell::Bytes(Arc::from([9_u8, 8])),
            )])),
        )]),
    }];
    let view = TableView::compose(&base, &patches).unwrap();
    assert_eq!(view.get("b", "record"), Some(Cell::Bytes(&[9, 8])));
    assert_eq!(view.get("a", "record"), Some(Cell::Bytes(&[0xff, 0, 3])));
    let record = records.get(0).unwrap();
    assert_eq!(
        record.as_ptr(),
        encoded[40..].as_ptr(),
        "records must borrow file bytes"
    );
    let mut malformed = encoded.clone();
    malformed[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(
        BytesArray::open(&malformed).is_err(),
        "out-of-range record offsets"
    );
    for end in 0..encoded.len() {
        assert!(
            BytesArray::open(&encoded[..end]).is_err(),
            "record truncation at {end}"
        );
    }
    assert!(
        BytesArray::open(&write_records(&[]).unwrap())
            .unwrap()
            .is_empty()
    );
}
