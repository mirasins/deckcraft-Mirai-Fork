use deckcraft_engine::Session;
use serde_json::{Value, json};

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

#[test]
fn presentation_deck_styles_existing_text_and_undoes() {
    let mut s = Session::with_new();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40], "text":"First"})).unwrap();
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    s.execute("insert.textBox", &json!({"rect":[10,10,200,40], "text":"Second"})).unwrap();
    s.execute("slide.go", &json!({"index":0})).unwrap();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let targets = context["targets"].as_array().unwrap();
    let first = targets.iter().position(|t| t["original"] == "First").unwrap();
    let deck = json!({"operations": [
        {"kind":"background", "color":"#112233"},
        {"kind":"set_style", "target":first, "style":{"bold":true,"italic":null,"size":18,"color":"#445566"}}
    ]});
    s.execute("assistant.applyPresentation", &json!({"context":context,"deck":deck})).unwrap();
    let shape = s.active().unwrap().doc.slides[0].shapes.iter().find(|sh| sh.text.as_ref().map(|t| t.text()) == Some("First".to_string())).unwrap();
    let run = &shape.text.as_ref().unwrap().paragraphs[0].runs[0];
    assert_eq!(run.props.bold, Some(true));
    assert_eq!(run.props.size, Some(18.0));
    s.execute("edit.undo", &json!({})).unwrap();
    let shape = s.active().unwrap().doc.slides[0].shapes.iter().find(|sh| sh.text.as_ref().map(|t| t.text()) == Some("First".to_string())).unwrap();
    let run = &shape.text.as_ref().unwrap().paragraphs[0].runs[0];
    assert_eq!(run.props.bold, None);
}

#[test]
fn presentation_deck_with_null_deck_applies_text_only() {
    let mut s = two_slides();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let targets = context["targets"].as_array().unwrap();
    let first = targets.iter().position(|t| t["original"] == "First").unwrap();
    let out = s.execute("assistant.applyPresentation", &json!({"context":context, "changes":[{"index":first,"text":"One"}], "deck":null})).unwrap();
    assert_eq!(out["applied"], 1);
    let next = s.execute("assistant.presentationContext", &json!({})).unwrap();
    assert_eq!(next["targets"][first]["original"], "One");
}

#[test]
fn presentation_deck_shape_fill_is_slide_scoped_and_undoable() {
    let mut s = Session::with_new();
    let id = s.execute("shape.insert", &json!({"preset":"rect","rect":[10,20,100,50]})).unwrap()["id"].as_u64().unwrap();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let geometry = s.execute("assistant.geometryContext", &json!({})).unwrap();
    let deck = json!({"operations":[{"kind":"set_shape_fill","shape":0,"color":"#102A43"}]});
    s.execute("assistant.applyPresentation", &json!({"context":context,"deck":deck,"geometry":geometry})).unwrap();
    assert!(s.active().unwrap().shape(deckcraft_model::ShapeId(id as u32)).unwrap().fill.is_some());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.active().unwrap().shape(deckcraft_model::ShapeId(id as u32)).unwrap().fill.is_none());
}

fn two_slides_with_shapes() -> (Session, u32, u32) {
    let mut s = Session::with_new();
    let first = s.execute("shape.insert", &json!({"preset":"rect","rect":[10,20,100,50]})).unwrap()["id"].as_u64().unwrap() as u32;
    s.execute("slide.new", &json!({"layout":"blank"})).unwrap();
    let second = s.execute("shape.insert", &json!({"preset":"ellipse","rect":[30,40,80,60]})).unwrap()["id"].as_u64().unwrap() as u32;
    s.execute("slide.go", &json!({"index":0})).unwrap();
    (s, first, second)
}

fn shape_on(s: &Session, slide: usize, id: u32) -> &deckcraft_model::Shape {
    s.active().unwrap().doc.slides[slide].shapes.iter().find(|sh| sh.id == deckcraft_model::ShapeId(id)).unwrap()
}

#[test]
fn presentation_geometry_edits_shapes_on_their_own_slides() {
    let (mut s, first, second) = two_slides_with_shapes();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let geometry = s.execute("assistant.presentationGeometryContext", &json!({})).unwrap();
    assert_eq!(geometry["scope"], "presentation");
    let shapes = geometry["shapes"].as_array().unwrap();
    assert_eq!(shapes.len(), 2);
    assert_eq!(shapes[0]["slide_index"], 0);
    assert_eq!(shapes[1]["slide_index"], 1);
    assert_ne!(shapes[0]["slide_id"], shapes[1]["slide_id"]);
    let deck = json!({"operations":[
        {"kind":"set_shape_fill","shape":0,"color":"#102A43"},
        {"kind":"set_shape_bounds","shape":1,"rect":[5,6,70,40]}
    ]});
    s.execute("assistant.applyPresentation", &json!({"context":context,"deck":deck,"geometry":geometry})).unwrap();
    assert!(shape_on(&s, 0, first).fill.is_some());
    let x = shape_on(&s, 1, second).xfrm.unwrap();
    assert_eq!((x.x, x.y, x.w, x.h), (5.0, 6.0, 70.0, 40.0));
    // One undo step reverts both slides' shape changes.
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(shape_on(&s, 0, first).fill.is_none());
    let x = shape_on(&s, 1, second).xfrm.unwrap();
    assert_eq!((x.x, x.y, x.w, x.h), (30.0, 40.0, 80.0, 60.0));
}

#[test]
fn presentation_geometry_rejects_forged_and_stale_entries() {
    let (mut s, _first, _second) = two_slides_with_shapes();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let geometry = s.execute("assistant.presentationGeometryContext", &json!({})).unwrap();
    let fill = json!({"operations":[{"kind":"set_shape_fill","shape":0,"color":"#102A43"}]});
    // A forged slide id on an entry is refused before any edit.
    let mut forged = geometry.clone();
    forged["shapes"][0]["slide_id"] = json!(999999);
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":fill.clone(),"geometry":forged})).is_err());
    // A shape moved to another slide's index is refused.
    let mut swapped = geometry.clone();
    swapped["shapes"][0]["slide_index"] = json!(1);
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":fill.clone(),"geometry":swapped})).is_err());
    // A duplicate shape across entries is refused.
    let mut twice = geometry.clone();
    twice["shapes"][1]["shape"] = twice["shapes"][0]["shape"].clone();
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":fill.clone(),"geometry":twice})).is_err());
    // An index outside the snapshot is refused.
    let out = json!({"operations":[{"kind":"set_shape_fill","shape":9,"color":"#102A43"}]});
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":out,"geometry":geometry.clone()})).is_err());
    // A stale revision is refused and nothing changed.
    s.execute("insert.textBox", &json!({"rect":[1,1,50,20],"text":"x"})).unwrap();
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context,"deck":fill,"geometry":geometry})).is_err());
}

#[test]
fn presentation_geometry_hostile_params_never_panic() {
    let (mut s, _first, _second) = two_slides_with_shapes();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    for geometry in [
        json!({"document_id":0,"revision":0,"slide_index":0,"slide_id":0,"shapes":[]}),
        json!({"document_id":"x","revision":null,"slide_index":-1,"slide_id":1e99,"shapes":{"a":1}}),
        json!({"document_id":0,"revision":0,"slide_index":0,"slide_id":0,"skipped":"many","shapes":[{"index":0,"shape":4294967295u64,"label":"\u{0001}","preset":"..","rect":[1,2],"fill":"#GGGGGG"}]}),
        json!({"document_id":0,"revision":0,"slide_index":9999999999i64,"slide_id":0,"shapes":[{"index":5,"slide_index":1e18,"slide_id":7,"shape":1,"preset":"rect","rect":[0,0,1,1],"fill":null}]}),
        Value::Null,
        json!("geometry"),
    ] {
        let deck = json!({"operations":[{"kind":"set_shape_fill","shape":0,"color":"#102A43"}]});
        assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":deck,"geometry":geometry})).is_err());
    }
    for params in [
        json!({}),
        json!({"context":context.clone()}),
        json!({"context":context.clone(),"deck":{"operations":[]}}),
        json!({"context":context.clone(),"deck":{"operations":[{"kind":"set_shape_bounds","shape":0,"rect":[-1,-1,0,0]}]}}),
    ] {
        assert!(s.execute("assistant.applyPresentation", &params).is_err());
    }
}

fn op(kind: &str, slide: Option<u64>) -> Value {
    json!({"kind":kind,"target":null,"layout":null,"text":null,"rect":null,"color":null,"style":null,"shape":null,"preset":null,"slide":slide})
}

#[test]
fn presentation_deck_paints_and_adds_on_any_existing_slide() {
    let mut s = two_slides();
    // The current slide is the second; the proposal works on the first by number.
    assert_eq!(s.active().unwrap().selection.slide, 1);
    let before = s.active().unwrap().doc.slides.clone();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let mut background = op("background", Some(1));
    background["color"] = json!("#102A43");
    let mut text = op("add_text", Some(1));
    text["rect"] = json!([20, 80, 300, 40]);
    text["text"] = json!("Added");
    let mut shape = op("add_shape", Some(1));
    shape["preset"] = json!("rect");
    shape["rect"] = json!([20, 140, 100, 50]);
    let deck = json!({"operations":[background, text, shape]});
    s.execute("assistant.applyPresentation", &json!({"context":context,"deck":deck})).unwrap();
    let st = s.active().unwrap();
    assert_eq!(st.selection.slide, 1, "the current slide stays put");
    let (first, second) = (&st.doc.slides[0], &st.doc.slides[1]);
    assert_eq!(first.shapes.len(), before[0].shapes.len() + 2);
    assert_ne!(first.background, before[0].background);
    assert_eq!(second, &before[1], "the current slide is untouched");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.slides, before, "one undo step");
}

#[test]
fn presentation_deck_slide_numbers_are_checked() {
    let mut s = two_slides();
    let context = s.execute("assistant.presentationContext", &json!({})).unwrap();
    let painted = |slide: Value| {
        let mut b = op("background", None);
        b["color"] = json!("#102A43");
        b["slide"] = slide;
        b
    };
    for slide in [json!(0), json!(3), json!(-1), json!("1"), json!(u64::MAX)] {
        let deck = json!({"operations":[painted(slide.clone())]});
        assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":deck})).is_err(), "slide {slide}");
    }
    // A slide number can't follow new_slide.
    let mut new_slide = op("new_slide", None);
    new_slide["layout"] = json!("blank");
    let deck = json!({"operations":[new_slide, painted(json!(1))]});
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context.clone(),"deck":deck})).is_err());
    // Only operations that place or paint take one.
    let mut set_text = op("set_text", Some(1));
    set_text["target"] = json!(0);
    set_text["text"] = json!("x");
    assert!(s.execute("assistant.applyPresentation", &json!({"context":context,"deck":{"operations":[set_text]}})).is_err());
    // A single-slide proposal never takes a slide number.
    let context = s.execute("assistant.context", &json!({"scope":"slide"})).unwrap();
    assert!(s.execute("assistant.applyDeck", &json!({"context":context,"deck":{"operations":[painted(json!(1))]}})).is_err());
}
