use deckcraft_engine::Session;
use serde_json::json;

fn two_slides() -> Session {
    let mut s = Session::with_new();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40],"text":"First"})).unwrap();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40],"text":"Second"})).unwrap();
    s.execute("slide.go", &json!({"index":0})).unwrap();
    s
}

#[test]
fn text_apply_from_another_slide_is_stale_and_keeps_selection() {
    let mut s = two_slides();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    s.execute("slide.go", &json!({"index":1})).unwrap();
    let before_slide = s.active().unwrap().selection.slide;
    let changes = json!([{"index":0,"text":"Changed"}]);
    assert!(s.execute("assistant.apply", &json!({"context":context,"changes":changes})).is_err());
    assert_eq!(s.active().unwrap().selection.slide, before_slide);
}

#[test]
fn deck_apply_from_another_slide_is_stale_and_keeps_selection() {
    let mut s = two_slides();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    s.execute("slide.go", &json!({"index":1})).unwrap();
    let before_slide = s.active().unwrap().selection.slide;
    let before_slides = s.active().unwrap().doc.slides.len();
    let deck = json!({"operations":[{"kind":"new_slide","layout":"blank","text":null,"target":null,"rect":null,"color":null}]});
    assert!(s.execute("assistant.applyDeck", &json!({"context":context,"deck":deck})).is_err());
    assert_eq!(s.active().unwrap().selection.slide, before_slide);
    assert_eq!(s.active().unwrap().doc.slides.len(), before_slides);
}

#[test]
fn presentation_apply_from_another_slide_is_stale_and_keeps_selection() {
    let mut s = two_slides();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    s.execute("slide.go", &json!({"index":1})).unwrap();
    let before_slide = s.active().unwrap().selection.slide;
    let targets = context["targets"].as_array().unwrap();
    let idx = targets.iter().position(|t| t["original"] == "First").unwrap();
    let changes = json!([{"index":idx,"text":"One"}]);
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context,"changes":changes})).is_err());
    assert_eq!(s.active().unwrap().selection.slide, before_slide);
}
