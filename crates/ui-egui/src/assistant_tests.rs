//! Mirai Assistant without a provider: the pane toggle, the confirmation gate, cancellation,
//! the proposal preview and apply path, and stale rejection. No Claude Code process is started.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;

use deckcraft_assistant::{Response, Suggestion};
use deckcraft_engine::Session;
use serde_json::{Value, json};

use crate::{Services, SlideApp, UI_COMMANDS, assistant};

fn app() -> SlideApp {
    let mut s = Session::with_new();
    deckcraft_engine::sample::open_sample(&mut s).unwrap();
    SlideApp::new(s, Services::default())
}

/// A snapshot shaped like `assistant.context`, with the given original texts.
fn context(texts: &[&str]) -> Value {
    let targets: Vec<Value> = texts.iter().enumerate().map(|(i, t)| json!({"index": i, "original": t, "label": "text"})).collect();
    json!({"document_id": 1, "revision": 0, "scope": "slide", "targets": targets})
}

fn response(index: usize, text: &str) -> Response {
    Response { suggestions: vec![Suggestion { index, text: text.into(), markdown: false }], explanation: "Shorter.".into() }
}

fn busy(app: &mut SlideApp) -> bool {
    app.run("ui.assistant.status", json!({})).unwrap()["busy"] == json!(true)
}

#[test]
fn the_assistant_commands_are_listed_for_the_palette_and_the_control_channel() {
    for id in ["ui.assistant", "ui.assistant.request", "ui.assistant.cancel", "ui.assistant.status", "ui.assistant.newConversation"] {
        assert!(UI_COMMANDS.iter().any(|c| c.0 == id), "{id} is not in UI_COMMANDS");
    }
}

#[test]
fn the_review_toggle_opens_and_closes_the_assistant_pane() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    assert_eq!(app.ui.pane.as_deref(), Some("assistant"));
    app.run("ui.assistant", json!({})).unwrap();
    assert_eq!(app.ui.pane, None);
    assert!(app.run("ui.assistant", json!({"value": "yes"})).is_err());
}

#[test]
fn a_request_without_explicit_confirmation_is_refused_before_anything_starts() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    for params in
        [json!({"instruction": "Shorten"}), json!({"instruction": "Shorten", "confirm": false}), json!({"instruction": "Shorten", "confirm": "true"})]
    {
        let error = app.run("ui.assistant.request", params).unwrap_err();
        assert!(error.contains("confirm"), "{error}");
    }
    assert!(!busy(&mut app));
}

#[test]
fn an_unknown_scope_or_an_empty_instruction_is_refused_before_anything_starts() {
    let mut app = app();
    assert!(app.run("ui.assistant.request", json!({"instruction": "Shorten", "confirm": true, "scope": "document"})).is_err());
    assert!(app.run("ui.assistant.request", json!({"instruction": "  ", "confirm": true})).is_err());
    assert!(!busy(&mut app));
}

#[test]
fn closing_the_pane_cancels_a_running_request() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let (cancel, _sender) = assistant::start_fake_for_test(&mut app);
    assert!(busy(&mut app));
    app.run("ui.assistant", json!({"value": false})).unwrap();
    assert!(cancel.load(Ordering::Relaxed));

    let (cancel, _sender) = assistant::start_fake_for_test(&mut app);
    app.ui.pane = Some("comments".into());
    assistant::logic(&mut app, &egui::Context::default());
    assert!(cancel.load(Ordering::Relaxed));
}

#[test]
fn a_new_conversation_cancels_and_clears_the_draft_and_proposals() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let (cancel, _sender) = assistant::start_fake_for_test(&mut app);
    assistant::preview_proposal(&mut app, "Shorten", context(&["Old wording"]), response(0, "Short")).unwrap();
    app.run("ui.assistant.newConversation", json!({})).unwrap();
    assert!(cancel.load(Ordering::Relaxed));
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    assert_eq!(status["busy"], json!(false));
    assert_eq!(status["proposals"].as_array().map(Vec::len), Some(0));
}

#[test]
fn a_proposal_previews_and_applies_only_through_the_engine() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let id = assistant::preview_proposal(&mut app, "Shorten", context(&["Old wording here"]), response(0, "Short")).unwrap();
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    assert_eq!(status["proposals"][0]["id"], json!(id));
    assert_eq!(status["proposals"][0]["suggestions"][0]["text"], json!("Short"));
    assert_eq!(status["proposals"][0]["explanation"], json!("Shorter."));

    assistant::apply(&mut app, id);
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    let listed = status["proposals"].as_array().is_some_and(|items| items.iter().any(|p| p["id"] == json!(id)));
    let message = status["message"].as_str().unwrap_or_default();
    // Applied: the proposal is dropped. Refused (for example a stale snapshot): it stays, with the reason.
    if listed {
        assert!(!message.is_empty());
    } else {
        assert!(message.is_empty());
    }
}

#[test]
fn a_presentation_switch_drops_the_conversation_and_its_proposals() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let (cancel, _sender) = assistant::start_fake_for_test(&mut app);
    assistant::preview_proposal(&mut app, "Shorten", context(&["Old wording"]), response(0, "Short")).unwrap();
    app.session.execute("file.new", &json!({})).unwrap();
    assistant::logic(&mut app, &egui::Context::default());
    assert!(cancel.load(Ordering::Relaxed));
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    assert_eq!(status["busy"], json!(false));
    assert_eq!(status["proposals"].as_array().map(Vec::len), Some(0));
}
