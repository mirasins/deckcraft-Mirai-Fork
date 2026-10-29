use deckcraft_engine::Session;
use serde_json::json;

#[test]
fn creates_and_fills_slide_as_one_undo_step() {
    let mut s = Session::with_new();
    let before = s.active().unwrap().doc.slides.len();
    let context = s.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank", "text":null, "target":null, "rect":null, "color":null},
        {"kind":"background", "layout":null, "text":null, "target":null, "rect":null, "color":"#123456"},
        {"kind":"add_text", "layout":null, "text":"Hello", "target":null, "rect":[30,30,200,60], "color":null},
        {"kind":"add_text", "layout":null, "text":"Details", "target":null, "rect":[30,120,200,60], "color":null}
    ]});
    s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.slides.len(), before + 1);
    let slide = d.doc.slides.get(d.selection.slide).unwrap();
    let texts: Vec<_> = slide.shapes.iter().filter_map(|shape| shape.text.as_ref().map(|t| t.text())).collect();
    assert!(texts.contains(&"Hello".to_string()));
    assert!(texts.contains(&"Details".to_string()));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
}

#[test]
fn rejects_invalid_plan_and_stale_context() {
    let mut s = Session::with_new();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let deck = json!({"operations":[{"kind":"new_slide", "layout":"blank"}, {"kind":"add_text", "rect":[0,0,-1,20], "text":"invalid"}]});
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).is_err());
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": {"operations":[{"kind":"new_slide", "layout":"blank"}]}})).is_err());
}

#[test]
fn rejects_apply_after_switching_to_master_view() {
    let mut s = Session::with_new();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    s.active_mut().unwrap().selection.target = deckcraft_engine::Target::NotesMaster;
    let deck = json!({"operations":[{"kind":"new_slide", "layout":"blank"}]});
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).is_err());
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
}

#[test]
fn modifies_existing_text_and_geometry_and_undoes_both() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect":[10,20,100,50],"text":"Old"})).unwrap()["id"].as_u64().unwrap();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let target = context["targets"].as_array().unwrap().iter().position(|t| t["shape"] == id).unwrap();
    let deck = json!({"operations":[
        {"kind":"set_text", "target":target, "text":"New"},
        {"kind":"set_bounds", "target":target, "rect":[50,60,200,80]}
    ]});
    s.execute("assistant.applyDeck", &json!({"context":context, "deck":deck})).unwrap();
    let shape = s.active().unwrap().shape(deckcraft_model::ShapeId(id as u32)).unwrap();
    assert_eq!(shape.text.as_ref().unwrap().text(), "New");
    assert_eq!(shape.xfrm.as_ref().unwrap().x, 50.0);
    s.execute("edit.undo", &json!({})).unwrap();
    let shape = s.active().unwrap().shape(deckcraft_model::ShapeId(id as u32)).unwrap();
    assert_eq!(shape.text.as_ref().unwrap().text(), "Old");
    assert_eq!(shape.xfrm.as_ref().unwrap().x, 10.0);
}

#[test]
fn creates_several_slides_with_distributed_content_as_one_undo_step() {
    let mut s = Session::with_new();
    let before = s.active().unwrap().doc.slides.len();
    let context = s.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank", "text":"Agenda"},
        {"kind":"add_text", "text":"First item", "rect":[40, 120, 500, 60]},
        {"kind":"new_slide", "layout":"titleAndContent", "text":"Details"},
        {"kind":"add_text", "text":"Deep dive", "rect":[40, 150, 500, 60]}
    ]});
    s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.slides.len(), before + 2);
    let texts = |s: &Session, index: usize| -> Vec<String> {
        s.active().unwrap().doc.slides.get(index).unwrap().shapes.iter().filter_map(|sh| sh.text.as_ref().map(|t| t.text())).collect()
    };
    assert!(texts(&s, before).contains(&"Agenda".to_string()));
    assert!(texts(&s, before).contains(&"First item".to_string()));
    assert!(texts(&s, before + 1).contains(&"Details".to_string()));
    assert!(texts(&s, before + 1).contains(&"Deep dive".to_string()));
    // One undo step reverts the whole proposal; redo brings both slides back.
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before + 2);
    assert!(texts(&s, before + 1).contains(&"Deep dive".to_string()));
}

#[test]
fn works_from_an_empty_slide() {
    let mut s = Session::with_new();
    // The default presentation opens on a slide whose placeholders are empty: nothing is captured.
    let context = s.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    assert!(context["targets"].as_array().unwrap().is_empty());
    let before = s.active().unwrap().doc.slides.len();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank"},
        {"kind":"add_text", "text":"Hello", "rect":[30, 30, 200, 60]}
    ]});
    s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.slides.len(), before + 1);
    assert!(d.doc.slides[d.selection.slide].shapes.iter().any(|sh| sh.text.as_ref().is_some_and(|t| t.text() == "Hello")));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
}
