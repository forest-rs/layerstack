use super::*;
use alloc::vec;

fn texts(values: &[&str]) -> Vec<Arc<str>> {
    values.iter().copied().map(Arc::from).collect()
}
fn text(value: &str) -> OwnedCell {
    OwnedCell::Text(Arc::from(value))
}
fn patch(key: &str, edit: RowEdit) -> TablePatch {
    TablePatch {
        rows: BTreeMap::from([(Arc::from(key), edit)]),
    }
}

#[test]
fn sparse_layers_preserve_other_cells_and_do_not_copy_the_base() {
    let keys = texts(&["A", "B", "C"]);
    let names = texts(&["Oak", "Gray", "White"]);
    let amounts = [100, 200, 300];
    let columns = [
        Column {
            name: "name",
            data: ColumnData::Text(&names),
        },
        Column {
            name: "amount",
            data: ColumnData::Int64(&amounts),
        },
    ];
    let base = KeyedTable::new(&keys, &columns).unwrap();
    let patches = [
        patch(
            "B",
            RowEdit::Update(BTreeMap::from([(Arc::from("name"), text("Dealer gray"))])),
        ),
        patch(
            "B",
            RowEdit::Update(BTreeMap::from([(
                Arc::from("amount"),
                OwnedCell::Int64(175),
            )])),
        ),
    ];
    let view = TableView::compose(&base, &patches).unwrap();
    assert_eq!(view.get("B", "name"), Some(Cell::Text("Dealer gray")));
    assert_eq!(view.get("B", "amount"), Some(Cell::Int64(175)));
    assert_eq!(view.get("A", "name"), Some(Cell::Text("Oak")));
    assert_eq!(base.get("B", "amount"), Some(Cell::Int64(200)));
    let Some(Cell::Text(borrow)) = view.get("C", "name") else {
        panic!("text cell")
    };
    assert!(core::ptr::eq(borrow, names[2].as_ref()));
    assert_eq!(view.len(), 3);
    assert!(view.contains("B"));
    assert!(!view.contains("missing"));
    assert_eq!(view.get("B", "missing"), None);
}

#[test]
fn row_deletion_needs_explicit_reinsertion_and_resets_older_cells() {
    let keys = texts(&["A"]);
    let names = texts(&["Original"]);
    let columns = [Column {
        name: "name",
        data: ColumnData::Text(&names),
    }];
    let base = KeyedTable::new(&keys, &columns).unwrap();
    let deleted = [patch("A", RowEdit::Delete)];
    let view = TableView::compose(&base, &deleted).unwrap();
    assert!(view.is_empty());
    assert_eq!(view.get("A", "name"), None);
    let invalid = [
        deleted[0].clone(),
        patch(
            "A",
            RowEdit::Update(BTreeMap::from([(
                Arc::from("name"),
                text("Accidental resurrection"),
            )])),
        ),
    ];
    assert!(
        matches!(TableView::compose(&base,&invalid),Err(TableError::MissingRow(row)) if row.as_ref()=="A")
    );
    let replaced = [
        deleted[0].clone(),
        patch("A", RowEdit::Insert(vec![text("Replacement")])),
    ];
    let view = TableView::compose(&base, &replaced).unwrap();
    assert_eq!(view.get("A", "name"), Some(Cell::Text("Replacement")));
    assert_eq!(view.len(), 1);
}

#[test]
fn malformed_bases_and_patches_are_refused_before_reads() {
    for keys in [texts(&["B", "A"]), texts(&["A", "A"]), texts(&[""])] {
        assert!(matches!(
            KeyedTable::new(&keys, &[]),
            Err(TableError::KeyOrder(_))
        ));
    }
    let keys = texts(&["A"]);
    let values = texts(&["value"]);
    let bad = [
        Column {
            name: "name",
            data: ColumnData::Text(&values),
        },
        Column {
            name: "name",
            data: ColumnData::Text(&values),
        },
    ];
    assert!(matches!(
        KeyedTable::new(&keys, &bad),
        Err(TableError::ColumnShape(1))
    ));
    let columns = [Column {
        name: "name",
        data: ColumnData::Text(&values),
    }];
    let base = KeyedTable::new(&keys, &columns).unwrap();
    for edit in [
        RowEdit::Insert(vec![]),
        RowEdit::Insert(vec![OwnedCell::Bool(true)]),
        RowEdit::Delete,
        RowEdit::Update(BTreeMap::new()),
    ] {
        assert!(TableView::compose(&base, &[patch("unknown", edit)]).is_err());
    }
    assert!(
        TableView::compose(
            &base,
            &[patch("A", RowEdit::Insert(vec![text("duplicate")]))]
        )
        .is_err()
    );
    assert!(
        TableView::compose(
            &base,
            &[patch(
                "A",
                RowEdit::Update(BTreeMap::from([(Arc::from("name"), OwnedCell::Bool(true))]))
            )]
        )
        .is_err()
    );
    let inserted = [patch("B", RowEdit::Insert(vec![text("new")]))];
    let view = TableView::compose(&base, &inserted).unwrap();
    assert_eq!(view.len(), 2);
    assert_eq!(view.get("B", "name"), Some(Cell::Text("new")));
}
