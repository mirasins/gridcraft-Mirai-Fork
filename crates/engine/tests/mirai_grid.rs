use gridcraft_engine::Session;
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_workbook();
    s
}

#[test]
fn fills_separate_cells_and_undoes_them_together() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [
        {"cell": "A1", "input": "Item"}, {"cell": "B1", "input": "Amount"},
        {"cell": "A2", "input": "Rent"}, {"cell": "B2", "input": "42"},
        {"cell": "B3", "input": "=SUM(B2:B2)"}
    ]});
    s.execute("assistant.applyGrid", json!({"context": context, "grid": grid})).unwrap();
    assert_eq!(s.execute("cell.get", json!({"cell": "A1"})).unwrap()["value"], "Item");
    assert_eq!(s.execute("cell.get", json!({"cell": "B1"})).unwrap()["value"], "Amount");
    assert_eq!(s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("B3").unwrap()), "=SUM(B2:B2)");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("A1").unwrap()).is_empty());
}

#[test]
fn creates_sheet_and_undoes_creation_and_fill() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let before = s.doc().unwrap().wb.sheets.len();
    s.execute(
        "assistant.applyGrid",
        json!({"context": context, "grid": {"sheet_name": "Expenses", "cells": [
            {"cell": "A1", "input": "Date"}, {"cell": "B1", "input": "Amount"}
        ]}}),
    )
    .unwrap();
    assert_eq!(s.doc().unwrap().wb.sheets.len(), before + 1);
    assert_eq!(s.doc().unwrap().wb.active().unwrap().name, "Expenses");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.doc().unwrap().wb.sheets.len(), before);
}

#[test]
fn rejects_invalid_or_stale_plan_without_writing() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let invalid = json!({"sheet_name": null, "cells": [{"cell":"A1","input":"x"},{"cell":"A1","input":"y"}]});
    assert!(s.execute("assistant.applyGrid", json!({"context": context, "grid": invalid})).is_err());
    assert!(s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("A1").unwrap()).is_empty());
    s.execute("cell.set", json!({"cell":"B1", "input":"changed"})).unwrap();
    assert!(
        s.execute("assistant.applyGrid", json!({"context": context, "grid": {"sheet_name": null, "cells": [{"cell":"A1","input":"x"}]}})).is_err()
    );
}

#[test]
fn mixed_text_and_grid_changes_are_one_undo_step() {
    let mut s = session();
    s.execute("cell.set", json!({"cell":"A1", "input":"Old"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    s.execute(
        "assistant.applyGrid",
        json!({"context":context, "changes":[{"index":0,"text":"005"}],
        "grid":{"sheet_name":null,"cells":[{"cell":"B1","input":"42"}]}}),
    )
    .unwrap();
    assert_eq!(s.execute("cell.get", json!({"cell":"A1"})).unwrap()["type"], "text");
    assert_eq!(s.execute("cell.get", json!({"cell":"B1"})).unwrap()["value"], 42.0);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.execute("cell.get", json!({"cell":"A1"})).unwrap()["value"], "Old");
    assert!(s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("B1").unwrap()).is_empty());
}
