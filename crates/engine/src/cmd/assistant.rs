//! Mirai writing assistant, engine side (text v1). `assistant.context` is a query that snapshots
//! the selected plain-text cells; `assistant.apply` writes validated literal text into those cells
//! as one undo step. Nothing here talks to a model: the transport lives outside the engine.

use std::collections::BTreeSet;

use gridcraft_core::{CellRef, MAX_COLS, MAX_ROWS, RangeRef, Value};
use gridcraft_model::{Color, Sheet};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use super::{CommandSpec, bad, cmd, has_doc};
use crate::{EngineError, Result, Session};

/// Most cells one snapshot holds (checked on the selection's area before any cell is visited) and
/// most changes one apply takes.
pub const MAX_CELLS: usize = 256;
/// Most bytes of cell text in one snapshot (the originals) and in one apply (the new text).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Most style, width, height, autofit or table operations in one design apply.
pub const MAX_DESIGN_OPS: usize = 64;
/// Most rows and columns each side of the selection that a formula context reads.
pub const MAX_FORMULA_WINDOW: u32 = 60;
/// Default rows and columns each side of the selection that a formula context reads.
const DEFAULT_FORMULA_WINDOW: u32 = 15;
/// Most bytes of cell text one formula context holds.
pub const MAX_FORMULA_WINDOW_BYTES: usize = 32 * 1024;
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
        cmd!(
            "assistant.applyGrid",
            "Apply Mirai Spreadsheet Changes",
            [],
            None,
            "{context: snapshot, grid?: {sheet_name: string|null, cells: [{cell, input}]}, design?: {styles?, columnWidths?, autofitColumns?, autofitRows?, rowHeights?, table?}}; optional new sheet, one undo step",
            has_doc,
            apply_grid
        ),
        cmd!(
            "assistant.applyDesign",
            "Apply Mirai Design Changes",
            [],
            None,
            "{context: snapshot, design: {styles?: [{range, style}], columnWidths?: [{cols, width?|chars?}], autofitColumns?: [cols], autofitRows?: [rows], rowHeights?: [{rows, height?|points?}], table?: {range, name?, style?, header?}}}; one undo step",
            has_doc,
            apply_design
        ),
        cmd!(
            query "assistant.formulaContext",
            "Mirai Assistant Formula Context",
            [],
            None,
            "{window?: rows and columns each side of the selection (default 15, at most 60)} → {document_id, revision, sheet, sheet_name, selection, from, to, cells: [{cell, text}]}; formulas as typed, at most 32 KiB",
            has_doc,
            formula_context
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GridPlan {
    sheet_name: Option<String>,
    cells: Vec<GridCell>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GridCell {
    cell: String,
    input: String,
}

fn apply_grid(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "assistant.applyGrid";
    let context = p.get("context").filter(|c| c.is_object()).ok_or_else(|| bad(CMD, "missing context"))?;
    let grid = p.get("grid").filter(|c| c.is_object());
    let design_owned = p.get("design").filter(|c| c.is_object()).map(normalize_design);
    let design = design_owned.as_ref();
    let changes = match p.get("changes") {
        None => &[][..],
        Some(Json::Array(changes)) => changes.as_slice(),
        _ => return Err(bad(CMD, "changes must be an array")),
    };
    check_shape(context, changes)?;
    if let Some(grid) = grid {
        only_keys(grid, &["sheet_name", "cells"], "grid")?;
        let cells = grid.get("cells").and_then(Json::as_array).ok_or_else(|| bad(CMD, "missing cells"))?;
        if cells.len().saturating_add(changes.len()) > MAX_CELLS {
            return Err(refuse(format!("the limit is {MAX_CELLS} cells")));
        }
        if grid.get("sheet_name").and_then(Json::as_str).is_some_and(|n| n.len() > MAX_SHEET_NAME_BYTES) {
            return Err(refuse("sheet name is too long"));
        }
        let mut bytes = 0usize;
        for cell in cells {
            only_keys(cell, &["cell", "input"], "cell")?;
            str_within(cell, "cell", MAX_ADDRESS_BYTES, "cell address")?;
            let input = cell.get("input").and_then(Json::as_str).ok_or_else(|| bad(CMD, "missing cell input"))?;
            bytes = bytes.saturating_add(input.len());
            if bytes > MAX_TEXT_BYTES || input.chars().any(|c| c == '\u{FFFC}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))) {
                return Err(refuse("cell input is too large or contains unsupported characters"));
            }
        }
    }
    if let Some(design) = design {
        validate_design(s, design)?;
    }
    let snap: Snapshot = serde_json::from_value(context.clone()).map_err(|e| bad(CMD, format!("bad context: {e}")))?;
    let plan: Option<GridPlan> = grid.map(|g| serde_json::from_value(g.clone()).map_err(|e| bad(CMD, format!("bad grid: {e}")))).transpose()?;
    verify(s, &snap)?;
    let mut seen = BTreeSet::new();
    let mut literal = Vec::with_capacity(changes.len());
    for raw in changes {
        let change: Change = serde_json::from_value(raw.clone()).map_err(|e| bad(CMD, format!("bad change: {e}")))?;
        let target = snap.targets.get(change.index).ok_or_else(|| bad(CMD, "change target is outside the snapshot"))?;
        if !seen.insert(target.cell.clone()) {
            return Err(bad(CMD, "duplicate change target"));
        }
        check_text(&change.text)?;
        literal.push((target.cell.clone(), change.text));
    }
    if let Some(plan) = &plan {
        let mut grid_cells = BTreeSet::new();
        for cell in &plan.cells {
            let at = CellRef::parse(&cell.cell).filter(|c| c.is_valid() && c.a1() == cell.cell).ok_or_else(|| bad(CMD, "invalid cell address"))?;
            if !grid_cells.insert(at) {
                return Err(bad(CMD, "duplicate cell address"));
            }
            if plan.sheet_name.is_none() && seen.contains(&cell.cell) {
                return Err(bad(CMD, "cell is changed twice"));
            }
            if plan.sheet_name.is_none() {
                let sh = s.doc()?.wb.sheet(snap.sheet).ok_or(EngineError::NoDocument)?;
                if sh.cell_pictures.contains_key(&at)
                    || sh.merge_at(at).is_some()
                    || sh.spill_anchor(at).is_some()
                    || sh.spill_ranges.contains_key(&at)
                {
                    return Err(refuse(format!("{} contains an object, merged cell or spilled array", cell.cell)));
                }
            }
        }
        if let Some(name) = &plan.sheet_name {
            s.doc()?.wb.check_sheet_name(name, None).map_err(EngineError::Other)?;
        }
    }
    let has_ops = !literal.is_empty() || plan.as_ref().is_some_and(|p| !p.cells.is_empty() || p.sheet_name.is_some()) || design.is_some();
    if has_ops {
        let design = design.cloned();
        s.grouped(GROUP_LABEL, |s| {
            for (cell, text) in &literal {
                s.execute("range.setValues", json!({"sheet": snap.sheet, "range": cell, "values": [[text]], "parse": false}))?;
            }
            let sheet = if let Some(plan) = &plan {
                if let Some(name) = &plan.sheet_name {
                    s.execute("home.insertSheet", json!({"name": name}))?;
                    s.doc()?.wb.active_sheet()
                } else {
                    snap.sheet
                }
            } else {
                snap.sheet
            };
            if let Some(plan) = &plan {
                for cell in &plan.cells {
                    s.execute("cell.set", json!({"sheet": sheet, "cell": cell.cell, "input": cell.input}))?;
                }
            }
            if let Some(design) = &design {
                apply_design_ops(s, sheet, design)?;
            }
            Ok(())
        })?;
    }
    let created_sheet = plan.as_ref().and_then(|p| p.sheet_name.clone());
    let applied = literal.len() + plan.as_ref().map_or(0, |p| p.cells.len()) + design_ops_count(design);
    Ok(json!({"applied": applied, "created_sheet": created_sheet}))
}

fn apply_design(s: &mut Session, p: &Json) -> Result<Json> {
    apply_grid(s, p)
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

/// A bounded, read-only window of sheet cells around the selection: what formula work may look at.
/// Nothing is edited and nothing here is checked back by an apply; the plain selection snapshot
/// stays the authority for changes. The rectangle is bounded by `window` and the sheet limits, and
/// the text is capped before it is copied.
fn formula_context(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "assistant.formulaContext";
    let window = match p.get("window") {
        None | Some(Json::Null) => DEFAULT_FORMULA_WINDOW,
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| bad(CMD, "`window` must be a whole number"))?;
            u32::try_from(n).map_err(|_| bad(CMD, format!("`window` must be at most {MAX_FORMULA_WINDOW}")))?
        }
    };
    if window > MAX_FORMULA_WINDOW {
        return Err(bad(CMD, format!("`window` must be at most {MAX_FORMULA_WINDOW}")));
    }
    let d = s.doc()?;
    let si = d.wb.active_sheet();
    let sh = d.wb.sheet(si).ok_or(EngineError::NoDocument)?;
    if sh.is_protected() {
        return Err(refuse(PROTECTED));
    }
    if d.selection.ranges.is_empty() {
        return Err(bad(CMD, "nothing is selected; select a cell first"));
    }
    let (mut min_row, mut min_col, mut max_row, mut max_col) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for r in &d.selection.ranges {
        min_row = min_row.min(r.start.row);
        min_col = min_col.min(r.start.col);
        max_row = max_row.max(r.end.row);
        max_col = max_col.max(r.end.col);
    }
    let from = CellRef::new(min_row.saturating_sub(window), min_col.saturating_sub(window));
    let to = CellRef::new(max_row.saturating_add(window).min(MAX_ROWS - 1), max_col.saturating_add(window).min(MAX_COLS - 1));
    let mut cells = Vec::new();
    let mut bytes = 0usize;
    for (at, _) in sh.stored_cells_in(RangeRef::new(from, to)) {
        let text = sh.input_text(at);
        if text.is_empty() || !is_plain(&text) {
            continue;
        }
        bytes = bytes.saturating_add(text.len());
        if bytes > MAX_FORMULA_WINDOW_BYTES {
            return Err(refuse(format!("the surrounding cells hold more than {MAX_FORMULA_WINDOW_BYTES} bytes; select a smaller window")));
        }
        cells.push(json!({"cell": at.a1(), "text": text}));
    }
    let selection = format!("{}:{}", CellRef::new(min_row, min_col).a1(), CellRef::new(max_row, max_col).a1());
    Ok(json!({
        "document_id": d.uid,
        "revision": d.revision,
        "sheet": si,
        "sheet_name": sh.name,
        "selection": selection,
        "from": from.a1(),
        "to": to.a1(),
        "cells": cells,
    }))
}

/// Sheet text a formula context may carry: no control characters and no object replacement.
fn is_plain(text: &str) -> bool {
    text.chars().all(|c| matches!(c, '\n' | '\r' | '\t') || (!c.is_control() && c != '\u{FFFC}'))
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
        // The length window refuses a changed long text without copying it. Numbers, booleans
        // and errors hold no stored text; their input text is short, so compare it directly.
        let stored = stored_text_len(sh, at);
        if stored > 0 && (t.original.len() < stored || t.original.len() > stored.saturating_add(1)) {
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

fn snake_case_keys(v: &Json) -> Json {
    match v {
        Json::Object(m) => Json::Object(
            m.iter()
                .map(|(k, v)| {
                    let key = if k.starts_with(|c: char| c.is_ascii_lowercase()) {
                        k.chars().fold(String::new(), |mut out, c| {
                            if c.is_ascii_uppercase() {
                                out.push('_');
                            }
                            out.push(c.to_ascii_lowercase());
                            out
                        })
                    } else {
                        k.clone()
                    };
                    (key, snake_case_keys(v))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A style patch from the model carries colours as `#RRGGBB` strings (or "auto"), the natural
/// form for text; the typed style takes a colour enum. This rewrites just the colour-typed fields
/// of a design payload (font colour, fill fg/bg, border colours) into the enum form, and expands a
/// plain colour string as a solid fill. Everything else passes through untouched, and an
/// unrecognised colour still fails validation with its own message.
fn normalize_design(design: &Json) -> Json {
    let mut out = design.clone();
    let Some(styles) = out.get_mut("styles").and_then(Json::as_array_mut) else { return out };
    for item in styles {
        if let Some(style) = item.get_mut("style").filter(|v| v.is_object()) {
            *style = normalize_style_patch(style);
        }
    }
    out
}

fn normalize_style_patch(patch: &Json) -> Json {
    let Some(obj) = patch.as_object() else { return patch.clone() };
    let mut out = serde_json::Map::new();
    for (key, value) in obj {
        if key == "fill"
            && let Some(text) = value.as_str()
        {
            out.insert(key.clone(), plain_fill(text));
            continue;
        }
        if matches!(key.as_str(), "color" | "fg" | "bg")
            && let Some(color) = plain_color(value)
        {
            out.insert(key.clone(), color);
            continue;
        }
        if value.is_object() {
            out.insert(key.clone(), normalize_style_patch(value));
        } else {
            out.insert(key.clone(), value.clone());
        }
    }
    Json::Object(out)
}

/// A colour-typed field as the model writes it: `#RRGGBB`, or "auto"/"none" for no colour.
/// `None` leaves the value untouched (validation reports it).
fn plain_color(value: &Json) -> Option<Json> {
    let text = value.as_str()?.trim();
    if text.eq_ignore_ascii_case("auto") || text.eq_ignore_ascii_case("none") {
        return Some(json!("Auto"));
    }
    match Color::from_hex(text) {
        Some(Color::Rgb(v)) => Some(json!({"Rgb": v})),
        _ => None,
    }
}

/// A fill written as a plain colour string: a solid fill of that colour, or no fill at all.
fn plain_fill(text: &str) -> Json {
    match plain_color(&Json::String(text.to_owned())) {
        Some(color) => json!({"pattern": "Solid", "fg": color}),
        None => json!({"pattern": "None", "fg": "Auto"}),
    }
}

fn validate_style_patch(s: &Session, patch: &Json) -> Result<()> {
    let base = serde_json::to_value(super::format::active_style(s)?).map_err(|e| EngineError::Other(e.to_string()))?;
    let merged = super::format::merge_json(base, &snake_case_keys(&patch.clone()));
    let _: gridcraft_model::Style = serde_json::from_value(merged).map_err(|e| bad("assistant.applyGrid", e.to_string()))?;
    Ok(())
}

/// The columns a column-width entry targets, under any of the keys the model uses.
fn width_cols(item: &Json) -> Option<&str> {
    ["cols", "columns", "range", "col"].iter().find_map(|k| item.get(*k)).and_then(Json::as_str)
}

/// A row/column spec without the spaces the model sometimes writes around its parts
/// (`" B, D:F "` → `"B,D:F"`); a quoted sheet name keeps its own spaces.
fn compact_lines(spec: &str) -> String {
    let spec = spec.trim();
    let (prefix, body) = match spec.rsplit_once('!') {
        Some((sheet, body)) => (Some(sheet), body),
        None => (None, spec),
    };
    let body = body.split(',').map(|part| part.split(char::is_whitespace).collect::<String>()).collect::<Vec<_>>().join(",");
    match prefix {
        Some(sheet) => format!("{sheet}!{body}"),
        None => body,
    }
}

/// Whether `spec` names rows or columns the way the size commands read them: `B`, `B:D`, `3`,
/// `2:5`, a comma list (`B:B,D:F`), optionally `Sheet!`-prefixed. Spaces are ignored.
fn valid_lines(spec: &str) -> bool {
    let spec = compact_lines(spec);
    let (_, body) = super::split_sheet(&spec);
    body.split(',').all(|part| super::parse_lines(part).is_some())
}

fn validate_design(s: &Session, design: &Json) -> Result<()> {
    const CMD: &str = "assistant.applyGrid";
    only_keys(
        design,
        &["styles", "columnWidths", "columns", "widths", "autofitColumns", "autofit", "autofitRows", "rowHeights", "table", "tables"],
        "design",
    )?;

    let mut ops = 0usize;
    if let Some(styles) = design.get("styles") {
        let arr = styles.as_array().ok_or_else(|| bad(CMD, "styles must be an array"))?;
        if arr.len() > MAX_DESIGN_OPS {
            return Err(refuse(format!("{} styles; the limit is {MAX_DESIGN_OPS}", arr.len())));
        }
        ops += arr.len();
        for item in arr {
            only_keys(item, &["range", "style"], "a style")?;
            let range = item.get("range").and_then(Json::as_str).ok_or_else(|| bad(CMD, "missing range"))?;
            if range.len() > MAX_ADDRESS_BYTES {
                return Err(refuse("range is too long"));
            }
            let r = RangeRef::parse(range).ok_or_else(|| bad(CMD, "invalid range"))?;
            let _ = r;
            let style = item.get("style").filter(|v| v.is_object()).ok_or_else(|| bad(CMD, "missing style"))?;
            validate_style_patch(s, style)?;
        }
    }

    let width_arrays: [&str; 3] = ["columnWidths", "columns", "widths"];
    for key in width_arrays {
        if let Some(v) = design.get(key) {
            let arr = v.as_array().ok_or_else(|| bad(CMD, "column widths must be an array"))?;
            if arr.len() > MAX_DESIGN_OPS {
                return Err(refuse(format!("{} column widths; the limit is {MAX_DESIGN_OPS}", arr.len())));
            }
            ops += arr.len();
            for item in arr {
                only_keys(item, &["cols", "columns", "range", "col", "width", "chars"], "a column width")?;
                let cols = width_cols(item).ok_or_else(|| bad(CMD, "missing cols"))?;
                if cols.len() > MAX_ADDRESS_BYTES {
                    return Err(refuse("cols is too long"));
                }
                if !valid_lines(cols) {
                    return Err(bad(CMD, format!("invalid cols `{cols}`")));
                }
                match (item.get("width").and_then(|v| v.as_f64()), item.get("chars").and_then(|v| v.as_f64())) {
                    (Some(w), _) => {
                        if !(0.0..=1800.0).contains(&w) {
                            return Err(refuse("column width must be between 0 and 1800 pixels"));
                        }
                    }
                    (None, Some(c)) => {
                        if !(0.0..=255.0).contains(&c) {
                            return Err(refuse("column width must be between 0 and 255 characters"));
                        }
                    }
                    _ => return Err(bad(CMD, "missing width or chars")),
                }
            }
        }
    }

    let autofit_keys: [&str; 2] = ["autofitColumns", "autofit"];
    for key in autofit_keys {
        if let Some(v) = design.get(key) {
            let arr = v.as_array().ok_or_else(|| bad(CMD, "autofit columns must be an array"))?;
            if arr.len() > MAX_DESIGN_OPS {
                return Err(refuse(format!("{} autofit columns; the limit is {MAX_DESIGN_OPS}", arr.len())));
            }
            ops += arr.len();
            for item in arr {
                let s = item.as_str().ok_or_else(|| bad(CMD, "autofit entry must be a string"))?;
                if s.len() > MAX_ADDRESS_BYTES {
                    return Err(refuse("autofit entry is too long"));
                }
                if !valid_lines(s) {
                    return Err(bad(CMD, format!("invalid autofit entry `{s}`")));
                }
            }
        }
    }
    if let Some(v) = design.get("autofitRows") {
        let arr = v.as_array().ok_or_else(|| bad(CMD, "autofit rows must be an array"))?;
        if arr.len() > MAX_DESIGN_OPS {
            return Err(refuse(format!("{} autofit rows; the limit is {MAX_DESIGN_OPS}", arr.len())));
        }
        ops += arr.len();
        for item in arr {
            let s = item.as_str().ok_or_else(|| bad(CMD, "autofit row entry must be a string"))?;
            if s.len() > MAX_ADDRESS_BYTES {
                return Err(refuse("autofit row entry is too long"));
            }
            if !valid_lines(s) {
                return Err(bad(CMD, format!("invalid autofit row entry `{s}`")));
            }
        }
    }

    if let Some(v) = design.get("rowHeights") {
        let arr = v.as_array().ok_or_else(|| bad(CMD, "row heights must be an array"))?;
        if arr.len() > MAX_DESIGN_OPS {
            return Err(refuse(format!("{} row heights; the limit is {MAX_DESIGN_OPS}", arr.len())));
        }
        ops += arr.len();
        for item in arr {
            only_keys(item, &["rows", "row", "height", "points"], "a row height")?;
            let rows = item.get("rows").or_else(|| item.get("row")).and_then(Json::as_str).ok_or_else(|| bad(CMD, "missing rows"))?;
            if rows.len() > MAX_ADDRESS_BYTES {
                return Err(refuse("rows is too long"));
            }
            if !valid_lines(rows) {
                return Err(bad(CMD, format!("invalid rows `{rows}`")));
            }
            match (item.get("height").and_then(|v| v.as_f64()), item.get("points").and_then(|v| v.as_f64())) {
                (Some(h), _) => {
                    if !(0.0..=546.0).contains(&h) {
                        return Err(refuse("row height must be between 0 and 546 pixels"));
                    }
                }
                (None, Some(pt)) => {
                    if !(0.0..=409.5).contains(&pt) {
                        return Err(refuse("row height must be between 0 and 409.5 points"));
                    }
                }
                _ => return Err(bad(CMD, "missing height or points")),
            }
        }
    }

    let table_items: Vec<&Json> = if let Some(t) = design.get("table") {
        vec![t]
    } else if let Some(arr) = design.get("tables").and_then(Json::as_array) {
        arr.iter().collect()
    } else {
        vec![]
    };
    if table_items.len() > MAX_DESIGN_OPS {
        return Err(refuse(format!("{} tables; the limit is {MAX_DESIGN_OPS}", table_items.len())));
    }
    ops += table_items.len();
    if ops > MAX_DESIGN_OPS * 4 {
        return Err(refuse("too many design operations"));
    }
    for item in table_items {
        only_keys(item, &["range", "name", "style", "header"], "a table")?;
        let range = item.get("range").and_then(Json::as_str).ok_or_else(|| bad(CMD, "missing range"))?;
        if range.len() > MAX_ADDRESS_BYTES {
            return Err(refuse("range is too long"));
        }
        let _ = RangeRef::parse(range).ok_or_else(|| bad(CMD, "invalid range"))?;
        if let Some(name) = item.get("name").and_then(Json::as_str)
            && name.len() > MAX_SHEET_NAME_BYTES
        {
            return Err(refuse("table name is too long"));
        }
        if let Some(style) = item.get("style").and_then(Json::as_str)
            && style.len() > MAX_ADDRESS_BYTES
        {
            return Err(refuse("table style is too long"));
        }
    }

    Ok(())
}

fn apply_design_ops(s: &mut Session, sheet: usize, design: &Json) -> Result<()> {
    // styles
    if let Some(styles) = design.get("styles").and_then(Json::as_array) {
        for item in styles {
            let range = item.get("range").and_then(Json::as_str).unwrap_or("");
            let style = item.get("style").cloned().unwrap_or(Json::Null);
            s.execute("home.formatCells", json!({"sheet": sheet, "range": range, "style": style}))?;
        }
    }
    // column widths (support aliases)
    for key in ["columnWidths", "columns", "widths"] {
        if let Some(arr) = design.get(key).and_then(Json::as_array) {
            for item in arr {
                let cols = width_cols(item).unwrap_or("");
                let mut p = json!({"sheet": sheet, "cols": compact_lines(cols)});
                if let Some(w) = item.get("width").and_then(|v| v.as_f64()) {
                    p["width"] = json!(w);
                }
                if let Some(c) = item.get("chars").and_then(|v| v.as_f64()) {
                    p["chars"] = json!(c);
                }
                s.execute("home.columnWidth", p)?;
            }
        }
    }
    // autofit columns
    for key in ["autofitColumns", "autofit"] {
        if let Some(arr) = design.get(key).and_then(Json::as_array) {
            for item in arr {
                if let Some(spec) = item.as_str() {
                    s.execute("home.autofitColumnWidth", json!({"sheet": sheet, "cols": compact_lines(spec)}))?;
                }
            }
        }
    }
    if let Some(arr) = design.get("autofitRows").and_then(Json::as_array) {
        for item in arr {
            if let Some(spec) = item.as_str() {
                s.execute("home.autofitRowHeight", json!({"sheet": sheet, "rows": compact_lines(spec)}))?;
            }
        }
    }
    // row heights
    if let Some(arr) = design.get("rowHeights").and_then(Json::as_array) {
        for item in arr {
            let rows = item.get("rows").or_else(|| item.get("row")).and_then(Json::as_str).unwrap_or("");
            let mut p = json!({"sheet": sheet, "rows": compact_lines(rows)});
            if let Some(h) = item.get("height").and_then(|v| v.as_f64()) {
                p["height"] = json!(h);
            }
            if let Some(pt) = item.get("points").and_then(|v| v.as_f64()) {
                p["points"] = json!(pt);
            }
            s.execute("home.rowHeight", p)?;
        }
    }
    // tables
    let table_items: Vec<&Json> = if let Some(t) = design.get("table") {
        vec![t]
    } else if let Some(arr) = design.get("tables").and_then(Json::as_array) {
        arr.iter().collect()
    } else {
        vec![]
    };
    for item in table_items {
        let mut p = json!({"sheet": sheet});
        if let Some(range) = item.get("range").and_then(Json::as_str) {
            p["range"] = json!(range);
        }
        if let Some(name) = item.get("name").and_then(Json::as_str) {
            p["name"] = json!(name);
        }
        if let Some(style) = item.get("style").and_then(Json::as_str) {
            p["style"] = json!(style);
        }
        if let Some(header) = item.get("header").and_then(Json::as_bool) {
            p["header"] = json!(header);
        }
        s.execute("insert.table", p)?;
    }
    Ok(())
}

fn design_ops_count(design: Option<&Json>) -> usize {
    let Some(design) = design else {
        return 0;
    };
    let mut n = 0;
    if let Some(v) = design.get("styles").and_then(Json::as_array) {
        n += v.len();
    }
    for key in ["columnWidths", "columns", "widths"] {
        if let Some(v) = design.get(key).and_then(Json::as_array) {
            n += v.len();
        }
    }
    for key in ["autofitColumns", "autofit"] {
        if let Some(v) = design.get(key).and_then(Json::as_array) {
            n += v.len();
        }
    }
    if let Some(v) = design.get("autofitRows").and_then(Json::as_array) {
        n += v.len();
    }
    if let Some(v) = design.get("rowHeights").and_then(Json::as_array) {
        n += v.len();
    }
    if design.get("table").is_some() {
        n += 1;
    }
    if let Some(v) = design.get("tables").and_then(Json::as_array) {
        n += v.len();
    }
    n
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
