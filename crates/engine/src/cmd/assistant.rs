//! Mirai writing assistant, engine side (text v1). `assistant.context` is a query that snapshots
//! the selected plain-text cells; `assistant.apply` writes validated literal text into those cells
//! as one undo step. Nothing here talks to a model: the transport lives outside the engine.

use std::collections::BTreeSet;

use gridcraft_core::{CellRef, Value};
use gridcraft_model::Sheet;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use super::{CommandSpec, bad, cmd, has_doc};
use crate::{EngineError, Result, Session};

/// Most cells one snapshot holds (checked on the selection's area before any cell is visited) and
/// most changes one apply takes.
pub const MAX_CELLS: usize = 256;
/// Most bytes of cell text in one snapshot (the originals) and in one apply (the new text).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Longest cell address or label a snapshot may carry back.
const MAX_ADDRESS_BYTES: usize = 64;
const MAX_SCOPE_BYTES: usize = 16;
const MAX_SHEET_NAME_BYTES: usize = 256;
const CONTEXT_KEYS: &[&str] = &["document_id", "revision", "scope", "sheet", "sheet_name", "targets"];
const TARGET_KEYS: &[&str] = &["index", "label", "cell", "original"];
const CHANGE_KEYS: &[&str] = &["index", "text", "markdown"];
const GROUP_LABEL: &str = "Mirai Assistant";
const SCOPE_LIMIT: &str = "only the \"selection\" scope is supported in this version";
const PROTECTED: &str = "the sheet is protected; the assistant can't change protected sheets in this version";
const MARKDOWN: &str = "Markdown suggestions aren't supported in this version; send plain text (markdown: false)";
const STALE_DOCUMENT: &str = "the document changed since the assistant read it; ask again for a new context";
const STALE_SHEET: &str = "the sheet changed since the assistant read it; ask again for a new context";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "assistant.context",
            "Mirai Assistant Context",
            [],
            None,
            "{scope?: \"selection\"} → {document_id, revision, scope, sheet, sheet_name, targets: [{index, label, cell, original}]} (plain text cells only; at most 256 cells and 64 KiB of text)",
            has_doc,
            context
        ),
        cmd!(
            "assistant.apply",
            "Apply Mirai Assistant Changes",
            [],
            None,
            "{context: <the whole snapshot from assistant.context>, changes: [{index, text, markdown?: false}]} (literal text only: a leading =, +, - or @ is refused; one undo step)",
            has_doc,
            apply
        ),
    ]
}

/// The snapshot: what the assistant read, and what [`apply`] checks before it writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    document_id: u64,
    revision: u64,
    scope: String,
    sheet: usize,
    sheet_name: String,
    targets: Vec<Target>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    index: usize,
    label: String,
    cell: String,
    original: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    index: usize,
    text: String,
    #[serde(default)]
    markdown: Option<bool>,
}

fn refuse(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

fn context(s: &mut Session, p: &Json) -> Result<Json> {
    match p.get("scope") {
        None | Some(Json::Null) => {}
        Some(Json::String(scope)) if scope == "selection" => {}
        Some(_) => return Err(bad("assistant.context", SCOPE_LIMIT)),
    }
    let snap = snapshot(s)?;
    serde_json::to_value(snap).map_err(|e| EngineError::Other(e.to_string()))
}

fn snapshot(s: &Session) -> Result<Snapshot> {
    let d = s.doc()?;
    let si = d.wb.active_sheet();
    let sh = d.wb.sheet(si).ok_or(EngineError::NoDocument)?;
    if sh.is_protected() {
        return Err(refuse(PROTECTED));
    }
    // The area is checked before any cell is visited, so a whole-sheet selection costs nothing.
    let area = d.selection.ranges.iter().fold(0u64, |n, r| n.saturating_add(r.count()));
    if area > MAX_CELLS as u64 {
        return Err(refuse(format!("the selection holds {area} cells; the assistant takes at most {MAX_CELLS} (select less)")));
    }
    // Overlapping areas are read once, in row order.
    let cells: BTreeSet<CellRef> = d.selection.ranges.iter().flat_map(|r| r.iter()).collect();
    let mut targets = Vec::with_capacity(cells.len());
    let mut bytes = 0usize;
    for (index, at) in cells.into_iter().enumerate() {
        check_plain(sh, at)?;
        // Refused from the stored length, before the input text is copied.
        if bytes.saturating_add(stored_text_len(sh, at)) > MAX_TEXT_BYTES {
            return Err(refuse(format!("the selected text is more than {MAX_TEXT_BYTES} bytes; select less")));
        }
        let original = sh.input_text(at);
        bytes = bytes.saturating_add(original.len());
        if bytes > MAX_TEXT_BYTES {
            return Err(refuse(format!("the selected text is more than {MAX_TEXT_BYTES} bytes; select less")));
        }
        let cell = at.a1();
        targets.push(Target { index, label: format!("cell {cell}"), cell, original });
    }
    Ok(Snapshot { document_id: d.uid, revision: d.revision, scope: "selection".into(), sheet: si, sheet_name: sh.name.clone(), targets })
}

/// Refuses cells the assistant can't rewrite as plain text without changing what they are.
fn check_plain(sh: &Sheet, at: CellRef) -> Result<()> {
    let why = if sh.cell(at).is_some_and(|c| c.formula.is_some()) {
        "holds a formula"
    } else if sh.cell_pictures.contains_key(&at) {
        "holds a picture"
    } else if sh.merge_at(at).is_some() {
        "is part of a merged cell"
    } else if sh.spill_anchor(at).is_some() || sh.spill_ranges.contains_key(&at) {
        "is part of a spilled array"
    } else {
        return Ok(());
    };
    Err(refuse(format!("{} {why}; the assistant changes plain text cells only in this version", at.a1())))
}

fn apply(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "assistant.apply";
    let context = p.get("context").filter(|c| c.is_object()).ok_or_else(|| bad(CMD, "missing `context` (the snapshot from assistant.context)"))?;
    let raw_changes = match p.get("changes") {
        Some(Json::Array(changes)) => changes,
        _ => return Err(bad(CMD, "missing `changes` (an array)")),
    };
    check_shape(context, raw_changes)?;
    let snap: Snapshot = serde_json::from_value(context.clone()).map_err(|e| bad(CMD, format!("bad context: {e}")))?;
    let changes: Vec<Change> = raw_changes
        .iter()
        .map(|c| serde_json::from_value::<Change>(c.clone()).map_err(|e| bad(CMD, format!("bad change: {e}"))))
        .collect::<Result<_>>()?;
    verify(s, &snap)?;

    let mut used = BTreeSet::new();
    let mut plan: Vec<(String, String)> = Vec::with_capacity(changes.len());
    for change in &changes {
        if change.markdown == Some(true) {
            return Err(refuse(MARKDOWN));
        }
        let target = snap
            .targets
            .get(change.index)
            .ok_or_else(|| bad(CMD, format!("index {} is not one of the {} targets", change.index, snap.targets.len())))?;
        if !used.insert(change.index) {
            return Err(bad(CMD, format!("index {} appears more than once", change.index)));
        }
        check_text(&change.text)?;
        plan.push((target.cell.clone(), change.text.clone()));
    }

    let sheet = snap.sheet;
    if !plan.is_empty() {
        // Every check has passed. Each write is a literal `range.setValues` (no typed parsing), and
        // a failure part-way rolls the whole group back.
        s.grouped(GROUP_LABEL, |s| {
            for (cell, text) in &plan {
                s.execute("range.setValues", json!({"sheet": sheet, "range": cell, "values": [[text]], "parse": false}))?;
            }
            Ok(())
        })?;
    }
    let d = s.doc()?;
    Ok(json!({"applied": plan.len(), "document_id": d.uid, "revision": d.revision, "scope": "selection"}))
}

/// Caps and shapes checked on the raw JSON, before anything is deserialized or copied: unknown keys
/// at every level, bounded strings, and the value types the typed structs need.
fn check_shape(context: &Json, changes: &[Json]) -> Result<()> {
    const CMD: &str = "assistant.apply";
    if changes.len() > MAX_CELLS {
        return Err(refuse(format!("{} changes; the limit is {MAX_CELLS}", changes.len())));
    }
    only_keys(context, CONTEXT_KEYS, "context")?;
    str_within(context, "scope", MAX_SCOPE_BYTES, "scope")?;
    str_within(context, "sheet_name", MAX_SHEET_NAME_BYTES, "sheet_name")?;
    let targets = match context.get("targets") {
        Some(Json::Array(targets)) => targets,
        _ => return Err(bad(CMD, "context has no `targets` list")),
    };
    if targets.len() > MAX_CELLS {
        return Err(refuse(format!("{} targets; the limit is {MAX_CELLS}", targets.len())));
    }
    let mut original_bytes = 0usize;
    for t in targets {
        only_keys(t, TARGET_KEYS, "a target")?;
        str_within(t, "cell", MAX_ADDRESS_BYTES, "a target's cell")?;
        str_within(t, "label", MAX_ADDRESS_BYTES, "a target's label")?;
        let original = t.get("original").and_then(Json::as_str).ok_or_else(|| bad(CMD, "each target needs an `original` string"))?;
        original_bytes = original_bytes.saturating_add(original.len());
        if original_bytes > MAX_TEXT_BYTES {
            return Err(refuse(format!("the snapshot's text is more than {MAX_TEXT_BYTES} bytes")));
        }
    }
    let mut text_bytes = 0usize;
    for c in changes {
        only_keys(c, CHANGE_KEYS, "a change")?;
        match c.get("markdown") {
            None | Some(Json::Null) | Some(Json::Bool(false)) => {}
            Some(Json::Bool(true)) => return Err(refuse(MARKDOWN)),
            Some(_) => return Err(bad(CMD, "`markdown` must be true or false")),
        }
        let text = c.get("text").and_then(Json::as_str).ok_or_else(|| bad(CMD, "each change needs a `text` string"))?;
        text_bytes = text_bytes.saturating_add(text.len());
        if text_bytes > MAX_TEXT_BYTES {
            return Err(refuse(format!("the new text is more than {MAX_TEXT_BYTES} bytes")));
        }
    }
    Ok(())
}

/// Refuses a value that is not a JSON object, or that has a key outside `allowed`. The key itself
/// is not echoed back: it is caller data.
fn only_keys<'a>(v: &'a Json, allowed: &[&str], what: &str) -> Result<&'a serde_json::Map<String, Json>> {
    let Json::Object(map) = v else { return Err(bad("assistant.apply", format!("{what} must be an object"))) };
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(bad("assistant.apply", format!("{what} has a field the assistant doesn't accept")));
    }
    Ok(map)
}

/// A string field that must be present and at most `max` bytes.
fn str_within(v: &Json, key: &str, max: usize, what: &str) -> Result<()> {
    match v.get(key).and_then(Json::as_str) {
        Some(s) if s.len() <= max => Ok(()),
        Some(_) => Err(refuse(format!("{what} is longer than {max} bytes"))),
        None => Err(bad("assistant.apply", format!("{what} must be a string"))),
    }
}

/// Bytes of a cell's stored text, read without copying it: the input text is this, or it with one
/// leading apostrophe. Numbers, booleans and errors have short input text and count as zero.
fn stored_text_len(sh: &Sheet, at: CellRef) -> usize {
    match sh.value(at) {
        Value::Text(t) => t.len(),
        _ => 0,
    }
}

/// The snapshot must still describe the document: same identity and revision, same sheet, and
/// every target cell still plain and still holding its original text. The selection may have
/// moved since; the changes go to the cells the snapshot names.
fn verify(s: &Session, snap: &Snapshot) -> Result<()> {
    let d = s.doc()?;
    if snap.scope != "selection" {
        return Err(refuse(SCOPE_LIMIT));
    }
    if snap.document_id != d.uid || snap.revision != d.revision {
        return Err(refuse(STALE_DOCUMENT));
    }
    let sh = d.wb.sheet(snap.sheet).filter(|sh| sh.name == snap.sheet_name).ok_or_else(|| refuse(STALE_SHEET))?;
    if sh.is_protected() {
        return Err(refuse(PROTECTED));
    }
    let mut seen = BTreeSet::new();
    for (i, t) in snap.targets.iter().enumerate() {
        let at = CellRef::parse(&t.cell).filter(|c| c.is_valid() && c.a1() == t.cell);
        let Some(at) = at else { return Err(refuse(format!("target {i} isn't a cell address"))) };
        if t.index != i || !seen.insert(at) {
            return Err(refuse("the snapshot's targets are out of order or repeat a cell"));
        }
        check_plain(sh, at)?;
        // Input text is the stored text or that plus one apostrophe, so a length outside that
        // window can't match the original; refuse without copying the cell.
        let stored = stored_text_len(sh, at);
        if t.original.len() < stored || t.original.len() > stored.saturating_add(1) {
            return Err(refuse(format!("{} changed since the assistant read it; ask again", t.cell)));
        }
        if sh.input_text(at) != t.original {
            return Err(refuse(format!("{} changed since the assistant read it; ask again", t.cell)));
        }
    }
    Ok(())
}

/// Literal text only: no control characters other than line breaks and tabs, no object
/// placeholder, and nothing that would read as a formula once typed.
fn check_text(text: &str) -> Result<()> {
    let bad_char = |c: char| c == '\u{FFFC}' || c == '\u{2028}' || c == '\u{2029}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t'));
    if text.chars().any(bad_char) {
        return Err(refuse("the text holds a control or object character; only line breaks and tabs are allowed"));
    }
    if text.trim_start().starts_with(['=', '+', '-', '@']) {
        return Err(refuse("the text starts with =, +, - or @, which would be read as a formula; the assistant writes plain text only"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcraft_core::CellRef;

    fn session() -> Session {
        let mut s = Session::new();
        s.new_workbook();
        s
    }

    fn set(s: &mut Session, cell: &str, input: &str) {
        s.execute("cell.set", json!({"cell": cell, "input": input})).unwrap();
    }

    fn select(s: &mut Session, range: &str) {
        s.execute("selection.set", json!({"range": range})).unwrap();
    }

    fn snap(s: &mut Session) -> Json {
        s.execute("assistant.context", json!({})).unwrap()
    }

    fn apply_with(s: &mut Session, context: Json, changes: Json) -> Result<Json> {
        s.execute("assistant.apply", json!({"context": context, "changes": changes}))
    }

    fn text_at(s: &Session, cell: &str) -> String {
        let d = s.doc().unwrap();
        d.wb.active().unwrap().input_text(CellRef::parse(cell).unwrap())
    }

    fn revision(s: &Session) -> u64 {
        s.doc().unwrap().revision
    }

    #[test]
    fn context_snapshots_the_selection_with_originals() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        set(&mut s, "B1", "42");
        select(&mut s, "A1:B1");
        let before = revision(&s);
        let c = snap(&mut s);
        assert_eq!(revision(&s), before, "the query changes nothing");
        assert_eq!(c["scope"], "selection");
        assert_eq!(c["document_id"], s.doc().unwrap().uid);
        assert_eq!(c["revision"], before);
        assert_eq!(c["targets"][0], json!({"index": 0, "label": "cell A1", "cell": "A1", "original": "Hi"}));
        assert_eq!(c["targets"][1], json!({"index": 1, "label": "cell B1", "cell": "B1", "original": "42"}));
        assert_eq!(c["targets"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn overlapping_selection_areas_are_read_once() {
        let mut s = session();
        set(&mut s, "A1", "x");
        select(&mut s, "A1:A2,A2");
        let c = snap(&mut s);
        assert_eq!(c["targets"].as_array().unwrap().len(), 2);
        assert_eq!(c["targets"][1]["cell"], "A2");
    }

    #[test]
    fn context_cap_is_checked_before_visiting_cells() {
        let mut s = session();
        select(&mut s, "A1:P16");
        assert_eq!(snap(&mut s)["targets"].as_array().unwrap().len(), MAX_CELLS);
        select(&mut s, "A1:Q16");
        let err = s.execute("assistant.context", json!({})).unwrap_err().to_string();
        assert!(err.contains("272") && err.contains("256"), "{err}");
    }

    #[test]
    fn context_text_cap_refuses_large_originals() {
        let mut s = session();
        let big = "a".repeat(MAX_TEXT_BYTES / 2 + 1);
        s.execute("range.setValues", json!({"range": "A1:A2", "values": [[big.clone()], [big]], "parse": false})).unwrap();
        select(&mut s, "A1:A2");
        assert!(s.execute("assistant.context", json!({})).is_err());
    }

    #[test]
    fn only_the_selection_scope_is_offered() {
        let mut s = session();
        assert!(s.execute("assistant.context", json!({"scope": "auto"})).is_err());
        assert!(s.execute("assistant.context", json!({"scope": "document"})).is_err());
        assert!(s.execute("assistant.context", json!({"scope": "selection"})).is_ok());
    }

    #[test]
    fn context_refuses_formula_picture_merge_and_protected_sheets() {
        let mut s = session();
        set(&mut s, "A1", "=1+1");
        select(&mut s, "A1");
        let err = s.execute("assistant.context", json!({})).unwrap_err().to_string();
        assert!(err.contains("formula"), "{err}");

        let mut s = session();
        set(&mut s, "A1", "x");
        s.execute("home.mergeCells", json!({"range": "A1:B1"})).unwrap();
        select(&mut s, "B1");
        let err = s.execute("assistant.context", json!({})).unwrap_err().to_string();
        assert!(err.contains("merged"), "{err}");

        let mut s = session();
        set(&mut s, "A1", "x");
        select(&mut s, "A1");
        s.execute("review.protectSheet", json!({"selectLockedCells": false})).unwrap();
        let err = s.execute("assistant.context", json!({})).unwrap_err().to_string();
        assert!(err.contains("protected"), "{err}");
    }

    #[test]
    fn apply_writes_literal_text_as_one_undo_step() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        set(&mut s, "A2", "Yo");
        select(&mut s, "A1:A2");
        let c = snap(&mut s);
        let r = apply_with(&mut s, c, json!([{"index": 0, "text": "Bye"}, {"index": 1, "text": "05.04.2026"}])).unwrap();
        assert_eq!(r["applied"], 2);
        assert_eq!(text_at(&s, "A1"), "Bye");
        assert_eq!(text_at(&s, "A2"), "05.04.2026");
        assert_eq!(s.execute("cell.get", json!({"cell": "A2"})).unwrap()["type"], "text", "parse:false keeps the date as text");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!((text_at(&s, "A1").as_str(), text_at(&s, "A2").as_str()), ("Hi", "Yo"), "one undo reverts the whole apply");
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!((text_at(&s, "A1").as_str(), text_at(&s, "A2").as_str()), ("Bye", "05.04.2026"));
    }

    #[test]
    fn empty_changes_mutate_nothing() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        let before = revision(&s);
        let r = apply_with(&mut s, c, json!([])).unwrap();
        assert_eq!(r["applied"], 0);
        assert_eq!(revision(&s), before);
    }

    #[test]
    fn stale_document_revision_or_original_is_refused() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);

        let mut other = c.clone();
        other["document_id"] = json!(c["document_id"].as_u64().unwrap() + 1);
        assert!(apply_with(&mut s, other, json!([{"index": 0, "text": "x"}])).is_err());

        set(&mut s, "B2", "moved on");
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "x"}])).is_err(), "revision moved");
        assert_eq!(text_at(&s, "A1"), "Hi");

        let c = snap(&mut s);
        let mut edited = c.clone();
        edited["targets"][0]["original"] = json!("Other");
        assert!(apply_with(&mut s, edited, json!([{"index": 0, "text": "x"}])).is_err(), "original mismatch");
        assert_eq!(text_at(&s, "A1"), "Hi");
    }

    #[test]
    fn changes_go_to_the_snapshot_cells_after_the_selection_moves() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        select(&mut s, "C3");
        apply_with(&mut s, c, json!([{"index": 0, "text": "Bye"}])).unwrap();
        assert_eq!(text_at(&s, "A1"), "Bye");
        assert_eq!(text_at(&s, "C3"), "");
    }

    #[test]
    fn duplicate_and_out_of_range_indexes_are_refused_without_writing() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1:A2");
        let c = snap(&mut s);
        let dup = json!([{"index": 0, "text": "a"}, {"index": 0, "text": "b"}]);
        assert!(apply_with(&mut s, c.clone(), dup).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 5, "text": "x"}])).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": -1, "text": "x"}])).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "fine"}, {"index": 1, "text": "=bad"}])).is_err());
        assert_eq!(text_at(&s, "A1"), "Hi", "a refused apply writes nothing, not even its valid changes");
    }

    #[test]
    fn formulas_and_markdown_are_refused() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        for text in ["=1+1", " =SUM(A1)", "+1", "-1", "@cmd", "\t=A1"] {
            assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": text}])).is_err(), "{text}");
        }
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "x", "markdown": true}])).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "x", "markdown": "yes"}])).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "x", "markdown": false}])).is_ok());
        assert_eq!(text_at(&s, "A1"), "x");
    }

    #[test]
    fn controls_and_object_characters_are_refused_but_line_breaks_pass() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "a\u{7}b"}])).is_err());
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "a\u{FFFC}b"}])).is_err());
        assert_eq!(text_at(&s, "A1"), "Hi");
        apply_with(&mut s, c, json!([{"index": 0, "text": "line\nbreak\tTab"}])).unwrap();
        assert_eq!(text_at(&s, "A1"), "line\nbreak\tTab");
    }

    #[test]
    fn caps_on_changes_and_text_are_refused_before_writing() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        let many: Vec<Json> = (0..=MAX_CELLS).map(|_| json!({"index": 0, "text": "x"})).collect();
        assert!(apply_with(&mut s, c.clone(), Json::Array(many)).is_err());
        let long = "a".repeat(MAX_TEXT_BYTES + 1);
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": long}])).is_err());
        assert_eq!(text_at(&s, "A1"), "Hi");
    }

    #[test]
    fn apply_on_a_protected_sheet_is_refused() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        s.execute("review.protectSheet", json!({"selectLockedCells": false})).unwrap();
        assert!(apply_with(&mut s, c, json!([{"index": 0, "text": "x"}])).is_err());
    }

    #[test]
    fn missing_or_malformed_params_are_errors() {
        let mut s = session();
        assert!(s.execute("assistant.apply", json!({})).is_err());
        assert!(s.execute("assistant.apply", json!({"context": {}, "changes": []})).is_err());
        assert!(s.execute("assistant.apply", json!({"context": {"targets": []}})).is_err());
    }

    #[test]
    fn unknown_or_oversized_fields_are_refused_before_they_are_copied() {
        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let c = snap(&mut s);
        let big = "x".repeat(1024 * 1024);
        let change = json!([{"index": 0, "text": "Bye"}]);

        let mut junk = c.clone();
        junk["junk"] = json!(big.clone());
        assert!(apply_with(&mut s, junk, change.clone()).is_err(), "unknown context field");

        let mut junk = c.clone();
        junk["targets"][0]["junk"] = json!(big.clone());
        assert!(apply_with(&mut s, junk, change.clone()).is_err(), "unknown target field");

        let mut long = c.clone();
        long["sheet_name"] = json!(big.clone());
        assert!(apply_with(&mut s, long, change.clone()).is_err(), "oversized sheet name");

        let mut long = c.clone();
        long["scope"] = json!("x".repeat(MAX_SCOPE_BYTES + 1));
        assert!(apply_with(&mut s, long, change.clone()).is_err(), "oversized scope");

        let mut typed = c.clone();
        typed["targets"][0]["original"] = json!(42);
        assert!(apply_with(&mut s, typed, change.clone()).is_err(), "original must be a string");

        let mut missing = c.clone();
        missing["targets"][0].as_object_mut().unwrap().remove("original");
        assert!(apply_with(&mut s, missing, change.clone()).is_err(), "original is required");

        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "Bye", "junk": big.clone()}])).is_err(), "unknown change field");
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "Bye", "markdown": "yes"}])).is_err(), "markdown type");
        assert!(apply_with(&mut s, c.clone(), json!([{"index": 0, "text": "Bye", "markdown": null}])).is_ok(), "null markdown is absent");
        assert_eq!(text_at(&s, "A1"), "Bye");
    }

    #[test]
    fn a_huge_stored_text_is_refused_without_copying_it() {
        let mut s = session();
        let big = "a".repeat(MAX_TEXT_BYTES + 1);
        s.execute("range.setValues", json!({"range": "A1", "values": [[big]], "parse": false})).unwrap();
        select(&mut s, "A1");
        assert!(s.execute("assistant.context", json!({})).is_err());

        let mut s = session();
        set(&mut s, "A1", "Hi");
        select(&mut s, "A1");
        let mut c = snap(&mut s);
        c["targets"][0]["original"] = json!("x".repeat(MAX_TEXT_BYTES));
        assert!(apply_with(&mut s, c, json!([{"index": 0, "text": "Bye"}])).is_err(), "length can't match the stored text");
        assert_eq!(text_at(&s, "A1"), "Hi");
    }
}
