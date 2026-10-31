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

use deckcraft_assistant::{
    AuthState, Error, GeometryTarget, MAX_IMAGE_BYTES, MAX_IMAGES, MAX_INSTRUCTION_BYTES, MAX_MODEL_BYTES, MAX_STRUCTURE_BYTES, Request,
    RequestImage, Response,
};
use egui::{Color32, CornerRadius, Event, ImeEvent, Key, Margin, RichText, Stroke, TextEdit, Ui};
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
const SCOPES: [(&str, &str); 3] = [("selection", "Selected text shapes"), ("slide", "Current slide"), ("presentation", "Entire presentation")];
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
    /// The geometry snapshot of the current slide, sent back with shape operations; `None` unless the
    /// request was for the current slide.
    geometry: Option<Value>,
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
    /// Geometry snapshot the proposal's shape operations index into; applied with them.
    geometry: Option<Value>,
    /// Original text per target index; immutable, the preview shows it even after edits.
    originals: Vec<String>,
    response: Response,
}

/// What the confirmation notice was shown for. Any change to one of these cancels the confirmation,
/// so a request is only sent with the scope, text and model the user actually reviewed.
#[derive(Clone, PartialEq)]
pub(crate) struct Armed {
    doc: u64,
    revision: u64,
    slide: usize,
    selection: deckcraft_engine::Selection,
    scope: &'static str,
    instruction: String,
    model: String,
    /// Whether rendered slide images will be sent with the request.
    images: bool,
}

/// The confirmation a change would be checked against, from the live session and pane state.
fn armed_now(app: &SlideApp) -> Option<Armed> {
    let doc = app.session.active()?;
    Some(Armed {
        doc: doc.uid,
        revision: doc.revision,
        slide: doc.selection.slide,
        selection: doc.selection.clone(),
        scope: app.assistant.scope,
        instruction: app.assistant.instruction.clone(),
        model: app.assistant.model.clone(),
        images: app.assistant.include_images,
    })
}

/// The instruction field's id, so the test and the composer agree on it.
const INSTRUCTION_ID: &str = "assistant-instruction";

/// Ephemeral assistant state on the app. Not saved with the presentation or `ui.json`.
pub struct State {
    /// The presentation (`uid`) the conversation belongs to.
    doc: Option<u64>,
    slide: Option<usize>,
    selection: Option<deckcraft_engine::Selection>,
    scope: &'static str,
    pub(crate) instruction: String,
    /// Claude model alias or id; empty keeps Claude Code's default.
    pub(crate) model: String,
    /// Whether the user chose to send rendered slide images; off by default and part of the
    /// confirmed state, so toggling it cancels an armed request.
    pub(crate) include_images: bool,
    /// Set by the first Enter or Send press; the notice is then shown for this exact state. A
    /// second Enter or Confirm sends. Cleared by any change, by send and by reset.
    pub(crate) armed: Option<Armed>,
    /// An IME composition is in progress in the instruction field.
    pub(crate) composing: bool,
    /// The instruction field's id, set while it is drawn.
    pub(crate) input_id: Option<egui::Id>,
    /// Enter is ignored up to this frame after an IME event, so a key that committed text cannot
    /// also act as a send.
    ime_until: u64,
    connection: Option<AuthState>,
    pub(crate) message: String,
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
            include_images: false,
            armed: None,
            composing: false,
            ime_until: 0,
            input_id: None,
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
        Some("presentation") => "presentation",
        Some(_) => return Err("`scope` must be selection, slide or presentation".into()),
    };
    let model_text = p.get("model").map(|v| v.as_str().ok_or("`model` must be a string")).transpose()?;
    let model = deckcraft_assistant::model_name(model_text).map_err(|e| e.to_string())?;
    let include_images = match p.get("images") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return Err("`images` must be a boolean".into()),
    };
    let context = if scope == "presentation" {
        app.session.execute("assistant.presentationContext", &json!({}))
    } else {
        app.session.execute("assistant.context", &json!({"scope": scope}))
    }
    .map_err(|e| e.to_string())?;
    let (originals, structure) = targets_of(&context)?;
    let slide_size = app.session.active().map(|d| {
        format!(
            "Scope: {scope}. Slide size in points: {} × {}. Slides: {}; current slide: {}.",
            d.doc.slide_size.width,
            d.doc.slide_size.height,
            d.doc.slides.len(),
            d.selection.slide.saturating_add(1)
        )
    });
    // Shapes are extras: a slide whose geometry can't be read still gets its text edits.
    let geometry = match scope {
        "slide" => app.session.execute("assistant.geometryContext", &json!({})).ok(),
        "presentation" => app.session.execute("assistant.presentationGeometryContext", &json!({})).ok(),
        _ => None,
    };
    let shapes = geometry.as_ref().map(geometry_of).transpose()?.unwrap_or_default();
    // Rendered slide images, only when the user explicitly chose to send them.
    let images = if include_images { capture_slides(app, scope)? } else { Vec::new() };
    let request = Request { instruction: instruction.clone(), targets: originals, structure, context: slide_size, geometry: shapes, images };
    request.validate().map_err(|e| e.to_string())?;
    launch(app, Job { doc, slide, scope, context, geometry, instruction, model, request })
}

/// The geometry entries of an `assistant.geometryContext` snapshot, numbered as the snapshot numbers
/// them. These indices are a separate space from the text targets.
fn geometry_of(snapshot: &Value) -> Result<Vec<GeometryTarget>, String> {
    let shapes = snapshot.get("shapes").and_then(Value::as_array).ok_or("the geometry context has no shapes")?;
    shapes
        .iter()
        .enumerate()
        .map(|(position, shape)| -> Result<GeometryTarget, String> {
            let index = shape.get("index").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok()).ok_or("invalid geometry shape")?;
            if index != position {
                return Err("invalid geometry shape".into());
            }
            let rect = shape.get("rect").and_then(Value::as_array).filter(|r| r.len() == 4).ok_or("invalid geometry shape")?;
            let mut bounds = [0.0_f64; 4];
            for (slot, value) in bounds.iter_mut().zip(rect) {
                *slot = value.as_f64().ok_or("invalid geometry shape")?;
            }
            Ok(GeometryTarget {
                index,
                label: shape.get("label").and_then(Value::as_str).unwrap_or("shape").to_owned(),
                preset: shape.get("preset").and_then(Value::as_str).unwrap_or_default().to_owned(),
                rect: bounds,
                fill: shape.get("fill").and_then(Value::as_str).map(str::to_owned),
                slide_index: shape.get("slide_index").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()),
            })
        })
        .collect()
}

/// Renders the slides the request covers to PNG, offscreen, bounded: the current slide for a
/// slide or selection request, the first [`MAX_IMAGES`] slides for a presentation request. The
/// render scale keeps the width near 1280 pixels; an image over [`MAX_IMAGE_BYTES`] is re-rendered
/// smaller, and one that stays over the cap fails the request.
fn capture_slides(app: &SlideApp, scope: &str) -> Result<Vec<RequestImage>, String> {
    let doc = app.session.active().ok_or("open a presentation first")?;
    let count = doc.doc.slides.len();
    let indices: Vec<usize> = if scope == "presentation" { (0..count).take(MAX_IMAGES).collect() } else { vec![doc.selection.slide] };
    let width = if doc.doc.slide_size.width.is_finite() && doc.doc.slide_size.width > 0.0 { doc.doc.slide_size.width } else { 720.0 };
    let mut images = Vec::with_capacity(indices.len());
    for index in indices {
        let mut scale = (1280.0 / width).clamp(0.5, 2.0);
        let mut png = None;
        for _ in 0..3 {
            let opts = deckcraft_render::RenderOpts { scale, edit: false, ..Default::default() };
            let bytes = deckcraft_render::render_slide(&doc.doc, index, &opts).to_png();
            if bytes.len() <= MAX_IMAGE_BYTES {
                png = Some(bytes);
                break;
            }
            scale /= 2.0;
        }
        let png = png.ok_or("a rendered slide image is too large to send; try again without images")?;
        images.push(RequestImage { png });
    }
    Ok(images)
}

/// Everything a worker needs, checked, plus what its answer is matched against.
struct Job {
    doc: u64,
    slide: usize,
    scope: &'static str,
    context: Value,
    geometry: Option<Value>,
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
    let Job { doc, slide, scope, context, geometry, instruction, model, request } = job;
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
    app.assistant.pending = Some(Pending { cancel, receiver, doc, slide, scope, context, geometry, instruction });
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
            if let Some(problem) = unusable_shape(&response) {
                state.message = format!("{problem} Try again.");
                return;
            }
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

/// Why an `add_shape` of the response can't be applied: a preset outside the gallery, or a flat box
/// for a shape that isn't a line. The protocol only checks the name's form and allows flat boxes
/// for lines; `assistant.applyDeck` would refuse these, so the proposal is never offered.
fn unusable_shape(response: &Response) -> Option<String> {
    let deck = response.deck.as_ref()?;
    deck.operations.iter().filter(|op| op.kind == "add_shape").find_map(|op| {
        let preset = op.preset.as_deref().unwrap_or_default();
        if deckcraft_geom::preset::info(preset).is_none() {
            return Some(format!("Claude proposed a `{preset}` shape, which isn't in the shape gallery."));
        }
        let flat = op.rect.is_some_and(|r| r[2] <= 0.0 || r[3] <= 0.0);
        (flat && !deckcraft_geom::preset::is_line_like(preset)).then(|| format!("Claude proposed a `{preset}` shape with no width or height."))
    })
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
        geometry: pending.geometry,
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
    let scope = match context.get("scope").and_then(Value::as_str) {
        Some("presentation") => "presentation",
        Some("slide") => "slide",
        _ => "selection",
    };
    let pending = Pending {
        cancel: Arc::new(AtomicBool::new(false)),
        receiver,
        doc,
        slide,
        scope,
        context,
        geometry: None,
        instruction: instruction.to_owned(),
    };
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
                .saturating_add(p.geometry.as_ref().map_or(0, |g| g.to_string().len()))
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
    state.armed = None;
    state.message.clear();
    state.history.clear();
}

/// Keeps the conversation bound to the active presentation, cancels a request when the pane
/// closes, and polls the worker. Runs every frame, whether or not the pane is open.
pub fn logic(app: &mut SlideApp, ctx: &egui::Context) {
    if app.assistant.wake.is_none() {
        app.assistant.wake = Some(ctx.clone());
    }
    if app.assistant.armed.is_some() && app.assistant.armed != armed_now(app) {
        app.assistant.armed = None;
        app.assistant.message = "The request changed, so the confirmation was cancelled. Press Enter again to review it.".into();
    }
    if app.ui.pane.as_deref() != Some(PANE) {
        app.assistant.armed = None;
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
        app.assistant.armed = None;
        if let Some(pending) = app.assistant.pending.take() {
            pending.cancel.store(true, Ordering::Relaxed);
            app.assistant.message = "The slide changed, so the request was cancelled. Text already sent cannot be retracted.".into();
        }
        app.assistant.slide = slide;
    }
    if app.assistant.selection != selection {
        app.assistant.armed = None;
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
    let geometry = proposal.geometry.clone();
    let (command, params) = if proposal.scope == "presentation" {
        if let Some(deck) = &proposal.response.deck {
            let mut params = json!({"context": context, "deck": deck, "changes": changes});
            if let Some(geometry) = geometry {
                params["geometry"] = geometry;
            }
            ("assistant.applyPresentation", params)
        } else {
            ("assistant.applyPresentation", json!({"context": context, "changes": changes}))
        }
    } else if let Some(deck) = &proposal.response.deck {
        let mut params = json!({"context": context, "deck": deck, "changes": changes});
        if let Some(geometry) = geometry {
            params["geometry"] = geometry;
        }
        ("assistant.applyDeck", params)
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
    /// Shows the confirmation notice for the current state.
    Arm,
    /// Sends the request; only valid while the notice is shown for the current state.
    Send,
    Disarm,
    Cancel,
    Apply(u64),
    Dismiss(u64),
}

/// The pane body under its header. Opening it never starts a Claude Code process.
pub fn show(app: &mut SlideApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let shapes = app.session.active().map(|d| d.selection.shapes.len()).unwrap_or(0);
    let current = app.session.active().map(|d| (d.uid, d.selection.slide));
    let armed_current = armed_now(app);
    let mut action: Option<Act> = None;
    let state = &mut app.assistant;
    let busy = state.pending.is_some();
    ui.spacing_mut().item_spacing.y = 8.0;
    brand_header(ui, &t);
    status_line(ui, state, &t);
    scope_chooser(ui, state, shapes, &t);
    if !state.message.is_empty() {
        notice(ui, &state.message, &t);
    }
    egui::Panel::bottom("assistant-composer").frame(egui::Frame::NONE).show(ui, |ui| {
        composer(ui, state, busy, armed_current.as_ref(), &mut action, &t);
    });
    egui::ScrollArea::vertical().id_salt("assistant-history").auto_shrink([false, false]).show(ui, |ui| {
        if ui.checkbox(&mut state.include_images, "Send rendered slide images").changed() {
            state.armed = None;
        }
        if state.include_images {
            let which = if state.scope == "presentation" {
                format!("Claude will also receive rendered pictures of the first {MAX_IMAGES} slides.")
            } else {
                "Claude will also receive a rendered picture of the current slide.".to_owned()
            };
            ui.label(RichText::new(which).small().color(t.text_dim));
        }
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
        Some(Act::Arm) => {
            app.assistant.armed = armed_now(app);
            app.assistant.message.clear();
        }
        Some(Act::Disarm) => app.assistant.armed = None,
        Some(Act::Send) => {
            let armed = app.assistant.armed.take();
            if armed.is_none() || armed != armed_now(app) {
                app.assistant.message = "The request changed, so the confirmation was cancelled. Review it again.".into();
                return;
            }
            let instruction = app.assistant.instruction.clone();
            let model = app.assistant.model.clone();
            let scope = app.assistant.scope;
            let images = app.assistant.include_images;
            let result = app
                .run("ui.assistant.request", json!({"instruction": instruction, "scope": scope, "model": model, "images": images, "confirm": true}));
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

const BRAND_SVG: &[u8] = include_bytes!("../../../assets/assistant/mirai-gorrion.svg");
const BRAND_HEIGHT: u32 = 64;

/// The logo texture, rasterised once per egui context. `None` means the SVG could not be loaded and
/// the text fallback is shown instead.
#[derive(Clone)]
struct Brand(Option<egui::TextureHandle>);

pub(crate) fn brand(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique("mirai-assistant-brand");
    if let Some(cached) = ctx.data(|d| d.get_temp::<Brand>(id)) {
        return cached.0;
    }
    let texture = load_svg_texture(ctx, BRAND_SVG);
    ctx.data_mut(|d| d.insert_temp(id, Brand(texture.clone())));
    texture
}

pub(crate) fn load_svg_texture(ctx: &egui::Context, svg: &[u8]) -> Option<egui::TextureHandle> {
    let size = egui::SizeHint::Height(BRAND_HEIGHT);
    let image = egui_extras::image::load_svg_bytes_with_size(svg, size, &Default::default()).ok()?;
    Some(crate::textures::upload(ctx, "mirai-assistant-brand", image))
}

fn brand_header(ui: &mut Ui, t: &Tokens) {
    match brand(ui.ctx()) {
        Some(texture) => {
            let image = egui::Image::new(&texture).max_height(BRAND_HEIGHT as f32).alt_text("Mirai logo");
            ui.add(image);
        }
        None => {
            ui.label(RichText::new("Mirai").strong().color(accent_text(t)));
        }
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
                state.armed = None;
            }
        }
    });
    let hint = if state.scope == "selection" {
        match shapes {
            0 => "Nothing selected. Select text shapes, or choose Current slide.".to_owned(),
            n => format!("{n} selected. Text shapes only; tables, groups and notes are left unchanged."),
        }
    } else if state.scope == "presentation" {
        "Text shapes across the entire presentation are sent. Tables, groups and notes are excluded; up to 256 text shapes.".to_owned()
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

fn composer(ui: &mut Ui, state: &mut State, busy: bool, current: Option<&Armed>, action: &mut Option<Act>, t: &Tokens) {
    let input_id = ui.make_persistent_id(INSTRUCTION_ID);
    state.input_id = Some(input_id);
    let focused = ui.memory(|m| m.has_focus(input_id));
    let frame = ui.ctx().cumulative_frame_nr();
    let ime: Vec<ImeEvent> = ui.input(|i| i.events.iter().filter_map(|e| if let Event::Ime(ev) = e { Some(ev.clone()) } else { None }).collect());
    for event in &ime {
        state.ime_until = frame.saturating_add(2);
        match event {
            ImeEvent::Preedit { text, .. } => state.composing = !text.is_empty(),
            ImeEvent::Commit(_) => state.composing = false,
            _ => {}
        }
    }
    if !focused {
        state.composing = false;
    }
    let plain_enter = focused && !busy && !state.composing && frame > state.ime_until;
    let enter = if plain_enter { ui.input_mut(|i| take_plain_enter(&mut i.events)) } else { None };
    let ready = !state.instruction.trim().is_empty();
    if enter == Some(false) && ready {
        let confirmed = state.armed.as_ref().is_some_and(|armed| Some(armed) == current);
        *action = Some(if confirmed { Act::Send } else { Act::Arm });
    }
    ui.add_enabled_ui(!busy, |ui| {
        ui.add(
            TextEdit::multiline(&mut state.instruction)
                .id(input_id)
                .hint_text("What should change? For example: shorten the bullets.")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        clamp_bytes(&mut state.instruction, MAX_INSTRUCTION_BYTES);
        ui.add(TextEdit::singleline(&mut state.model).hint_text("Model (optional): Claude Code's default").desired_width(f32::INFINITY));
        clamp_bytes(&mut state.model, MAX_MODEL_BYTES);
        let confirmed = state.armed.as_ref().is_some_and(|armed| Some(armed) == current);
        if confirmed {
            let what = match state.scope {
                "selection" => "the selected text shapes",
                "presentation" => "eligible text shapes across the entire presentation",
                _ => "the text and unlocked shapes on this slide",
            };
            let model = if state.model.trim().is_empty() { String::new() } else { format!(" with model {}", state.model.trim()) };
            let images = if state.include_images { ", plus rendered pictures of the slides" } else { "" };
            notice(
                ui,
                &format!(
                    "Confirm before sending: your instruction, the original text and shape descriptions of {what} go to Claude Code{model}{images}. Press Enter again or choose Confirm and send. Any change cancels this."
                ),
                t,
            );
            ui.horizontal(|ui| {
                if ui.button("Confirm and send").clicked() {
                    *action = Some(Act::Send);
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(Act::Disarm);
                }
            });
        } else {
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, egui::Button::new("Send")).clicked() {
                    *action = Some(Act::Arm);
                }
                if !ready {
                    ui.label(RichText::new("Write a request to send it.").small().color(t.text_dim));
                } else {
                    ui.label(RichText::new("Enter reviews the request; Enter again sends. Shift+Enter adds a line.").small().color(t.text_dim));
                }
            });
        }
    });
}

/// Removes the first plain Enter press (no modifiers) from this frame's events, so the text field
/// never also inserts a line for it. Returns whether that press was a key repeat.
fn take_plain_enter(events: &mut Vec<Event>) -> Option<bool> {
    let index = events.iter().position(|e| {
        matches!(e, Event::Key { key: Key::Enter, pressed: true, modifiers, .. }
            if !(modifiers.shift || modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd))
    })?;
    match events.remove(index) {
        Event::Key { repeat, .. } => Some(repeat),
        _ => None,
    }
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
            if let Some(label) = proposal
                .context
                .get("targets")
                .and_then(Value::as_array)
                .and_then(|t| t.get(suggestion.index))
                .and_then(|t| t.get("label"))
                .and_then(Value::as_str)
            {
                ui.label(RichText::new(label).small().color(t.text_dim));
            }
            ui.label(RichText::new("Original").small().color(t.text_dim));
            ui.label(original);
            ui.label(RichText::new("Proposal").small().color(t.text_dim));
            let text = if suggestion.text.is_empty() { "(empty: the text is removed)" } else { suggestion.text.as_str() };
            ui.label(RichText::new(text).color(accent_text(t)));
        }
        if let Some(deck) = &proposal.response.deck {
            let labels = target_labels(&proposal.context);
            let shapes = shape_labels(proposal.geometry.as_ref());
            for op in &deck.operations {
                ui.separator();
                ui.label(RichText::new(deck_op_line(op, &labels, &shapes)).small().color(t.text_dim));
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

/// The label of each geometry entry, for naming shape operations. Its indices are not the text
/// targets' indices.
fn shape_labels(geometry: Option<&Value>) -> Vec<String> {
    geometry
        .and_then(|g| g.get("shapes"))
        .and_then(Value::as_array)
        .map(|shapes| shapes.iter().map(|s| s.get("label").and_then(Value::as_str).unwrap_or("shape").to_owned()).collect())
        .unwrap_or_default()
}

/// One line describing a deck operation for the review card. `labels` names text targets by
/// `target`; `shapes` names geometry entries by `shape`.
fn deck_op_line(op: &deckcraft_assistant::DeckOperation, labels: &[String], shapes: &[String]) -> String {
    let label = op.target.and_then(|i| labels.get(i)).map_or_else(|| "a shape".to_owned(), |l| format!("\u{201C}{l}\u{201D}"));
    let shape = op.shape.and_then(|i| shapes.get(i)).map_or_else(|| "a shape".to_owned(), |l| format!("\u{201C}{l}\u{201D}"));
    let rect = op.rect.map_or_else(String::new, |[x, y, w, h]| format!(" at {x}, {y} · {w} × {h} pt"));
    let style = op.style.as_ref().map_or_else(String::new, style_line);
    let on = op.slide.map_or_else(String::new, |n| format!(" on slide {n}"));
    let line = match op.kind.as_str() {
        "new_slide" => match op.layout.as_deref() {
            Some("title") => "New Title slide".to_owned(),
            Some("titleAndContent") => "New Title and Content slide".to_owned(),
            _ => "New Blank slide".to_owned(),
        },
        "add_text" => format!("Text box{rect}{style}"),
        "set_text" => format!("Replace the text of {label}"),
        "set_style" => format!("Style {label}{style}"),
        "set_bounds" => format!("Move and resize {label}{rect}"),
        "set_shape_bounds" => format!("Move and resize {shape}{rect}"),
        "set_shape_fill" => format!("Fill {shape} with {}", op.color.as_deref().unwrap_or_default()),
        "add_shape" => {
            let color = op.color.as_deref().map_or_else(String::new, |c| format!(" in {c}"));
            format!("New {} shape{rect}{color}", op.preset.as_deref().unwrap_or_default())
        }
        "background" => format!("Solid background colour {}", op.color.as_deref().unwrap_or_default()),
        other => other.to_owned(),
    };
    format!("{line}{on}")
}

/// The style fields an add_text operation carries, joined for the review card.
fn style_line(style: &deckcraft_assistant::TextStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    if style.bold == Some(true) {
        parts.push("bold".into());
    }
    if style.italic == Some(true) {
        parts.push("italic".into());
    }
    if let Some(size) = style.size {
        parts.push(format!("{size} pt"));
    }
    if let Some(color) = &style.color {
        parts.push(color.clone());
    }
    if parts.is_empty() { String::new() } else { format!(" · {}", parts.join(", ")) }
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
        geometry: None,
        instruction: "Shorten".into(),
    });
    (cancel, sender)
}

#[cfg(test)]
mod slide_consent_tests {
    use super::*;

    #[test]
    fn document_edit_invalidates_notice_without_selection_change() {
        let mut app = SlideApp::new(deckcraft_engine::Session::with_new(), crate::Services::default());
        let before = armed_now(&app).unwrap();
        app.session.execute("design.background", &json!({"color":"#123456"})).unwrap();
        let after = armed_now(&app).unwrap();
        assert!(before != after);
    }

    #[test]
    fn changing_selection_clears_consent_on_same_slide() {
        let mut session = deckcraft_engine::Session::with_new();
        deckcraft_engine::sample::open_sample(&mut session).unwrap();
        let mut app = SlideApp::new(session, crate::Services::default());
        let ctx = egui::Context::default();
        logic(&mut app, &ctx);
        app.assistant.armed = armed_now(&app);
        app.session.active_mut().unwrap().selection.shapes.push(deckcraft_model::ShapeId(999));
        logic(&mut app, &ctx);
        assert!(app.assistant.armed.is_none());
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
        app.assistant.armed = armed_now(&app);
        let original_slide = app.session.active().unwrap().selection.slide;
        app.session.active_mut().unwrap().selection.slide = original_slide.saturating_add(1);
        logic(&mut app, &ctx);
        assert!(app.assistant.armed.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.assistant.pending.is_none());
        assert_eq!(app.assistant.history.len(), 1);
        assert!(status(&app)["proposals"].as_array().unwrap().is_empty());
        assert!(app.assistant.message.contains("cancelled"));
    }

    /// A blank slide with one plain rectangle, and the rectangle's id.
    fn app_with_rect() -> (SlideApp, u32) {
        let mut session = deckcraft_engine::Session::with_new();
        let id = session.execute("shape.insert", &json!({"preset": "rect", "rect": [10, 20, 100, 50]})).unwrap()["id"].as_u64().unwrap();
        (SlideApp::new(session, crate::Services::default()), u32::try_from(id).unwrap())
    }

    /// A proposal with one fill of geometry entry 0, made from the slide's current snapshots.
    fn fill_proposal(app: &mut SlideApp) -> u64 {
        let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
        let geometry = app.session.execute("assistant.geometryContext", &json!({})).unwrap();
        let response: Response = serde_json::from_value(json!({
            "suggestions": [],
            "explanation": "Fill it.",
            "deck": {"operations": [{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}]}
        }))
        .unwrap();
        let id = preview_proposal(app, "Fill it", context, response).unwrap();
        app.assistant.history.front_mut().unwrap().geometry = Some(geometry);
        id
    }

    #[test]
    fn add_shape_presets_must_be_gallery_shapes_and_only_lines_may_be_flat() {
        let answer = |preset: &str, height: f64| -> Response {
            serde_json::from_value(json!({
                "suggestions": [],
                "explanation": "Add it.",
                "deck": {"operations": [{"kind": "add_shape", "preset": preset, "rect": [10.0, 20.0, 100.0, height]}]}
            }))
            .unwrap()
        };
        assert_eq!(unusable_shape(&answer("star5", 50.0)), None);
        assert_eq!(unusable_shape(&answer("line", 0.0)), None);
        assert_eq!(unusable_shape(&answer("arc", 50.0)), None);
        assert!(unusable_shape(&answer("hexagon2", 50.0)).is_some());
        assert!(unusable_shape(&answer("rect", 0.0)).is_some());
    }

    fn filled(app: &SlideApp, shape: u32) -> bool {
        app.session.active().unwrap().shape(deckcraft_model::ShapeId(shape)).unwrap().fill.is_some()
    }

    #[test]
    fn shape_proposal_fills_the_shape_it_was_made_for_and_undo_restores_it() {
        let (mut app, rect) = app_with_rect();
        let id = fill_proposal(&mut app);
        apply(&mut app, id);
        assert_eq!(app.assistant.message, "");
        assert!(filled(&app, rect));
        assert!(app.assistant.history.is_empty(), "an applied proposal leaves the history");
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert!(!filled(&app, rect));
    }

    #[test]
    fn a_shape_moved_after_the_proposal_is_refused_and_kept() {
        let (mut app, rect) = app_with_rect();
        let id = fill_proposal(&mut app);
        app.session.execute("shape.setBounds", &json!({"ids": [rect], "x": 90, "y": 90, "w": 40, "h": 40})).unwrap();
        apply(&mut app, id);
        assert!(app.assistant.message.starts_with("Could not apply the proposal"), "{}", app.assistant.message);
        assert!(!filled(&app, rect));
        assert_eq!(app.assistant.history.len(), 1, "a refused proposal stays for review");
    }

    #[test]
    fn geometry_entries_keep_the_snapshot_order_as_their_indices() {
        let snapshot = json!({"shapes": [
            {"index": 0, "shape": 7, "label": "Rectangle 2", "preset": "rect", "rect": [1, 2, 3, 4], "fill": null},
            {"index": 1, "shape": 9, "label": "Oval 3", "preset": "ellipse", "rect": [0, 0, 5, 5], "fill": "#102A43"}
        ]});
        let shapes = geometry_of(&snapshot).unwrap();
        assert_eq!(shapes[1].index, 1);
        assert_eq!(shapes[1].label, "Oval 3");
        assert_eq!(shapes[1].fill.as_deref(), Some("#102A43"));
        assert_eq!(shapes[0].rect, [1.0, 2.0, 3.0, 4.0]);
        assert!(geometry_of(&json!({"shapes": [{"rect": [1, 2, 3]}]})).is_err());
    }

    #[test]
    fn shape_operations_are_named_by_geometry_not_by_text_targets() {
        use deckcraft_assistant::DeckOperation;
        let labels = vec!["Title text".to_owned()];
        let shapes = vec!["Rectangle 2".to_owned(), "Oval 3".to_owned()];
        let fill: DeckOperation = serde_json::from_value(json!({"kind": "set_shape_fill", "shape": 1, "color": "#102A43"})).unwrap();
        assert_eq!(deck_op_line(&fill, &labels, &shapes), "Fill \u{201C}Oval 3\u{201D} with #102A43");
        let add: DeckOperation =
            serde_json::from_value(json!({"kind": "add_shape", "preset": "roundRect", "rect": [1, 2, 3, 4], "color": "#FF8800"})).unwrap();
        assert_eq!(deck_op_line(&add, &labels, &shapes), "New roundRect shape at 1, 2 · 3 × 4 pt in #FF8800");
        let text: DeckOperation = serde_json::from_value(json!({"kind": "set_text", "target": 0, "text": "x"})).unwrap();
        assert_eq!(deck_op_line(&text, &labels, &shapes), "Replace the text of \u{201C}Title text\u{201D}");
    }
}
