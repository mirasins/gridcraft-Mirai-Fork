use gridcraft_engine::Session;
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_workbook();
    s
}

#[test]
fn applies_style_width_and_table_as_one_undo_step() {
    let mut s = session();
    s.execute("cell.set", json!({"cell": "A1", "input": "Name"})).unwrap();
    s.execute("cell.set", json!({"cell": "B1", "input": "Amount"})).unwrap();
    s.execute("cell.set", json!({"cell": "A2", "input": "Rent"})).unwrap();
    s.execute("cell.set", json!({"cell": "B2", "input": "42"})).unwrap();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({
        "styles": [{"range": "A1:B1", "style": {"font": {"bold": true}}}],
        "columnWidths": [{"cols": "A:A", "chars": 12.0}],
        "autofitColumns": ["B:B"],
        "table": {"range": "A1:B2", "name": "Expenses", "style": "TableStyleMedium2", "header": true}
    });
    s.execute("assistant.applyGrid", json!({"context": context, "design": design})).unwrap();
    let doc = s.doc().unwrap();
    let sheet = doc.wb.active().unwrap();
    assert!(sheet.tables.iter().any(|t| t.name == "Expenses"));
    s.execute("edit.undo", json!({})).unwrap();
    let doc = s.doc().unwrap();
    let sheet = doc.wb.active().unwrap();
    assert!(sheet.tables.is_empty());
}

#[test]
fn rejects_invalid_design_without_writing() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let invalid = json!({"columnWidths": [{"cols": "A:A", "width": 9999.0}]});
    assert!(s.execute("assistant.applyGrid", json!({"context": context, "design": invalid})).is_err());
}

#[test]
fn null_table_field_is_ignored() {
    // The output schema allows `table` to be null; the engine must not treat that as "a table".
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({"rowHeights": [{"rows": "1:1", "points": 20.0}], "table": null});
    s.execute("assistant.applyGrid", json!({"context": context, "design": design})).unwrap();
}

#[test]
fn apply_design_command_works() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({"rowHeights": [{"rows": "1:1", "points": 20.0}]});
    s.execute("assistant.applyDesign", json!({"context": context, "design": design})).unwrap();
}

#[test]
fn stale_context_refuses_design() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    s.execute("cell.set", json!({"cell": "B1", "input": "changed"})).unwrap();
    let design = json!({"rowHeights": [{"rows": "1:1", "points": 20.0}]});
    assert!(s.execute("assistant.applyDesign", json!({"context": context, "design": design})).is_err());
}

#[test]
fn hex_colours_in_style_patches_apply_as_one_undo_step() {
    // The model writes colours as #RRGGBB; the typed style takes its colour enum. Regression for
    // "unknown variant `#7F7F7F`, expected one of `Auto`, `Rgb`, `Theme`".
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({"styles": [{"range": "A1:B2", "style": {
        "font": {"bold": true, "color": "#1F4E78"},
        "fill": {"pattern": "Solid", "fg": "#DCE6F1"},
        "borders": {"bottom": {"style": "Thin", "color": "#7F7F7F"}}
    }}]});
    s.execute("assistant.applyGrid", json!({"context": context, "design": design})).unwrap();
    let a1 = s.execute("cell.get", json!({"cell": "A1"})).unwrap();
    assert_eq!(a1["style"]["font"]["bold"], json!(true));
    assert_eq!(a1["style"]["font"]["color"], json!({"Rgb": 0x1F4E78}));
    assert_eq!(a1["style"]["fill"]["pattern"], json!("Solid"));
    assert_eq!(a1["style"]["fill"]["fg"], json!({"Rgb": 0xDCE6F1}));
    assert_eq!(a1["style"]["borders"]["bottom"]["style"], json!("Thin"));
    assert_eq!(a1["style"]["borders"]["bottom"]["color"], json!({"Rgb": 0x7F7F7F}));
    s.execute("edit.undo", json!({})).unwrap();
    let a1 = s.execute("cell.get", json!({"cell": "A1"})).unwrap();
    assert_eq!(a1["style"]["font"]["color"], json!("Auto"));
}

#[test]
fn auto_colours_and_a_plain_fill_string_apply() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({"styles": [
        {"range": "A1", "style": {"font": {"color": "auto"}}},
        {"range": "A2", "style": {"fill": "#DCE6F1"}}
    ]});
    s.execute("assistant.applyGrid", json!({"context": context, "design": design})).unwrap();
    let a1 = s.execute("cell.get", json!({"cell": "A1"})).unwrap();
    assert_eq!(a1["style"]["font"]["color"], json!("Auto"));
    let a2 = s.execute("cell.get", json!({"cell": "A2"})).unwrap();
    assert_eq!(a2["style"]["fill"]["pattern"], json!("Solid"));
    assert_eq!(a2["style"]["fill"]["fg"], json!({"Rgb": 0xDCE6F1}));
}

#[test]
fn an_unrecognised_colour_is_refused_without_changes() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({"styles": [{"range": "A1", "style": {"font": {"color": "bright red"}}}]});
    assert!(s.execute("assistant.applyGrid", json!({"context": context, "design": design})).is_err());
    let a1 = s.execute("cell.get", json!({"cell": "A1"})).unwrap();
    assert_eq!(a1["style"]["font"]["color"], json!("Auto"));
}

#[test]
fn bare_and_listed_columns_and_rows_are_accepted() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let design = json!({
        "columnWidths": [{"cols": "B", "width": 120.0}, {"col": "D", "width": 80.0}, {"cols": " F:F, H ", "chars": 10.0}],
        "autofitColumns": ["C"],
        "autofitRows": ["2"],
        "rowHeights": [{"rows": "3", "points": 20.0}]
    });
    s.execute("assistant.applyGrid", json!({"context": context, "design": design})).unwrap();
    let doc = s.doc().unwrap();
    let sheet = doc.wb.active().unwrap();
    let width = |c: u32| sheet.cols.get(&c).and_then(|e| e.size);
    assert_eq!(width(1), Some(120.0));
    assert_eq!(width(3), Some(80.0));
    assert!(width(5).is_some() && width(7).is_some());
}

#[test]
fn a_malformed_column_spec_is_refused() {
    let mut s = session();
    let context = s.execute("assistant.context", json!({})).unwrap();
    let invalid = json!({"columnWidths": [{"cols": "B:2", "width": 50.0}]});
    assert!(s.execute("assistant.applyGrid", json!({"context": context, "design": invalid})).is_err());
}
