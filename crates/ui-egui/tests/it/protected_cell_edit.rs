//! Typing into a locked cell on a protected sheet is refused before a draft opens (#2945):
//! the old flow let the edit start and fail on commit, leaving an open draft the user could
//! neither undo nor save away.

use serde_json::json;

fn harness() -> egui_kittest::Harness<'static, gridcraft_ui_egui::SheetApp> {
    let mut h = crate::support::blank_harness(4);
    h.state_mut().session.execute("review.protectSheet", json!({})).unwrap();
    h.run_steps(2);
    h
}

/// A harness whose A1 is unlocked before the sheet is protected, as Allow Edit Ranges leaves it.
fn unlocked_a1_harness() -> egui_kittest::Harness<'static, gridcraft_ui_egui::SheetApp> {
    let mut h = crate::support::blank_harness(4);
    h.state_mut().session.execute("home.lockCell", json!({ "range": "A1", "on": false })).unwrap();
    h.state_mut().session.execute("review.protectSheet", json!({})).unwrap();
    h.run_steps(2);
    h
}

#[test]
fn typing_into_a_locked_cell_is_refused_before_a_draft_opens() {
    let mut h = harness();
    h.state_mut().begin_edit(Some("typed".into()), false);
    h.run_steps(2);
    let s = h.state();
    assert!(s.editor.is_none(), "no draft may open on a locked cell");
    let message = s.message.as_ref().map(|(_, m)| m.clone()).unwrap_or_default();
    assert!(message.contains("protected"), "the protection warning shows: {message}");
}

#[test]
fn a_locked_cell_refusal_leaves_the_workbook_savable() {
    let mut h = harness();
    let dirty_before = h.state().session.active().unwrap().is_dirty();
    h.state_mut().begin_edit(Some("typed".into()), false);
    h.run_steps(2);
    // The refusal opened no draft and changed nothing: no stuck edit state stands between
    // the user and a save, and the refused typing didn't dirty the workbook further.
    let s = h.state();
    assert!(s.editor.is_none());
    assert_eq!(s.session.active().unwrap().is_dirty(), dirty_before, "the refused typing changed nothing");
}

#[test]
fn an_unlocked_cell_still_edits_on_a_protected_sheet() {
    let mut h = unlocked_a1_harness();
    h.state_mut().begin_edit(Some("typed".into()), false);
    h.run_steps(2);
    assert!(h.state().editor.is_some(), "an unlocked cell on a protected sheet still opens a draft");
}
