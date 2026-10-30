use gridcraft_engine::Session;
use serde_json::json;

fn session() -> Session {
    let mut s = Session::new();
    s.new_workbook();
    s
}

/// Fills a small table and selects a cell in the middle of it.
fn table(s: &mut Session) {
    let context = s.execute("assistant.context", json!({})).unwrap();
    let grid = json!({"sheet_name": null, "cells": [
        {"cell": "A1", "input": "Month"}, {"cell": "B1", "input": "Sales"},
        {"cell": "A2", "input": "Jan"}, {"cell": "B2", "input": "1200"},
        {"cell": "A3", "input": "Feb"}, {"cell": "B3", "input": "1500"},
        {"cell": "B5", "input": "=SUM(B2:B3)"}
    ]});
    s.execute("assistant.applyGrid", json!({"context": context, "grid": grid})).unwrap();
    s.execute("selection.set", json!({"range": "B2:B3"})).unwrap();
}

#[test]
fn window_lists_surrounding_cells_with_addresses() {
    let mut s = session();
    table(&mut s);
    let w = s.execute("assistant.formulaContext", json!({})).unwrap();
    assert_eq!(w["selection"], "B2:B3");
    assert_eq!(w["from"], "A1");
    let cells = w["cells"].as_array().unwrap();
    // The default window covers the whole table, formulas included as typed.
    let find = |a: &str| cells.iter().find(|c| c["cell"] == json!(a)).map(|c| c["text"].as_str().unwrap().to_owned());
    assert_eq!(find("A1").as_deref(), Some("Month"));
    assert_eq!(find("B2").as_deref(), Some("1200"));
    assert_eq!(find("B5").as_deref(), Some("=SUM(B2:B3)"));
    // Hostile or incoherent params are refused; a window of zero keeps just the selection.
    assert!(s.execute("assistant.formulaContext", json!({"window": 61})).is_err());
    assert!(s.execute("assistant.formulaContext", json!({"window": -1})).is_err());
    assert!(s.execute("assistant.formulaContext", json!({"window": "big"})).is_err());
    let tight = s.execute("assistant.formulaContext", json!({"window": 0})).unwrap();
    let cells = tight["cells"].as_array().unwrap();
    assert!(cells.iter().all(|c| c["cell"] == json!("B2") || c["cell"] == json!("B3")));
    assert_eq!(tight["from"], "B2");
    assert_eq!(tight["to"], "B3");
}

#[test]
fn window_text_is_capped_and_reads_nothing_beyond_it() {
    let mut s = session();
    table(&mut s);
    // Text far outside the window is never read: a huge cell beyond it is invisible.
    let far = json!({"cell": "CZ100", "input": "x".repeat(40_000)});
    let context = s.execute("assistant.context", json!({})).unwrap();
    s.execute("assistant.applyGrid", json!({"context": context, "grid": json!({"sheet_name": null, "cells": [far]})})).unwrap();
    let w = s.execute("assistant.formulaContext", json!({"window": 10})).unwrap();
    assert!(w["cells"].as_array().unwrap().iter().all(|c| c["cell"] != json!("CZ100")));
    // A window full of text refuses past its cap instead of truncating silently.
    let mut s = session();
    let cells: Vec<_> = (1..=61u32).map(|r| json!({"cell": format!("B{r}"), "input": "y".repeat(600)})).collect();
    let context = s.execute("assistant.context", json!({})).unwrap();
    s.execute("assistant.applyGrid", json!({"context": context, "grid": json!({"sheet_name": null, "cells": cells})})).unwrap();
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    assert!(s.execute("assistant.formulaContext", json!({"window": 60})).is_err());
}

#[test]
fn window_clamps_at_the_sheet_edges_and_survives_hostile_params() {
    let mut s = session();
    // A window near the sheet's top-left corner clamps instead of underflowing.
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    let w = s.execute("assistant.formulaContext", json!({})).unwrap();
    assert_eq!(w["from"], "A1");
    assert_eq!(w["to"], "P16");
    // Hostile params never panic.
    for bad in [json!({"window": 1e30}), json!({"window": null, "x": 1}), json!({"window": [1]})] {
        let _ = s.execute("assistant.formulaContext", bad);
    }
}

#[test]
fn numeric_selection_is_a_valid_context_and_applies() {
    // Regression: a selected numeric cell used to make every apply of its context fail as
    // "changed", because numbers hold no stored text for the length-window check.
    let mut s = session();
    table(&mut s);
    let context = s.execute("assistant.context", json!({})).unwrap();
    assert_eq!(context["targets"][0]["original"], "1200");
    s.execute("assistant.applyGrid", json!({"context": context, "grid": json!({"sheet_name": null, "cells": [{"cell": "D2", "input": "=B2*2"}]})}))
        .unwrap();
    let input = s.doc().unwrap().wb.active().unwrap().input_text(gridcraft_core::CellRef::parse("D2").unwrap());
    assert_eq!(input, "=B2*2");
}
