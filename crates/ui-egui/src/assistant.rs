//! Mirai Assistant: a task pane that asks the official Claude Code CLI for text suggestions for the
//! selected text shapes or the current slide's text shapes. Suggestions are only proposals. Nothing
//! changes until the user applies one through `assistant.apply`, which checks the snapshot the
//! proposal was made from and is undoable. The conversation is ephemeral and bound to one
//! presentation and slide.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use deckcraft_assistant::{AuthState, Error, MAX_INSTRUCTION_BYTES, MAX_MODEL_BYTES, MAX_STRUCTURE_BYTES, Request, Response};
use egui::{Color32, CornerRadius, Margin, RichText, Stroke, TextEdit, Ui};
use serde_json::{Value, json};

use crate::SlideApp;
use crate::theme::Tokens;

/// Task pane id (`ui.pane`).
pub const PANE: &str = "assistant";
const HISTORY_LIMIT: usize = 4;
const HISTORY_BYTES: usize = 512 * 1024;
const INSTALL_HINT: &str = "Install the official Claude Code CLI, then sign in with `claude auth login`.";
const SIGN_IN_COMMAND: &str = "claude auth login";

/// Scope values, as `ui.assistant.request` takes them, with their labels.
const SCOPES: [(&str, &str); 2] = [("selection", "Selected text shapes"), ("slide", "Current slide")];
/// Quick drafts: they fill the composer and never send anything.
const CHIPS: [(&str, &str); 4] = [
    ("Shorten", "Shorten this text and keep the key points."),
    ("Fix grammar", "Fix grammar and spelling. Change nothing else."),
    ("Clearer", "Make this text clearer and easier to read."),
    ("More formal", "Rewrite this text in a more formal tone."),
];

/// The result of one worker run: the sign-in used and the validated response, or a sanitized error.
type Answer = Result<(AuthState, Response), Error>;

struct Pending {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Answer>,
    doc: u64,
    slide: usize,
    scope: &'static str,
    /// The whole context snapshot the request was built from, sent back to `assistant.apply`.
    context: Value,
    /// The user's own words.
    instruction: String,
}

struct Proposal {
    id: u64,
    doc: u64,
    slide: usize,
    scope: &'static str,
    instruction: String,
    context: Value,
    /// Original text per target index; immutable, the preview shows it even after edits.
    originals: Vec<String>,
    response: Response,
}

/// Ephemeral assistant state on the app. Not saved with the presentation or `ui.json`.
pub struct State {
    /// The presentation (`uid`) the conversation belongs to.
    doc: Option<u64>,
    slide: Option<usize>,
    selection: Option<deckcraft_engine::Selection>,
    scope: &'static str,
    instruction: String,
    /// Claude model alias or id; empty keeps Claude Code's default.
    model: String,
    /// Explicit per-request consent. Reset on send, scope change and slide or presentation switch.
    consent: bool,
    connection: Option<AuthState>,
    message: String,
    pending: Option<Pending>,
    history: VecDeque<Proposal>,
    next_id: u64,
    /// Repaints the UI when a worker finishes.
    wake: Option<egui::Context>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            doc: None,
            slide: None,
            selection: None,
            scope: "slide",
            instruction: String::new(),
            model: String::new(),
            consent: false,
            connection: None,
            message: String::new(),
            pending: None,
            history: VecDeque::new(),
            next_id: 1,
            wake: None,
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Parses the `ui.assistant.request` params, validates the request and starts the worker. Nothing
/// is sent without `confirm: true`, and every check runs before the worker starts.
fn request(app: &mut SlideApp, p: &Value) -> Result<Value, String> {
    if p.get("confirm") != Some(&Value::Bool(true)) {
        return Err("confirm: true is required; the chosen text is sent to Claude Code only after you confirm".into());
    }
    if app.assistant.pending.is_some() {
        return Err("an assistant request is already running".into());
    }
    let (doc, slide) = match app.session.active() {
        Some(d) => (d.uid, d.selection.slide),
        None => return Err("open a presentation first".into()),
    };
    let instruction = match p.get("instruction") {
        None => app.assistant.instruction.clone(),
        Some(v) => v.as_str().ok_or("`instruction` must be a string")?.to_owned(),
    };
    let instruction = instruction.trim().to_owned();
    if instruction.is_empty() {
        return Err("give an instruction".into());
    }
    if instruction.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!("the instruction is longer than {MAX_INSTRUCTION_BYTES} bytes"));
    }
    let scope = match p.get("scope").map(|v| v.as_str().ok_or("`scope` must be a string")).transpose()? {
        Some("selection") => "selection",
        None | Some("slide") => "slide",
        Some(_) => return Err("`scope` must be selection or slide".into()),
    };
    let model_text = p.get("model").map(|v| v.as_str().ok_or("`model` must be a string")).transpose()?;
    let model = deckcraft_assistant::model_name(model_text).map_err(|e| e.to_string())?;
    let context = app.session.execute("assistant.context", &json!({"scope": scope})).map_err(|e| e.to_string())?;
    let (originals, structure) = targets_of(&context)?;
    let slide_size = app.session.active().map(|d| format!("Slide size in points: {} × {}", d.doc.slide_size.width, d.doc.slide_size.height));
    let request = Request { instruction: instruction.clone(), targets: originals, structure, context: slide_size };
    request.validate().map_err(|e| e.to_string())?;
    launch(app, Job { doc, slide, scope, context, instruction, model, request })
}

/// Everything a worker needs, checked, plus what its answer is matched against.
struct Job {
    doc: u64,
    slide: usize,
    scope: &'static str,
    context: Value,
    instruction: String,
    model: Option<String>,
    request: Request,
}

#[cfg(target_arch = "wasm32")]
fn launch(_app: &mut SlideApp, _job: Job) -> Result<Value, String> {
    Err("Mirai Assistant needs the desktop app and the official Claude Code CLI; it isn't available in the web version".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn launch(app: &mut SlideApp, job: Job) -> Result<Value, String> {
    let Job { doc, slide, scope, context, instruction, model, request } = job;
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let wake = app.assistant.wake.clone();
    std::thread::Builder::new()
        .name("deckcraft-assistant".into())
        .spawn(move || {
            let answer = deckcraft_assistant::Client::discover()
                .and_then(|client| client.with_model(model.as_deref()))
                .and_then(|client| client.generate_with_auth(&request, &worker_cancel));
            let _ = sender.send(answer);
            if let Some(ctx) = wake {
                ctx.request_repaint();
            }
        })
        .map_err(|_| "could not start the assistant worker".to_owned())?;
    app.assistant.pending = Some(Pending { cancel, receiver, doc, slide, scope, context, instruction });
    app.assistant.message = "Waiting for Claude Code…".into();
    Ok(json!({"started": true, "poll": "ui.assistant.status"}))
}

/// The original text of each target, and a short description of what it is.
fn targets_of(context: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let targets = context.get("targets").and_then(Value::as_array).ok_or("the assistant context has no targets")?;
    let mut originals = Vec::with_capacity(targets.len());
    let mut structure = Vec::with_capacity(targets.len());
    for target in targets {
        let original = target.get("original").and_then(Value::as_str).ok_or("invalid assistant target")?;
        originals.push(original.to_owned());
        let kind = target.get("label").and_then(Value::as_str).filter(|s| s.len() <= MAX_STRUCTURE_BYTES).unwrap_or("text");
        structure.push(kind.to_owned());
    }
    Ok((originals, structure))
}

/// Settles a finished worker on the UI thread. Results for another presentation are dropped.
pub fn poll(app: &mut SlideApp) {
    let answer = match app.assistant.pending.as_ref().map(|p| p.receiver.try_recv()) {
        None | Some(Err(TryRecvError::Empty)) => return,
        Some(Ok(answer)) => answer,
        Some(Err(TryRecvError::Disconnected)) => Err(Error::Failed),
    };
    let current = app.session.active().map(|d| d.uid);
    let Some(pending) = app.assistant.pending.take() else { return };
    let state = &mut app.assistant;
    if pending.cancel.load(Ordering::Relaxed) {
        state.message = "Request cancelled. Text already sent cannot be retracted.".into();
        return;
    }
    if current != Some(pending.doc) {
        return;
    }
    match answer {
        Ok((auth, response)) => {
            state.connection = Some(auth);
            state.message.clear();
            record(state, pending, response);
        }
        Err(error) => {
            if matches!(error, Error::Unavailable | Error::Authentication | Error::UnsupportedAuth) {
                state.connection = None;
            }
            state.message = explain(error);
        }
    }
}

/// A proposal from a validated response. Suggestions that change nothing are dropped.
fn record(state: &mut State, pending: Pending, mut response: Response) {
    let originals = targets_of(&pending.context).map(|(o, _)| o).unwrap_or_default();
    response.suggestions.retain(|s| originals.get(s.index).is_some_and(|o| *o != s.text));
    let id = state.next_id;
    state.next_id = id.saturating_add(1);
    state.history.push_front(Proposal {
        id,
        doc: pending.doc,
        slide: pending.slide,
        scope: pending.scope,
        instruction: pending.instruction,
        context: pending.context,
        originals,
        response,
    });
    trim(state);
}

/// Adds a proposal for the current slide from a snapshot and a response, without starting Claude
/// Code. Used by the headless screenshot and the offline tests. Returns the proposal id.
#[doc(hidden)]
pub fn preview_proposal(app: &mut SlideApp, instruction: &str, context: Value, response: Response) -> Result<u64, String> {
    let (doc, slide) = app.session.active().map(|d| (d.uid, d.selection.slide)).ok_or("open a presentation first")?;
    app.assistant.doc = Some(doc);
    app.assistant.slide = Some(slide);
    let (_, receiver) = std::sync::mpsc::channel();
    let pending =
        Pending { cancel: Arc::new(AtomicBool::new(false)), receiver, doc, slide, scope: "selection", context, instruction: instruction.to_owned() };
    let id = app.assistant.next_id;
    record(&mut app.assistant, pending, response);
    Ok(id)
}

fn trim(state: &mut State) {
    while state.history.len() > HISTORY_LIMIT || proposal_bytes(&state.history) > HISTORY_BYTES {
        if state.history.pop_back().is_none() {
            break;
        }
    }
}

fn proposal_bytes(history: &VecDeque<Proposal>) -> usize {
    history
        .iter()
        .map(|p| {
            p.context
                .to_string()
                .len()
                .saturating_add(p.instruction.len())
                .saturating_add(p.originals.iter().map(String::len).sum::<usize>())
                .saturating_add(p.response.explanation.len())
                .saturating_add(p.response.suggestions.iter().map(|s| s.text.len()).sum::<usize>())
                .saturating_add(p.response.deck.as_ref().map_or(0, |deck| {
                    deck.operations.iter().fold(0usize, |bytes, op| {
                        bytes
                            .saturating_add(op.kind.len())
                            .saturating_add(op.layout.as_ref().map_or(0, String::len))
                            .saturating_add(op.text.as_ref().map_or(0, String::len))
                            .saturating_add(op.color.as_ref().map_or(0, String::len))
                            .saturating_add(128)
                    })
                }))
        })
        .sum()
}

/// Sanitized, actionable text for the panel. Never a raw CLI message or a key.
fn explain(error: Error) -> String {
    match error {
        Error::Unavailable => format!("Claude Code was not found. {INSTALL_HINT}"),
        Error::Authentication => format!("Claude Code is not signed in. Run `{SIGN_IN_COMMAND}` in a terminal, then try again."),
        Error::UnsupportedAuth => {
            format!("Claude Code uses a sign-in method DeckCraft does not accept. Sign in with your Claude account: `{SIGN_IN_COMMAND}`.")
        }
        other => other.to_string(),
    }
}

fn cancel(state: &mut State) {
    if let Some(pending) = &state.pending {
        pending.cancel.store(true, Ordering::Relaxed);
        state.message = "Cancelling…".into();
    }
}

/// Clears the conversation: cancels a running request and drops the draft, consent, proposals
/// and messages. The model choice stays.
fn reset(state: &mut State) {
    if let Some(pending) = state.pending.take() {
        pending.cancel.store(true, Ordering::Relaxed);
    }
    state.instruction.clear();
    state.consent = false;
    state.message.clear();
    state.history.clear();
}

/// Keeps the conversation bound to the active presentation, cancels a request when the pane
/// closes, and polls the worker. Runs every frame, whether or not the pane is open.
pub fn logic(app: &mut SlideApp, ctx: &egui::Context) {
    if app.assistant.wake.is_none() {
        app.assistant.wake = Some(ctx.clone());
    }
    let current = app.session.active().map(|d| (d.uid, d.selection.slide));
    let selection = app.session.active().map(|d| d.selection.clone());
    let doc = current.map(|(doc, _)| doc);
    let slide = current.map(|(_, slide)| slide);
    if app.assistant.doc != doc {
        let had_pending = app.assistant.pending.is_some();
        reset(&mut app.assistant);
        app.assistant.doc = doc;
        app.assistant.slide = slide;
        if had_pending {
            app.assistant.message = "The presentation changed, so the request was cancelled.".into();
        }
    } else if app.assistant.slide != slide {
        app.assistant.consent = false;
        if let Some(pending) = app.assistant.pending.take() {
            pending.cancel.store(true, Ordering::Relaxed);
            app.assistant.message = "The slide changed, so the request was cancelled. Text already sent cannot be retracted.".into();
        }
        app.assistant.slide = slide;
    }
    if app.assistant.selection != selection {
        app.assistant.consent = false;
        app.assistant.selection = selection;
    }
    if app.assistant.pending.is_some() && app.ui.pane.as_deref() != Some(PANE) {
        cancel(&mut app.assistant);
    }
    poll(app);
    if app.assistant.pending.is_some() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

/// Applies a proposal's suggestions through the engine: the stored snapshot and the changes only.
/// The engine checks the snapshot against the document and reports a stale one. A success drops
/// the proposal; a failure keeps it and shows why.
pub fn apply(app: &mut SlideApp, id: u64) {
    let current = app.session.active().map(|d| (d.uid, d.selection.slide));
    let Some(proposal) = app.assistant.history.iter().find(|p| p.id == id) else {
        app.assistant.message = "This proposal is no longer available.".into();
        return;
    };
    if current != Some((proposal.doc, proposal.slide)) {
        app.assistant.message = "Go back to the slide this proposal was made for to apply it.".into();
        return;
    }
    let changes: Vec<Value> = proposal.response.suggestions.iter().map(|s| json!({"index": s.index, "text": s.text, "markdown": false})).collect();
    if changes.is_empty() && proposal.response.deck.is_none() {
        app.assistant.message = "No text changes to apply.".into();
        return;
    }
    let context = proposal.context.clone();
    if !changes.is_empty() && proposal.response.deck.is_some() {
        app.assistant.message = "The proposal mixes text and slide operations. Ask again.".into();
        return;
    }
    let (command, params) = if let Some(deck) = &proposal.response.deck {
        ("assistant.applyDeck", json!({"context": context, "deck": deck}))
    } else {
        ("assistant.apply", json!({"context": context, "changes": changes}))
    };
    match app.run(command, params) {
        Ok(_) => {
            app.assistant.history.retain(|p| p.id != id);
            app.assistant.message.clear();
        }
        Err(error) => {
            app.assistant.message = format!("Could not apply the proposal: {error}. If the slide changed, ask for a new proposal.");
        }
    }
}

/// `ui.assistant`, `ui.assistant.request`, `.cancel`, `.status`, `.newConversation`: the same
/// entry point for the window, the ribbon, the command palette and the control channel.
pub fn command(app: &mut SlideApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "ui.assistant" => {
            let open = match p.get("value") {
                None => app.ui.pane.as_deref() != Some(PANE),
                Some(Value::Bool(on)) => *on,
                Some(_) => return Err("`value` must be a boolean".into()),
            };
            if open {
                app.ui.pane = Some(PANE.into());
            } else {
                if app.ui.pane.as_deref() == Some(PANE) {
                    app.ui.pane = None;
                }
                cancel(&mut app.assistant);
            }
            Ok(json!({"open": open}))
        }
        "ui.assistant.request" => request(app, p),
        "ui.assistant.cancel" => {
            cancel(&mut app.assistant);
            Ok(json!({"cancelling": app.assistant.pending.is_some()}))
        }
        "ui.assistant.status" => {
            poll(app);
            Ok(status(app))
        }
        "ui.assistant.newConversation" => {
            reset(&mut app.assistant);
            Ok(json!({"cleared": true}))
        }
        _ => Err(format!("unknown assistant command {id}")),
    }
}

fn status(app: &SlideApp) -> Value {
    let state = &app.assistant;
    let current = app.session.active().map(|d| (d.uid, d.selection.slide));
    json!({
        "open": app.ui.pane.as_deref() == Some(PANE),
        "busy": state.pending.is_some(),
        "scope": state.scope,
        "connection": match state.connection {
            Some(AuthState::Account) => "claude_account",
            Some(AuthState::OAuthToken) => "oauth_token",
            None => "disconnected",
        },
        "message": state.message,
        "proposals": state
            .history
            .iter()
            .filter(|p| current == Some((p.doc, p.slide)))
            .map(|p| json!({"id": p.id, "scope": p.scope, "explanation": p.response.explanation, "suggestions": p.response.suggestions, "deck": p.response.deck}))
            .collect::<Vec<_>>(),
    })
}

/// A button press in the pane, run after the pane is drawn.
enum Act {
    Send,
    Cancel,
    Apply(u64),
    Dismiss(u64),
}

/// The pane body under its header. Opening it never starts a Claude Code process.
pub fn show(app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let shapes = app.session.active().map(|d| d.selection.shapes.len()).unwrap_or(0);
    let current = app.session.active().map(|d| (d.uid, d.selection.slide));
    let mut action: Option<Act> = None;
    let state = &mut app.assistant;
    let busy = state.pending.is_some();
    ui.spacing_mut().item_spacing.y = 8.0;
    status_line(ui, state, &t);
    scope_chooser(ui, state, shapes, &t);
    if !state.message.is_empty() {
        notice(ui, &state.message, &t);
    }
    egui::Panel::bottom("assistant-composer").frame(egui::Frame::NONE).show(ui, |ui| {
        composer(ui, state, busy, &mut action, &t);
    });
    egui::ScrollArea::vertical().id_salt("assistant-history").auto_shrink([false, false]).show(ui, |ui| {
        if let Some(pending) = &state.pending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Claude Code is working…");
                if ui.button("Cancel").clicked() {
                    action = Some(Act::Cancel);
                }
            });
            ui.label(RichText::new(format!("You asked: {}", pending.instruction)).small().color(t.text_dim));
        }
        let mut shown = false;
        if let Some((doc, slide)) = current {
            for proposal in state.history.iter().filter(|p| p.doc == doc && p.slide == slide) {
                shown = true;
                proposal_card(ui, proposal, &t, &mut action);
            }
        }
        if !shown && !busy {
            welcome(ui, state, &t);
        }
    });
    match action {
        None => {}
        Some(Act::Send) => {
            let consent = std::mem::take(&mut app.assistant.consent);
            let instruction = app.assistant.instruction.clone();
            let model = app.assistant.model.clone();
            let scope = app.assistant.scope;
            let result = app.run("ui.assistant.request", json!({"instruction": instruction, "scope": scope, "model": model, "confirm": consent}));
            match result {
                Ok(_) => app.assistant.instruction.clear(),
                Err(error) => app.assistant.message = error,
            }
        }
        Some(Act::Cancel) => {
            let _ = app.run("ui.assistant.cancel", json!({}));
        }
        Some(Act::Apply(id)) => apply(app, id),
        Some(Act::Dismiss(id)) => app.assistant.history.retain(|p| p.id != id),
    }
}

fn status_line(ui: &mut Ui, state: &mut State, t: &Tokens) {
    let text = match state.connection {
        Some(AuthState::Account) => "Connected with your Claude account",
        Some(AuthState::OAuthToken) => "Connected with your Claude OAuth token",
        None => "Not connected yet. Your first request checks sign-in.",
    };
    ui.label(RichText::new(text).small().color(t.text_dim));
    if ui.add_enabled(state.pending.is_none(), egui::Button::new("New conversation").small()).clicked() {
        reset(state);
    }
    ui.collapsing("Sign in", |ui| {
        ui.label("Claude Code signs you in. DeckCraft never reads or stores your credentials.");
        ui.horizontal_wrapped(|ui| {
            ui.monospace(SIGN_IN_COMMAND);
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(SIGN_IN_COMMAND.into());
            }
        });
    });
}

fn scope_chooser(ui: &mut Ui, state: &mut State, shapes: usize, t: &Tokens) {
    ui.label(RichText::new("Change").small().color(t.text_dim));
    ui.horizontal_wrapped(|ui| {
        for (value, label) in SCOPES {
            if ui.selectable_label(state.scope == value, label).clicked() && state.scope != value {
                state.scope = value;
                state.consent = false;
            }
        }
    });
    let hint = if state.scope == "selection" {
        match shapes {
            0 => "Nothing selected. Select text shapes, or choose Current slide.".to_owned(),
            n => format!("{n} selected. Text shapes only; tables, groups and notes are left unchanged."),
        }
    } else {
        "Every text shape on this slide. Tables, groups and notes are left unchanged.".to_owned()
    };
    ui.label(RichText::new(hint).small().color(t.text_dim));
}

fn notice(ui: &mut Ui, text: &str, t: &Tokens) {
    egui::Frame::new().fill(t.text_field).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.label(RichText::new(text).color(t.contextual));
    });
}

fn composer(ui: &mut Ui, state: &mut State, busy: bool, action: &mut Option<Act>, t: &Tokens) {
    ui.add_enabled_ui(!busy, |ui| {
        ui.add(
            TextEdit::multiline(&mut state.instruction)
                .hint_text("What should change? For example: shorten the bullets.")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        clamp_bytes(&mut state.instruction, MAX_INSTRUCTION_BYTES);
        ui.add(TextEdit::singleline(&mut state.model).hint_text("Model (optional): Claude Code's default").desired_width(f32::INFINITY));
        clamp_bytes(&mut state.model, MAX_MODEL_BYTES);
        ui.checkbox(&mut state.consent, "Send the instruction, chosen text and shape descriptions to Claude Code for this request");
        ui.horizontal(|ui| {
            let ready = state.consent && !state.instruction.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Send")).clicked() {
                *action = Some(Act::Send);
            }
            if !ready {
                ui.label(RichText::new("Tick the box and write a request.").small().color(t.text_dim));
            }
        });
    });
}

/// Keeps a text field within `max` bytes, cut on a character boundary.
fn clamp_bytes(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

fn welcome(ui: &mut Ui, state: &mut State, t: &Tokens) {
    ui.label(RichText::new("Ask for a change to the text you chose.").strong());
    ui.label(RichText::new("Nothing changes until you apply a proposal.").small().color(t.text_dim));
    ui.horizontal_wrapped(|ui| {
        for (label, prompt) in CHIPS {
            if ui.small_button(label).clicked() {
                state.instruction = prompt.into();
            }
        }
    });
}

fn proposal_card(ui: &mut Ui, proposal: &Proposal, t: &Tokens, action: &mut Option<Act>) {
    let scope = SCOPES.iter().find(|(v, _)| *v == proposal.scope).map(|(_, l)| *l).unwrap_or("Current slide");
    egui::Frame::new().stroke(Stroke::new(1.0, t.card_border)).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.label(RichText::new(format!("Proposal · {scope}")).strong());
        ui.label(RichText::new(format!("“{}”", proposal.instruction)).small().color(t.text_dim));
        if !proposal.response.explanation.is_empty() {
            ui.label(proposal.response.explanation.as_str());
        }
        if proposal.response.suggestions.is_empty() && proposal.response.deck.is_none() {
            ui.label(RichText::new("No changes proposed.").color(t.text_dim));
        }
        for suggestion in &proposal.response.suggestions {
            let original = proposal.originals.get(suggestion.index).map_or("", String::as_str);
            ui.separator();
            ui.label(RichText::new("Original").small().color(t.text_dim));
            ui.label(original);
            ui.label(RichText::new("Proposal").small().color(t.text_dim));
            let text = if suggestion.text.is_empty() { "(empty: the text is removed)" } else { suggestion.text.as_str() };
            ui.label(RichText::new(text).color(accent_text(t)));
        }
        if let Some(deck) = &proposal.response.deck {
            let labels = target_labels(&proposal.context);
            for op in &deck.operations {
                ui.separator();
                ui.label(RichText::new(deck_op_line(op, &labels)).small().color(t.text_dim));
                if let Some(text) = op.text.as_deref().filter(|t| !t.is_empty()) {
                    ui.label(RichText::new(text).color(accent_text(t)));
                }
            }
        }
        ui.horizontal(|ui| {
            let apply = !proposal.response.suggestions.is_empty() || proposal.response.deck.is_some();
            if ui.add_enabled(apply, egui::Button::new("Apply all")).clicked() {
                *action = Some(Act::Apply(proposal.id));
            }
            if ui.button("Dismiss").clicked() {
                *action = Some(Act::Dismiss(proposal.id));
            }
        });
    });
}

/// The shape label of each captured target index, for naming deck operations.
fn target_labels(context: &Value) -> Vec<String> {
    context
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| targets.iter().map(|t| t.get("label").and_then(Value::as_str).unwrap_or("text").to_owned()).collect())
        .unwrap_or_default()
}

/// One line describing a deck operation for the review card.
fn deck_op_line(op: &deckcraft_assistant::DeckOperation, labels: &[String]) -> String {
    let label = op.target.and_then(|i| labels.get(i)).map_or_else(|| "a shape".to_owned(), |l| format!("\u{201C}{l}\u{201D}"));
    let rect = op.rect.map_or_else(String::new, |[x, y, w, h]| format!(" at {x}, {y} · {w} × {h} pt"));
    match op.kind.as_str() {
        "new_slide" => match op.layout.as_deref() {
            Some("title") => "New Title slide".to_owned(),
            Some("titleAndContent") => "New Title and Content slide".to_owned(),
            _ => "New Blank slide".to_owned(),
        },
        "add_text" => format!("Text box{rect}"),
        "set_text" => format!("Replace the text of {label}"),
        "set_bounds" => format!("Move and resize {label}{rect}"),
        "background" => format!("Solid background colour {}", op.color.as_deref().unwrap_or_default()),
        other => other.to_owned(),
    }
}

fn accent_text(t: &Tokens) -> Color32 {
    if t.dark { t.text } else { t.accent }
}

/// Review a proposal beside its first target without changing the slide until Apply is pressed.
pub(crate) fn show_inline(app: &mut SlideApp, ui: &Ui, xf: crate::canvas::Xf, available: egui::Rect, t: &Tokens) {
    let Some(doc) = app.session.active() else { return };
    if doc.selection.target != deckcraft_engine::Target::Slides || !xf.scale.is_finite() || xf.scale <= 0.0 {
        return;
    }
    let Some(proposal) =
        app.assistant.history.iter().find(|p| p.doc == doc.uid && p.slide == doc.selection.slide && !p.response.suggestions.is_empty())
    else {
        return;
    };
    let Some(index) = proposal.response.suggestions.first().map(|s| s.index) else { return };
    let Some(shape_id) = proposal
        .context
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.get(index))
        .and_then(|target| target.get("shape"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    else {
        return;
    };
    let Some(shape) = doc.shape(deckcraft_model::ShapeId(shape_id)) else { return };
    let transform = deckcraft_engine::cmd::xfrm_of(&doc.doc, &doc.selection, shape);
    let affine = transform.affine();
    let corners = [
        deckcraft_geom::Point::new(0.0, 0.0),
        deckcraft_geom::Point::new(transform.w, 0.0),
        deckcraft_geom::Point::new(transform.w, transform.h),
        deckcraft_geom::Point::new(0.0, transform.h),
    ];
    let points = corners.map(|p| xf.to_screen(affine * p));
    if points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) || available.width() < 120.0 || available.height() < 80.0 {
        return;
    }
    let right = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let top = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let width = 260.0_f32.min(available.width() - 16.0);
    let x = (right + 8.0).clamp(available.left() + 4.0, available.right() - width - 4.0);
    let y = if right + width + 12.0 <= available.right() { top } else { available.top() + 4.0 };
    let y = y.clamp(available.top() + 4.0, available.bottom() - 80.0);
    let id = proposal.id;
    let mut apply_clicked = false;
    let mut dismiss_clicked = false;
    let message = app.assistant.message.clone();
    let Some(proposal) = app.assistant.history.iter_mut().find(|p| p.id == id) else { return };
    egui::Area::new(egui::Id::unique(("mirai_inline", id))).order(egui::Order::Foreground).fixed_pos(egui::pos2(x, y)).show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.label(RichText::new("Mirai · Review on slide").strong());
                ui.label(RichText::new(&proposal.instruction).small().color(t.text_dim));
                if !message.is_empty() {
                    ui.label(RichText::new(&message).small().color(t.text_dim));
                }
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for (n, suggestion) in proposal.response.suggestions.iter_mut().enumerate() {
                        ui.label(RichText::new(proposal.originals.get(suggestion.index).map_or("", String::as_str)).small().color(t.text_dim));
                        ui.add(
                            TextEdit::multiline(&mut suggestion.text)
                                .id_salt((id, n))
                                .hint_text("Edit proposed text")
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        );
                        if suggestion.text.is_empty() {
                            ui.label(RichText::new("Empty proposal: applying removes this text.").small().color(t.contextual));
                        }
                        if suggestion.text.len() > deckcraft_assistant::MAX_TEXT_BYTES {
                            let mut end = deckcraft_assistant::MAX_TEXT_BYTES;
                            while !suggestion.text.is_char_boundary(end) {
                                end -= 1;
                            }
                            suggestion.text.truncate(end);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    apply_clicked = ui.button("Apply all").clicked();
                    dismiss_clicked = ui.button("Dismiss").clicked();
                });
            });
    });
    if apply_clicked {
        apply(app, id);
    }
    if dismiss_clicked {
        app.assistant.history.retain(|p| p.id != id);
    }
}

/// A running request with no process behind it: the test keeps the sender, so the answer is
/// whatever the test sends (or nothing).
#[cfg(test)]
pub(crate) fn start_fake_for_test(app: &mut SlideApp) -> (Arc<AtomicBool>, std::sync::mpsc::Sender<Answer>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let doc = app.session.active().map(|d| d.uid).unwrap_or_default();
    let slide = app.session.active().map(|d| d.selection.slide).unwrap_or(0);
    app.assistant.doc = Some(doc);
    app.assistant.slide = Some(slide);
    app.assistant.pending = Some(Pending {
        cancel: cancel.clone(),
        receiver,
        doc,
        slide,
        scope: "selection",
        context: json!({"targets": []}),
        instruction: "Shorten".into(),
    });
    (cancel, sender)
}

#[cfg(test)]
mod slide_consent_tests {
    use super::*;

    #[test]
    fn changing_selection_clears_consent_on_same_slide() {
        let mut session = deckcraft_engine::Session::with_new();
        deckcraft_engine::sample::open_sample(&mut session).unwrap();
        let mut app = SlideApp::new(session, crate::Services::default());
        let ctx = egui::Context::default();
        logic(&mut app, &ctx);
        app.assistant.consent = true;
        app.session.active_mut().unwrap().selection.shapes.push(deckcraft_model::ShapeId(999));
        logic(&mut app, &ctx);
        assert!(!app.assistant.consent);
    }

    #[test]
    fn inline_apply_button_changes_the_slide_and_undo_restores_it() {
        use egui_kittest::kittest::Queryable;
        let mut session = deckcraft_engine::Session::with_new();
        deckcraft_engine::sample::open_sample(&mut session).unwrap();
        let mut app = SlideApp::new(session, crate::Services::default());
        let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
        let original = context["targets"][0]["original"].as_str().unwrap().to_owned();
        let shape = context["targets"][0]["shape"].as_u64().unwrap() as u32;
        let response = Response {
            suggestions: vec![deckcraft_assistant::Suggestion { index: 0, text: "Inline replacement".into(), markdown: false }],
            explanation: String::new(),
            deck: None,
        };
        preview_proposal(&mut app, "Rewrite", context, response).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).build_ui_state(
            |ui, app: &mut SlideApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run_steps(3);
        h.get_by_label("Apply all").click();
        h.run_steps(3);
        let text = |app: &SlideApp| app.session.active().unwrap().shape(deckcraft_model::ShapeId(shape)).unwrap().text.as_ref().unwrap().text();
        assert_eq!(text(h.state()), "Inline replacement");
        h.state_mut().session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(text(h.state()), original);
    }

    #[test]
    fn edited_proposal_applies_to_slide_and_undo_restores_text() {
        let mut session = deckcraft_engine::Session::with_new();
        deckcraft_engine::sample::open_sample(&mut session).unwrap();
        let mut app = SlideApp::new(session, crate::Services::default());
        let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
        let original = context["targets"][0]["original"].as_str().unwrap().to_owned();
        let shape = context["targets"][0]["shape"].as_u64().unwrap() as u32;
        let response = Response {
            suggestions: vec![deckcraft_assistant::Suggestion { index: 0, text: "Suggested".into(), markdown: false }],
            explanation: String::new(),
            deck: None,
        };
        let id = preview_proposal(&mut app, "Rewrite", context, response).unwrap();
        app.assistant.history.front_mut().unwrap().response.suggestions[0].text = "Edited draft".into();
        apply(&mut app, id);
        let text = |app: &SlideApp| app.session.active().unwrap().shape(deckcraft_model::ShapeId(shape)).unwrap().text.as_ref().unwrap().text();
        assert_eq!(text(&app), "Edited draft");
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(text(&app), original);
    }

    #[test]
    fn changing_slide_clears_consent_and_cancels_pending_without_erasing_history() {
        let mut session = deckcraft_engine::Session::with_new();
        deckcraft_engine::sample::open_sample(&mut session).unwrap();
        let mut app = SlideApp::new(session, crate::Services::default());
        app.ui.pane = Some(PANE.into());
        let ctx = egui::Context::default();
        logic(&mut app, &ctx);
        let (cancel, _sender) = start_fake_for_test(&mut app);
        let context = json!({"targets": [{"index": 0, "original": "Old", "label": "text"}]});
        let response = Response {
            suggestions: vec![deckcraft_assistant::Suggestion { index: 0, text: "New".into(), markdown: false }],
            explanation: String::new(),
            deck: None,
        };
        preview_proposal(&mut app, "Shorten", context, response).unwrap();
        app.assistant.consent = true;
        let original_slide = app.session.active().unwrap().selection.slide;
        app.session.active_mut().unwrap().selection.slide = original_slide.saturating_add(1);
        logic(&mut app, &ctx);
        assert!(!app.assistant.consent);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.assistant.pending.is_none());
        assert_eq!(app.assistant.history.len(), 1);
        assert!(status(&app)["proposals"].as_array().unwrap().is_empty());
        assert!(app.assistant.message.contains("cancelled"));
    }
}
