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

#[test]
fn mixed_text_and_slide_changes_apply_as_one_undo_step() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect":[10,20,100,50],"text":"Old"})).unwrap()["id"].as_u64().unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let target = context["targets"].as_array().unwrap().iter().position(|t| t["shape"] == id).unwrap();
    let changes = json!([{"index": target, "text": "Rewritten"}]);
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank", "text":"Next", "target":null, "rect":null, "color":null},
        {"kind":"add_text", "layout":null, "text":"Detail", "target":null, "rect":[30,30,200,60], "color":null}
    ]});
    let out = s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck, "changes": changes})).unwrap();
    assert_eq!(out["text_changes"], 1);
    assert_eq!(s.active().unwrap().doc.slides.len(), before + 1);
    let shape = s.active().unwrap().doc.slides[1].shapes.iter().find(|sh| sh.id.0 == id as u32).unwrap().text.as_ref().unwrap().text();
    assert_eq!(shape, "Rewritten");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
    let shape = s.active().unwrap().doc.slides[1].shapes.iter().find(|sh| sh.id.0 == id as u32).unwrap().text.as_ref().unwrap().text();
    assert_eq!(shape, "Old");
}

#[test]
fn a_target_with_both_a_suggestion_and_set_text_changes_nothing() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect":[10,20,100,50],"text":"Old"})).unwrap()["id"].as_u64().unwrap();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let target = context["targets"].as_array().unwrap().iter().position(|t| t["shape"] == id).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank", "text":null, "target":null, "rect":null, "color":null},
        {"kind":"set_text", "layout":null, "text":"Conflict", "target":target, "rect":null, "color":null}
    ]});
    let changes = json!([{"index": target, "text": "Rewritten"}]);
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck, "changes": changes})).is_err());
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
    let shape = s.active().unwrap().doc.slides[1].shapes.iter().find(|sh| sh.id.0 == id as u32).unwrap().text.as_ref().unwrap().text();
    assert_eq!(shape, "Old");
}

#[test]
fn a_failed_text_change_in_a_mixed_apply_leaves_the_slides_alone() {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    let id = s.execute("insert.textBox", &json!({"rect":[10,20,100,50],"text":"Old"})).unwrap()["id"].as_u64().unwrap();
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    let target = context["targets"].as_array().unwrap().iter().position(|t| t["shape"] == id).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank", "text":null, "target":null, "rect":null, "color":null},
        {"kind":"add_text", "layout":null, "text":"Bad", "target":null, "rect":[0,0,-1,20], "color":null}
    ]});
    let changes = json!([{"index": target, "text": "Rewritten"}]);
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck, "changes": changes})).is_err());
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
    let shape = s.active().unwrap().doc.slides[1].shapes.iter().find(|sh| sh.id.0 == id as u32).unwrap().text.as_ref().unwrap().text();
    assert_eq!(shape, "Old");
}

#[test]
fn styled_text_box_applies_format_and_undoes_as_one_step() {
    let mut s = Session::with_new();
    let before = s.active().unwrap().doc.slides.len();
    let context = s.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let deck = json!({"operations": [
        {"kind":"new_slide", "layout":"blank"},
        {"kind":"add_text", "text":"Headline", "rect":[40, 40, 600, 80],
         "style":{"bold":true, "italic":false, "size":24, "color":"#102A43"}}
    ]});
    s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).unwrap();
    let d = s.active().unwrap();
    let slide = d.doc.slides.get(d.selection.slide).unwrap();
    let shape = slide.shapes.iter().find(|sh| sh.text.as_ref().is_some_and(|t| t.text() == "Headline")).unwrap();
    let run = &shape.text.as_ref().unwrap().paragraphs[0].runs[0];
    assert_eq!(run.props.bold, Some(true));
    assert_eq!(run.props.size, Some(24.0));
    assert!(matches!(run.props.fill, Some(deckcraft_model::Fill::Solid { ref color })
        if matches!(color.base, deckcraft_model::ColorBase::Rgb { rgb } if rgb.r == 0x10 && rgb.g == 0x2A && rgb.b == 0x43)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides.len(), before);
}

#[test]
fn invalid_style_is_rejected_without_changes() {
    let mut s = Session::with_new();
    let context = s.execute("assistant.context", &json!({"scope": "slide"})).unwrap();
    let before = s.active().unwrap().doc.slides.len();
    let shapes_before = s.active().unwrap().doc.slides[s.active().unwrap().selection.slide].shapes.len();
    for style in [
        json!({}),
        json!({"bold": null, "italic": null, "size": null, "color": null}),
        json!({"size": 0}),
        json!({"size": 401}),
        json!({"color": "red"}),
        json!({"bold": "yes"}),
        json!({"bold": true, "extra": 1}),
        json!({"bold": true, "italic": null, "size": null, "color": null, "rect": [0, 0, 1, 1]}),
    ] {
        let deck = json!({"operations": [{"kind":"add_text", "text":"x", "rect":[0, 0, 100, 50], "style": style}]});
        assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).is_err(), "style {style} was accepted");
    }
    let d = s.active().unwrap();
    assert_eq!(d.doc.slides.len(), before);
    assert_eq!(d.doc.slides[d.selection.slide].shapes.len(), shapes_before);
    // A style on an operation that cannot take one is refused.
    let deck = json!({"operations": [{"kind":"background", "color":"#123456", "style":{"bold":true}}]});
    assert!(s.execute("assistant.applyDeck", &json!({"context": context, "deck": deck})).is_err());
}
