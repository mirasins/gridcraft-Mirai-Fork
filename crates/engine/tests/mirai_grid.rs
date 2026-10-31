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

#[test]
fn echoes_current_sheet_name_instead_of_creating_a_duplicate() {
    let mut s = session();
    s.execute("cell.set", json!({"cell": "B2", "input": "1200"})).unwrap();
    s.execute("cell.set", json!({"cell": "B3", "input": "1300"})).unwrap();
    s.execute("cell.set", json!({"cell": "B4", "input": "1900"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let before = s.doc().unwrap().wb.sheets.len();
    // Models sometimes echo the current sheet's name where `null` was meant.
    s.execute(
        "assistant.applyGrid",
        json!({"context": context, "grid": {"sheet_name": "Sheet1", "cells": [
            {"cell": "C6", "input": "=SUM(B2:B4)"}
        ]}}),
    )
    .unwrap();
    assert_eq!(s.doc().unwrap().wb.sheets.len(), before, "no duplicate sheet is created");
    assert_eq!(s.doc().unwrap().wb.active().unwrap().name, "Sheet1");
    assert_eq!(s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("C6").unwrap()), "=SUM(B2:B4)");
}

fn text(s: &Session, cell: &str) -> String {
    s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse(cell).unwrap())
}

/// A small table of existing data in A1:B3.
fn with_data() -> Session {
    let mut s = session();
    for (cell, input) in [("A1", "Item"), ("B1", "Cost"), ("A2", "Rent"), ("B2", "900"), ("A3", "Food"), ("B3", "300")] {
        s.execute("cell.set", json!({"cell": cell, "input": input})).unwrap();
    }
    s
}

#[test]
fn without_a_selection_new_data_moves_below_existing_data() {
    let mut s = with_data();
    // Only the cursor, on an empty cell: nothing selected to edit.
    s.execute("selection.set", json!({"range": "D1"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [
        {"cell": "A1", "input": "Month"},
        {"cell": "A2", "input": "10"},
        {"cell": "A3", "input": "=A2*2+B2"}
    ]});
    let design = json!({"styles": [{"range": "A1", "style": {"font": {"bold": true}}}], "rowHeights": [{"rows": "1:1", "points": 20.0}]});
    let out = s.execute("assistant.applyGrid", json!({"context": context, "grid": grid, "design": design})).unwrap();
    assert_eq!(out["relocated"], json!({"from": "A1:A3", "to": "A5"}));
    // The existing data is untouched.
    assert_eq!(text(&s, "A1"), "Item");
    assert_eq!(text(&s, "A2"), "Rent");
    assert_eq!(text(&s, "A3"), "Food");
    // The block starts one blank row below; a reference into the block follows it, one to the
    // existing data stays.
    assert!(text(&s, "A4").is_empty());
    assert_eq!(text(&s, "A5"), "Month");
    assert_eq!(text(&s, "A6"), "10");
    assert_eq!(text(&s, "A7"), "=A6*2+B2");
    let sh = s.doc().unwrap().wb.active().unwrap().clone();
    let style = |cell: &str| sh.style_id(gridcraft_core::CellRef::parse(cell).unwrap());
    assert_ne!(style("A5"), style("A4"), "the style moved with the block");
    assert_eq!(style("A1"), style("A4"), "the existing header keeps its style");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(text(&s, "A5").is_empty());
}

#[test]
fn without_a_selection_a_new_table_stays_off_existing_tables() {
    let mut s = with_data();
    s.execute("insert.table", json!({"range": "A1:B3"})).unwrap();
    s.execute("selection.set", json!({"range": "H20"})).unwrap();
    let area = s.execute("assistant.freeArea", json!({})).unwrap();
    assert_eq!(area["free"], "A5");
    assert_eq!(area["tables"], json!(["A1:B3"]));
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [{"cell": "B2", "input": "Q1"}, {"cell": "C2", "input": "Q2"}]});
    let design = json!({"table": {"range": "B2:C3", "header": true}});
    let out = s.execute("assistant.applyGrid", json!({"context": context, "grid": grid, "design": design})).unwrap();
    assert_eq!(out["relocated"]["to"], "B5");
    assert_eq!(text(&s, "B2"), "900");
    assert_eq!(text(&s, "B5"), "Q1");
    let tables: Vec<String> = s.doc().unwrap().wb.active().unwrap().tables.iter().map(|t| t.range.a1()).collect();
    assert_eq!(tables, ["A1:B3", "B5:C6"]);
}

#[test]
fn with_a_selection_mirai_writes_where_the_model_says() {
    let mut s = with_data();
    // The user selected cells to work on: no move, even onto other occupied cells.
    s.execute("selection.set", json!({"range": "A1:B3"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [{"cell": "B2", "input": "950"}, {"cell": "C1", "input": "Total"}]});
    let out = s.execute("assistant.applyGrid", json!({"context": context, "grid": grid})).unwrap();
    assert!(out["relocated"].is_null());
    assert_eq!(text(&s, "B2"), "950");
    assert_eq!(text(&s, "C1"), "Total");
}

#[test]
fn without_a_selection_free_space_is_used_as_is() {
    let mut s = with_data();
    s.execute("selection.set", json!({"range": "D1"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [{"cell": "D1", "input": "Note"}]});
    let out = s.execute("assistant.applyGrid", json!({"context": context, "grid": grid})).unwrap();
    assert!(out["relocated"].is_null());
    assert_eq!(text(&s, "D1"), "Note");
}
