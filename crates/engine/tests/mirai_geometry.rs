//! Mirai geometry and fill operations: `assistant.geometryContext` and the typed shape operations of
//! `assistant.applyDeck`. Offline; no provider, no network.

use std::sync::Arc;

use deckcraft_engine::Session;
use deckcraft_model::{ColorBase, Fill, Shape, ShapeId};
use serde_json::{Value, json};

fn blank_slide() -> Session {
    let mut s = Session::with_new();
    s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
    s
}

fn insert(s: &mut Session, preset: &str, rect: [i32; 4]) -> u32 {
    let id = s.execute("shape.insert", &json!({"preset": preset, "rect": rect})).unwrap()["id"].as_u64().unwrap();
    u32::try_from(id).unwrap()
}

fn slide_shapes(s: &Session) -> Vec<Shape> {
    let d = s.active().unwrap();
    d.doc.slides.get(d.selection.slide).unwrap().shapes.clone()
}

fn shape(s: &Session, id: u32) -> Shape {
    s.active().unwrap().shape(ShapeId(id)).unwrap().clone()
}

fn solid_rgb(sh: &Shape) -> Option<(u8, u8, u8)> {
    match &sh.fill {
        Some(Fill::Solid { color }) => match &color.base {
            ColorBase::Rgb { rgb } => Some((rgb.r, rgb.g, rgb.b)),
            _ => None,
        },
        _ => None,
    }
}

fn geometry(s: &mut Session) -> Value {
    s.execute("assistant.geometryContext", &json!({})).unwrap()
}

fn text_context(s: &mut Session) -> Value {
    s.execute("assistant.context", &json!({"scope": "slide"})).unwrap()
}

/// Applies a deck with an optional geometry snapshot. Returns whether it was accepted.
fn apply(s: &mut Session, geo: Option<&Value>, ops: Value) -> bool {
    let ctx = text_context(s);
    let mut params = json!({"context": ctx, "deck": {"operations": ops}});
    if let Some(g) = geo {
        params["geometry"] = g.clone();
    }
    s.execute("assistant.applyDeck", &params).is_ok()
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

#[test]
fn geometry_context_lists_only_unlocked_text_free_autoshapes_with_their_own_indices() {
    let mut s = blank_slide();
    let rect = insert(&mut s, "rect", [10, 20, 100, 50]);
    s.execute("insert.textBox", &json!({"rect": [0, 0, 200, 40], "text": "Words"})).unwrap();
    let locked = insert(&mut s, "rect", [200, 20, 100, 50]);
    insert(&mut s, "straightConnector1", [0, 200, 80, 0]);
    let ellipse = insert(&mut s, "ellipse", [300, 20, 60, 60]);
    s.edit(|doc, sel| {
        let idx = sel.slide;
        let slide = Arc::make_mut(doc.slides.get_mut(idx).unwrap());
        slide.shapes.iter_mut().find(|sh| sh.id == ShapeId(locked)).unwrap().locked = true;
        Ok(())
    })
    .unwrap();

    let ctx = geometry(&mut s);
    let shapes = ctx["shapes"].as_array().unwrap();
    assert_eq!(shapes.len(), 2, "text box, connector and locked shape are not listed");
    assert_eq!(shapes[0]["index"], 0);
    assert_eq!(shapes[0]["shape"], json!(rect));
    assert_eq!(shapes[0]["preset"], "rect");
    assert_eq!(shapes[0]["fill"], Value::Null, "an inherited fill has no plain colour");
    assert_eq!(shapes[1]["index"], 1);
    assert_eq!(shapes[1]["shape"], json!(ellipse));
    assert_eq!(shapes[1]["preset"], "ellipse");
    assert_eq!(ctx["skipped"], 3);
    assert_eq!(number(&shapes[0]["rect"][2]), Some(100.0));
}

#[test]
fn geometry_indices_and_text_indices_are_separate_spaces() {
    let mut s = blank_slide();
    let text = s.execute("insert.textBox", &json!({"rect": [0, 0, 200, 40], "text": "Words"})).unwrap()["id"].as_u64().unwrap();
    let text = u32::try_from(text).unwrap();
    let rect = insert(&mut s, "rect", [10, 20, 100, 50]);
    let text_before = shape(&s, text);
    let geo = geometry(&mut s);
    // Geometry index 0 is the rectangle, although the text box is text target 0.
    assert!(apply(&mut s, Some(&geo), json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])));
    assert_eq!(solid_rgb(&shape(&s, rect)), Some((0x10, 0x2A, 0x43)));
    assert_eq!(shape(&s, text), text_before, "the text box is untouched");
}

#[test]
fn set_bounds_and_fill_apply_as_one_undo_step() {
    let mut s = blank_slide();
    let rect = insert(&mut s, "rect", [10, 20, 100, 50]);
    let before = shape(&s, rect);
    let geo = geometry(&mut s);
    let ops = json!([
        {"kind": "set_shape_bounds", "shape": 0, "rect": [50, 60, 200, 80]},
        {"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}
    ]);
    assert!(apply(&mut s, Some(&geo), ops));
    let after = shape(&s, rect);
    assert_eq!(after.xfrm.unwrap().x, 50.0);
    assert_eq!(after.xfrm.unwrap().w, 200.0);
    assert_eq!(solid_rgb(&after), Some((0x10, 0x2A, 0x43)));
    s.execute("edit.undo", &json!({})).unwrap();
    let undone = shape(&s, rect);
    assert_eq!(undone.xfrm.unwrap().x, before.xfrm.unwrap().x);
    assert_eq!(undone.fill, before.fill, "undo restores the fill too");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(shape(&s, rect).xfrm.unwrap().x, 50.0);
}

#[test]
fn add_shape_creates_a_filled_closed_shape_and_undo_removes_it() {
    let mut s = blank_slide();
    let before = slide_shapes(&s).len();
    let ops = json!([{"kind": "add_shape", "preset": "roundRect", "rect": [30, 40, 120, 60], "color": "#FF8800"}]);
    assert!(apply(&mut s, None, ops), "add_shape needs no geometry snapshot");
    let shapes = slide_shapes(&s);
    assert_eq!(shapes.len(), before + 1);
    let created = shapes.last().unwrap();
    assert_eq!(created.geom.preset_name(), Some("roundRect"));
    assert_eq!(solid_rgb(created), Some((0xFF, 0x88, 0x00)));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(slide_shapes(&s).len(), before);
}

#[test]
fn add_shape_without_color_keeps_the_default_look() {
    let mut s = blank_slide();
    let ops = json!([{"kind": "add_shape", "preset": "ellipse", "rect": [30, 40, 120, 60]}]);
    assert!(apply(&mut s, None, ops));
    let created = slide_shapes(&s).last().unwrap().clone();
    assert_eq!(created.geom.preset_name(), Some("ellipse"));
    assert_eq!(created.fill, None);
}

#[test]
fn a_refused_proposal_changes_nothing() {
    let mut s = blank_slide();
    let rect = insert(&mut s, "rect", [10, 20, 100, 50]);
    let geo = geometry(&mut s);
    let before = s.active().unwrap().doc.clone();
    let refused = [
        json!([{"kind": "set_shape_bounds", "shape": 5, "rect": [0, 0, 10, 10]}]),
        json!([{"kind": "set_shape_bounds", "shape": 0, "rect": [0, 0, -10, 10]}]),
        json!([{"kind": "set_shape_bounds", "shape": 0, "rect": [0, 0, 10, 0]}]),
        json!([{"kind": "set_shape_fill", "shape": 0, "color": "red"}]),
        json!([{"kind": "set_shape_fill", "shape": 0, "color": "#12"}]),
        json!([{"kind": "set_shape_fill", "shape": 0}]),
        json!([{"kind": "add_shape", "preset": "straightConnector1", "rect": [0, 0, 0, 0]}]),
        json!([{"kind": "add_shape", "preset": "noSuchShape", "rect": [0, 0, 50, 50]}]),
        json!([{"kind": "add_shape", "preset": "rect", "rect": [0, 0, 50, 50], "color": "#12"}]),
        json!([{"kind": "add_shape", "preset": "rect", "rect": [0, 0, 0, 50]}]),
        json!([{"kind": "add_shape", "preset": "rect", "rect": [0, 0, 50, 50], "target": 0}]),
        json!([{"kind": "set_shape_bounds", "shape": 0, "rect": [0, 0, 10, 10], "text": "x"}]),
        json!([
            {"kind": "set_shape_bounds", "shape": 0, "rect": [0, 0, 10, 10]},
            {"kind": "add_shape", "preset": "noSuchShape", "rect": [0, 0, 50, 50]}
        ]),
        json!([{"kind": "new_slide", "layout": "blank"}, {"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}]),
    ];
    for ops in refused {
        assert!(!apply(&mut s, Some(&geo), ops.clone()), "accepted: {ops}");
    }
    assert!(!apply(&mut s, None, json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])), "no geometry snapshot");
    assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc), "no edit was made");
    assert_eq!(shape(&s, rect).xfrm.unwrap().x, 10.0);
}

#[test]
fn too_many_operations_are_refused() {
    let mut s = blank_slide();
    insert(&mut s, "rect", [10, 20, 100, 50]);
    let geo = geometry(&mut s);
    let ops: Vec<Value> = (0..101).map(|_| json!({"kind": "set_shape_fill", "shape": 0, "color": "#102A43"})).collect();
    let before = s.active().unwrap().doc.clone();
    assert!(!apply(&mut s, Some(&geo), Value::Array(ops)));
    assert!(Arc::ptr_eq(&before, &s.active().unwrap().doc));
}

#[test]
fn a_change_after_the_snapshot_makes_it_stale() {
    let mut s = blank_slide();
    let rect = insert(&mut s, "rect", [10, 20, 100, 50]);
    let geo = geometry(&mut s);
    s.execute("shape.setBounds", &json!({"ids": [rect], "x": 90, "y": 90, "w": 40, "h": 40})).unwrap();
    assert!(!apply(&mut s, Some(&geo), json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])));
    assert_eq!(solid_rgb(&shape(&s, rect)), None);
}

#[test]
fn a_forged_locked_or_renamed_entry_is_refused() {
    let mut s = blank_slide();
    let open = insert(&mut s, "rect", [10, 20, 100, 50]);
    let locked = insert(&mut s, "ellipse", [200, 20, 100, 50]);
    s.edit(|doc, sel| {
        let idx = sel.slide;
        let slide = Arc::make_mut(doc.slides.get_mut(idx).unwrap());
        slide.shapes.iter_mut().find(|sh| sh.id == ShapeId(locked)).unwrap().locked = true;
        Ok(())
    })
    .unwrap();
    let mut forged = geometry(&mut s);
    forged["shapes"][0]["shape"] = json!(locked);
    let fill = json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}]);
    assert!(!apply(&mut s, Some(&forged), fill), "a locked shape is never edited");
    let mut renamed = geometry(&mut s);
    renamed["shapes"][0]["preset"] = json!("ellipse");
    assert!(!apply(&mut s, Some(&renamed), json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])));
    let mut doubled = geometry(&mut s);
    doubled["shapes"] = json!([doubled["shapes"][0].clone(), doubled["shapes"][0].clone()]);
    doubled["shapes"][1]["index"] = json!(1);
    assert!(!apply(&mut s, Some(&doubled), json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])));
    assert_eq!(solid_rgb(&shape(&s, open)), None);
    assert_eq!(solid_rgb(&shape(&s, locked)), None);
}

#[test]
fn a_bad_geometry_snapshot_is_refused_without_panicking() {
    let mut s = blank_slide();
    insert(&mut s, "rect", [10, 20, 100, 50]);
    let good = geometry(&mut s);
    let junk = [
        json!(5),
        json!({"extra": 1}),
        json!({"document_id": "x"}),
        json!({"document_id": good["document_id"], "revision": good["revision"], "slide_index": 0, "slide_id": good["slide_id"], "shapes": "no"}),
        json!({"document_id": good["document_id"], "revision": good["revision"], "slide_index": 0, "slide_id": good["slide_id"], "shapes": [{"index": 3}]}),
    ];
    for geo in junk {
        assert!(!apply(&mut s, Some(&geo), json!([{"kind": "set_shape_fill", "shape": 0, "color": "#102A43"}])), "accepted: {geo}");
    }
}

/// Runs `f` on shape `id` of the current slide.
fn edit_shape(s: &mut Session, id: u32, f: impl FnOnce(&mut Shape)) {
    s.edit(|doc, sel| {
        let idx = sel.slide;
        let slide = Arc::make_mut(doc.slides.get_mut(idx).unwrap());
        f(slide.shapes.iter_mut().find(|sh| sh.id == ShapeId(id)).unwrap());
        Ok(())
    })
    .unwrap();
}

#[test]
fn lines_are_listed_but_presets_outside_the_protocol_are_not() {
    let mut s = blank_slide();
    let odd = insert(&mut s, "rect", [10, 20, 100, 50]);
    edit_shape(&mut s, odd, |sh| sh.geom = deckcraft_model::Geom::preset("my-shape"));
    let line = insert(&mut s, "line", [10, 100, 100, 0]);
    let kept = insert(&mut s, "ellipse", [200, 20, 100, 50]);
    let ids: Vec<u64> = geometry(&mut s)["shapes"].as_array().unwrap().iter().map(|e| e["shape"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![u64::from(line), u64::from(kept)], "odd {odd} must not be listed");
}

/// The solid colour of a shape's stroke.
fn line_rgb(sh: &Shape) -> Option<(u8, u8, u8)> {
    solid_rgb(&Shape { fill: sh.line.as_ref()?.fill.clone(), ..Default::default() })
}

#[test]
fn a_line_is_coloured_by_its_stroke_and_moved_flat() {
    let mut s = blank_slide();
    let line = insert(&mut s, "line", [10, 100, 100, 0]);
    let g = geometry(&mut s);
    let ops = json!([
        {"kind": "set_shape_fill", "shape": 0, "color": "#102A43"},
        {"kind": "set_shape_bounds", "shape": 0, "rect": [20, 200, 300, 0]}
    ]);
    assert!(apply(&mut s, Some(&g), ops));
    let sh = shape(&s, line);
    assert_eq!(line_rgb(&sh), Some((0x10, 0x2A, 0x43)));
    assert_eq!(solid_rgb(&sh), None, "a line gets no fill");
    let x = sh.xfrm.unwrap();
    assert_eq!((x.x, x.y, x.w, x.h), (20.0, 200.0, 300.0, 0.0));
}

#[test]
fn lines_can_be_added_flat_but_closed_shapes_cannot() {
    let mut s = blank_slide();
    let before = slide_shapes(&s).len();
    assert!(apply(&mut s, None, json!([{"kind": "add_shape", "preset": "line", "rect": [10, 20, 300, 0], "color": "#102A43"}])));
    let added = slide_shapes(&s);
    assert_eq!(added.len(), before + 1);
    assert_eq!(added.last().and_then(line_rgb), Some((0x10, 0x2A, 0x43)));
    assert!(!apply(&mut s, None, json!([{"kind": "add_shape", "preset": "rect", "rect": [10, 20, 300, 0]}])));
    assert!(!apply(&mut s, None, json!([{"kind": "add_shape", "preset": "line", "rect": [10, 20, 0, 0]}])));
    assert!(!apply(&mut s, None, json!([{"kind": "add_shape", "preset": "hexagon2", "rect": [10, 20, 30, 30]}])));
}

#[test]
fn labels_never_carry_separators_the_protocol_refuses() {
    let mut s = blank_slide();
    let id = insert(&mut s, "rect", [10, 20, 100, 50]);
    edit_shape(&mut s, id, |sh| sh.name = "Box\u{2028}1\u{2029}\u{FFFC}".into());
    let label = geometry(&mut s)["shapes"][0]["label"].as_str().unwrap().to_owned();
    assert!(!label.contains(['\u{2028}', '\u{2029}', '\u{FFFC}']), "{label:?}");
    assert!(label.starts_with("Box"));
}

#[test]
fn a_crowded_slide_lists_the_cap_and_skips_the_rest() {
    let mut s = blank_slide();
    for i in 0..260 {
        insert(&mut s, "rect", [i % 50, i / 50, 5, 5]);
    }
    let g = geometry(&mut s);
    assert_eq!(g["shapes"].as_array().unwrap().len(), 256);
    assert!(g["skipped"].as_u64().unwrap() >= 4);
}

#[test]
fn an_added_shape_is_left_unselected() {
    let mut s = blank_slide();
    let before = slide_shapes(&s).len();
    assert!(apply(&mut s, None, json!([{"kind": "add_shape", "preset": "star5", "rect": [10, 20, 100, 50], "color": "#102A43"}])));
    assert_eq!(slide_shapes(&s).len(), before + 1);
    assert!(s.active().unwrap().selection.shapes.is_empty());
}
