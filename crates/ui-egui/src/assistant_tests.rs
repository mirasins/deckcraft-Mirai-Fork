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
    Response { suggestions: vec![Suggestion { index, text: text.into(), markdown: false }], explanation: "Shorter.".into(), deck: None }
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
fn slide_proposal_is_applied_in_app_and_undoable() {
    let mut app = app();
    let before = app.session.active().unwrap().doc.slides.len();
    let context = app.session.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let response: Response = serde_json::from_value(json!({"suggestions":[], "explanation":"New slide", "deck":{"operations":[
        {"kind":"new_slide", "target":null, "layout":"blank", "text":null, "rect":null, "color":null},
        {"kind":"add_text", "target":null, "layout":null, "text":"Hello", "rect":[30,30,200,60], "color":null}
    ]}}))
    .unwrap();
    let id = assistant::preview_proposal(&mut app, "Create a slide", context, response).unwrap();
    assistant::apply(&mut app, id);
    let doc = app.session.active().unwrap();
    assert_eq!(doc.doc.slides.len(), before + 1);
    assert!(doc.doc.slides[doc.selection.slide].shapes.iter().any(|s| s.text.as_ref().is_some_and(|t| t.text() == "Hello")));
    app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.active().unwrap().doc.slides.len(), before);
}

#[test]
fn presentation_proposal_applies_changes_across_slides() {
    let mut session = Session::with_new();
    session.execute("insert.textBox", &json!({"rect":[10,10,200,40],"text":"First"})).unwrap();
    session.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    session.execute("insert.textBox", &json!({"rect":[10,10,200,40],"text":"Second"})).unwrap();
    let mut app = SlideApp::new(session, Services::default());
    let context = app.session.execute("assistant.presentationContext", &json!({})).unwrap();
    let targets = context["targets"].as_array().unwrap();
    let first = targets.iter().position(|t| t["original"] == "First").unwrap();
    let second = targets.iter().position(|t| t["original"] == "Second").unwrap();
    let answer = Response {
        suggestions: vec![
            Suggestion { index: first, text: "One".into(), markdown: false },
            Suggestion { index: second, text: "Two".into(), markdown: false },
        ],
        explanation: String::new(),
        deck: None,
    };
    let id = assistant::preview_proposal(&mut app, "Rewrite both", context, answer).unwrap();
    assistant::apply(&mut app, id);
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    assert_eq!(status["message"], "", "{status}");
    let updated = app.session.execute("assistant.presentationContext", &json!({})).unwrap();
    assert_eq!(updated["targets"][first]["original"], "One");
    assert_eq!(updated["targets"][second]["original"], "Two");
    app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.execute("assistant.presentationContext", &json!({})).unwrap()["targets"][first]["original"], "First");
}

#[test]
fn a_deck_proposal_card_lists_readable_operations() {
    use egui_kittest::kittest::Queryable;
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let response: Response = serde_json::from_value(json!({
        "suggestions": [], "explanation": "A new slide carries the findings.",
        "deck": {"operations": [
            {"kind":"new_slide", "target":null, "layout":"titleAndContent", "text":"Findings", "rect":null, "color":null},
            {"kind":"add_text", "target":null, "layout":null, "text":"The aurora peaks near midnight.", "rect":[40, 150, 620, 80], "color":null,
             "style":{"bold":true, "italic":null, "size":24, "color":"#102A43"}},
            {"kind":"background", "target":null, "layout":null, "text":null, "rect":null, "color":"#102A43"}
        ]}
    }))
    .unwrap();
    assistant::preview_proposal(&mut app, "Add a findings slide.", context, response).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    for line in [
        "New Title and Content slide",
        "Text box at 40, 150 · 620 × 80 pt · bold, 24 pt, #102A43",
        "Solid background colour #102A43",
        "The aurora peaks near midnight.",
    ] {
        h.get_by_label(line);
    }
}

/// Harness over the pane, with the instruction field focused and a draft written.
fn composing_harness(mut app: SlideApp) -> egui_kittest::Harness<'static, SlideApp> {
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.state_mut().assistant.instruction = "Shorten".into();
    h.run_steps(2);
    h.ctx.memory_mut(|m| m.request_focus(h.state().assistant.input_id.unwrap()));
    h.run_steps(2);
    h
}

fn enter(h: &mut egui_kittest::Harness<'static, SlideApp>) {
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
}

#[test]
fn the_first_enter_reviews_the_request_and_adds_no_line() {
    let mut h = composing_harness(app());
    enter(&mut h);
    assert!(h.state().assistant.armed.is_some());
    assert_eq!(h.state().assistant.instruction, "Shorten");
    assert!(!busy(h.state_mut()));
}

#[test]
fn shift_enter_adds_a_line_and_does_not_review() {
    let mut h = composing_harness(app());
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    h.run_steps(2);
    assert!(h.state().assistant.armed.is_none());
    assert!(h.state().assistant.instruction.contains('\n'));
}

#[test]
fn enter_during_ime_composition_is_left_to_the_field() {
    let mut h = composing_harness(app());
    h.event(egui::Event::Ime(egui::ImeEvent::Preedit { text: "ni".into(), active_range_chars: None }));
    h.run_steps(1);
    enter(&mut h);
    assert!(h.state().assistant.armed.is_none());
    assert!(h.state().assistant.composing);
}

#[test]
fn enter_right_after_an_ime_commit_is_ignored() {
    let mut h = composing_harness(app());
    h.event(egui::Event::Ime(egui::ImeEvent::Commit("x".into())));
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(h.state().assistant.armed.is_none());
}

#[test]
fn enter_outside_the_instruction_field_does_nothing() {
    let mut app = app();
    app.run("ui.assistant", json!({"value": true})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.state_mut().assistant.instruction = "Shorten".into();
    h.run_steps(2);
    enter(&mut h);
    assert!(h.state().assistant.armed.is_none());
}

#[test]
fn changing_the_request_after_review_cancels_the_confirmation() {
    let mut h = composing_harness(app());
    enter(&mut h);
    assert!(h.state().assistant.armed.is_some());
    h.state_mut().assistant.instruction.push_str(" more");
    h.run_steps(2);
    assert!(h.state().assistant.armed.is_none());
    assert!(h.state().assistant.message.contains("cancelled"));
}

#[test]
fn changing_the_model_after_review_cancels_the_confirmation() {
    let mut h = composing_harness(app());
    enter(&mut h);
    h.state_mut().assistant.model = "sonnet".into();
    h.run_steps(2);
    assert!(h.state().assistant.armed.is_none());
}

#[test]
fn the_send_button_reviews_and_cancel_withdraws_the_review() {
    use egui_kittest::kittest::Queryable;
    let mut h = composing_harness(app());
    h.get_by_label("Send").click();
    h.run_steps(3);
    assert!(h.state().assistant.armed.is_some());
    h.get_by_label("Confirm and send");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().assistant.armed.is_none());
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

#[test]
fn the_brand_logo_is_rasterised_once_per_context_and_is_not_retried() {
    let ctx = egui::Context::default();
    let first = assistant::brand(&ctx).expect("the bundled SVG loads");
    let second = assistant::brand(&ctx).expect("the cached logo is returned");
    assert_eq!(first.id(), second.id());
    let other = egui::Context::default();
    assert!(assistant::brand(&other).is_some());
}

#[test]
fn a_corrupt_logo_falls_back_to_no_texture_instead_of_panicking() {
    let ctx = egui::Context::default();
    assert!(assistant::load_svg_texture(&ctx, b"<svg").is_none());
    assert!(assistant::load_svg_texture(&ctx, &[0xff, 0x00, 0x7f]).is_none());
    assert!(assistant::load_svg_texture(&ctx, b"").is_none());
}

#[test]
fn the_assistant_pane_shows_the_logo_with_an_accessible_name() {
    use egui_kittest::kittest::Queryable;
    let mut h = composing_harness(app());
    h.run_steps(2);
    h.get_by_label("Mirai logo");
}

#[test]
fn a_mixed_text_and_slide_proposal_applies_in_one_undo_step() {
    let mut app = app();
    let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let slide = app.session.active().unwrap().selection.slide;
    let before = app.session.active().unwrap().doc.slides.len();
    let original = context["targets"][0]["original"].as_str().unwrap().to_owned();
    let shape = context["targets"][0]["shape"].as_u64().unwrap() as u32;
    let response: Response = serde_json::from_value(json!({
        "suggestions": [{"index": 0, "text": "Rewritten", "markdown": false}],
        "explanation": "Mixed",
        "deck": {"operations": [{"kind": "new_slide", "target": null, "layout": "blank", "text": null, "rect": null, "color": null}]}
    }))
    .unwrap();
    let id = assistant::preview_proposal(&mut app, "Mixed", context, response).unwrap();
    assistant::apply(&mut app, id);
    let text = |app: &SlideApp| {
        let doc = app.session.active().unwrap();
        doc.doc.slides[slide].shapes.iter().find(|sh| sh.id.0 == shape).unwrap().text.as_ref().unwrap().text()
    };
    assert_eq!(app.assistant.message, "");
    assert_eq!(app.session.active().unwrap().doc.slides.len(), before + 1);
    assert_eq!(text(&app), "Rewritten");
    app.session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.active().unwrap().doc.slides.len(), before);
    assert_eq!(text(&app), original);
}

#[test]
fn moving_the_selection_keeps_a_proposal_applicable() {
    let mut app = app();
    let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let shape = context["targets"][0]["shape"].as_u64().unwrap();
    let id = assistant::preview_proposal(&mut app, "Shorten", context, response(0, "Short")).unwrap();
    app.session.execute("edit.select", &json!({"ids": [shape]})).unwrap();
    assistant::apply(&mut app, id);
    assert_eq!(app.assistant.message, "");
    let status = app.run("ui.assistant.status", json!({})).unwrap();
    assert!(status["proposals"].as_array().is_some_and(Vec::is_empty), "{status}");
}
