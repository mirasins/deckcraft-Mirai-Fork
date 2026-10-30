use deckcraft_engine::Session;
use serde_json::json;

fn two_slides() -> Session {
    let mut s = Session::with_new();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40], "text":"First"})).unwrap();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40], "text":"Second"})).unwrap();
    s
}

#[test]
fn presentation_edits_multiple_slides_with_one_undo() {
    let mut s = two_slides();
    let before = s.active().unwrap().selection.slide;
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let targets = context["targets"].as_array().unwrap();
    let first = targets.iter().position(|t| t["original"] == "First").unwrap();
    let second = targets.iter().position(|t| t["original"] == "Second").unwrap();
    assert_ne!(targets[first]["slide_id"], targets[second]["slide_id"]);
    let changes = json!([{"index":first,"text":"One"},{"index":second,"text":"Two"}]);
    s.execute("assistant.applyPresentation", &json!({"context":context,"changes":changes})).unwrap();
    assert_eq!(s.active().unwrap().selection.slide, before);
    let next = s.execute("assistant.presentationContext", &json!({})).unwrap();
    assert_eq!(next["targets"][first]["original"], "One");
    assert_eq!(next["targets"][second]["original"], "Two");
    s.execute("edit.undo", &json!({})).unwrap();
    let undone = s.execute("assistant.presentationContext", &json!({})).unwrap();
    assert_eq!(undone["targets"][first]["original"], "First");
    assert_eq!(undone["targets"][second]["original"], "Second");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.execute("assistant.presentationContext", &json!({})).unwrap()["targets"][second]["original"], "Two");
}

#[test]
fn presentation_rejects_forgery_and_stale_context() {
    let mut s = two_slides();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let mut forged = context.clone();
    forged["targets"][0]["slide_id"] = json!(999999);
    assert!(s.execute("assistant.applyPresentation", &json!({"context":forged,"changes":[{"index":0,"text":"bad"}]})).is_err());
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context,"changes":[{"index":0,"text":"bad"}]})).is_err());
}
