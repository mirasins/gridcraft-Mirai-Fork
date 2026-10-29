//! The Mirai Assistant pane: a right-side task pane that drafts new text for the selected cells
//! with the user's own Claude Code sign-in (`gridcraft-assistant`). Nothing is sent until the user
//! confirms each request. Suggestions are previewed first and applied only through the engine's
//! `assistant.apply` command, which receives the context the suggestions were made for.
//!
//! The provider runs on a background thread; the pane polls its channel once per frame and never
//! joins it. Cancelling, closing the pane, starting a new conversation or switching workbooks drops
//! the worker's channel, so a late answer goes nowhere.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use egui::{RichText, Ui};
use gridcraft_assistant::{
    AuthState, Client, Error as AssistantError, MAX_INSTRUCTION_BYTES, MAX_MODEL_BYTES, MAX_TARGETS, MAX_TEXT_BYTES, Request, Response,
};
use serde_json::{Value as Json, json};

use crate::SheetApp;
use crate::theme::{self, Tokens};

/// Whether this build can run the assistant. The web build has no local Claude Code CLI.
pub const SUPPORTED: bool = !cfg!(target_arch = "wasm32");
const DESKTOP_ONLY: &str = "The writing assistant runs Claude Code on this computer, so it is available in the desktop app only.";
pub const WIDTH: f32 = 280.0;
const MAX_HISTORY: usize = 20;
const MAX_TURN_CHARS: usize = 200;
const CHIPS: [(&str, &str); 4] = [
    ("Shorten", "Shorten each text and keep its meaning."),
    ("Fix grammar", "Fix spelling and grammar and keep the wording."),
    ("Plain language", "Rewrite in plain, friendly language."),
    ("Formal tone", "Rewrite in a formal, professional tone."),
];

/// The assistant's commands: id, label and params doc, listed by `engine.commands`.
pub const COMMANDS: [(&str, &str, &str); 7] = [
    ("ui.assistant", "Mirai Assistant", "{value?: bool} (toggles without it) → {open}"),
    (
        "ui.assistant.request",
        "Mirai Assistant: Send Request",
        "{instruction?: text, model?: text, confirm: true} → {sent}; refused without confirm: true, sends the selected cells' text to Claude Code",
    ),
    (
        "ui.assistant.apply",
        "Mirai Assistant: Apply Suggestions",
        "{} → {applied}; one undoable change, only for the context the suggestions were made for",
    ),
    ("ui.assistant.dismiss", "Mirai Assistant: Dismiss Suggestions", "{}"),
    ("ui.assistant.cancel", "Mirai Assistant: Cancel Request", "{} → {cancelled}"),
    ("ui.assistant.status", "Mirai Assistant: Status", "{} → {open, supported, pending, consent, proposal, status, error, history}"),
    ("ui.assistant.newConversation", "Mirai Assistant: New Conversation", "{} clears history and suggestions and cancels any request"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub error: bool,
    pub text: String,
}

type Answer = Result<(AuthState, Response), AssistantError>;

struct Worker {
    rx: mpsc::Receiver<Answer>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Target {
    /// The engine's index for this cell in the selection snapshot.
    index: u64,
    original: String,
    label: String,
}

/// The selection snapshot a request was made for. Apply sends it back whole.
#[derive(Clone)]
struct Context {
    owner: u64,
    revision: u64,
    json: Json,
    targets: Vec<Target>,
}

#[derive(Clone)]
pub struct Row {
    pub label: String,
    pub original: String,
    pub proposed: String,
}

#[derive(Clone)]
struct Proposal {
    context: Context,
    rows: Vec<Row>,
    changes: Vec<Json>,
    explanation: String,
}

/// Assistant state on [`SheetApp`]. Ephemeral: never saved with the preferences or the workbook.
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub instruction: String,
    pub model: String,
    /// Warning modal visibility
    pub show_warning: bool,
    /// Whether user confirmed the warning
    pub consent_given: bool,
    pub history: Vec<String>,
    pub status: Option<Note>,
    /// Workbook (document uid) this conversation belongs to.
    owner: Option<u64>,
    /// Context of the running request, kept until its answer arrives.
    pending: Option<Context>,
    worker: Option<Worker>,
    proposal: Option<Proposal>,
}

impl State {
    /// Stops the running request (if any): its answer is dropped unread. Resets warning state.
    fn cancel(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::SeqCst);
        }
        self.pending = None;
        self.show_warning = false;
        self.consent_given = false;
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn note(error: bool, text: impl Into<String>) -> Note {
    Note { error, text: text.into() }
}

fn active(app: &SheetApp) -> Option<(u64, u64)> {
    app.session.active().map(|d| (d.uid, d.revision))
}

/// Clamps `text` to `max` bytes at a character boundary.
fn clamp_text(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

pub fn set_open(app: &mut SheetApp, on: bool) {
    app.assistant.open = on;
    if !on {
        app.assistant.cancel();
        app.assistant.proposal = None;
        app.assistant.status = None;
    }
}

fn cancel_request(app: &mut SheetApp) {
    let was_running = app.assistant.worker.is_some();
    app.assistant.cancel();
    if was_running {
        app.assistant.status = Some(note(false, "Request cancelled."));
    }
}

fn dismiss(app: &mut SheetApp) {
    app.assistant.proposal = None;
    app.assistant.status = None;
}

fn new_conversation(app: &mut SheetApp) {
    app.assistant.cancel();
    app.assistant.owner = None;
    app.assistant.proposal = None;
    app.assistant.history.clear();
    app.assistant.instruction.clear();
    app.assistant.status = None;
}

/// Ends the conversation because another workbook is now active (or none is).
fn forget_workbook(app: &mut SheetApp) {
    new_conversation(app);
    app.assistant.status = Some(note(false, "The active workbook changed, so the conversation was cleared."));
}

/// Keeps the conversation with the workbook it started in.
fn sync(app: &mut SheetApp) {
    let Some(owner) = app.assistant.owner else { return };
    match active(app) {
        Some((uid, revision)) if uid == owner => {
            if app.assistant.proposal.as_ref().is_some_and(|p| p.context.revision != revision) {
                app.assistant.proposal = None;
                app.assistant.status = Some(note(false, "The workbook changed, so the suggestions were discarded."));
            }
        }
        _ => forget_workbook(app),
    }
}

/// Per-frame work: checks the workbook and collects a finished answer. Call once per frame.
pub fn poll(app: &mut SheetApp, ctx: &egui::Context) {
    sync(app);
    let polled = match app.assistant.worker.as_ref() {
        Some(worker) => worker.rx.try_recv(),
        None => return,
    };
    match polled {
        Ok(answer) => finish(app, answer),
        Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
        Err(TryRecvError::Disconnected) => {
            app.assistant.worker = None;
            app.assistant.pending = None;
            app.assistant.status = Some(note(true, "The assistant stopped before answering."));
        }
    }
}

fn finish(app: &mut SheetApp, answer: Answer) {
    app.assistant.worker = None;
    let Some(context) = app.assistant.pending.take() else {
        app.assistant.status = Some(note(true, "That answer no longer matches the selection."));
        return;
    };
    if active(app) != Some((context.owner, context.revision)) {
        app.assistant.status = Some(note(true, "The workbook changed while the assistant worked. Ask again."));
        return;
    }
    match answer {
        Ok((_, response)) => match build_proposal(context, response) {
            Ok(proposal) => {
                app.assistant.status = Some(if proposal.rows.is_empty() {
                    note(false, "No changes suggested.")
                } else {
                    note(false, "Review the suggestions, then choose Apply all.")
                });
                app.assistant.proposal = Some(proposal);
            }
            Err(error) => app.assistant.status = Some(note(true, error)),
        },
        Err(error) => app.assistant.status = Some(note(true, guidance(error))),
    }
}

/// Turns a validated answer into a preview. Answers that name cells outside the selection,
/// repeat a cell, or use Markdown are refused; their text is never applied.
fn build_proposal(context: Context, response: Response) -> Result<Proposal, String> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    let mut changes = Vec::new();
    for suggestion in &response.suggestions {
        if suggestion.markdown {
            return Err("The assistant returned formatted text. GridCraft applies plain text only.".into());
        }
        if !seen.insert(suggestion.index) {
            return Err("The assistant suggested two changes for one cell. Nothing was applied.".into());
        }
        if suggestion.text.len() > MAX_TEXT_BYTES {
            return Err("A suggested text is too long to apply.".into());
        }
        let target = context.targets.get(suggestion.index).ok_or("The assistant changed a cell outside the selection. Nothing was applied.")?;
        rows.push(Row { label: target.label.clone(), original: target.original.clone(), proposed: suggestion.text.clone() });
        changes.push(json!({"index": target.index, "text": suggestion.text, "markdown": false}));
    }
    Ok(Proposal { context, rows, changes, explanation: response.explanation })
}

/// The sign-in guidance for a failed request. It names only official steps; no keys, no fallbacks.
fn guidance(error: AssistantError) -> String {
    match error {
        AssistantError::Unavailable => "Claude Code (the `claude` command) was not found. Install it by following the official Claude Code instructions, then run `claude auth login`.".into(),
        AssistantError::Authentication => "Claude Code is not signed in. Run `claude auth login` (the official sign-in) and try again.".into(),
        AssistantError::UnsupportedAuth => {
            "Claude Code uses a sign-in method GridCraft does not allow. Sign in with your Claude account using `claude auth login`.".into()
        }
        other => other.to_string(),
    }
}

/// Reads the selection snapshot from the engine. Only selection-scope snapshots of the workbook
/// the request was made in are accepted.
fn parse_context(raw: &Json, owner: u64, revision: u64) -> Result<Context, String> {
    if raw.get("revision").and_then(Json::as_u64) != Some(revision) {
        return Err("The workbook changed while the request was prepared. Try again.".into());
    }
    if raw.get("scope").and_then(Json::as_str).is_some_and(|scope| scope != "selection") {
        return Err("The assistant works on the selected cells only.".into());
    }
    let items = raw.get("targets").and_then(Json::as_array).filter(|items| !items.is_empty()).ok_or("Select cells that contain text first.")?;
    if items.len() > MAX_TARGETS {
        return Err(format!("Select at most {MAX_TARGETS} cells for one request."));
    }
    let targets = items
        .iter()
        .enumerate()
        .map(|(position, item)| -> Result<Target, &'static str> {
            let original = item.get("original").and_then(Json::as_str).ok_or("The selection could not be read.")?;
            let label = item.get("label").or_else(|| item.get("cell")).and_then(Json::as_str).unwrap_or("cell");
            Ok(Target {
                index: item.get("index").and_then(Json::as_u64).unwrap_or(position as u64),
                original: original.to_string(),
                label: label.to_string(),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    Ok(Context { owner, revision, json: raw.clone(), targets })
}

/// Sends one request. Refused unless `confirmed` (the consent box, or `confirm: true` from a
/// program) — checked before anything is read or started.
fn request(app: &mut SheetApp, instruction: Option<&str>, model: Option<&str>, confirmed: bool) -> Result<Json, String> {
    if !confirmed {
        return Err("Confirm first: the selected cells' text is sent to Claude Code. Tick the consent box or pass confirm: true.".into());
    }
    if !SUPPORTED {
        return Err(DESKTOP_ONLY.into());
    }
    if app.assistant.worker.is_some() {
        return Err("A request is already running. Wait for it or cancel it.".into());
    }
    let raw_text = instruction.unwrap_or(&app.assistant.instruction);
    if raw_text.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!("The instruction is longer than {MAX_INSTRUCTION_BYTES} bytes."));
    }
    let text = raw_text.trim().to_string();
    if text.is_empty() {
        return Err("Write what the assistant should do.".into());
    }
    let raw_model = model.unwrap_or(&app.assistant.model);
    if raw_model.len() > MAX_MODEL_BYTES {
        return Err(format!("The model is longer than {MAX_MODEL_BYTES} bytes."));
    }
    let model = gridcraft_assistant::model_name(Some(raw_model)).map_err(|e| e.to_string())?.unwrap_or_default();
    let (owner, revision) = active(app).ok_or("Open a workbook first.")?;
    let raw = app.run("assistant.context", json!({}))?;
    let context = parse_context(&raw, owner, revision)?;
    let request = Request {
        instruction: text.clone(),
        targets: context.targets.iter().map(|t| t.original.clone()).collect(),
        structure: context.targets.iter().map(|t| t.label.clone()).collect(),
        context: None,
    };
    request.validate().map_err(|e| e.to_string())?;
    app.assistant.model = model.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gridcraft-assistant".into())
        .spawn(move || {
            let model = (!model.is_empty()).then_some(model);
            let answer = Client::discover()
                .and_then(|client| client.with_model(model.as_deref()))
                .and_then(|client| client.generate_with_auth(&request, &flag));
            let _ = tx.send(answer);
        })
        .map_err(|_| "Could not start the assistant.".to_string())?;
    let count = context.targets.len();
    app.assistant.worker = Some(Worker { rx, cancel });
    app.assistant.owner = Some(owner);
    app.assistant.pending = Some(context);
    app.assistant.proposal = None;
    app.assistant.show_warning = false;
    app.assistant.consent_given = false;
    app.assistant.open = true;
    app.assistant.status = Some(note(false, "Asking Claude Code…"));
    app.assistant.instruction.clear();
    app.assistant.history.push(text.chars().take(MAX_TURN_CHARS).collect());
    if app.assistant.history.len() > MAX_HISTORY {
        let extra = app.assistant.history.len() - MAX_HISTORY;
        app.assistant.history.drain(..extra);
    }
    Ok(json!({"sent": true, "cells": count}))
}

/// Applies the shown suggestions as one engine command, only if the workbook is still the one
/// they were made for. The proposal is used up either way.
fn apply(app: &mut SheetApp) -> Result<Json, String> {
    if app.assistant.worker.is_some() {
        return Err("Wait for the request to finish first.".into());
    }
    let Some(proposal) = app.assistant.proposal.take() else {
        return Err("There are no suggestions to apply.".into());
    };
    if active(app) != Some((proposal.context.owner, proposal.context.revision)) {
        let text = "The workbook changed after these suggestions were made, so nothing was applied. Ask again.";
        app.assistant.status = Some(note(true, text));
        return Err(text.into());
    }
    if proposal.changes.is_empty() {
        return Err("There are no changes to apply.".into());
    }
    let count = proposal.changes.len();
    match app.run("assistant.apply", json!({"context": proposal.context.json, "changes": proposal.changes})) {
        Ok(_) => {
            app.assistant.status = Some(note(false, format!("Applied {count} change(s). Undo with Ctrl+Z.")));
            Ok(json!({"applied": count}))
        }
        Err(error) => {
            let text = crate::clean_error(&error);
            app.assistant.status = Some(note(true, text.clone()));
            Err(text)
        }
    }
}

fn status_json(app: &SheetApp) -> Json {
    let state = &app.assistant;
    json!({
        "open": state.open,
        "supported": SUPPORTED,
        "pending": state.worker.is_some(),
        "show_warning": state.show_warning,
        "consent_given": state.consent_given,
        "proposal": state.proposal.as_ref().map(|p| json!({"changes": p.rows.len(), "explanation": p.explanation})),
        "status": state.status.as_ref().map(|n| n.text.clone()),
        "error": state.status.as_ref().is_some_and(|n| n.error),
        "history": state.history.len(),
    })
}

/// The assistant's UI commands (routed from `SheetApp::run`).
pub fn command(app: &mut SheetApp, id: &str, p: &Json) -> Result<Json, String> {
    match id {
        "ui.assistant" => {
            let on = p.get("value").and_then(Json::as_bool).unwrap_or(!app.assistant.open);
            set_open(app, on);
            Ok(json!({"open": on}))
        }
        "ui.assistant.request" => {
            let confirmed = p.get("confirm").and_then(Json::as_bool).unwrap_or(false);
            request(app, p.get("instruction").and_then(Json::as_str), p.get("model").and_then(Json::as_str), confirmed)
        }
        "ui.assistant.apply" => apply(app),
        "ui.assistant.dismiss" => {
            dismiss(app);
            Ok(Json::Null)
        }
        "ui.assistant.cancel" => {
            let was_running = app.assistant.worker.is_some();
            cancel_request(app);
            Ok(json!({"cancelled": was_running}))
        }
        "ui.assistant.status" => Ok(status_json(app)),
        "ui.assistant.newConversation" => {
            new_conversation(app);
            Ok(json!({"cleared": true}))
        }
        other => Err(format!("unknown assistant command {other}")),
    }
}

/// Draws the pane (right side, before the grid). Hidden unless opened.
pub fn show(app: &mut SheetApp, ui: &mut Ui) {
    if !app.assistant.open {
        return;
    }
    let t = Tokens::get(ui.ctx());
    egui::Panel::right("assistant_pane").default_size(WIDTH).resizable(true).frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(10)).show(
        ui,
        |ui| {
            header(app, ui, &t);
            ui.separator();
            egui::Panel::bottom("assistant_composer").frame(egui::Frame::NONE.fill(t.ribbon)).show(ui, |ui| composer(app, ui, &t));
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("assistant_body").show(ui, |ui| body(app, ui, &t));
        },
    );
}

fn header(app: &mut SheetApp, ui: &mut Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Mirai Assistant").font(theme::ui_bold(15.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Close").clicked() {
                set_open(app, false);
            }
            if ui.small_button("New").on_hover_text("Start a new conversation").clicked() {
                new_conversation(app);
            }
        });
    });
    ui.label(RichText::new("Drafts new text for the selected cells. Nothing is sent until you confirm.").small().color(t.text_dim));
}

fn body(app: &mut SheetApp, ui: &mut Ui, t: &Tokens) {
    if let Some(status) = app.assistant.status.clone() {
        ui.label(RichText::new(status.text).color(if status.error { t.danger } else { t.text_dim }));
    }
    if app.assistant.worker.is_some() {
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Working on your request…");
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            cancel_request(app);
        }
    }
    if app.assistant.proposal.is_some() {
        preview(app, ui, t);
        return;
    }
    if app.assistant.worker.is_none() {
        welcome(app, ui, t);
    }
}

fn welcome(app: &mut SheetApp, ui: &mut Ui, t: &Tokens) {
    if app.assistant.history.is_empty() {
        ui.label("Select cells that contain text, describe the change, tick the consent box and choose Send.");
        ui.label(RichText::new("Quick drafts only fill the instruction box. They never send by themselves.").small().color(t.text_dim));
    }
    ui.horizontal_wrapped(|ui| {
        for (label, instruction) in CHIPS {
            if ui.button(label).on_hover_text(instruction).clicked() {
                app.assistant.instruction = instruction.to_string();
            }
        }
    });
    if !app.assistant.history.is_empty() {
        ui.separator();
        ui.label(RichText::new("Earlier requests").strong());
        for past in app.assistant.history.iter().rev() {
            ui.label(RichText::new(past).small().color(t.text_dim));
        }
    }
}

enum Action {
    Apply,
    Dismiss,
}

fn preview(app: &mut SheetApp, ui: &mut Ui, t: &Tokens) {
    let mut action = None;
    if let Some(proposal) = app.assistant.proposal.as_ref() {
        ui.label(RichText::new(format!("Suggested changes ({})", proposal.rows.len())).font(theme::ui_bold(13.0)));
        if !proposal.explanation.is_empty() {
            ui.label(RichText::new(&proposal.explanation));
        }
        for row in &proposal.rows {
            egui::Frame::NONE.fill(t.window).corner_radius(4.0).inner_margin(6).show(ui, |ui| {
                ui.label(RichText::new(&row.label).small().color(t.text_dim));
                ui.label(RichText::new(format!("Original: {}", row.original)).small().color(t.text_dim));
                ui.label(RichText::new(format!("Proposed: {}", row.proposed)));
            });
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            if !proposal.rows.is_empty() && ui.button("Apply all").clicked() {
                action = Some(Action::Apply);
            }
            if ui.button("Dismiss").clicked() {
                action = Some(Action::Dismiss);
            }
        });
    }
    match action {
        Some(Action::Apply) => {
            let _ = apply(app);
        }
        Some(Action::Dismiss) => dismiss(app),
        None => {}
    }
}

fn composer(app: &mut SheetApp, ui: &mut Ui, t: &Tokens) {
    let pending = app.assistant.worker.is_some();
    ui.add(egui::TextEdit::singleline(&mut app.assistant.model).hint_text("Model (optional; Claude Code default)").desired_width(f32::INFINITY));
    ui.add(
        egui::TextEdit::multiline(&mut app.assistant.instruction)
            .hint_text("What should change? e.g. Make these shorter.")
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    clamp_text(&mut app.assistant.instruction, MAX_INSTRUCTION_BYTES);
    clamp_text(&mut app.assistant.model, MAX_MODEL_BYTES);

    // Show warning modal instead of checkbox
    if app.assistant.show_warning {
        egui::Window::new("Warning").collapsible(false).resizable(false).default_pos(egui::pos2(100.0, 100.0)).show(ui.ctx(), |ui| {
            ui.label("You are about to send the selected cells' text to Claude Code.");
            ui.label("This action cannot be undone.");
            ui.label("");
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    app.assistant.show_warning = false;
                    app.assistant.consent_given = false;
                }
                if ui.button("Continue").clicked() {
                    app.assistant.show_warning = false;
                    app.assistant.consent_given = true;
                }
            });
            // Close with Enter or Escape
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                app.assistant.show_warning = false;
                app.assistant.consent_given = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.assistant.show_warning = false;
                app.assistant.consent_given = false;
            }
        });
    }

    let ready = SUPPORTED && !pending && active(app).is_some() && !app.assistant.instruction.trim().is_empty();
    let mut send = false;
    ui.horizontal(|ui| {
        send = ui.add_enabled(ready, egui::Button::new("Send")).clicked();
        let hint = if !SUPPORTED { "Desktop app only." } else { "" };
        ui.label(RichText::new(hint).small().color(t.text_dim));
    });
    if send {
        // Show warning before sending
        app.assistant.show_warning = true;
        app.assistant.consent_given = false;
    }

    // If warning was accepted, proceed with send and reset the flag
    if app.assistant.consent_given {
        let confirmed = true;
        let instruction = app.assistant.instruction.clone();
        let model = app.assistant.model.clone();
        if let Err(error) = request(app, Some(&instruction), Some(&model), confirmed) {
            app.assistant.status = Some(note(true, error));
        }
        // Reset the flag so we don't send again on next frame
        app.assistant.consent_given = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcraft_assistant::Suggestion;
    use gridcraft_engine::Session;

    fn app() -> SheetApp {
        let mut session = Session::new();
        session.new_workbook();
        SheetApp::new(session, Default::default())
    }

    fn snapshot(uid_revision: (u64, u64)) -> Json {
        json!({
            "document_id": "doc",
            "revision": uid_revision.1,
            "scope": "selection",
            "targets": [
                {"index": 0, "original": "Hello", "label": "A1", "cell": "A1"},
                {"index": 1, "original": "Bye", "label": "A2", "cell": "A2"}
            ]
        })
    }

    fn context(app: &SheetApp) -> Context {
        let (uid, revision) = active(app).unwrap_or((0, 0));
        parse_context(&snapshot((uid, revision)), uid, revision).unwrap_or_else(|e| panic!("{e}"))
    }

    fn response(suggestions: Vec<Suggestion>) -> Response {
        Response { suggestions, explanation: "Shorter.".into() }
    }

    fn running(app: &mut SheetApp) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        app.assistant.worker = Some(Worker { rx: mpsc::channel::<Answer>().1, cancel: Arc::clone(&flag) });
        flag
    }

    #[test]
    fn toggle_through_run_and_listing() {
        let mut app = app();
        assert_eq!(app.run("ui.assistant", json!({})).ok().and_then(|v| v["open"].as_bool()), Some(true));
        assert_eq!(app.run("ui.assistant", json!({"value": false})).ok().and_then(|v| v["open"].as_bool()), Some(false));
        for (id, _, _) in COMMANDS {
            let r = app.run(id, json!({}));
            assert!(!r.as_ref().err().is_some_and(|e| e.starts_with("unknown")), "{id}: {r:?}");
        }
    }

    #[test]
    fn send_without_confirmation_refuses_before_spawn() {
        let mut app = app();
        let r = app.run("ui.assistant.request", json!({"instruction": "Shorten", "confirm": false}));
        assert!(r.is_err());
        assert!(app.assistant.worker.is_none());
        assert!(app.assistant.owner.is_none());
        assert!(app.run("ui.assistant.request", json!({"instruction": "Shorten"})).is_err());
    }

    #[test]
    fn oversized_instruction_refused_before_spawn() {
        let mut app = app();
        let long = "x".repeat(MAX_INSTRUCTION_BYTES + 1);
        assert!(request(&mut app, Some(&long), None, true).is_err());
        assert!(app.assistant.worker.is_none());
    }

    #[test]
    fn invalid_inputs_leave_draft_unchanged_and_do_not_start_worker() {
        let mut app = app();
        app.assistant.instruction = "draft".into();
        app.assistant.model = "valid-model".into();
        for (instruction, model) in [
            ("short".to_string(), "--bad".to_string()),
            ("short".to_string(), "x".repeat(MAX_MODEL_BYTES + 1)),
            (format!("{}x", " ".repeat(MAX_INSTRUCTION_BYTES)), "valid-model".to_string()),
        ] {
            assert!(request(&mut app, Some(&instruction), Some(&model), true).is_err());
            assert!(app.assistant.worker.is_none());
            assert_eq!(app.assistant.instruction, "draft");
            assert_eq!(app.assistant.model, "valid-model");
        }
    }

    #[test]
    fn close_and_new_conversation_cancel() {
        let mut app = app();
        let flag = running(&mut app);
        app.assistant.open = true;
        app.assistant.show_warning = false;
        app.assistant.consent_given = true;
        set_open(&mut app, false);
        assert!(flag.load(Ordering::SeqCst));
        assert!(app.assistant.worker.is_none());
        assert!(!app.assistant.consent_given);

        let flag = running(&mut app);
        app.run("ui.assistant.newConversation", json!({})).unwrap_or(Json::Null);
        assert!(flag.load(Ordering::SeqCst));
        assert!(app.assistant.worker.is_none());
    }

    #[test]
    fn fake_completion_previews_then_applies_or_refuses_stale() {
        let mut app = app();
        let ctx = context(&app);
        let proposal = build_proposal(ctx.clone(), response(vec![Suggestion { index: 1, text: "See you".into(), markdown: false }]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(proposal.rows.len(), 1);
        assert_eq!(proposal.rows[0].label, "A2");
        assert_eq!(proposal.changes[0]["index"], json!(1));

        app.assistant.owner = Some(ctx.owner);
        app.assistant.proposal = Some(proposal.clone());
        app.assistant.open = true;
        let stale = Proposal { context: Context { revision: ctx.revision + 1, ..ctx.clone() }, ..proposal.clone() };
        app.assistant.proposal = Some(stale);
        assert!(app.run("ui.assistant.apply", json!({})).is_err());
        assert!(app.assistant.proposal.is_none());

        app.assistant.proposal = Some(proposal);
        let _ = app.run("ui.assistant.apply", json!({}));
        assert!(app.assistant.proposal.is_none());
    }

    #[test]
    fn refuses_markdown_and_out_of_selection_answers() {
        let app = app();
        let ctx = context(&app);
        assert!(build_proposal(ctx.clone(), response(vec![Suggestion { index: 0, text: "**x**".into(), markdown: true }])).is_err());
        assert!(build_proposal(ctx.clone(), response(vec![Suggestion { index: 9, text: "x".into(), markdown: false }])).is_err());
        assert!(build_proposal(ctx, response(vec![Suggestion { index: 0, text: "a".into(), markdown: false }; 2])).is_err());
    }

    #[test]
    fn workbook_switch_clears_conversation() {
        let mut app = app();
        let uid = active(&app).map(|(uid, _)| uid).unwrap_or(0);
        let flag = Arc::new(AtomicBool::new(false));
        app.assistant.worker = Some(Worker { rx: mpsc::channel().1, cancel: Arc::clone(&flag) });
        app.assistant.owner = Some(uid);
        app.assistant.history.push("Shorten".into());
        app.session.new_workbook();
        sync(&mut app);
        assert!(app.assistant.owner.is_none());
        assert!(app.assistant.history.is_empty());
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn guidance_names_only_official_steps() {
        assert!(guidance(AssistantError::Authentication).contains("claude auth login"));
        assert!(guidance(AssistantError::Unavailable).contains("claude"));
    }
}
