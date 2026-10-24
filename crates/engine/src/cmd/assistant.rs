//! Assistant: the text of the current slide as a bounded snapshot an assistant reads, and the text
//! changes it proposes, applied as one undo step or not at all. First arc: the text of plain shapes
//! only (not tables, groups, notes or pictures).

use std::collections::HashSet;

use deckcraft_model::text::TextBody;
use deckcraft_model::{Shape, ShapeId, ShapeKind};
use serde_json::{Value, json};

use super::*;
use crate::{DocState, Result, Session, Target};

/// Most shapes a snapshot holds, and most changes a batch can make.
const MAX_TARGETS: usize = 256;
/// Most text a snapshot holds, and most new text a batch carries (bytes).
const MAX_BYTES: usize = 64 << 10;
const CONTEXT: &str = "assistant.context";
const APPLY: &str = "assistant.apply";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "assistant.context",
            "Mirai Assistant: Context",
            [],
            None,
            "{} (the selected shapes, else every shape with text, on the current slide) → {document_id, revision, slide_id, targets: [{shape_id, text}]}",
            has_doc,
            context
        ),
        cmd!(
            "assistant.apply",
            "Mirai Assistant: Apply Changes",
            [],
            None,
            "{context: snapshot from assistant.context, changes: [{index, text}]} (index is a position in targets; one undo step) → {applied}",
            has_doc,
            apply
        ),
    ]
}

/// A shape the assistant may read and change: a plain shape with a text body.
fn text_shape(sh: &Shape) -> bool {
    matches!(sh.kind, ShapeKind::Shape) && sh.text.is_some()
}

fn open_slide<'a>(st: &'a DocState, cmd: &str) -> Result<&'a deckcraft_model::Slide> {
    if st.selection.target != Target::Slides {
        return Err(bad(cmd, "the assistant works on slides, not on masters or layouts"));
    }
    st.current_slide().ok_or_else(|| bad(cmd, "no slide"))
}

/// The exact UTF-8 length of `body.text()`, measured from the runs without copying them: the
/// content of every run plus one `\n` between paragraphs.
fn text_bytes(body: &TextBody) -> usize {
    let runs = body.paragraphs.iter().flat_map(|p| p.runs.iter()).fold(0usize, |n, r| n.saturating_add(r.content().len()));
    runs.saturating_add(body.paragraphs.len().saturating_sub(1))
}

/// `body.text()`, once it is known to fit in `budget` bytes. A larger body is refused before its
/// text is built.
fn text_within(body: &TextBody, budget: usize, cmd: &str) -> Result<String> {
    if text_bytes(body) > budget {
        return Err(bad(cmd, format!("the text is larger than {MAX_BYTES} bytes; select fewer shapes")));
    }
    Ok(body.text())
}

fn context(s: &mut Session, _p: &Value) -> Result<Value> {
    let st = s.doc()?;
    let slide = open_slide(st, CONTEXT)?;
    // One past the limit is enough to know the selection is too big; nothing beyond it is collected.
    let picked: Vec<&Shape> = slide
        .shapes
        .iter()
        .filter(|sh| text_shape(sh) && (st.selection.shapes.is_empty() || st.selection.shapes.contains(&sh.id)))
        .take(MAX_TARGETS + 1)
        .collect();
    if picked.len() > MAX_TARGETS {
        return Err(bad(CONTEXT, format!("more than {MAX_TARGETS} shapes with text; select fewer")));
    }
    let mut bytes = 0usize;
    let mut targets = Vec::with_capacity(picked.len());
    for sh in picked {
        let Some(body) = sh.text.as_ref() else { continue };
        let text = text_within(body, MAX_BYTES.saturating_sub(bytes), CONTEXT)?;
        bytes = bytes.saturating_add(text.len());
        targets.push(json!({"shape_id": sh.id.0, "text": text}));
    }
    Ok(json!({"document_id": st.uid, "revision": st.revision, "slide_id": slide.id.0, "targets": targets}))
}

struct Snapshot {
    document_id: u64,
    revision: u64,
    slide_id: u32,
    targets: Vec<(ShapeId, String)>,
}

fn parse_snapshot(v: &Value) -> Result<Snapshot> {
    let obj = v.as_object().ok_or_else(|| bad(APPLY, "`context` is not the snapshot from assistant.context"))?;
    let num = |key: &str| obj.get(key).and_then(Value::as_u64).ok_or_else(|| bad(APPLY, format!("`context.{key}` is not a whole number")));
    let document_id = num("document_id")?;
    let revision = num("revision")?;
    let slide_id = u32::try_from(num("slide_id")?).map_err(|_| bad(APPLY, "`context.slide_id` is not valid"))?;
    let rows = obj.get("targets").and_then(Value::as_array).ok_or_else(|| bad(APPLY, "`context.targets` is not a list"))?;
    if rows.len() > MAX_TARGETS {
        return Err(bad(APPLY, format!("more than {MAX_TARGETS} targets")));
    }
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .get("shape_id")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| bad(APPLY, "a target's `shape_id` is not a valid id"))?;
        let text = row.get("text").and_then(Value::as_str).ok_or_else(|| bad(APPLY, "a target's `text` is not a string"))?;
        if !seen.insert(id) {
            return Err(bad(APPLY, format!("shape {id} is listed twice")));
        }
        bytes = bytes.saturating_add(text.len());
        if bytes > MAX_BYTES {
            return Err(bad(APPLY, format!("the snapshot's text is larger than {MAX_BYTES} bytes")));
        }
        targets.push((ShapeId(id), text.to_string()));
    }
    Ok(Snapshot { document_id, revision, slide_id, targets })
}

fn parse_changes(p: &Value, count: usize) -> Result<Vec<(usize, String)>> {
    let list = p.get("changes").and_then(Value::as_array).ok_or_else(|| bad(APPLY, "missing `changes` (a list of {index, text})"))?;
    if list.len() > count {
        return Err(bad(APPLY, "more changes than targets"));
    }
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    let mut out = Vec::with_capacity(list.len());
    for ch in list {
        let index = ch
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|i| *i < count)
            .ok_or_else(|| bad(APPLY, format!("`index` must be a target index below {count}")))?;
        if !seen.insert(index) {
            return Err(bad(APPLY, format!("index {index} is changed twice")));
        }
        let text = ch.get("text").and_then(Value::as_str).ok_or_else(|| bad(APPLY, "a change's `text` is not a string"))?;
        if text.contains('\u{FFFC}') {
            return Err(bad(APPLY, "text with U+FFFC (object replacement character) is not supported"));
        }
        bytes = bytes.saturating_add(text.len());
        if bytes > MAX_BYTES {
            return Err(bad(APPLY, format!("the new text is larger than {MAX_BYTES} bytes")));
        }
        out.push((index, text.to_string()));
    }
    Ok(out)
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let snap = parse_snapshot(p.get("context").ok_or_else(|| bad(APPLY, "missing `context` (the snapshot from assistant.context)"))?)?;
    let changes = parse_changes(p, snap.targets.len())?;
    // Everything is checked before the first edit, so a refused batch changes nothing.
    let plan = {
        let st = s.doc()?;
        if snap.document_id != st.uid {
            return Err(bad(APPLY, "the snapshot is from another presentation"));
        }
        if snap.revision != st.revision {
            return Err(bad(APPLY, "the presentation changed since assistant.context; take a new snapshot"));
        }
        let slide = open_slide(st, APPLY)?;
        if slide.id.0 != snap.slide_id {
            return Err(bad(APPLY, "the snapshot is for another slide"));
        }
        let mut locked = Vec::with_capacity(snap.targets.len());
        let mut left = MAX_BYTES;
        for (id, text) in &snap.targets {
            let sh = slide.shapes.iter().find(|x| x.id == *id).ok_or_else(|| bad(APPLY, format!("shape {id} is not on this slide")))?;
            let body = match sh.text.as_ref() {
                Some(body) if text_shape(sh) => body,
                _ => return Err(bad(APPLY, format!("shape {id} has no text"))),
            };
            let now = text_within(body, left, APPLY)?;
            left = left.saturating_sub(now.len());
            if now != *text {
                return Err(bad(APPLY, format!("shape {id}'s text changed since assistant.context")));
            }
            locked.push(sh.locked);
        }
        let mut plan = Vec::with_capacity(changes.len());
        for (index, text) in changes {
            let Some((id, _)) = snap.targets.get(index) else {
                return Err(bad(APPLY, "no such target"));
            };
            if locked.get(index).copied().unwrap_or(true) {
                return Err(bad(APPLY, format!("shape {id} is locked")));
            }
            plan.push((*id, text));
        }
        plan
    };
    // Each text.set keeps its shape's formatting. A failure part-way is rolled back by the outer
    // command, which is also the only undo step.
    for (id, text) in &plan {
        s.execute("text.set", &json!({"id": id.0, "text": text}))?;
    }
    Ok(json!({"applied": plan.len()}))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn deck_with_texts(texts: &[&str]) -> (Session, Vec<u32>) {
        let mut s = Session::with_new();
        // The new deck's placeholders have text too; start from an empty slide.
        s.execute("edit.selectAll", &json!({})).unwrap();
        s.execute("edit.delete", &json!({})).unwrap();
        let mut ids = Vec::new();
        for t in texts {
            let id = s.execute("shape.insert", &json!({"preset": "rect", "rect": [36, 36, 200, 100]})).unwrap()["id"].as_u64().unwrap();
            s.execute("text.set", &json!({"id": id, "text": t})).unwrap();
            ids.push(id as u32);
        }
        // shape.insert leaves the last shape selected; the tests start with nothing selected.
        s.execute("edit.deselect", &json!({})).unwrap();
        (s, ids)
    }

    fn context_of(s: &mut Session) -> Value {
        s.execute("assistant.context", &json!({})).unwrap()
    }

    fn text_of(s: &Session, id: u32) -> String {
        s.doc().unwrap().shape(ShapeId(id)).and_then(|sh| sh.text.as_ref()).map(TextBody::text).unwrap_or_default()
    }

    #[test]
    fn context_reads_the_slide_and_changes_nothing() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        let doc = s.doc().unwrap().doc.clone();
        let revision = s.doc().unwrap().revision;
        let undo = s.doc().unwrap().history.undo.len();
        let snap = context_of(&mut s);
        assert!(Arc::ptr_eq(&doc, &s.doc().unwrap().doc));
        assert_eq!(s.doc().unwrap().revision, revision);
        assert_eq!(s.doc().unwrap().history.undo.len(), undo);
        assert_eq!(snap["revision"], revision);
        assert_eq!(snap["targets"].as_array().unwrap().len(), 2);
        assert_eq!(snap["targets"][0]["shape_id"], ids[0]);
        assert_eq!(snap["targets"][1]["text"], "Dos");
    }

    #[test]
    fn a_selection_narrows_the_context() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        s.execute("edit.select", &json!({"ids": [ids[1]]})).unwrap();
        let snap = context_of(&mut s);
        assert_eq!(snap["targets"].as_array().unwrap().len(), 1);
        assert_eq!(snap["targets"][0]["shape_id"], ids[1]);
    }

    #[test]
    fn context_is_bounded_in_count_and_bytes() {
        let many = vec!["x"; MAX_TARGETS + 1];
        let (mut s, _) = deck_with_texts(&many);
        assert!(s.execute("assistant.context", &json!({})).is_err());

        let big = "a".repeat(70_000);
        let (mut s, _) = deck_with_texts(&[big.as_str()]);
        assert!(s.execute("assistant.context", &json!({})).is_err());

        let half = "b".repeat(40_000);
        let (mut s, _) = deck_with_texts(&[half.as_str(), half.as_str()]);
        assert!(s.execute("assistant.context", &json!({})).is_err(), "the second text would pass the byte budget");

        // 40 000 characters of 2 bytes each: under the budget in characters, over it in bytes.
        let wide = "ñ".repeat(40_000);
        let (mut s, _) = deck_with_texts(&[wide.as_str()]);
        assert!(s.execute("assistant.context", &json!({})).is_err(), "measured in bytes, not characters");
    }

    #[test]
    fn apply_changes_two_shapes_in_one_undo_step() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        let snap = context_of(&mut s);
        let undo = s.doc().unwrap().history.undo.len();
        let journal = s.journal.len();
        let p = json!({"context": snap, "changes": [{"index": 0, "text": "Primero"}, {"index": 1, "text": "Segundo\nlínea"}]});
        assert_eq!(s.execute("assistant.apply", &p).unwrap()["applied"], 2);
        assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1);
        assert_eq!(s.journal.len(), journal + 1, "one journal entry for the whole batch");
        assert_eq!(text_of(&s, ids[0]), "Primero");
        assert_eq!(text_of(&s, ids[1]), "Segundo\nlínea");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(text_of(&s, ids[0]), "Uno");
        assert_eq!(text_of(&s, ids[1]), "Dos");
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(text_of(&s, ids[0]), "Primero");
        assert_eq!(text_of(&s, ids[1]), "Segundo\nlínea");
    }

    #[test]
    fn stale_or_mismatched_snapshots_change_nothing() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        let snap = context_of(&mut s);
        let change = |snap: Value| json!({"context": snap, "changes": [{"index": 0, "text": "Nuevo"}]});

        let uid = s.doc().unwrap().uid;
        let mut other = snap.clone();
        other["document_id"] = json!(uid.wrapping_add(1));
        assert!(s.execute("assistant.apply", &change(other)).is_err(), "another document");

        let mut tampered = snap.clone();
        tampered["targets"][0]["text"] = json!("Otro");
        assert!(s.execute("assistant.apply", &change(tampered)).is_err(), "original text differs");

        let mut twice = snap.clone();
        twice["targets"][1]["shape_id"] = json!(ids[0]);
        assert!(s.execute("assistant.apply", &change(twice)).is_err(), "a shape listed twice");

        s.execute("text.set", &json!({"id": ids[1], "text": "Cambio"})).unwrap();
        assert!(s.execute("assistant.apply", &change(snap)).is_err(), "the presentation changed since");
        assert_eq!(text_of(&s, ids[0]), "Uno");
        assert_eq!(text_of(&s, ids[1]), "Cambio");
    }

    #[test]
    fn a_locked_shape_refuses_the_whole_batch() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        s.execute("shape.lock", &json!({"id": ids[1], "locked": true})).unwrap();
        let snap = context_of(&mut s);
        let undo = s.doc().unwrap().history.undo.len();
        let journal = s.journal.len();
        let both = json!({"context": snap.clone(), "changes": [{"index": 0, "text": "A"}, {"index": 1, "text": "B"}]});
        assert!(s.execute("assistant.apply", &both).is_err());
        let only_locked = json!({"context": snap, "changes": [{"index": 1, "text": "B"}]});
        assert!(s.execute("assistant.apply", &only_locked).is_err());
        assert_eq!(text_of(&s, ids[0]), "Uno", "the free shape waits for the batch");
        assert_eq!(text_of(&s, ids[1]), "Dos");
        assert_eq!(s.doc().unwrap().history.undo.len(), undo);
        assert_eq!(s.journal.len(), journal, "a refused batch is not journaled");
    }

    #[test]
    fn unicode_and_multiline_text_keeps_the_formatting() {
        let (mut s, ids) = deck_with_texts(&["Uno"]);
        s.edit(|doc, sel| {
            let shapes = crate::shapes_mut(doc, sel).ok_or_else(|| crate::EngineError::Other("no slide".into()))?;
            let sh = deckcraft_model::find_shape_mut(shapes, ShapeId(ids[0])).ok_or_else(|| crate::EngineError::Other("no shape".into()))?;
            for body in sh.text.iter_mut() {
                for p in &mut body.paragraphs {
                    for r in &mut p.runs {
                        r.props.bold = Some(true);
                    }
                }
            }
            Ok(())
        })
        .unwrap();
        let snap = context_of(&mut s);
        let text = "Ñandú 🙂\nlínea 2\n\n3";
        s.execute("assistant.apply", &json!({"context": snap, "changes": [{"index": 0, "text": text}]})).unwrap();
        assert_eq!(text_of(&s, ids[0]), text);
        let body = s.doc().unwrap().shape(ShapeId(ids[0])).and_then(|sh| sh.text.clone()).unwrap();
        assert_eq!(body.paragraphs.len(), 4);
        assert_eq!(body.paragraphs[0].runs[0].props.bold, Some(true), "the first run's bold survives");
    }

    #[test]
    fn hostile_indexes_and_types_are_refused() {
        let (mut s, ids) = deck_with_texts(&["Uno", "Dos"]);
        let snap = context_of(&mut s);
        let undo = s.doc().unwrap().history.undo.len();
        let batch = |changes: Value| json!({"context": snap.clone(), "changes": changes});
        let cases = vec![
            batch(json!([{"index": -1, "text": "a"}])),
            batch(json!([{"index": "0", "text": "a"}])),
            batch(json!([{"index": 2, "text": "a"}])),
            batch(json!([{"index": 0, "text": "a"}, {"index": 0, "text": "b"}])),
            batch(json!([{"index": 0, "text": 7}])),
            batch(json!([{"index": 0, "text": "a\u{FFFC}b"}])),
            batch(json!("not a list")),
            batch(json!([{"index": 0, "text": "a"}, {"index": 1, "text": "b"}, {"index": 0, "text": "c"}])),
            json!({"changes": []}),
            json!({"context": "x", "changes": []}),
            json!({"context": {"document_id": "x"}, "changes": []}),
            json!({"context": snap.clone(), "changes": {"index": 0}}),
        ];
        for case in &cases {
            assert!(s.execute("assistant.apply", case).is_err(), "refused: {case}");
        }
        let mut negative = snap.clone();
        negative["targets"][0]["shape_id"] = json!(-4);
        assert!(s.execute("assistant.apply", &json!({"context": negative, "changes": []})).is_err());
        let mut huge = snap.clone();
        huge["targets"][0]["shape_id"] = json!(u64::from(u32::MAX) + 1);
        assert!(s.execute("assistant.apply", &json!({"context": huge, "changes": []})).is_err());
        assert_eq!(text_of(&s, ids[0]), "Uno");
        assert_eq!(text_of(&s, ids[1]), "Dos");
        assert_eq!(s.doc().unwrap().history.undo.len(), undo);
    }
}
