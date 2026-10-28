//! Renders the Mirai Assistant pane over the sample deck, headless: no window, no Claude Code. The
//! proposal is synthetic (a preview, not a request), so nothing leaves the machine.
//!
//! ```sh
//! cargo run -p deckcraft-ui-egui --example assistant_shot -- /tmp/opencode/assistant.png
//! ```

use deckcraft_assistant::{Response, Suggestion};
use deckcraft_engine::Session;
use deckcraft_ui_egui::{Services, SlideApp, assistant};
use egui::vec2;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "assistant_pane.png".to_owned());
    let mut session = Session::with_new();
    deckcraft_engine::sample::open_sample(&mut session).map_err(|e| e.to_string())?;
    let mut app = SlideApp::new(session, Services::default());
    app.run("ui.assistant", json!({"value": true}))?;
    app.run("ui.assistant.newConversation", json!({}))?;

    let context = app.session.execute("assistant.context", &json!({"scope": "slide"})).map_err(|e| e.to_string())?;
    let response = Response {
        suggestions: vec![Suggestion { index: 0, text: "The Northern Lights".into(), markdown: false }],
        explanation: "Shortened to the key point.".into(),
    };
    assistant::preview_proposal(&mut app, "Shorten this text.", context, response)?;

    let mut harness = egui_kittest::Harness::builder().with_size(vec2(1100.0, 720.0)).with_pixels_per_point(1.5).wgpu().build_ui_state(
        |ui, app: &mut SlideApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    harness.run_steps(3);
    let image = harness.render()?;
    if let Some(dir) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(dir)?;
    }
    image.save(&out)?;
    println!("wrote {out}");
    Ok(())
}
