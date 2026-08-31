//! Claude Haiku summarization of a voice-note transcript.
//!
//! architecture.md calls this out explicitly: "Isolate from whisper — handle
//! API-error case independently." whisper.cpp does not exist yet (Session 4+,
//! flagged highest-risk), so this module takes a plain `String` transcript and
//! knows nothing about how it was produced. Whichever component eventually
//! owns the microphone just needs to call [`handle_transcript`] once it has a
//! final transcript.
//!
//! Only `voice_note.summary` and `voice_note.recorded_at` are ever written
//! here, through the one shared [`SnapshotStore`] writer (architecture.md,
//! Session 0) — never a second writer. A failure anywhere in this module
//! (missing key, network error, API error, empty transcript) degrades to
//! `summary: null` rather than propagating, so it can never take the rest of
//! the snapshot write down with it.

use std::env;
use std::fmt;
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};

use crate::snapshot::{now_iso8601, SnapshotStore};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: u32 = 100;

const SYSTEM_PROMPT: &str = "You summarize a spoken voice note into a single short sentence for a \
'resume card' — present tense, capturing what the person was about to do or \
working on. No preamble, just the sentence.";

#[derive(Debug)]
pub enum SummarizeError {
    /// `ANTHROPIC_API_KEY` was not set. Not a network/API failure, but
    /// resolved the same way: no summary, and the rest of the snapshot is
    /// unaffected.
    MissingApiKey,
    Network(reqwest::Error),
    Api { status: u16, body: String },
    UnexpectedResponseShape(String),
}

impl fmt::Display for SummarizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(f, "ANTHROPIC_API_KEY is not set"),
            Self::Network(err) => write!(f, "network failure calling Claude: {err}"),
            Self::Api { status, body } => {
                write!(f, "Claude API returned {status}: {body}")
            }
            Self::UnexpectedResponseShape(detail) => {
                write!(f, "unexpected Claude response shape: {detail}")
            }
        }
    }
}

impl std::error::Error for SummarizeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(err) => Some(err),
            _ => None,
        }
    }
}

/// Take a transcript from wherever it came from (whisper.cpp, once it
/// exists) and get a summary into `snapshot.json`.
///
/// Runs the HTTP call on its own thread so the caller — a future whisper.cpp
/// integration point — is never blocked on network I/O, matching the
/// fire-and-forget style of the other background producers.
///
/// `recorded_at` is stamped unconditionally: a voice note event happened
/// regardless of whether summarization itself succeeds.
pub fn handle_transcript(store: Arc<SnapshotStore>, transcript: String) {
    thread::Builder::new()
        .name("humcon-summarize".into())
        .spawn(move || {
            let summary = if should_call_api(&transcript) {
                match call_haiku_api(&transcript) {
                    Ok(text) => Some(text),
                    Err(err) => {
                        eprintln!("[summarize] {err}");
                        None
                    }
                }
            } else {
                println!("[summarize] empty transcript, skipping the API call");
                None
            };

            let recorded_at = now_iso8601();
            if let Err(err) = store.update(|snapshot| {
                snapshot.voice_note.recorded_at = Some(recorded_at);
                snapshot.voice_note.summary = summary;
            }) {
                eprintln!("[summarize] could not write snapshot: {err}");
            }
        })
        .expect("failed to spawn summarize thread");
}

/// Whether a transcript is worth spending an API call on.
///
/// Split out as a pure predicate so empty/whitespace-only transcripts (e.g. a
/// recording that captured only silence) are testable without a network
/// call.
fn should_call_api(transcript: &str) -> bool {
    !transcript.trim().is_empty()
}

/// Build the Messages API request body for a given transcript.
fn build_request_body(transcript: &str) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "messages": [
            { "role": "user", "content": transcript }
        ]
    })
}

/// Parse a Messages API response into the summary text, or classify the
/// failure.
///
/// Takes the raw status code and body string (rather than a `reqwest`
/// response) so this can be unit-tested with canned strings instead of
/// mocking HTTP.
fn parse_summary_response(status: u16, body: &str) -> Result<String, SummarizeError> {
    if status != 200 {
        return Err(SummarizeError::Api {
            status,
            body: body.to_string(),
        });
    }

    let parsed: Value = serde_json::from_str(body)
        .map_err(|err| SummarizeError::UnexpectedResponseShape(err.to_string()))?;

    let text = parsed
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SummarizeError::UnexpectedResponseShape(format!(
                "no content[0].text in body: {body}"
            ))
        })?;

    Ok(text.trim().to_string())
}

fn call_haiku_api(transcript: &str) -> Result<String, SummarizeError> {
    let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| SummarizeError::MissingApiKey)?;

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(ANTHROPIC_API_URL)
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&build_request_body(transcript))
        .send()
        .map_err(SummarizeError::Network)?;

    let status = response.status().as_u16();
    let body = response.text().map_err(SummarizeError::Network)?;

    parse_summary_response(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_call_api_rejects_empty_and_whitespace() {
        assert!(!should_call_api(""));
        assert!(!should_call_api("   "));
        assert!(!should_call_api("\n\t  \n"));
    }

    #[test]
    fn should_call_api_accepts_normal_and_short_text() {
        assert!(should_call_api("ok"));
        assert!(should_call_api(
            "I need to finish the quarterly report before lunch."
        ));
    }

    #[test]
    fn build_request_body_contains_transcript_and_model() {
        let body = build_request_body("remember to call the plumber");
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["max_tokens"], MAX_TOKENS);
        assert_eq!(body["messages"][0]["content"], "remember to call the plumber");
    }

    #[test]
    fn parse_summary_response_extracts_text_on_success() {
        let body = r#"{"content":[{"type":"text","text":"Finishing the quarterly report."}]}"#;
        let summary = parse_summary_response(200, body).unwrap();
        assert_eq!(summary, "Finishing the quarterly report.");
    }

    #[test]
    fn parse_summary_response_trims_whitespace() {
        let body = r#"{"content":[{"type":"text","text":"  Padded.  \n"}]}"#;
        let summary = parse_summary_response(200, body).unwrap();
        assert_eq!(summary, "Padded.");
    }

    #[test]
    fn parse_summary_response_classifies_auth_error() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let err = parse_summary_response(401, body).unwrap_err();
        match err {
            SummarizeError::Api { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn parse_summary_response_classifies_rate_limit_and_server_error() {
        let rate_limited = parse_summary_response(429, r#"{"error":"rate limited"}"#);
        assert!(matches!(
            rate_limited,
            Err(SummarizeError::Api { status: 429, .. })
        ));

        let server_error = parse_summary_response(500, r#"{"error":"internal"}"#);
        assert!(matches!(
            server_error,
            Err(SummarizeError::Api { status: 500, .. })
        ));
    }

    #[test]
    fn parse_summary_response_flags_missing_content_field() {
        let body = r#"{"type":"message","usage":{}}"#;
        let err = parse_summary_response(200, body).unwrap_err();
        assert!(matches!(err, SummarizeError::UnexpectedResponseShape(_)));
    }

    #[test]
    fn parse_summary_response_flags_malformed_json() {
        let err = parse_summary_response(200, "not json at all").unwrap_err();
        assert!(matches!(err, SummarizeError::UnexpectedResponseShape(_)));
    }
}
