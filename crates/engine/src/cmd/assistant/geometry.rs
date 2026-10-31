//! Geometry and fill slice of the Mirai slide proposals. `assistant.geometryContext` reads a bounded,
//! revision-checked list of the top-level autoshapes on the current slide that carry no text, are not
//! placeholders, are not locked and have a finite box. Their indices are a separate space from the
//! text targets of `assistant.context`. [`parse`] and [`check`] validate a geometry snapshot before any
//! edit; `assistant.applyDeck` then moves, fills or creates shapes through `shape.setBounds`,
//! `shape.fill` and `shape.insert`, so the outer command's guard rolls back a failed proposal and one
//! undo step reverts it.

use std::collections::HashSet;

use deckcraft_model::text::RunKind;
use deckcraft_model::{ColorBase, Fill, Shape, ShapeId, ShapeKind, SlideId};
use serde_json::{Value, json};

use super::{MAX_LABEL_BYTES, MAX_SLIDES, MAX_TARGETS, is_hex_color, label_of, object, short, text_field, uint};
use crate::cmd::bad;
use crate::{Result, Session, Target};

const CMD_CONTEXT: &str = "assistant.geometryContext";
const CMD_PRESENTATION_CONTEXT: &str = "assistant.presentationGeometryContext";
/// Longest preset name a geometry entry or an `add_shape` operation carries.
pub const MAX_PRESET_BYTES: usize = 64;

/// A shape the snapshot lists: its index, id, label, preset, box and plain fill colour. A
/// presentation-wide entry also carries the slide it was read from.
struct Entry {
    index: usize,
    slide_index: usize,
    slide_id: SlideId,
    shape: ShapeId,
    label: String,
    preset: String,
    rect: [f64; 4],
    fill: Option<String>,
}

impl Entry {
    fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "slide_index": self.slide_index,
            "slide_id": self.slide_id.0,
            "shape": self.shape.0,
            "label": self.label,
            "preset": self.preset,
            "rect": self.rect,
            "fill": self.fill,
        })
    }
}

pub(super) struct Snapshot {
    document_id: u64,
    revision: u64,
    scope: &'static str,
    slide_index: usize,
    slide_id: SlideId,
    skipped: usize,
    shapes: Vec<Entry>,
}

impl Snapshot {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "document_id": self.document_id,
            "revision": self.revision,
            "scope": self.scope,
            "slide_index": self.slide_index,
            "slide_id": self.slide_id.0,
            "skipped": self.skipped,
            "shapes": self.shapes.iter().map(Entry::to_json).collect::<Vec<_>>(),
        })
    }
}

/// One geometry entry a proposal names: only what the checks need. Its box and fill were validated
/// on the way in; the current shape is read again before any edit. `slide_index`/`slide_id` link
/// the entry to the slide it was captured from; a single-slide snapshot leaves them at the
/// snapshot's own slide.
pub(super) struct Claim {
    index: usize,
    slide_index: usize,
    slide_id: SlideId,
    shape: ShapeId,
    preset: String,
}

/// A parsed geometry snapshot sent back with a deck proposal.
pub(super) struct Request {
    document_id: u64,
    revision: u64,
    slide_index: usize,
    slide_id: u32,
    pub(super) shapes: Vec<Claim>,
}

/// A shape with no text of its own: an autoshape whose preset is the whole of it. Pictures, groups,
/// connectors, tables, charts, media, ink, placeholders and text boxes are not geometry here.
fn text_free(sh: &Shape) -> bool {
    sh.text.as_ref().is_none_or(|body| {
        body.paragraphs.len() <= 1 && body.paragraphs.iter().all(|p| p.runs.iter().all(|r| r.text.is_empty() && matches!(r.kind, RunKind::Text)))
    })
}

fn is_geometry(sh: &Shape) -> bool {
    matches!(sh.kind, ShapeKind::Shape) && sh.ph.is_none() && !sh.text_box && sh.geom.preset_name().is_some() && text_free(sh)
}

/// The box of a shape, if it has an explicit one and every number is finite.
fn rect_of(sh: &Shape) -> Option<[f64; 4]> {
    let x = sh.xfrm?;
    let rect = [x.x, x.y, x.w, x.h];
    rect.iter().all(|v| v.is_finite()).then_some(rect)
}

/// A preset name the protocol carries: letters and digits only, capped. A deck may name anything
/// (`my-shape`); such a shape isn't offered to the model.
fn plain_preset(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_PRESET_BYTES && name.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Whether the snapshot lists the shape: geometry with a plain preset name (closed shapes, lines,
/// arcs and connector presets alike), unlocked, with a finite box of non-negative size.
fn listed(sh: &Shape) -> bool {
    is_geometry(sh) && !sh.locked && rect_of(sh).is_some_and(|r| r[2] >= 0.0 && r[3] >= 0.0) && sh.geom.preset_name().is_some_and(plain_preset)
}

/// `#RRGGBB` of a plain solid fill; `None` for inherited, theme, gradient or picture fills.
fn fill_hex(sh: &Shape) -> Option<String> {
    let Some(Fill::Solid { color }) = &sh.fill else { return None };
    if !color.mods.is_empty() {
        return None;
    }
    let ColorBase::Rgb { rgb } = &color.base else { return None };
    Some(format!("#{:02X}{:02X}{:02X}", rgb.r, rgb.g, rgb.b))
}

fn slide_selection(s: &Session, cmd: &str) -> Result<()> {
    let st = s.doc()?;
    if st.selection.target != Target::Slides
        || st.selection.cells.is_some()
        || st.selection.text.as_ref().is_some_and(|t| t.notes || t.cell.is_some())
    {
        return Err(bad(cmd, "the assistant edits slides only: not masters, layouts, notes, handout pages or table cells"));
    }
    Ok(())
}

fn read(s: &Session, cmd: &str) -> Result<Snapshot> {
    slide_selection(s, cmd)?;
    let st = s.doc()?;
    let index = st.selection.slide;
    let slide = st.doc.slides.get(index).ok_or_else(|| bad(cmd, "no current slide"))?;
    // Past the cap the rest count as `skipped`, so a crowded slide still gets its text edits.
    let mut shapes = Vec::new();
    for sh in slide.shapes.iter().filter(|sh| listed(sh)).take(MAX_TARGETS) {
        let (Some(rect), Some(preset)) = (rect_of(sh), sh.geom.preset_name()) else { continue };
        shapes.push(Entry {
            index: shapes.len(),
            slide_index: index,
            slide_id: slide.id,
            shape: sh.id,
            label: label_of(sh),
            preset: preset.to_owned(),
            rect,
            fill: fill_hex(sh),
        });
    }
    let skipped = slide.shapes.len().saturating_sub(shapes.len());
    Ok(Snapshot { document_id: st.uid, revision: st.content_revision, scope: "slide", slide_index: index, slide_id: slide.id, skipped, shapes })
}

/// `assistant.geometryContext`: the listed shapes of the current slide, read-only.
pub(super) fn context(s: &mut Session, _v: &Value) -> Result<Value> {
    Ok(read(s, CMD_CONTEXT)?.to_json())
}

/// Reads the listed shapes of every slide, in slide then shape order. Each entry carries the
/// slide it was read from, so a proposal can not move a shape onto another slide's geometry.
pub(super) fn read_presentation(s: &Session, cmd: &str) -> Result<Snapshot> {
    slide_selection(s, cmd)?;
    let st = s.doc()?;
    if st.doc.slides.len() > MAX_SLIDES {
        return Err(bad(cmd, format!("more than {MAX_SLIDES} slides; the limit is {MAX_SLIDES}")));
    }
    let current = st.selection.slide;
    let current_id = st.doc.slides.get(current).map(|sl| sl.id).ok_or_else(|| bad(cmd, "no current slide"))?;
    let mut shapes = Vec::new();
    let mut skipped = 0usize;
    for (slide_index, slide) in st.doc.slides.iter().enumerate() {
        for sh in slide.shapes.iter().filter(|sh| listed(sh)) {
            if shapes.len() >= MAX_TARGETS {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let (Some(rect), Some(preset)) = (rect_of(sh), sh.geom.preset_name()) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            shapes.push(Entry {
                index: shapes.len(),
                slide_index,
                slide_id: slide.id,
                shape: sh.id,
                label: label_of(sh),
                preset: preset.to_owned(),
                rect,
                fill: fill_hex(sh),
            });
        }
    }
    Ok(Snapshot {
        document_id: st.uid,
        revision: st.content_revision,
        scope: "presentation",
        slide_index: current,
        slide_id: current_id,
        skipped,
        shapes,
    })
}

/// `assistant.presentationGeometryContext`: the listed shapes of every slide, read-only.
pub(super) fn presentation_context(s: &mut Session, _v: &Value) -> Result<Value> {
    Ok(read_presentation(s, CMD_PRESENTATION_CONTEXT)?.to_json())
}

/// `[x, y, w, h]` of finite numbers.
fn plain_rect(v: Option<&Value>) -> bool {
    v.and_then(Value::as_array).is_some_and(|r| r.len() == 4 && r.iter().all(|n| n.as_f64().is_some_and(f64::is_finite)))
}

/// Checks the raw JSON shape of a geometry snapshot and its caps before anything is copied out.
/// Entries may carry the slide they were captured from (`slide_index`, `slide_id`); without them
/// the snapshot's own slide is used, which is what a single-slide snapshot says.
pub(super) fn parse(v: &Value, cmd: &str) -> Result<Request> {
    let m = object(v, "geometry", &["document_id", "revision", "scope", "slide_index", "slide_id", "skipped", "shapes"], cmd)?;
    let document_id = uint(m, "document_id", cmd)?;
    let revision = uint(m, "revision", cmd)?;
    if m.contains_key("scope") {
        let scope = text_field(m, "scope", cmd, 16)?;
        if !matches!(scope, "slide" | "presentation") {
            return Err(bad(cmd, format!("unknown `scope` {:?}: slide or presentation", short(scope))));
        }
    }
    let slide_index = usize::try_from(uint(m, "slide_index", cmd)?).map_err(|_| bad(cmd, "`slide_index` is out of range"))?;
    let slide_id = u32::try_from(uint(m, "slide_id", cmd)?).map_err(|_| bad(cmd, "`slide_id` is out of range"))?;
    if m.contains_key("skipped") {
        uint(m, "skipped", cmd)?;
    }
    let raw = m.get("shapes").and_then(Value::as_array).ok_or_else(|| bad(cmd, "`shapes` must be an array"))?;
    if raw.len() > MAX_TARGETS {
        return Err(bad(cmd, format!("{} geometry shapes; the limit is {MAX_TARGETS}", raw.len())));
    }
    let mut shapes = Vec::with_capacity(raw.len());
    for (i, e) in raw.iter().enumerate() {
        let e = object(e, "geometry entry", &["index", "slide_index", "slide_id", "shape", "label", "preset", "rect", "fill"], cmd)?;
        let index = usize::try_from(uint(e, "index", cmd)?).map_err(|_| bad(cmd, format!("geometry {i}: `index` is out of range")))?;
        if index != i {
            return Err(bad(cmd, format!("geometry {i}: `index` must be {i}")));
        }
        let entry_slide_index = match e.get("slide_index") {
            None | Some(Value::Null) => slide_index,
            Some(_) => usize::try_from(uint(e, "slide_index", cmd)?).map_err(|_| bad(cmd, format!("geometry {i}: `slide_index` is out of range")))?,
        };
        let entry_slide_id = match e.get("slide_id") {
            None | Some(Value::Null) => slide_id,
            Some(_) => u32::try_from(uint(e, "slide_id", cmd)?).map_err(|_| bad(cmd, format!("geometry {i}: `slide_id` is out of range")))?,
        };
        let shape = u32::try_from(uint(e, "shape", cmd)?).map_err(|_| bad(cmd, format!("geometry {i}: `shape` is out of range")))?;
        text_field(e, "label", cmd, MAX_LABEL_BYTES)?;
        let preset = text_field(e, "preset", cmd, MAX_PRESET_BYTES)?;
        if !plain_rect(e.get("rect")) {
            return Err(bad(cmd, format!("geometry {i}: `rect` must be four finite numbers")));
        }
        match e.get("fill") {
            Some(Value::Null) => {}
            Some(Value::String(color)) if is_hex_color(color) => {}
            _ => return Err(bad(cmd, format!("geometry {i}: `fill` must be #RRGGBB or null"))),
        }
        shapes.push(Claim {
            index,
            slide_index: entry_slide_index,
            slide_id: SlideId(entry_slide_id),
            shape: ShapeId(shape),
            preset: preset.to_owned(),
        });
    }
    Ok(Request { document_id, revision, slide_index, slide_id, shapes })
}

/// Every snapshot entry must still be what it says, on the slide it names: same document and
/// revision, unlocked, text-free geometry with the same preset. Runs before the first edit.
pub(super) fn check(s: &Session, req: &Request, cmd: &str) -> Result<()> {
    let st = s.doc()?;
    if st.uid != req.document_id || st.content_revision != req.revision {
        return Err(bad(cmd, "stale geometry: the document changed since it was read; read it again"));
    }
    let mut seen = HashSet::new();
    for c in &req.shapes {
        if !seen.insert(c.shape) {
            return Err(bad(cmd, format!("geometry {}: shape {} appears twice", c.index, c.shape)));
        }
        let slide = st
            .doc
            .slides
            .get(c.slide_index)
            .filter(|sl| sl.id == c.slide_id)
            .ok_or_else(|| bad(cmd, format!("geometry {}: the slide it was captured from is gone or moved; read it again", c.index)))?;
        let sh = slide
            .shapes
            .iter()
            .find(|sh| sh.id == c.shape)
            .ok_or_else(|| bad(cmd, format!("geometry {}: shape {} isn't a top-level shape on the captured slide", c.index, c.shape)))?;
        if !listed(sh) || sh.geom.preset_name() != Some(c.preset.as_str()) {
            return Err(bad(cmd, format!("geometry {}: shape {} is no longer unlocked geometry without text; read it again", c.index, c.shape)));
        }
    }
    Ok(())
}

/// The preset of the shape a geometry index names, as the snapshot recorded it.
pub(super) fn preset(geometry: Option<&Request>, index: usize) -> Option<&str> {
    geometry?.shapes.get(index).map(|c| c.preset.as_str())
}

/// The slide a geometry snapshot was read from.
pub(super) fn slide_index(geometry: &Request) -> usize {
    geometry.slide_index
}

/// The slide id a geometry snapshot was read from.
pub(super) fn slide_id(geometry: &Request) -> u32 {
    geometry.slide_id
}

/// The shape a geometry index names, from a snapshot that passed [`check`].
pub(super) fn target(geometry: Option<&Request>, index: usize, cmd: &str) -> Result<ShapeId> {
    let geometry = geometry.ok_or_else(|| bad(cmd, "shape operations need the geometry from assistant.geometryContext"))?;
    geometry.shapes.get(index).map(|c| c.shape).ok_or_else(|| bad(cmd, "shape outside the geometry context"))
}

/// The slide and shape a geometry index names, from a snapshot that passed [`check`]. The slide is
/// the one the entry was captured from, so a presentation-wide proposal edits each shape where it
/// lives.
pub(super) fn claim(geometry: Option<&Request>, index: usize, cmd: &str) -> Result<(usize, ShapeId)> {
    let geometry = geometry.ok_or_else(|| bad(cmd, "shape operations need the geometry from assistant.geometryContext"))?;
    geometry.shapes.get(index).map(|c| (c.slide_index, c.shape)).ok_or_else(|| bad(cmd, "shape outside the geometry context"))
}

#[cfg(test)]
mod tests {
    use super::{fill_hex, is_geometry, listed};
    use deckcraft_color::Rgba;
    use deckcraft_model::text::TextBody;
    use deckcraft_model::{ColorRef, Fill, Shape, ShapeKind};

    #[test]
    fn text_and_special_shapes_are_not_geometry() {
        let plain = Shape { kind: ShapeKind::Shape, ..Default::default() };
        assert!(is_geometry(&plain));
        let worded = Shape { text: Some(TextBody::from_text("Hi")), ..Default::default() };
        assert!(!is_geometry(&worded));
        let boxed = Shape { text_box: true, ..Default::default() };
        assert!(!is_geometry(&boxed));
        let connector = Shape { kind: ShapeKind::Connector { start: None, end: None }, ..Default::default() };
        assert!(!is_geometry(&connector));
    }

    #[test]
    fn only_plain_solid_fills_are_reported() {
        let red = Shape { fill: Some(Fill::solid(ColorRef::rgb(Rgba::rgb(255, 0, 16)))), ..Default::default() };
        assert_eq!(fill_hex(&red).as_deref(), Some("#FF0010"));
        let inherited = Shape::default();
        assert_eq!(fill_hex(&inherited), None);
    }

    #[test]
    fn locked_or_unboxed_shapes_are_not_listed() {
        let mut sh = Shape::default();
        assert!(!listed(&sh), "no explicit box");
        sh.xfrm = Some(deckcraft_geom::Xfrm::new(1.0, 2.0, 3.0, 4.0));
        assert!(listed(&sh));
        sh.locked = true;
        assert!(!listed(&sh));
    }
}
