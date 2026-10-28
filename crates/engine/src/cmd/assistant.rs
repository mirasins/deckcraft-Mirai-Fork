//! Writing-assistant engine slice: a bounded, revision-checked snapshot of the plain text shapes a
//! writer selected (or every plain text shape on the current slide) and an undoable, all-or-nothing
//! apply of plain-text replacements. Replacements go through `text.set`, so paragraph levels and
//! properties and the first run's formatting follow the standard text path. No network and no
//! provider: the assistant transport lives outside the engine.

use std::collections::HashSet;

use deckcraft_model::text::RunKind;
use deckcraft_model::{Shape, ShapeId, ShapeKind, SlideId};
use serde_json::{Map, Value, json};

use super::{CommandSpec, bad, cmd, has_slide};
use crate::{Result, Session, Target};

const CMD_CONTEXT: &str = "assistant.context";
const CMD_APPLY: &str = "assistant.apply";
/// Most targets a snapshot holds, and most changes one apply takes.
pub const MAX_TARGETS: usize = 256;
/// Most text, in bytes, a snapshot holds (its originals) and an apply writes (its replacements).
pub const MAX_TEXT_BYTES: usize = 65_536;
/// Longest label a target carries.
pub const MAX_LABEL_BYTES: usize = 64;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "assistant.context",
            "Writing Assistant Context",
            [],
            None,
            "{scope?: selection|slide} (selection: default, the selected top-level text shapes; slide: every nonempty plain text shape on the current slide, up to 256; empty placeholders are skipped) → {document_id, revision, scope, slide_id, slide_index, skipped, targets: [{index, shape, label, original}]}",
            has_slide,
            context
        ),
        cmd!(
            "assistant.apply",
            "Apply Writing Assistant Changes",
            [],
            None,
            "{context: snapshot from assistant.context, changes: [{index, text, markdown?: false}]} → {applied, document_id, revision, slide_id}; one undo step, all or nothing",
            has_slide,
            apply
        ),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Selection,
    Slide,
}

impl Scope {
    fn named(name: &str, cmd: &str) -> Result<Scope> {
        match name {
            "selection" => Ok(Scope::Selection),
            "slide" => Ok(Scope::Slide),
            other => Err(bad(cmd, format!("unknown `scope` {:?}: selection or slide", short(other)))),
        }
    }

    fn parse(v: Option<&Value>, cmd: &str) -> Result<Scope> {
        match v {
            None | Some(Value::Null) => Ok(Scope::Selection),
            Some(Value::String(name)) => Scope::named(name, cmd),
            Some(_) => Err(bad(cmd, "`scope` must be a string")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Scope::Selection => "selection",
            Scope::Slide => "slide",
        }
    }
}

/// One text shape in a snapshot: its position, id, label and the plain text it held when read.
struct Piece {
    index: usize,
    shape: ShapeId,
    label: String,
    original: String,
}

struct Snapshot {
    document_id: u64,
    revision: u64,
    scope: Scope,
    slide_id: SlideId,
    slide_index: usize,
    skipped: usize,
    targets: Vec<Piece>,
}

impl Snapshot {
    fn to_json(&self) -> Value {
        json!({
            "document_id": self.document_id,
            "revision": self.revision,
            "scope": self.scope.name(),
            "slide_id": self.slide_id.0,
            "slide_index": self.slide_index,
            "skipped": self.skipped,
            "targets": self.targets.iter().map(|t| json!({
                "index": t.index,
                "shape": t.shape.0,
                "label": t.label,
                "original": t.original,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Text that `text`/`text.set` reproduces exactly: newlines, carriage returns and tabs only.
/// Other control characters (line breaks, field codes) and the object-replacement character are
/// refused.
fn plain_ok(text: &str) -> bool {
    text.chars().all(|c| matches!(c, '\n' | '\r' | '\t') || (!c.is_control() && c != '\u{FFFC}'))
}

/// The plain text of a top-level shape the assistant may rewrite: an autoshape or text box whose
/// runs are all text. `Ok(None)` for pictures, groups, tables, charts, fields, math, line breaks
/// and characters `text.set` wouldn't reproduce. The size is measured from the runs before any
/// text is built: a shape longer than `remaining` bytes is an error, never silently skipped.
fn plain_text(sh: &Shape, remaining: usize, cmd: &str) -> Result<Option<String>> {
    if !matches!(sh.kind, ShapeKind::Shape) {
        return Ok(None);
    }
    let Some(body) = &sh.text else { return Ok(Some(String::new())) };
    let runs = || body.paragraphs.iter().flat_map(|p| p.runs.iter());
    if !runs().all(|r| matches!(r.kind, RunKind::Text)) {
        return Ok(None);
    }
    let separators = body.paragraphs.len().saturating_sub(1);
    let size = runs().fold(separators, |n, r| n.saturating_add(r.text.len()));
    if size > remaining {
        return Err(bad(cmd, format!("a text shape holds more than {remaining} bytes of text; select fewer shapes")));
    }
    if !runs().all(|r| plain_ok(&r.text)) {
        return Ok(None);
    }
    Ok(Some(body.text()))
}

/// Empty layout placeholders aren't slide prose. Keep them selectable so an assistant can fill an
/// explicitly chosen placeholder, but omit them from a slide-wide context.
fn is_empty_placeholder(sh: &Shape) -> bool {
    sh.ph.is_some() && sh.text.as_ref().is_none_or(|body| body.paragraphs.iter().all(|p| p.runs.iter().all(|r| r.text.is_empty())))
}

/// At most [`MAX_LABEL_BYTES`] of the name, control characters as spaces, built as it goes so a
/// long name is never copied whole.
fn label_of(sh: &Shape) -> String {
    let mut out = String::new();
    for c in sh.name.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len().saturating_add(c.len_utf8()) > MAX_LABEL_BYTES {
            break;
        }
        out.push(c);
    }
    out
}

/// The first characters of a value a caller sent, for an error message.
fn short(s: &str) -> String {
    s.chars().take(32).collect()
}

fn push(targets: &mut Vec<Piece>, bytes: &mut usize, sh: &Shape, original: String, cmd: &str) -> Result<()> {
    if targets.len() >= MAX_TARGETS {
        return Err(bad(cmd, format!("more than {MAX_TARGETS} text shapes; select fewer")));
    }
    *bytes = bytes.saturating_add(original.len());
    if *bytes > MAX_TEXT_BYTES {
        return Err(bad(cmd, format!("more than {MAX_TEXT_BYTES} bytes of text; select fewer shapes")));
    }
    targets.push(Piece { index: targets.len(), shape: sh.id, label: label_of(sh), original });
    Ok(())
}

/// Reads the snapshot of the current slide. Refuses everything outside the slide's top-level
/// shapes: masters, layouts, notes, handouts, table cells and grouped objects.
fn read(s: &Session, scope: Scope, cmd: &str) -> Result<Snapshot> {
    let st = s.doc()?;
    if st.selection.target != Target::Slides {
        return Err(bad(cmd, "the assistant edits slide text only: not masters, layouts, notes or handout pages"));
    }
    if st.selection.cells.is_some() {
        return Err(bad(cmd, "table cells aren't supported yet"));
    }
    if let Some(t) = &st.selection.text
        && (t.notes || t.cell.is_some())
    {
        return Err(bad(cmd, "speaker notes and table cells aren't supported yet"));
    }
    let index = st.selection.slide;
    let slide = st.doc.slides.get(index).ok_or_else(|| bad(cmd, "no current slide"))?;
    let mut skipped = 0usize;
    let mut targets: Vec<Piece> = Vec::new();
    let mut bytes = 0usize;
    match scope {
        Scope::Selection => {
            let count = st.selection.shapes.len();
            if count > MAX_TARGETS {
                return Err(bad(cmd, format!("{count} shapes; the limit is {MAX_TARGETS}")));
            }
            let mut wanted: HashSet<ShapeId> = st.selection.shapes.iter().copied().collect();
            if let Some(t) = &st.selection.text {
                wanted.insert(t.shape);
            }
            if wanted.is_empty() {
                return Err(bad(cmd, "nothing is selected: select text shapes, or use scope \"slide\""));
            }
            if wanted.len() > MAX_TARGETS {
                return Err(bad(cmd, format!("{} shapes; the limit is {MAX_TARGETS}", wanted.len())));
            }
            for sh in slide.shapes.iter().filter(|sh| wanted.contains(&sh.id)) {
                let remaining = MAX_TEXT_BYTES.saturating_sub(bytes);
                let text = plain_text(sh, remaining, cmd)?.ok_or_else(|| {
                    bad(cmd, format!("shape {} isn't plain text (pictures, groups, tables, fields and line breaks aren't supported)", sh.id))
                })?;
                push(&mut targets, &mut bytes, sh, text, cmd)?;
            }
            if targets.len() != wanted.len() {
                return Err(bad(cmd, "a selected object isn't a top-level shape on the current slide (grouped objects aren't supported)"));
            }
        }
        Scope::Slide => {
            let candidates = slide.shapes.iter().filter(|sh| matches!(sh.kind, ShapeKind::Shape) && !is_empty_placeholder(sh)).count();
            if candidates > MAX_TARGETS {
                return Err(bad(cmd, format!("{candidates} text shapes on this slide; the limit is {MAX_TARGETS}")));
            }
            for sh in &slide.shapes {
                if is_empty_placeholder(sh) {
                    skipped += 1;
                    continue;
                }
                match plain_text(sh, MAX_TEXT_BYTES.saturating_sub(bytes), cmd)? {
                    Some(text) => push(&mut targets, &mut bytes, sh, text, cmd)?,
                    None => skipped += 1,
                }
            }
        }
    }
    Ok(Snapshot { document_id: st.uid, revision: st.revision, scope, slide_id: slide.id, slide_index: index, skipped, targets })
}

fn context(s: &mut Session, v: &Value) -> Result<Value> {
    let scope = Scope::parse(v.get("scope"), CMD_CONTEXT)?;
    Ok(read(s, scope, CMD_CONTEXT)?.to_json())
}

/// A bounded, shape-checked apply request. Nothing here reads the document yet.
struct Request {
    document_id: u64,
    revision: u64,
    slide_id: u32,
    slide_index: usize,
    targets: Vec<Piece>,
    changes: Vec<(usize, String)>,
}

fn object<'a>(v: &'a Value, what: &str, allowed: &[&str], cmd: &str) -> Result<&'a Map<String, Value>> {
    let m = v.as_object().ok_or_else(|| bad(cmd, format!("`{what}` must be an object")))?;
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(bad(cmd, format!("unknown field `{}` in `{what}`", short(k.as_str()))));
    }
    Ok(m)
}

fn uint(m: &Map<String, Value>, key: &str, cmd: &str) -> Result<u64> {
    m.get(key).and_then(Value::as_u64).ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number")))
}

fn text_field<'a>(m: &'a Map<String, Value>, key: &str, cmd: &str, max: usize) -> Result<&'a str> {
    let s = m.get(key).and_then(Value::as_str).ok_or_else(|| bad(cmd, format!("`{key}` must be a string")))?;
    if s.len() > max {
        return Err(bad(cmd, format!("`{key}` is {} bytes; the limit is {max}", s.len())));
    }
    Ok(s)
}

/// Checks the raw JSON shape and every cap before anything is copied out of it.
fn parse(v: &Value) -> Result<Request> {
    let cmd = CMD_APPLY;
    let top = object(v, "parameters", &["context", "changes"], cmd)?;
    let ctx = top.get("context").ok_or_else(|| bad(cmd, "missing `context`"))?;
    let c = object(ctx, "context", &["document_id", "revision", "scope", "slide_id", "slide_index", "skipped", "targets"], cmd)?;
    let document_id = uint(c, "document_id", cmd)?;
    let revision = uint(c, "revision", cmd)?;
    Scope::named(text_field(c, "scope", cmd, 16)?, cmd)?;
    let slide_id = u32::try_from(uint(c, "slide_id", cmd)?).map_err(|_| bad(cmd, "`slide_id` is out of range"))?;
    let slide_index = usize::try_from(uint(c, "slide_index", cmd)?).map_err(|_| bad(cmd, "`slide_index` is out of range"))?;
    if c.contains_key("skipped") {
        uint(c, "skipped", cmd)?;
    }
    let raw = c.get("targets").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`targets` must be an array"))?;
    if raw.len() > MAX_TARGETS {
        return Err(bad(cmd, format!("{} targets; the limit is {MAX_TARGETS}", raw.len())));
    }
    let mut targets = Vec::with_capacity(raw.len());
    let mut bytes = 0usize;
    for (i, t) in raw.iter().enumerate() {
        let m = object(t, "target", &["index", "shape", "label", "original"], cmd)?;
        let index = usize::try_from(uint(m, "index", cmd)?).map_err(|_| bad(cmd, format!("target {i}: `index` is out of range")))?;
        if index != i {
            return Err(bad(cmd, format!("target {i}: `index` must be {i}")));
        }
        let shape = u32::try_from(uint(m, "shape", cmd)?).map_err(|_| bad(cmd, format!("target {i}: `shape` is out of range")))?;
        let label = text_field(m, "label", cmd, MAX_LABEL_BYTES)?.to_string();
        let original = text_field(m, "original", cmd, MAX_TEXT_BYTES)?;
        bytes = bytes.saturating_add(original.len());
        if bytes > MAX_TEXT_BYTES {
            return Err(bad(cmd, format!("more than {MAX_TEXT_BYTES} bytes of original text")));
        }
        targets.push(Piece { index, shape: ShapeId(shape), label, original: original.to_string() });
    }
    let raw_changes = top.get("changes").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`changes` must be an array"))?;
    if raw_changes.len() > MAX_TARGETS {
        return Err(bad(cmd, format!("{} changes; the limit is {MAX_TARGETS}", raw_changes.len())));
    }
    let mut changes = Vec::with_capacity(raw_changes.len());
    let mut text_bytes = 0usize;
    for (n, ch) in raw_changes.iter().enumerate() {
        let m = object(ch, "change", &["index", "text", "markdown"], cmd)?;
        let index = usize::try_from(uint(m, "index", cmd)?).map_err(|_| bad(cmd, format!("change {n}: `index` is out of range")))?;
        match m.get("markdown") {
            None | Some(Value::Bool(false)) => {}
            Some(_) => return Err(bad(cmd, format!("change {n}: markdown isn't supported yet; send plain text"))),
        }
        let text = text_field(m, "text", cmd, MAX_TEXT_BYTES)?;
        text_bytes = text_bytes.saturating_add(text.len());
        if text_bytes > MAX_TEXT_BYTES {
            return Err(bad(cmd, format!("more than {MAX_TEXT_BYTES} bytes of replacement text")));
        }
        changes.push((index, text.to_string()));
    }
    Ok(Request { document_id, revision, slide_id, slide_index, targets, changes })
}

/// Every target must still be what the snapshot says, on the slide the snapshot names. A forged
/// target (a shape on another slide, a group child, a changed text) is refused before any edit.
fn check_targets(s: &Session, req: &Request) -> Result<()> {
    let st = s.doc()?;
    let slide = st
        .doc
        .slides
        .get(req.slide_index)
        .filter(|sl| sl.id.0 == req.slide_id)
        .ok_or_else(|| bad(CMD_APPLY, "the context's slide is gone or moved; read it again"))?;
    let mut seen = HashSet::new();
    for t in &req.targets {
        if !seen.insert(t.shape) {
            return Err(bad(CMD_APPLY, format!("target {}: shape {} appears twice", t.index, t.shape)));
        }
        let sh = slide
            .shapes
            .iter()
            .find(|sh| sh.id == t.shape)
            .ok_or_else(|| bad(CMD_APPLY, format!("target {}: shape {} isn't a top-level shape on the captured slide", t.index, t.shape)))?;
        // Read at most as much text as the snapshot holds: a longer current text is a change.
        match plain_text(sh, t.original.len(), CMD_APPLY) {
            Ok(Some(now)) if now == t.original => {}
            Ok(None) => return Err(bad(CMD_APPLY, format!("target {}: shape {} is no longer plain text", t.index, t.shape))),
            _ => return Err(bad(CMD_APPLY, format!("target {}: its text changed since the context was read; read it again", t.index))),
        }
    }
    Ok(())
}

/// The replacements that change something, each checked against its target. Unchanged text is
/// dropped, so an apply with nothing to change writes nothing.
fn plan(req: &Request) -> Result<Vec<(ShapeId, String)>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (index, text) in &req.changes {
        let t = req.targets.get(*index).ok_or_else(|| bad(CMD_APPLY, format!("change for target {index}: there is no such target")))?;
        if !seen.insert(*index) {
            return Err(bad(CMD_APPLY, format!("target {index} is changed twice")));
        }
        if !plain_ok(text) {
            return Err(bad(
                CMD_APPLY,
                format!("target {index}: the text has a control character (only newline, carriage return and tab are allowed)"),
            ));
        }
        if text != &t.original {
            out.push((t.shape, text.clone()));
        }
    }
    Ok(out)
}

/// Validates the whole request, then applies it through `text.set`. Validation runs before the
/// first edit. The nested calls are part of this command, so one undo step reverts all of them;
/// if one of them fails, the outermost command's guard puts the presentation back.
fn apply(s: &mut Session, v: &Value) -> Result<Value> {
    let req = parse(v)?;
    let (uid, revision) = {
        let st = s.doc()?;
        (st.uid, st.revision)
    };
    if req.document_id != uid || req.revision != revision {
        return Err(bad(CMD_APPLY, "stale context: the document changed since the context was read; read it again"));
    }
    check_targets(s, &req)?;
    let planned = plan(&req)?;
    if planned.is_empty() {
        return Ok(json!({"applied": 0, "document_id": uid, "revision": revision, "slide_id": req.slide_id}));
    }
    for (shape, text) in &planned {
        s.execute("text.set", &json!({"id": shape.0, "text": text}))?;
    }
    let st = s.doc()?;
    Ok(json!({"applied": planned.len(), "document_id": st.uid, "revision": st.revision, "slide_id": req.slide_id}))
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::is_empty_placeholder;
    use deckcraft_model::ShapeId;
    use serde_json::{Value, json};

    use crate::{Result, Session, Target};

    fn deck() -> Session {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
        s
    }

    fn text_box(s: &mut Session, text: &str) -> u32 {
        let id = s.execute("insert.textBox", &json!({"rect": [100, 100, 300, 60]})).unwrap()["id"].as_u64().unwrap();
        let id = u32::try_from(id).unwrap();
        s.execute("text.set", &json!({"id": id, "text": text})).unwrap();
        id
    }

    fn select(s: &mut Session, ids: &[u32]) {
        let selection = &mut s.doc_mut().unwrap().selection;
        selection.shapes = ids.iter().map(|i| ShapeId(*i)).collect();
        selection.text = None;
    }

    fn text_of(s: &Session, id: u32) -> String {
        s.doc().unwrap().shape(ShapeId(id)).unwrap().text.as_ref().unwrap().text()
    }

    fn text_of_at(s: &Session, slide_index: usize, id: u32) -> String {
        s.doc().unwrap().doc.slides.get(slide_index).unwrap().shapes.iter().find(|sh| sh.id == ShapeId(id)).unwrap().text.as_ref().unwrap().text()
    }

    fn snap(s: &mut Session, scope: &str) -> Value {
        s.execute("assistant.context", &json!({"scope": scope})).unwrap()
    }

    fn apply(s: &mut Session, ctx: &Value, changes: Value) -> Result<Value> {
        s.execute("assistant.apply", &json!({"context": ctx, "changes": changes}))
    }

    fn state(s: &Session) -> (u64, u64) {
        let st = s.doc().unwrap();
        (st.uid, st.revision)
    }

    #[test]
    fn slide_scope_lists_text_shapes_in_order() {
        let mut s = deck();
        let a = text_box(&mut s, "Alpha");
        let b = text_box(&mut s, "Beta\nSecond");
        let slide_index = s.doc().unwrap().selection.slide;
        let ctx = snap(&mut s, "slide");
        let (uid, rev) = state(&s);
        assert_eq!(ctx["document_id"], json!(uid));
        assert_eq!(ctx["revision"], json!(rev));
        assert_eq!(ctx["scope"], "slide");
        assert_eq!(ctx["slide_index"], slide_index);
        assert_eq!(ctx["skipped"], 0);
        let targets = ctx["targets"].as_array().unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0]["index"], 0);
        assert_eq!(targets[0]["shape"], json!(a));
        assert_eq!(targets[0]["original"], "Alpha");
        assert_eq!(targets[1]["shape"], json!(b));
        assert_eq!(targets[1]["original"], "Beta\nSecond");
        assert!(targets[0]["label"].as_str().is_some_and(|l| l.len() <= 64));
    }

    #[test]
    fn slide_scope_skips_empty_placeholders_but_selected_placeholders_remain_targets() {
        let mut s = Session::with_new();
        let empty_placeholders: Vec<ShapeId> = s.doc().unwrap().shapes().iter().filter(|sh| is_empty_placeholder(sh)).map(|sh| sh.id).collect();
        assert!(!empty_placeholders.is_empty());

        let ctx = snap(&mut s, "slide");
        assert_eq!(ctx["targets"].as_array().unwrap().len(), 0);
        assert_eq!(ctx["skipped"], json!(empty_placeholders.len()));

        s.doc_mut().unwrap().selection.shapes = vec![empty_placeholders[0]];
        let selected = snap(&mut s, "selection");
        assert_eq!(selected["targets"].as_array().unwrap().len(), 1);
        assert_eq!(selected["targets"][0]["shape"], json!(empty_placeholders[0].0));
        assert_eq!(selected["targets"][0]["original"], "");
    }

    #[test]
    fn selection_is_the_default_scope_and_needs_a_selection() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let b = text_box(&mut s, "Two");
        s.doc_mut().unwrap().selection.shapes.clear();
        let text_only = s.execute("assistant.context", &json!({})).unwrap();
        assert_eq!(text_only["scope"], "selection");
        assert_eq!(text_only["targets"].as_array().unwrap().len(), 1);
        assert_eq!(text_only["targets"][0]["shape"], json!(b));
        assert_eq!(text_only["targets"][0]["original"], "Two");
        s.doc_mut().unwrap().selection.text = None;
        assert!(s.execute("assistant.context", &json!({})).is_err());
        select(&mut s, &[a]);
        let ctx = s.execute("assistant.context", &json!({})).unwrap();
        assert_eq!(ctx["scope"], "selection");
        assert_eq!(ctx["targets"].as_array().unwrap().len(), 1);
        assert_eq!(ctx["targets"][0]["original"], "One");
    }

    #[test]
    fn unknown_scope_and_non_slide_targets_are_refused() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        select(&mut s, &[a]);
        assert!(s.execute("assistant.context", &json!({"scope": "document"})).is_err());
        assert!(s.execute("assistant.context", &json!({"scope": 3})).is_err());
        s.doc_mut().unwrap().selection.target = Target::Handout;
        assert!(s.execute("assistant.context", &json!({"scope": "slide"})).is_err());
        s.doc_mut().unwrap().selection.target = Target::Slides;
        s.execute("text.edit", &json!({"id": a, "notes": true})).unwrap();
        assert!(s.execute("assistant.context", &json!({"scope": "selection"})).is_err());
    }

    #[test]
    fn caps_refuse_too_many_shapes_and_too_much_text() {
        let mut s = deck();
        text_box(&mut s, &"a".repeat(65_537));
        assert!(s.execute("assistant.context", &json!({"scope": "slide"})).is_err());
        let mut s = deck();
        for _ in 0..257 {
            s.execute("insert.textBox", &json!({"rect": [10, 10, 50, 20]})).unwrap();
        }
        assert!(s.execute("assistant.context", &json!({"scope": "slide"})).is_err());
    }

    #[test]
    fn raw_target_count_is_refused_before_anything_else() {
        let mut s = deck();
        text_box(&mut s, "One");
        let targets: Vec<Value> = (0..257).map(|i| json!({"index": i, "shape": 1, "label": "x", "original": "a"})).collect();
        let ctx = json!({"document_id": 1, "revision": 1, "scope": "slide", "slide_id": 1, "slide_index": 0, "targets": targets});
        assert!(apply(&mut s, &ctx, json!([])).is_err());
    }

    #[test]
    fn stale_document_revision_and_original_are_refused() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let ctx = snap(&mut s, "slide");
        let mut forged_uid = ctx.clone();
        forged_uid["document_id"] = json!(ctx["document_id"].as_u64().unwrap() + 1);
        assert!(apply(&mut s, &forged_uid, json!([{"index": 0, "text": "X"}])).is_err());
        let mut forged_text = ctx.clone();
        forged_text["targets"][0]["original"] = json!("Other");
        assert!(apply(&mut s, &forged_text, json!([{"index": 0, "text": "X"}])).is_err());
        s.execute("text.set", &json!({"id": a, "text": "Changed"})).unwrap();
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X"}])).is_err());
        assert_eq!(text_of(&s, a), "Changed");
    }

    #[test]
    fn forged_target_outside_the_captured_slide_is_refused() {
        let mut s = deck();
        let one = text_box(&mut s, "One");
        let one_slide = s.doc().unwrap().selection.slide;
        s.execute("slide.new", &json!({"layout": "blank"})).unwrap();
        let two = text_box(&mut s, "Two");
        let two_slide = s.doc().unwrap().selection.slide;
        s.doc_mut().unwrap().selection.slide = one_slide;
        let mut ctx = snap(&mut s, "slide");
        assert_eq!(ctx["targets"][0]["shape"], json!(one));
        ctx["targets"][0]["shape"] = json!(two);
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X"}])).is_err());
        assert_eq!(text_of_at(&s, two_slide, two), "Two");
        assert_eq!(text_of_at(&s, one_slide, one), "One");
    }

    #[test]
    fn bad_indices_and_markdown_mutate_nothing() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let b = text_box(&mut s, "Two");
        let ctx = snap(&mut s, "slide");
        let before = state(&s);
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X"}, {"index": 0, "text": "Y"}])).is_err());
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X"}, {"index": 5, "text": "Y"}])).is_err());
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X", "markdown": true}])).is_err());
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "X", "bogus": 1}])).is_err());
        assert_eq!(state(&s), before);
        assert_eq!(text_of(&s, a), "One");
        assert_eq!(text_of(&s, b), "Two");
    }

    #[test]
    fn control_characters_are_refused_and_safe_whitespace_kept() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let ctx = snap(&mut s, "slide");
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "a\u{1}b"}])).is_err());
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "a\u{FFFC}b"}])).is_err());
        assert!(apply(&mut s, &ctx, json!([{"index": 0, "text": "a\u{b}b"}])).is_err());
        assert_eq!(text_of(&s, a), "One");
        apply(&mut s, &ctx, json!([{"index": 0, "text": "x\ty\nz"}])).unwrap();
        assert_eq!(text_of(&s, a), "x\ty\nz");
    }

    #[test]
    fn apply_is_one_undo_step_and_redo_restores_it() {
        let mut s = deck();
        let a = text_box(&mut s, "A");
        let b = text_box(&mut s, "B");
        let ctx = snap(&mut s, "slide");
        let out = apply(&mut s, &ctx, json!([{"index": 0, "text": "A2"}, {"index": 1, "text": "B2"}])).unwrap();
        assert_eq!(out["applied"], 2);
        assert_eq!(text_of(&s, a), "A2");
        assert_eq!(text_of(&s, b), "B2");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(text_of(&s, a), "A");
        assert_eq!(text_of(&s, b), "B");
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(text_of(&s, a), "A2");
        assert_eq!(text_of(&s, b), "B2");
    }

    #[test]
    fn empty_or_unchanged_changes_write_nothing() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let ctx = snap(&mut s, "slide");
        let before = state(&s);
        let out = apply(&mut s, &ctx, json!([])).unwrap();
        assert_eq!(out["applied"], 0);
        let out = apply(&mut s, &ctx, json!([{"index": 0, "text": "One"}])).unwrap();
        assert_eq!(out["applied"], 0);
        assert_eq!(state(&s), before);
        assert_eq!(text_of(&s, a), "One");
    }

    #[test]
    fn paragraph_formatting_survives_an_apply() {
        let mut s = deck();
        let a = text_box(&mut s, "One\nTwo");
        let slide_index = s.doc().unwrap().selection.slide;
        s.edit(|doc, _| {
            let slide = Arc::make_mut(doc.slides.get_mut(slide_index).unwrap());
            let shape = slide.shapes.iter_mut().find(|sh| sh.id == ShapeId(a)).unwrap();
            let body = shape.text.get_or_insert_with(Default::default);
            body.paragraphs[0].runs[0].props.bold = Some(true);
            body.paragraphs[1].level = 1;
            Ok(())
        })
        .unwrap();
        let ctx = snap(&mut s, "slide");
        apply(&mut s, &ctx, json!([{"index": 0, "text": "Uno\nDos"}])).unwrap();
        let body = s.doc().unwrap().shape(ShapeId(a)).unwrap().text.clone().unwrap();
        assert_eq!(body.text(), "Uno\nDos");
        assert_eq!(body.paragraphs[0].runs[0].props.bold, Some(true));
        assert_eq!(body.paragraphs[1].level, 1);
    }

    #[test]
    fn selection_scope_applies_only_to_selected_shapes() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let b = text_box(&mut s, "Two");
        select(&mut s, &[b]);
        let ctx = snap(&mut s, "selection");
        apply(&mut s, &ctx, json!([{"index": 0, "text": "Deux"}])).unwrap();
        assert_eq!(text_of(&s, a), "One");
        assert_eq!(text_of(&s, b), "Deux");
    }

    #[test]
    fn hostile_parameters_never_panic() {
        let mut s = deck();
        text_box(&mut s, "One");
        for bad in [json!(null), json!(5), json!({"scope": 5}), json!({"scope": "slide", "x": 1}), json!({"scope": "slide", "text": [1]})] {
            let _ = s.execute("assistant.context", &bad);
        }
        let ctx = snap(&mut s, "slide");
        for changes in [json!(null), json!("x"), json!([null]), json!([{"index": -1, "text": "x"}]), json!([{"index": 1e30, "text": "x"}])] {
            assert!(apply(&mut s, &ctx, changes).is_err());
        }
        assert!(s.execute("assistant.apply", &json!({})).is_err());
        assert!(s.execute("assistant.apply", &json!({"context": 5, "changes": []})).is_err());
        assert!(s.execute("assistant.apply", &json!({"context": {"targets": "x"}, "changes": []})).is_err());
    }

    #[test]
    fn oversized_name_is_cut_to_the_label_limit() {
        let mut s = deck();
        let a = text_box(&mut s, "One");
        let long = format!("{}é{}", "n".repeat(63), "x".repeat(10_000));
        let slide_index = s.doc().unwrap().selection.slide;
        s.edit(|doc, _| {
            let slide = Arc::make_mut(doc.slides.get_mut(slide_index).unwrap());
            slide.shapes.iter_mut().find(|sh| sh.id == ShapeId(a)).unwrap().name = long.clone();
            Ok(())
        })
        .unwrap();
        let ctx = snap(&mut s, "slide");
        let label = ctx["targets"][0]["label"].as_str().unwrap();
        assert!(label.len() <= 64);
        assert_eq!(label, "n".repeat(63));
    }

    #[test]
    fn byte_sum_boundary_counts_separators_exactly() {
        let mut s = deck();
        text_box(&mut s, &format!("{}\n{}", "a".repeat(32_768), "b".repeat(32_767)));
        let ctx = snap(&mut s, "slide");
        assert_eq!(ctx["targets"][0]["original"].as_str().unwrap().len(), 65_536);

        let mut s = deck();
        text_box(&mut s, &format!("{}\n{}", "a".repeat(32_768), "b".repeat(32_768)));
        assert!(s.execute("assistant.context", &json!({"scope": "slide"})).is_err());
    }

    #[test]
    fn oversized_original_is_refused_before_it_is_copied() {
        let mut s = deck();
        text_box(&mut s, "One");
        let ctx = json!({"document_id": 1, "revision": 1, "scope": "slide", "slide_id": 1, "slide_index": 0,
            "targets": [{"index": 0, "shape": 1, "label": "x", "original": "a".repeat(65_537)}]});
        assert!(apply(&mut s, &ctx, json!([])).is_err());
    }

    #[test]
    fn selection_over_the_cap_is_refused_before_it_is_collected() {
        let mut s = deck();
        let ids: Vec<u32> = (1..=257).collect();
        select(&mut s, &ids);
        assert!(s.execute("assistant.context", &json!({"scope": "selection"})).is_err());
    }
}
