//! `deckcraft-cli mirai`: asks the Mirai Assistant for text changes to one slide and prints them as
//! JSON. Preview only: nothing is applied and nothing is saved.

use std::sync::atomic::AtomicBool;

use deckcraft_assistant::{Client, MAX_INSTRUCTION_BYTES, Provider, Request, Response};
use deckcraft_engine::Session;
use serde_json::{Value, json};

const NAME: &str = "Mirai Assistant";
/// Target shapes with a vertical tab (U+000B, a line break inside a shape) become newlines for the model.
const SOFT_BREAK: char = '\u{000B}';

#[derive(Debug, PartialEq, Eq)]
struct Options {
    input: String,
    instruction: String,
    model: Option<String>,
    /// 0-based slide index; `None` keeps the slide that is current after opening.
    slide: Option<usize>,
}

fn once<'a>(slot: &mut Option<&'a str>, flag: &str, value: Option<&'a String>) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("mirai: {flag} is given twice"));
    }
    *slot = Some(value.ok_or_else(|| format!("mirai: {flag} needs a value"))?.as_str());
    Ok(())
}

/// Reads and checks the arguments before any file is opened or any process is started.
fn parse_args(args: &[String]) -> Result<Options, String> {
    let (mut input, mut instruction, mut model, mut slide) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--in" => once(&mut input, "--in", it.next())?,
            "--instruction" => once(&mut instruction, "--instruction", it.next())?,
            "--model" => once(&mut model, "--model", it.next())?,
            "--slide" => {
                if slide.is_some() {
                    return Err("mirai: --slide is given twice".into());
                }
                let n = it.next().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n >= 1).ok_or("mirai: --slide needs a slide number from 1")?;
                slide = Some(n - 1);
            }
            other => return Err(format!("mirai: unknown argument `{other}`")),
        }
    }
    let input = input.ok_or("mirai needs --in FILE")?;
    let instruction = instruction.ok_or("mirai needs --instruction TEXT")?;
    let instruction = instruction.trim();
    if instruction.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!("mirai: the instruction is longer than {MAX_INSTRUCTION_BYTES} bytes"));
    }
    Request { instruction: instruction.to_owned(), targets: Vec::new(), context: None }.validate().map_err(|e| format!("mirai: {e}"))?;
    let model = deckcraft_assistant::model_name(model).map_err(|e| format!("mirai: {e}"))?;
    Ok(Options { input: input.to_owned(), instruction: instruction.to_owned(), model, slide })
}

/// The assistant request for `snapshot` (from `assistant.context`). The snapshot itself stays as
/// it is; only the text sent to the model is normalized.
fn request_from_snapshot(instruction: &str, snapshot: &Value) -> Result<Request, String> {
    let rows = snapshot.get("targets").and_then(Value::as_array).ok_or("mirai: the slide snapshot has no targets")?;
    if rows.is_empty() {
        return Err("mirai: the slide has no text shapes to change".into());
    }
    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let text = row.get("text").and_then(Value::as_str).ok_or("mirai: a target's text is not a string")?;
        targets.push(text.replace(SOFT_BREAK, "\n"));
    }
    Ok(Request { instruction: instruction.to_owned(), targets, context: None })
}

/// The printed preview: the snapshot the changes refer to, the changes by target index, and the explanation.
/// Nothing is applied: `preview` and `applied` say so, and `note` says the snapshot is tied to this session.
fn preview(snapshot: Value, response: &Response) -> Value {
    let changes: Vec<Value> = response.suggestions.iter().map(|s| json!({"index": s.index, "text": s.text})).collect();
    json!({
        "assistant": NAME,
        "preview": true,
        "applied": false,
        "note": "The snapshot belongs to the live session that created it and cannot be reapplied after reopening the file in a separate process.",
        "context": snapshot,
        "changes": changes,
        "explanation": response.explanation,
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let opts = parse_args(args)?;
    let client = Client::discover().map_err(|e| format!("mirai: {e}"))?.with_model(opts.model.as_deref()).map_err(|e| format!("mirai: {e}"))?;
    let mut s = Session::new();
    crate::open(&mut s, Some(opts.input.as_str()), false)?;
    if let Some(index) = opts.slide {
        s.execute("slide.go", &json!({"index": index})).map_err(|e| e.to_string())?;
    }
    let snapshot = s.execute("assistant.context", &json!({})).map_err(|e| e.to_string())?;
    let request = request_from_snapshot(&opts.instruction, &snapshot)?;
    let cancel = AtomicBool::new(false);
    let response = client.generate(&request, &cancel).map_err(|e| format!("mirai: {e}"))?;
    let text = serde_json::to_string_pretty(&preview(snapshot, &response)).map_err(|e| format!("mirai: {e}"))?;
    outln!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use deckcraft_assistant::Suggestion;

    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_required_and_optional_flags() {
        let opts = parse_args(&args(&["--in", "deck.deckcraft", "--instruction", "Shorter", "--model", "sonnet", "--slide", "3"])).unwrap();
        assert_eq!(opts, Options { input: "deck.deckcraft".into(), instruction: "Shorter".into(), model: Some("sonnet".into()), slide: Some(2) });
        let plain = parse_args(&args(&["--in", "a.pptx", "--instruction", "Fix"])).unwrap();
        assert_eq!(plain.slide, None);
        assert_eq!(plain.model, None);
    }

    #[test]
    fn rejects_bad_arguments_before_anything_is_opened() {
        assert!(parse_args(&args(&["--instruction", "x"])).unwrap_err().contains("--in"));
        assert!(parse_args(&args(&["--in", "a.pptx"])).unwrap_err().contains("--instruction"));
        assert!(parse_args(&args(&["--in", "a", "--instruction", "x", "--slide", "0"])).is_err());
        assert!(parse_args(&args(&["--in", "a", "--instruction", "x", "--slide", "two"])).is_err());
        assert!(parse_args(&args(&["--in", "a", "--instruction", "x", "--slide", "1", "--slide", "2"])).is_err());
        assert!(parse_args(&args(&["--in", "a", "--in", "b", "--instruction", "x"])).is_err());
        assert!(parse_args(&args(&["--in", "a", "--instruction", "x", "--out", "o.pptx"])).unwrap_err().contains("--out"));
        assert!(parse_args(&args(&["--in", "a", "--instruction", "   "])).is_err());
        assert!(parse_args(&args(&["--in", "a", "--instruction", "x", "--model", "-p"])).is_err());
    }

    #[test]
    fn caps_the_instruction_length() {
        let long = "x".repeat(MAX_INSTRUCTION_BYTES + 1);
        assert!(parse_args(&args(&["--in", "a", "--instruction", &long])).is_err());
        let fits = "x".repeat(MAX_INSTRUCTION_BYTES);
        assert!(parse_args(&args(&["--in", "a", "--instruction", &fits])).is_ok());
    }

    #[test]
    fn maps_the_snapshot_into_request_targets() {
        let snapshot = json!({
            "document_id": 1, "revision": 2, "slide_id": 3,
            "targets": [{"shape_id": 4, "text": "Line one\u{000B}Line two"}, {"shape_id": 5, "text": "Plain"}]
        });
        let request = request_from_snapshot("Shorter", &snapshot).unwrap();
        assert_eq!(request.instruction, "Shorter");
        assert_eq!(request.targets, ["Line one\nLine two", "Plain"]);
        assert!(request.context.is_none());
        assert!(request.validate().is_ok());
        assert_eq!(snapshot["targets"][0]["text"], "Line one\u{000B}Line two", "the snapshot is not altered");
    }

    #[test]
    fn refuses_a_slide_without_text_shapes() {
        let empty = json!({"document_id": 1, "revision": 2, "slide_id": 3, "targets": []});
        assert!(request_from_snapshot("x", &empty).unwrap_err().contains("no text shapes"));
        assert!(request_from_snapshot("x", &json!({})).is_err());
        assert!(request_from_snapshot("x", &json!({"targets": [{"shape_id": 1}]})).is_err());
    }

    #[test]
    fn preview_lists_changes_by_target_index() {
        let snapshot = json!({"document_id": 1, "revision": 2, "slide_id": 3, "targets": [{"shape_id": 4, "text": "a"}]});
        let response = Response { suggestions: vec![Suggestion { index: 0, text: "b".into() }], explanation: "Shorter.".into() };
        let out = preview(snapshot.clone(), &response);
        assert_eq!(out["assistant"], "Mirai Assistant");
        assert_eq!(out["preview"], true);
        assert_eq!(out["applied"], false);
        assert!(out["note"].as_str().is_some_and(|n| n.contains("live session")));
        assert_eq!(out["context"], snapshot);
        assert_eq!(out["changes"], json!([{"index": 0, "text": "b"}]));
        assert_eq!(out["explanation"], "Shorter.");
    }
}
