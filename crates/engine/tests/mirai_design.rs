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
