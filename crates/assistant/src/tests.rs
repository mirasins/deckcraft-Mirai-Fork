use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use crate::process::{AUTH_ARGS, GENERATION_ARGS, Limits, credential_override_keys, key_present, run};
use crate::protocol::{Source, parse_auth_status, parse_response, prompt_payload, select_auth};
use crate::{AuthState, Client, Error, MAX_OUTPUT_BYTES, MAX_TARGETS, MAX_TEXT_BYTES, Provider, Request};

fn request(targets: &[&str]) -> Request {
    Request { instruction: "Make it formal".into(), targets: targets.iter().map(|t| (*t).to_owned()).collect(), structure: Vec::new(), context: None }
}

fn success(structured: &str) -> Vec<u8> {
    format!(r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured}}}"#).into_bytes()
}

#[test]
fn stored_session_accepts_only_claude_ai() {
    let ok = br#"{"loggedIn":true,"authMethod":"claude.ai"}"#;
    assert_eq!(parse_auth_status(ok, Source::StoredSession), Ok(AuthState::Account));
    let token = br#"{"loggedIn":true,"authMethod":"oauth_token"}"#;
    assert_eq!(parse_auth_status(token, Source::StoredSession), Err(Error::Authentication));
    for method in ["api_key", "api_key_helper", "third_party", "something_new"] {
        let payload = format!(r#"{{"loggedIn":true,"authMethod":"{method}"}}"#);
        assert_eq!(parse_auth_status(payload.as_bytes(), Source::StoredSession), Err(Error::UnsupportedAuth), "{method}");
    }
    let logged_out = br#"{"loggedIn":false,"authMethod":"none"}"#;
    assert_eq!(parse_auth_status(logged_out, Source::StoredSession), Err(Error::Authentication));
    let not_logged = br#"{"loggedIn":false,"authMethod":"claude.ai"}"#;
    assert_eq!(parse_auth_status(not_logged, Source::StoredSession), Err(Error::Authentication));
    assert_eq!(parse_auth_status(b"not json", Source::StoredSession), Err(Error::InvalidResponse));
    assert_eq!(parse_auth_status(b"{}", Source::StoredSession), Err(Error::Authentication));
    let first_party = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#;
    assert_eq!(parse_auth_status(first_party, Source::StoredSession), Ok(AuthState::Account));
    for provider in ["bedrock", "vertex", "foundry", "gateway"] {
        let payload = format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"{provider}"}}"#);
        assert_eq!(parse_auth_status(payload.as_bytes(), Source::StoredSession), Err(Error::UnsupportedAuth), "{provider}");
        assert_eq!(parse_auth_status(payload.as_bytes(), Source::EnvToken), Err(Error::UnsupportedAuth), "{provider}");
    }
}

#[test]
fn env_token_probe_accepts_only_oauth_token() {
    let token = br#"{"loggedIn":true,"authMethod":"oauth_token"}"#;
    assert_eq!(parse_auth_status(token, Source::EnvToken), Ok(AuthState::OAuthToken));
    let account = br#"{"loggedIn":true,"authMethod":"claude.ai"}"#;
    assert_eq!(parse_auth_status(account, Source::EnvToken), Err(Error::Authentication));
    let key = br#"{"loggedIn":true,"authMethod":"api_key"}"#;
    assert_eq!(parse_auth_status(key, Source::EnvToken), Err(Error::UnsupportedAuth));
}

#[test]
fn auth_rejects_oversized_output() {
    let big = vec![b' '; crate::protocol::MAX_AUTH_OUTPUT_BYTES + 1];
    assert_eq!(parse_auth_status(&big, Source::StoredSession), Err(Error::InvalidResponse));
}

/// Precedence table with no real credentials: the env probe is a closure, and a flag shows whether it ran.
#[test]
fn stored_session_wins_over_env_token() {
    let ran = std::cell::Cell::new(false);
    let picked = select_auth(Ok(AuthState::Account), true, || {
        ran.set(true);
        Ok(AuthState::OAuthToken)
    });
    assert_eq!(picked, Ok(AuthState::Account));
    assert!(!ran.get());
}

#[test]
fn unsupported_stored_key_falls_back_to_env_oauth_only() {
    let picked = select_auth(Err(Error::UnsupportedAuth), true, || Ok(AuthState::OAuthToken));
    assert_eq!(picked, Ok(AuthState::OAuthToken));
    let no_token = std::cell::Cell::new(false);
    let rejected = select_auth(Err(Error::UnsupportedAuth), false, || {
        no_token.set(true);
        Ok(AuthState::OAuthToken)
    });
    assert_eq!(rejected, Err(Error::UnsupportedAuth));
    assert!(!no_token.get());
}

#[test]
fn stored_rejection_without_token_stays_authentication() {
    assert_eq!(select_auth(Err(Error::Authentication), false, || Ok(AuthState::OAuthToken)), Err(Error::Authentication));
}

#[test]
fn env_probe_rejection_keeps_stored_category() {
    let picked = select_auth(Err(Error::UnsupportedAuth), true, || Err(Error::Authentication));
    assert_eq!(picked, Err(Error::UnsupportedAuth));
    let picked = select_auth(Err(Error::Authentication), true, || Err(Error::UnsupportedAuth));
    assert_eq!(picked, Err(Error::Authentication));
}

#[test]
fn runtime_failures_are_not_masked_as_authentication() {
    let env_probe_ran = std::cell::Cell::new(false);
    for error in [Error::Unavailable, Error::Network, Error::Timeout, Error::Cancelled, Error::InvalidResponse] {
        assert_eq!(select_auth(Err(error), true, || Err(Error::Authentication)), Err(error));
        let picked = select_auth(Err(Error::Authentication), true, || {
            env_probe_ran.set(true);
            Err(error)
        });
        assert_eq!(picked, Err(error));
    }
    assert!(env_probe_ran.get());
}

#[test]
fn env_probe_success_never_yields_account() {
    assert_eq!(select_auth(Err(Error::Authentication), true, || Ok(AuthState::Account)), Err(Error::InvalidResponse));
}

#[test]
fn credential_overrides_are_removed_for_account_mode() {
    let keys = [
        "ANTHROPIC_API_KEY",
        "anthropic_auth_token",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "claude_code_oauth_token",
        "PATH",
    ]
    .map(OsString::from);
    let removed = credential_override_keys(keys.clone(), false);
    assert!(removed.contains(&OsString::from("ANTHROPIC_API_KEY")));
    assert!(removed.contains(&OsString::from("anthropic_auth_token")));
    assert!(removed.contains(&OsString::from("CLAUDE_CODE_USE_VERTEX")));
    assert!(removed.contains(&OsString::from("claude_code_oauth_token")));
    assert!(!removed.contains(&OsString::from("PATH")));

    let token_mode = credential_override_keys(keys, true);
    assert!(!token_mode.contains(&OsString::from("claude_code_oauth_token")));
    assert!(token_mode.contains(&OsString::from("ANTHROPIC_API_KEY")));
}

#[test]
fn token_presence_is_detected_without_values() {
    let keys = ["PATH", "claude_code_oauth_token"].map(OsString::from);
    assert!(key_present(keys, "CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(!key_present(["PATH"].map(OsString::from), "CLAUDE_CODE_OAUTH_TOKEN"));
}

#[test]
fn auth_and_generation_share_safe_mode() {
    assert_eq!(AUTH_ARGS.first(), Some(&"--safe-mode"));
    assert_eq!(GENERATION_ARGS.first(), Some(&"--safe-mode"));
    assert!(GENERATION_ARGS.contains(&"--no-session-persistence"));
    assert!(GENERATION_ARGS.contains(&"--strict-mcp-config"));
    assert!(GENERATION_ARGS.contains(&"dontAsk"));
    assert!(GENERATION_ARGS.contains(&"--output-format"));
    assert!(!GENERATION_ARGS.contains(&"--bare"));
    assert!(!AUTH_ARGS.contains(&"--bare"));
    let tools = GENERATION_ARGS.iter().position(|a| *a == "--tools");
    assert_eq!(tools.and_then(|i| GENERATION_ARGS.get(i + 1)), Some(&""));
}

#[test]
fn payload_is_json_data() {
    let mut req = request(&["He said \"hi\"\u{0007}"]);
    req.context = Some("line one\nline two".into());
    let payload = prompt_payload(&req).unwrap_or_default();
    assert!(payload.contains("\"instruction\":\"Make it formal\""));
    assert!(payload.contains("\\\"hi\\\""));
    assert!(!format!("{req:?}").contains("hi"));
}

#[test]
fn request_limits_are_enforced() {
    assert_eq!(request(&["ok"]).validate(), Ok(()));
    let mut empty = request(&["ok"]);
    empty.instruction = "  ".into();
    assert_eq!(empty.validate(), Err(Error::InvalidRequest));
    let mut long = request(&["ok"]);
    long.instruction = "x".repeat(crate::MAX_INSTRUCTION_BYTES + 1);
    assert_eq!(long.validate(), Err(Error::TooLong));
    let many = request(&vec!["a"; MAX_TARGETS + 1]);
    assert_eq!(many.validate(), Err(Error::TooLong));
    let big = request(&[&"x".repeat(MAX_TEXT_BYTES)]);
    assert_eq!(big.validate(), Err(Error::TooLong));
    assert_eq!(request(&["a\nb"]).validate(), Ok(()));
    assert_eq!(request(&["a\u{FFFC}b"]).validate(), Err(Error::InvalidRequest));
    assert_eq!(request(&["a\u{0000}b"]).validate(), Err(Error::InvalidRequest));
    assert_eq!(request(&["a\u{2028}b"]).validate(), Err(Error::InvalidRequest));
    assert_eq!(request(&["a\u{2029}b"]).validate(), Err(Error::InvalidRequest));
}

#[test]
fn multiline_plain_targets_are_accepted() {
    let req = request(&["first line\nsecond line", "tab\there", "cr\rhere"]);
    assert_eq!(req.validate(), Ok(()));
    let payload = prompt_payload(&req).unwrap_or_default();
    assert!(payload.contains("first line\\nsecond line"));
}

#[test]
fn markdown_suggestions_are_rejected() {
    let markdown = success(r##"{"suggestions":[{"index":0,"text":"# Title","markdown":true}],"explanation":"ok"}"##);
    assert_eq!(parse_response(&markdown, 1).map(|_| ()), Err(Error::InvalidResponse));
}

#[test]
fn structure_is_sent_and_bounded() {
    let mut req = request(&["Intro", "Body"]);
    let plain = prompt_payload(&req).unwrap_or_default();
    assert!(!plain.contains("structure"), "no structure key without structure");
    req.structure = vec!["heading 1".into(), "paragraph".into()];
    assert_eq!(req.validate(), Ok(()));
    assert!(prompt_payload(&req).unwrap_or_default().contains(r#""structure":["heading 1","paragraph"]"#));
    req.structure.push("extra".into());
    assert_eq!(req.validate(), Err(Error::TooLong));
    req.structure = vec!["x".repeat(crate::MAX_STRUCTURE_BYTES + 1)];
    assert_eq!(req.validate(), Err(Error::TooLong));
    req.structure = vec!["a\nb".into()];
    assert_eq!(req.validate(), Err(Error::InvalidRequest));
}

#[test]
fn plaintext_suggestions_may_span_lines() {
    let multi = success(
        r##"{"suggestions":[{"index":0,"text":"Line one\nLine two\tindented","markdown":false},{"index":1,"text":"","markdown":false}],"explanation":"ok"}"##,
    );
    let parsed = parse_response(&multi, 2).unwrap();
    assert!(parsed.suggestions.iter().all(|s| !s.markdown));
    assert_eq!(parsed.suggestions.first().map(|s| s.text.lines().count()), Some(2));
    let markdown = success(r##"{"suggestions":[{"index":0,"text":"# Title","markdown":true}],"explanation":"ok"}"##);
    assert_eq!(parse_response(&markdown, 1).map(|_| ()), Err(Error::InvalidResponse));
    let object = success(r#"{"suggestions":[{"index":0,"text":"a￼b","markdown":false}],"explanation":"ok"}"#);
    assert_eq!(parse_response(&object, 1).map(|_| ()), Err(Error::InvalidResponse));
    assert!(crate::protocol::OUTPUT_SCHEMA.contains(r#""markdown":{"type":"boolean"}"#));
}

#[test]
fn executable_must_be_absolute_and_exist() {
    assert_eq!(Client::with_executable("claude-does-not-exist-xyz").map(|_| ()), Err(Error::Unavailable));
    let missing = std::env::temp_dir().join("deckcraft-assistant-missing-claude-binary");
    assert_eq!(Client::with_executable(missing).map(|_| ()), Err(Error::Unavailable));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cancelled_flag_short_circuits_before_spawning() {
    // The test binary is an absolute, existing file. Cancellation is checked before any spawn.
    let exe = std::env::current_exe().unwrap_or_default();
    let client = Client::with_executable(exe);
    assert!(client.is_ok());
    let cancel = AtomicBool::new(true);
    if let Ok(client) = client {
        assert_eq!(client.auth(&cancel), Err(Error::Cancelled));
    }
}

#[test]
fn response_validates_structured_output() {
    let good = success(r#"{"suggestions":[{"index":0,"text":"Estimado señor."}],"explanation":"More formal."}"#);
    let parsed = parse_response(&good, 2);
    assert_eq!(parsed.as_ref().map(|r| r.suggestions.len()), Ok(1));
    assert_eq!(parsed.map(|r| r.explanation), Ok("More formal.".into()));

    let empty = success(r#"{"suggestions":[],"explanation":"Nothing to change."}"#);
    assert_eq!(parse_response(&empty, 1).map(|r| r.suggestions.len()), Ok(0));

    assert_eq!(parse_response(&success("{}"), 1).map(|_| ()), Err(Error::InvalidResponse));
    assert_eq!(parse_response(&success("null"), 1).map(|_| ()), Err(Error::InvalidResponse));
    assert_eq!(parse_response(b"garbage", 1).map(|_| ()), Err(Error::InvalidResponse));
}

fn envelope(structured: &str) -> String {
    format!(r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured}}}"#)
}

fn deck_envelope(operations: &str) -> String {
    envelope(&format!(r#"{{"suggestions":[],"explanation":"e","deck":{{"operations":[{operations}]}}}}"#))
}

#[test]
fn deck_proposal_parses_valid_operations() {
    let valid = deck_envelope(
        r##"{"kind":"new_slide","layout":"titleAndContent","text":"Plan"},
           {"kind":"add_text","rect":[10,20,300.5,120],"text":"Line one\nLine two"},
           {"kind":"background","color":"#1a2B3c"}"##,
    );
    let parsed = parse_response(valid.as_bytes(), 0).map(|r| r.deck.map(|d| d.operations.len()));
    assert_eq!(parsed, Ok(Some(3)));

    let edit = deck_envelope(r#"{"kind":"set_text","target":1,"text":"Shorter"},{"kind":"set_bounds","target":0,"rect":[0,0,10000,10000]}"#);
    assert!(parse_response(edit.as_bytes(), 2).is_ok());

    let absent = envelope(r#"{"suggestions":[{"index":0,"text":"Short"}],"explanation":"e","deck":null}"#);
    assert_eq!(parse_response(absent.as_bytes(), 1).map(|r| r.deck.is_none()), Ok(true));
}

#[test]
fn deck_proposal_rejects_hostile_or_inconsistent_operations() {
    let hostile = [
        // Mixing suggestions with deck work.
        envelope(r#"{"suggestions":[{"index":0,"text":"x"}],"explanation":"e","deck":{"operations":[{"kind":"new_slide","layout":"blank"}]}}"#),
        // An empty deck is malformed: without slide work the model answers `deck: null`.
        envelope(r#"{"suggestions":[],"explanation":"e","deck":{"operations":[]}}"#),
        // Unknown kinds, layouts and fields.
        deck_envelope(r#"{"kind":"delete_slide"}"#),
        deck_envelope(r#"{"kind":"new_slide"}"#),
        deck_envelope(r#"{"kind":"new_slide","layout":"chart"}"#),
        deck_envelope(r#"{"kind":"new_slide","layout":"blank","rect":[0,0,1,1]}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,1,1],"text":"x","extra":1}"#),
        envelope(r#"{"suggestions":[],"explanation":"e","deck":{"operations":[],"extra":1}}"#),
        // add_text needs a valid rect and text.
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,1,1]}"#),
        deck_envelope(r#"{"kind":"add_text","text":"x"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[-1,0,1,1],"text":"x"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,10001,1],"text":"x"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,0,1],"text":"x"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,1],"text":"x"}"#),
        // set_text and set_bounds target only existing context shapes, before any new slide.
        deck_envelope(r#"{"kind":"set_text","target":2,"text":"x"}"#),
        deck_envelope(r#"{"kind":"set_text","text":"x"}"#),
        deck_envelope(r#"{"kind":"set_text","target":0}"#),
        deck_envelope(r##"{"kind":"set_text","target":0,"text":"x","color":"#000000"}"##),
        deck_envelope(r#"{"kind":"set_bounds","target":0,"rect":[0,0,1,1],"text":"x"}"#),
        deck_envelope(r#"{"kind":"new_slide","layout":"blank"},{"kind":"set_text","target":0,"text":"x"}"#),
        deck_envelope(r#"{"kind":"new_slide","layout":"blank"},{"kind":"set_bounds","target":0,"rect":[0,0,1,1]}"#),
        // background needs a #RRGGBB colour and nothing else.
        deck_envelope(r#"{"kind":"background","color":"red"}"#),
        deck_envelope(r##"{"kind":"background","color":"#GGGGGG"}"##),
        deck_envelope(r##"{"kind":"background","color":"#12345"}"##),
        deck_envelope(r##"{"kind":"background","color":"#1234567"}"##),
        deck_envelope(r#"{"kind":"background"}"#),
        deck_envelope(r##"{"kind":"background","color":"#123456","layout":"blank"}"##),
        // Text must be plain: no control characters, no object replacement or line separators.
        deck_envelope(r#"{"kind":"new_slide","layout":"blank","text":"bad\u0007"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,1,1],"text":"a\u2028b"}"#),
        deck_envelope(r#"{"kind":"add_text","rect":[0,0,1,1],"text":"a\ufffcb"}"#),
        // Too many operations, and total text beyond the limit.
        deck_envelope(&vec![r##"{"kind":"background","color":"#000000"}"##; 101].join(",")),
        deck_envelope(&format!(
            r#"{{"kind":"add_text","rect":[0,0,1,1],"text":"{}"}},{{"kind":"add_text","rect":[0,0,1,1],"text":"{}"}}"#,
            "x".repeat(40_000),
            "x".repeat(30_000)
        )),
    ];
    for fixture in &hostile {
        assert_eq!(parse_response(fixture.as_bytes(), 2).map(|_| ()), Err(Error::InvalidResponse), "{fixture}");
    }
    let max = deck_envelope(&vec![r##"{"kind":"background","color":"#000000"}"##; 100].join(","));
    assert_eq!(parse_response(max.as_bytes(), 0).map(|r| r.deck.map(|d| d.operations.len())), Ok(Some(100)));
}

#[test]
fn model_error_fixtures_map_to_sanitized_categories() {
    let cases: [(&str, Error); 8] = [
        (r#"{"type":"result","is_error":true,"api_error_status":401,"result":"x"}"#, Error::Authentication),
        (r#"{"type":"result","is_error":true,"api_error_status":403,"result":"x"}"#, Error::Authentication),
        (r#"{"type":"result","is_error":true,"api_error_status":429,"result":"x"}"#, Error::Quota),
        (r#"{"type":"result","is_error":true,"api_error_status":402,"result":"x"}"#, Error::Quota),
        (r#"{"type":"result","is_error":true,"api_error_status":529,"result":"x"}"#, Error::Network),
        (r#"{"type":"result","is_error":true,"api_error_status":null,"result":"x"}"#, Error::Failed),
        (r#"{"type":"result","subtype":"error_max_structured_output_retries","is_error":true}"#, Error::Failed),
        (r#"{"type":"result","is_error":true,"api_error_status":"429"}"#, Error::InvalidResponse),
    ];
    for (fixture, expected) in cases {
        assert_eq!(parse_response(fixture.as_bytes(), 1).map(|_| ()), Err(expected), "{fixture}");
    }
}

#[test]
fn error_result_is_classified_before_structured_parse() {
    let fixture = r#"{"type":"result","is_error":true,"api_error_status":429,"structured_output":{"bad":1}}"#;
    assert_eq!(parse_response(fixture.as_bytes(), 1).map(|_| ()), Err(Error::Quota));
}

#[test]
fn response_classification_preserves_error_probe_schema_rules() {
    let huge_content = "x".repeat(48 * 1024);
    let cases = [
        (format!(r#"{{"is_error":true,"api_error_status":401,"structured_output":"{huge_content}"}}"#), Error::Authentication),
        (format!(r#"{{"is_error":true,"api_error_status":403,"structured_output":"{huge_content}"}}"#), Error::Authentication),
        (format!(r#"{{"is_error":true,"api_error_status":429,"structured_output":"{huge_content}"}}"#), Error::Quota),
        (format!(r#"{{"is_error":true,"api_error_status":500,"structured_output":"{huge_content}"}}"#), Error::Network),
        (format!(r#"{{"is_error":true,"structured_output":"{huge_content}"}}"#), Error::Failed),
        // The legacy probe rejects malformed recognized fields before parsing structured output,
        // even when the error flag is false or a valid success output is present.
        (r#"{"is_error":false,"api_error_status":"429","structured_output":{"suggestions":[],"explanation":""}}"#.to_owned(), Error::InvalidResponse),
        (r#"{"is_error":"true","api_error_status":429,"structured_output":{"suggestions":[],"explanation":""}}"#.to_owned(), Error::InvalidResponse),
        (r#"{"is_error":true,"api_error_status":"429","structured_output":"bad"}"#.to_owned(), Error::InvalidResponse),
        (r#"{"is_error":true,"api_error_status":null,"structured_output":"bad"}"#.to_owned(), Error::Failed),
    ];

    for (fixture, expected) in cases {
        assert!(fixture.len() <= crate::MAX_OUTPUT_BYTES, "fixture exceeds output cap");
        assert_eq!(parse_response(fixture.as_bytes(), 1).map(|_| ()), Err(expected), "{fixture}");
    }

    // Unknown top-level fields (including an unknown spelling of the flag) remain ignored.
    let success_with_unknown_fields = br#"{"isError":true,"unknown_flag":false,"structured_output":{"suggestions":[],"explanation":"ok"}}"#;
    assert_eq!(parse_response(success_with_unknown_fields, 1).map(|r| r.explanation), Ok("ok".to_owned()));

    for fixture in [r#"[]"#, "null", r#"{"structured_output":[]}"#] {
        assert_eq!(parse_response(fixture.as_bytes(), 1).map(|_| ()), Err(Error::InvalidResponse), "{fixture}");
    }
}

#[test]
fn response_rejects_out_of_range_duplicate_and_unsafe_text() {
    let out_of_range = success(r#"{"suggestions":[{"index":5,"text":"x"}],"explanation":""}"#);
    assert_eq!(parse_response(&out_of_range, 2).map(|_| ()), Err(Error::InvalidResponse));
    let duplicate = success(r#"{"suggestions":[{"index":0,"text":"a"},{"index":0,"text":"b"}],"explanation":""}"#);
    assert_eq!(parse_response(&duplicate, 2).map(|_| ()), Err(Error::InvalidResponse));
    let newline = success(r#"{"suggestions":[{"index":0,"text":"a\nb"}],"explanation":""}"#);
    assert_eq!(parse_response(&newline, 2).map(|r| r.suggestions.len()), Ok(1));
    let object = success("{\"suggestions\":[{\"index\":0,\"text\":\"a\\uFFFCb\"}],\"explanation\":\"\"}");
    assert_eq!(parse_response(&object, 2).map(|_| ()), Err(Error::InvalidResponse));
    let control = success("{\"suggestions\":[{\"index\":0,\"text\":\"a\\u0007b\"}],\"explanation\":\"\"}");
    assert_eq!(parse_response(&control, 2).map(|_| ()), Err(Error::InvalidResponse));
    let separator = success("{\"suggestions\":[{\"index\":0,\"text\":\"a\\u2028b\"}],\"explanation\":\"\"}");
    assert_eq!(parse_response(&separator, 2).map(|_| ()), Err(Error::InvalidResponse));
    let negative = success(r#"{"suggestions":[{"index":-1,"text":"a"}],"explanation":""}"#);
    assert_eq!(parse_response(&negative, 2).map(|_| ()), Err(Error::InvalidResponse));
    let too_many = success(r#"{"suggestions":[{"index":0,"text":"a"},{"index":1,"text":"b"}],"explanation":""}"#);
    assert_eq!(parse_response(&too_many, 1).map(|_| ()), Err(Error::InvalidResponse));
}

#[test]
fn response_rejects_unknown_fields_in_structured_output() {
    let extra_suggestion = success(r#"{"suggestions":[{"index":0,"text":"a","score":1}],"explanation":""}"#);
    assert_eq!(parse_response(&extra_suggestion, 1).map(|_| ()), Err(Error::InvalidResponse));
    let extra_output = success(r#"{"suggestions":[],"explanation":"","extra":true}"#);
    assert_eq!(parse_response(&extra_output, 1).map(|_| ()), Err(Error::InvalidResponse));
}

#[test]
fn response_bounds_total_text_and_sizes() {
    let half = "x".repeat(MAX_TEXT_BYTES / 2 + 1);
    let over_total = success(&format!(r#"{{"suggestions":[{{"index":0,"text":"{half}"}},{{"index":1,"text":"{half}"}}],"explanation":""}}"#));
    assert_eq!(parse_response(&over_total, 2).map(|_| ()), Err(Error::InvalidResponse));

    let at_total = "x".repeat(MAX_TEXT_BYTES);
    let exact = success(&format!(r#"{{"suggestions":[{{"index":0,"text":"{at_total}"}}],"explanation":""}}"#));
    assert_eq!(parse_response(&exact, 1).map(|r| r.suggestions.len()), Ok(1));

    let huge = vec![b' '; MAX_OUTPUT_BYTES + 1];
    assert_eq!(parse_response(&huge, 1).map(|_| ()), Err(Error::InvalidResponse));
    let long_explanation = success(&format!(r#"{{"suggestions":[],"explanation":"{}"}}"#, "x".repeat(crate::MAX_EXPLANATION_BYTES + 1)));
    assert_eq!(parse_response(&long_explanation, 1).map(|_| ()), Err(Error::InvalidResponse));
}

#[test]
fn response_debug_hides_text() {
    let parsed = parse_response(&success(r#"{"suggestions":[{"index":0,"text":"secret words"}],"explanation":"e"}"#), 1);
    let debug = format!("{:?}", parsed.unwrap_or_else(|_| crate::Response { suggestions: vec![], explanation: String::new(), deck: None }));
    assert!(!debug.contains("secret words"));
}

#[cfg(not(target_arch = "wasm32"))]
const HELPER_ENV: &str = "DECKCRAFT_ASSISTANT_TEST_HELPER_MODE";

/// Builds a command that re-runs this test binary into the ignored `helper_child` test.
#[cfg(not(target_arch = "wasm32"))]
fn helper_command(mode: &str) -> Command {
    let exe = std::env::current_exe().unwrap_or_default();
    let mut command = Command::new(exe);
    command.args(["--exact", "tests::helper_child", "--ignored", "--nocapture", "--test-threads=1"]).env(HELPER_ENV, mode);
    command
}

/// Child-side behaviour for the subprocess tests. Ignored, so it only runs when a test asks for it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore]
fn helper_child() {
    let Ok(mode) = std::env::var(HELPER_ENV) else {
        return;
    };
    match mode.as_str() {
        "sleep" => thread::sleep(Duration::from_secs(60)),
        "flood" => {
            let chunk = vec![b'x'; 65_536];
            let mut out = io::stdout().lock();
            for _ in 0..32 {
                let _ = out.write_all(&chunk);
            }
            let _ = out.flush();
        }
        "echo" => {
            let mut buf = Vec::new();
            let _ = io::stdin().read_to_end(&mut buf);
            println!("ECHO:{}", buf.len());
        }
        _ => {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn subprocess_stdin_and_stdout_round_trip() {
    let cancel = AtomicBool::new(false);
    let limits = Limits { stdout: 64 * 1024, deadline: Duration::from_secs(60) };
    let captured = run(helper_command("echo"), b"hello", limits, &cancel);
    assert!(captured.is_ok());
    if let Ok(captured) = captured {
        assert!(captured.success);
        assert!(String::from_utf8_lossy(&captured.stdout).contains("ECHO:5"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn subprocess_times_out_and_is_reaped() {
    let cancel = AtomicBool::new(false);
    let limits = Limits { stdout: 1024, deadline: Duration::from_millis(300) };
    let started = Instant::now();
    assert_eq!(run(helper_command("sleep"), b"", limits, &cancel).map(|_| ()), Err(Error::Timeout));
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn subprocess_cancellation_stops_the_child() {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let setter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let limits = Limits { stdout: 1024, deadline: Duration::from_secs(60) };
    let started = Instant::now();
    assert_eq!(run(helper_command("sleep"), b"", limits, &cancel).map(|_| ()), Err(Error::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(30));
    let _ = setter.join();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn subprocess_overflow_is_rejected() {
    let cancel = AtomicBool::new(false);
    let limits = Limits { stdout: 1024, deadline: Duration::from_secs(60) };
    assert_eq!(run(helper_command("flood"), b"", limits, &cancel).map(|_| ()), Err(Error::InvalidResponse));
}

#[test]
fn model_names_are_checked_before_use() {
    use crate::protocol::model_name;
    assert_eq!(model_name(None), Ok(None));
    assert_eq!(model_name(Some("  ")), Ok(None));
    for ok in ["sonnet", "opus", "haiku", "claude-opus-5-5", "opus[1m]", "claude-sonnet-5.5"] {
        assert_eq!(model_name(Some(ok)), Ok(Some(ok.to_owned())), "{ok}");
    }
    assert_eq!(model_name(Some(" opus ")), Ok(Some("opus".to_owned())));
    let long = "a".repeat(crate::MAX_MODEL_BYTES + 1);
    for bad in ["--tools", "-p", "opus model", "opus;rm", "op\u{e9}s", "opus\u{0}", "\"opus\"", long.as_str()] {
        assert_eq!(model_name(Some(bad)), Err(Error::InvalidRequest), "{bad:?}");
    }
}

#[cfg(windows)]
#[test]
fn windows_executable_must_be_an_exe() {
    let dir = std::env::temp_dir().join(format!("deckcraft-assistant-shim-{}", std::process::id()));
    let _ = std::fs::create_dir(&dir);
    let shim = dir.join("claude.cmd");
    let _ = std::fs::write(&shim, b"@echo off\r\n");
    assert_eq!(Client::with_executable(&shim).map(|_| ()), Err(Error::Unavailable));
    let _ = std::fs::remove_file(&shim);
    let _ = std::fs::remove_dir(&dir);
}

/// Live check against the signed-in Claude Code account: one tiny synthetic request. Opt-in only
/// (`--ignored` and `DECKCRAFT_ASSISTANT_LIVE=1`); no document text, nothing is stored.
#[test]
#[ignore = "sends one synthetic request with the signed-in Claude Code account"]
fn live_generation_with_signed_in_account() {
    if std::env::var_os("DECKCRAFT_ASSISTANT_LIVE").is_none() {
        eprintln!("skipped: set DECKCRAFT_ASSISTANT_LIVE=1");
        return;
    }
    let model = std::env::var("DECKCRAFT_ASSISTANT_MODEL").ok();
    let client = Client::discover().and_then(|c| c.with_model(model.as_deref())).unwrap();
    let cancel = AtomicBool::new(false);
    let auth = client.auth(&cancel).unwrap();
    assert!(matches!(auth, AuthState::Account | AuthState::OAuthToken));
    let request = Request {
        instruction: "Correct spelling only.".into(),
        targets: vec!["Ths sentense has two erors.".into(), "This one is fine.".into()],
        structure: Vec::new(),
        context: None,
    };
    let response = client.generate(&request, &cancel).unwrap();
    eprintln!("live: auth {auth:?}, {} suggestion(s): {:?}", response.suggestions.len(), response.suggestions);
    let first = response.suggestions.iter().find(|s| s.index == 0).map(|s| s.text.as_str());
    assert_eq!(first.map(|t| t.contains("sentence") && t.contains("errors")), Some(true), "{:?}", response.suggestions);
}

#[test]
fn claude_starts_without_a_console_window() {
    // Win32 CREATE_NO_WINDOW; `run_claude` sets it on Windows, where std can't read it back.
    assert_eq!(crate::process::CREATE_NO_WINDOW, 0x0800_0000);
}
