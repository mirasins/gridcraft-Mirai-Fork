//! Mirai Assistant backend: a read-only snapshot of a small range (`assistant.context`) and an
//! undoable, all-or-nothing apply of changes made against that snapshot (`assistant.apply`).
//! The snapshot is plain data. Nothing in these commands calls a model or the network.

use gridcraft_core::{CellRef, RangeRef, Value};
use gridcraft_model::{Cell, Formula};
use serde_json::{Value as Json, json};
use std::sync::Arc;

use super::edit::{check_arg_counts, parse_typed_formula};
use super::inspect::value_json;
use super::*;

/// Most cells one snapshot holds, and most changes one apply takes.
const MAX_TARGETS: usize = 256;
/// Most bytes of snapshot or change data one call takes, measured before anything is cloned.
const MAX_BYTES: usize = 64 * 1024;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "assistant.context", "Mirai Assistant Context", [], None, "{range?: \"A1:B2\" (default: the selected range, at most 256 cells), sheet?} → {document_id, revision, sheet, range, targets:[{cell, original:{value, formula?}, text}]}; read only", has_doc, context),
        cmd!(
            "assistant.apply",
            "Mirai Assistant Apply",
            [],
            None,
            "{context: <snapshot from assistant.context>, changes:[{index, value: scalar} | {index, formula: \"=…\"}], allow_formulas?: bool (default false)} → {cells}; one undo step, all or nothing",
            has_doc,
            apply
        ),
    ]
}

/// A writer that counts bytes and fails once they pass [`MAX_BYTES`], so a value is measured
/// without building its serialised text in memory.
struct Limit {
    written: usize,
}

impl std::io::Write for Limit {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written = self.written.saturating_add(buf.len());
        if self.written > MAX_BYTES { Err(std::io::Error::other("over the limit")) } else { Ok(buf.len()) }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Refuses a value whose serialised (escaped) form passes [`MAX_BYTES`]. Both the snapshot that
/// `assistant.context` returns and the parts `assistant.apply` takes are checked this way, so a
/// snapshot that context hands out always fits the apply side.
fn within_limit(v: &Json, cmd: &str, what: &str) -> Result<()> {
    let mut w = Limit { written: 0 };
    match serde_json::to_writer(&mut w, v) {
        Ok(()) => Ok(()),
        Err(_) => Err(bad(cmd, format!("`{what}` is larger than 64 KiB once escaped"))),
    }
}

/// The range a snapshot covers. A `range` that is not a string is an error, not the selection.
fn context_range(s: &Session, p: &Json) -> Result<RangeRef> {
    match p.get("range") {
        None | Some(Json::Null) => {
            let d = s.doc()?;
            match d.selection.ranges.as_slice() {
                [only] => Ok(*only),
                _ => Err(bad("assistant.context", "select one range or pass `range`")),
            }
        }
        Some(Json::String(r)) => {
            if r.contains(',') {
                return Err(bad("assistant.context", "pass one range"));
            }
            target_range(s, p)
        }
        Some(other) => Err(bad("assistant.context", format!("range {other}: pass a range such as \"A1:B2\""))),
    }
}

/// One entry per cell of `range`, in the order `RangeRef::iter` gives. The raw content is sized by
/// reference first, so a cell over the byte limit is refused before anything is cloned.
fn snapshot_targets(d: &DocState, si: usize, range: RangeRef) -> Result<Vec<Json>> {
    if range.count() > MAX_TARGETS as u64 {
        return Err(bad("assistant.context", format!("the range has more than {MAX_TARGETS} cells")));
    }
    let sh = d.wb.sheet(si).ok_or_else(|| bad("assistant.context", "no such sheet"))?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for at in range.iter() {
        let raw = sh.cell(at);
        // Spilled values can exist without a stored cell. Bound their displayed data too,
        // before value_json copies it into the snapshot.
        let displayed = sh.value_ref(at);
        if matches!(displayed, Some(Value::Array(_))) {
            return Err(bad("assistant.context", "array results can't be sent to the assistant"));
        }
        let displayed_len = match displayed {
            Some(Value::Text(t)) => t.len(),
            _ => 0,
        };
        let (value_len, formula_len) = match raw {
            Some(c) => {
                if matches!(c.value, Value::Array(_)) {
                    return Err(bad("assistant.context", "array results can't be sent to the assistant"));
                }
                let vlen = match &c.value {
                    Value::Text(t) => t.len(),
                    _ => 0,
                };
                (vlen, c.formula.as_ref().map_or(0, |f| f.text.len()))
            }
            None => (0, 0),
        };
        bytes = bytes.saturating_add(value_len).saturating_add(formula_len).saturating_add(displayed_len);
        if bytes > MAX_BYTES {
            return Err(bad("assistant.context", "the range holds more than 64 KiB of data"));
        }
        let value = sh.value(at);
        let text = raw.map(Cell::input_text).unwrap_or_default();
        let formula = raw.and_then(|c| c.formula.as_ref()).map(|f| f.text.clone());
        out.push(json!({"cell": at.a1(), "original": {"value": value_json(&value), "formula": formula}, "text": text}));
    }
    Ok(out)
}

fn context(s: &mut Session, p: &Json) -> Result<Json> {
    within_limit(p, "assistant.context", "params")?;
    match p.get("sheet") {
        None | Some(Json::Null) | Some(Json::String(_)) | Some(Json::Number(_)) => {}
        Some(other) => return Err(bad("assistant.context", format!("sheet {other}: a sheet name or index"))),
    }
    let si = target_sheet(s, p)?;
    let range = context_range(s, p)?;
    let d = s.doc()?;
    let targets = snapshot_targets(d, si, range)?;
    let name = d.wb.sheet(si).map(|sh| sh.name.clone()).unwrap_or_default();
    let snapshot = json!({"document_id": d.uid, "revision": d.revision, "sheet": name, "range": range.a1(), "targets": targets});
    within_limit(&snapshot, "assistant.context", "context")?;
    Ok(snapshot)
}

/// What one change does to its cell, decided before anything is edited.
enum Input {
    Clear,
    Value(Value),
    Formula(Formula),
}

/// A `value` entry: text is literal (never parsed, so "=SUM(1,2)" stays text), numbers and
/// booleans are stored as they are, and null clears the cell.
fn scalar_input(v: &Json) -> Result<Input> {
    Ok(match v {
        Json::Null => Input::Clear,
        Json::Bool(b) => Input::Value(Value::Bool(*b)),
        Json::Number(n) => {
            let f = n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad("assistant.apply", "number out of range"))?;
            Input::Value(Value::number(f))
        }
        Json::String(t) => {
            if t.len() > MAX_BYTES {
                return Err(bad("assistant.apply", "a text value is larger than 64 KiB"));
            }
            Input::Value(Value::Text(Arc::from(t.as_str())))
        }
        _ => return Err(bad("assistant.apply", "`value` must be a string, number, boolean or null")),
    })
}

/// A `formula` entry, only when the caller allowed formulas. It goes through the same parser and
/// argument check as typed entry.
fn formula_input(f: &Json) -> Result<Input> {
    let text = f.as_str().ok_or_else(|| bad("assistant.apply", "`formula` must be a string"))?;
    let body = text.strip_prefix('=').unwrap_or(text);
    if body.trim().is_empty() {
        return Err(bad("assistant.apply", "empty formula"));
    }
    let expr = parse_typed_formula(body)?;
    check_arg_counts(&expr)?;
    Ok(Input::Formula(Formula::from_expr(expr)))
}

fn apply(s: &mut Session, p: &Json) -> Result<Json> {
    let ctx = p
        .get("context")
        .filter(|v| v.is_object())
        .ok_or_else(|| bad("assistant.apply", "missing `context` (the snapshot from assistant.context)"))?;
    within_limit(ctx, "assistant.apply", "context")?;
    within_limit(p.get("changes").unwrap_or(&Json::Null), "assistant.apply", "changes")?;
    let changes = p.get("changes").and_then(Json::as_array).ok_or_else(|| bad("assistant.apply", "missing `changes` (array)"))?;
    if changes.len() > MAX_TARGETS {
        return Err(bad("assistant.apply", format!("more than {MAX_TARGETS} changes")));
    }
    let allow_formulas = match p.get("allow_formulas") {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(other) => return Err(bad("assistant.apply", format!("allow_formulas {other}: true or false"))),
    };

    let d = s.doc()?;
    if ctx.get("document_id").and_then(Json::as_u64) != Some(d.uid) {
        return Err(EngineError::Other("the workbook is not the one the context was taken from".into()));
    }
    if ctx.get("revision").and_then(Json::as_u64) != Some(d.revision) {
        return Err(EngineError::Other("the workbook changed since assistant.context; take a new context".into()));
    }
    let sheet = ctx.get("sheet").and_then(Json::as_str).ok_or_else(|| bad("assistant.apply", "context has no `sheet`"))?;
    let si = d.wb.sheet_index(sheet).ok_or_else(|| bad("assistant.apply", format!("no sheet named `{sheet}`")))?;
    let range_text = ctx.get("range").and_then(Json::as_str).ok_or_else(|| bad("assistant.apply", "context has no `range`"))?;
    let range = RangeRef::parse(range_text).ok_or_else(|| bad("assistant.apply", format!("not a range: `{range_text}`")))?;

    let fresh = snapshot_targets(d, si, range)?;
    let given = ctx.get("targets").and_then(Json::as_array).ok_or_else(|| bad("assistant.apply", "context has no `targets`"))?;
    if given.len() != fresh.len() || given.iter().zip(fresh.iter()).any(|(a, b)| a != b) {
        return Err(EngineError::Other("the original cells differ from the context; take a new context".into()));
    }
    let cells: Vec<CellRef> = range.iter().collect();
    let sh = d.wb.sheet(si).ok_or_else(|| bad("assistant.apply", "no such sheet"))?;

    let mut seen = vec![false; cells.len()];
    let mut plan: Vec<(CellRef, Input)> = Vec::with_capacity(changes.len());
    for ch in changes {
        let obj = ch.as_object().ok_or_else(|| bad("assistant.apply", "each change must be an object"))?;
        let index = obj
            .get("index")
            .and_then(Json::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| bad("assistant.apply", "`index` must name a target of the context"))?;
        let at = *cells.get(index).ok_or_else(|| bad("assistant.apply", "`index` must name a target of the context"))?;
        let slot = seen.get_mut(index).ok_or_else(|| bad("assistant.apply", "`index` must name a target of the context"))?;
        if std::mem::replace(slot, true) {
            return Err(bad("assistant.apply", format!("duplicate index {index}")));
        }
        let input = match (obj.get("value"), obj.get("formula")) {
            (Some(v), None) => scalar_input(v)?,
            (None, Some(f)) => {
                if !allow_formulas {
                    return Err(bad("assistant.apply", "formulas aren't allowed; pass allow_formulas: true"));
                }
                formula_input(f)?
            }
            _ => return Err(bad("assistant.apply", "each change needs exactly one of `value` or `formula`")),
        };
        if sh.is_protected()
            && d.wb.styles.get(sh.style_id(at)).protection.locked
            && let Some(m) = locked_cell_error(sh, at, &d.unlocked_ranges)
        {
            return Err(EngineError::Other(m));
        }
        plan.push((at, input));
    }

    let n = plan.len();
    if n == 0 {
        return Ok(json!({"cells": 0}));
    }
    edit(s, |cx| {
        for (at, input) in plan {
            let sh = cx.sheet_mut(si)?;
            let style = sh.style_id(at);
            let cell = match input {
                Input::Clear => Cell { style, ..Cell::default() },
                Input::Value(v) => Cell { value: v, formula: None, style },
                Input::Formula(f) => Cell { value: Value::Empty, formula: Some(Arc::new(f)), style },
            };
            if cell.is_blank() {
                sh.remove_cell(at);
            } else {
                sh.set_cell(at, cell);
            }
            super::data::grow_autofilter(sh, at);
            cx.touch(si, at);
        }
        Ok(())
    })?;
    Ok(json!({"cells": n}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(cells: &[(&str, Json)]) -> Session {
        let mut s = Session::new();
        s.new_workbook();
        for (a, v) in cells {
            s.execute("cell.set", json!({"cell": a, "input": v})).unwrap();
        }
        s
    }

    fn put_raw_text(s: &mut Session, a: &str, text: &str) {
        let d = s.doc_mut().unwrap();
        let wb = std::sync::Arc::make_mut(&mut d.wb);
        let sh = wb.sheet_mut(0).unwrap();
        sh.set_cell(CellRef::parse(a).unwrap(), Cell::value(Value::Text(Arc::from(text))));
    }

    fn get(s: &mut Session, a: &str) -> Json {
        s.execute("cell.get", json!({"cell": a})).unwrap()["value"].clone()
    }

    fn snapshot(s: &mut Session, range: &str) -> Json {
        s.execute("assistant.context", json!({"range": range})).unwrap()
    }

    #[test]
    fn context_is_read_only_and_limited() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!("x"))]);
        let before = s.doc().unwrap().revision;
        let undo = s.doc().unwrap().undo.len();
        let ctx = snapshot(&mut s, "A1:B1");
        assert_eq!(ctx["targets"].as_array().unwrap().len(), 2);
        assert_eq!(s.doc().unwrap().revision, before);
        assert_eq!(s.doc().unwrap().undo.len(), undo);
        assert!(s.execute("assistant.context", json!({"range": "A1:Z100"})).is_err());
        assert!(s.execute("assistant.context", json!({"range": "A:A"})).is_err());
    }

    #[test]
    fn context_refuses_huge_text() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        put_raw_text(&mut s, "A1", &"a".repeat(MAX_BYTES + 1));
        assert!(s.execute("assistant.context", json!({"range": "A1"})).is_err());
    }

    #[test]
    fn context_bounds_spilled_text_before_copying() {
        let mut s = sheet_with(&[]);
        let sheet = Arc::make_mut(&mut s.doc_mut().unwrap().wb).sheet_mut(0).unwrap();
        sheet.spill.insert(CellRef::parse("A1").unwrap(), Value::Text(Arc::from("x".repeat(MAX_BYTES + 1))));
        assert!(s.execute("assistant.context", json!({"range": "A1"})).is_err());
        assert!(s.execute("assistant.context", json!({"sheet": "x".repeat(MAX_BYTES + 1)})).is_err());
    }

    #[test]
    fn malformed_range_is_refused_not_replaced_by_selection() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        for range in [json!(5), json!(true), json!("nonsense"), json!("A1,B1"), json!("")] {
            assert!(s.execute("assistant.context", json!({"range": range})).is_err(), "{range}");
        }
    }

    #[test]
    fn apply_two_cells_is_one_undo_step() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!(2))]);
        let ctx = snapshot(&mut s, "A1:B1");
        s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": "x"}, {"index": 1, "value": 5}]})).unwrap();
        assert_eq!(get(&mut s, "A1"), json!("x"));
        assert_eq!(get(&mut s, "B1"), json!(5.0));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(get(&mut s, "A1"), json!(1.0));
        assert_eq!(get(&mut s, "B1"), json!(2.0));
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(get(&mut s, "B1"), json!(5.0));
    }

    #[test]
    fn stale_context_is_refused() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        let ctx = snapshot(&mut s, "A1");
        let change = json!([{"index": 0, "value": 9}]);
        let mut wrong_uid = ctx.clone();
        wrong_uid["document_id"] = json!(ctx["document_id"].as_u64().unwrap() + 1);
        assert!(s.execute("assistant.apply", json!({"context": wrong_uid, "changes": change})).is_err());
        let mut wrong_rev = ctx.clone();
        wrong_rev["revision"] = json!(ctx["revision"].as_u64().unwrap() + 1);
        assert!(s.execute("assistant.apply", json!({"context": wrong_rev, "changes": change})).is_err());
        s.execute("cell.set", json!({"cell": "A1", "input": 2})).unwrap();
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": change})).is_err());
        assert_eq!(get(&mut s, "A1"), json!(2.0));
    }

    #[test]
    fn original_mismatch_is_refused_at_the_same_revision() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        let mut ctx = snapshot(&mut s, "A1");
        ctx["targets"][0]["original"]["value"] = json!(99);
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": 9}]})).is_err());
        assert_eq!(get(&mut s, "A1"), json!(1.0));
    }

    #[test]
    fn hostile_changes_are_refused_before_editing() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!(2))]);
        let ctx = snapshot(&mut s, "A1:B1");
        let bad_changes = [
            json!([{"index": 0, "value": 1}, {"index": 0, "value": 2}]),
            json!([{"index": 2, "value": 1}]),
            json!([{"index": -1, "value": 1}]),
            json!([{"index": 0, "value": [1]}]),
            json!([{"index": 0, "value": 1, "formula": "=1"}]),
            json!([{"index": 0}]),
            json!([{"index": 0, "value": 1}, {"index": 1, "formula": "=1"}]),
            json!(["x"]),
        ];
        for changes in bad_changes {
            assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": changes})).is_err());
        }
        assert_eq!(get(&mut s, "A1"), json!(1.0));
        assert_eq!(get(&mut s, "B1"), json!(2.0));
    }

    #[test]
    fn huge_text_is_refused_in_apply() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        let ctx = snapshot(&mut s, "A1");
        let huge = "a".repeat(MAX_BYTES + 1);
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": huge}]})).is_err());
        assert_eq!(get(&mut s, "A1"), json!(1.0));
    }

    #[test]
    fn escaped_text_counts_its_escaped_size() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        let ctx = snapshot(&mut s, "A1");
        // 12,000 raw bytes, but each control character escapes to six bytes of JSON.
        let escaped = "\u{1}".repeat(12_000);
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": escaped}]})).is_err());
        assert_eq!(get(&mut s, "A1"), json!(1.0));
    }

    #[test]
    fn context_refuses_escaped_text_that_apply_would_refuse() {
        let mut s = sheet_with(&[("A1", json!(1))]);
        // Under 64 KiB raw, over it once every control character is escaped.
        put_raw_text(&mut s, "A1", &"\u{1}".repeat(12_000));
        assert!(s.execute("assistant.context", json!({"range": "A1"})).is_err());
    }

    #[test]
    fn context_snapshot_round_trips_through_apply() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!("x"))]);
        let ctx = snapshot(&mut s, "A1:B1");
        let before = s.doc().unwrap().revision;
        let undo = s.doc().unwrap().undo.len();
        s.execute("assistant.apply", json!({"context": ctx, "changes": []})).unwrap();
        assert_eq!(s.doc().unwrap().revision, before);
        assert_eq!(s.doc().unwrap().undo.len(), undo);
        let r = s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 1, "value": "y"}]})).unwrap();
        assert_eq!(r["cells"], json!(1));
        assert_eq!(get(&mut s, "B1"), json!("y"));
    }

    #[test]
    fn text_stays_literal_and_formulas_need_permission() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!(2))]);
        let ctx = snapshot(&mut s, "A1:B1");
        s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": "=SUM(1,2)"}]})).unwrap();
        assert_eq!(get(&mut s, "A1"), json!("=SUM(1,2)"));
        let ctx = snapshot(&mut s, "A1:B1");
        let f = json!([{"index": 1, "formula": "=A1*2"}]);
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": f.clone()})).is_err());
        s.execute("assistant.apply", json!({"context": ctx, "changes": f, "allow_formulas": true})).unwrap();
        assert_eq!(s.execute("cell.get", json!({"cell": "B1"})).unwrap()["formula"], json!("=A1*2"));
    }

    #[test]
    fn valid_and_invalid_formulas_under_permission() {
        let mut s = sheet_with(&[("A1", json!(1)), ("A2", json!(2)), ("B1", json!(0))]);
        let ctx = snapshot(&mut s, "B1");
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "formula": "=1+*"}], "allow_formulas": true})).is_err());
        s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "formula": "=SUM(A1:A2)"}], "allow_formulas": true})).unwrap();
        assert_eq!(get(&mut s, "B1"), json!(3.0));
    }

    #[test]
    fn protected_sheet_refuses_the_whole_batch() {
        let mut s = sheet_with(&[("A1", json!(1)), ("B1", json!(2))]);
        s.execute("review.protectSheet", json!({})).unwrap();
        let ctx = snapshot(&mut s, "A1:B1");
        assert!(s.execute("assistant.apply", json!({"context": ctx, "changes": [{"index": 0, "value": 7}, {"index": 1, "value": 8}]})).is_err());
        assert_eq!(get(&mut s, "A1"), json!(1.0));
        assert_eq!(get(&mut s, "B1"), json!(2.0));
    }
}
