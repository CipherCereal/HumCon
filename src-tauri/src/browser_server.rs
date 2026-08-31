//! Loopback HTTP receiver for the browser extension (`extension/`).
//!
//! Session 0's schema froze `browser_tab` with no producer, and nothing built
//! in Sessions 1-5 defines how one would reach it — this module and the
//! extension define that contract together (architecture.md, Session 6).
//!
//! The extension `POST`s `{"url": "...", "title": "..."}` to `/tab`; this
//! module writes it into `snapshot.json`'s `browser_tab` key through the same
//! shared `SnapshotStore` every other producer uses, so Session 0's
//! single-mutex rule holds with no second writer.
//!
//! `tiny_http` rather than `axum`/`warp`: those pull in an async runtime, and
//! this codebase has consistently chosen blocking work on its own thread
//! instead (see `summarize.rs`'s `reqwest` note, and the Session 3 log). This
//! is the same call for the receiving side.
//!
//! ## Why a required header actually stops a hostile web page
//!
//! A `fetch()` from any web page carrying a custom header (`X-HumCon-Extension`
//! below) is not a CORS "simple request", so the browser must send a
//! preflight `OPTIONS` first. This server answers every `OPTIONS` with `405`
//! and never emits an `Access-Control-Allow-*` header, so the browser refuses
//! to send the real request. The extension is exempt because an MV3 service
//! worker holding `host_permissions` for this origin bypasses CORS entirely.
//! The header requirement also closes the "simple request" hole: without it, a
//! page could still POST with `Content-Type: text/plain` and no preflight at
//! all — it couldn't read the response, but the write would land regardless.
//!
//! Not defended against: another **local process**, which can trivially send
//! the header too. Accepted for a first pass — the blast radius is a wrong
//! `browser_tab` in a local file, not a leak. A shared token would close that
//! gap but was declined as setup friction (see the plan).

use std::io::Read;
use std::sync::Arc;
use std::thread;

use serde::Deserialize;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::snapshot::{now_iso8601, BrowserTab, SnapshotStore};

/// Header every legitimate request must carry. Its mere presence, not its
/// value, is what matters — see the module doc for why that's enough to stop
/// a web page without needing a secret.
const EXTENSION_HEADER: &str = "X-HumCon-Extension";

/// Requests larger than this are rejected before being read into memory. A
/// tab's URL and title are a few hundred bytes at most; this leaves generous
/// headroom without letting a malformed or hostile client allocate freely.
const MAX_BODY_BYTES: u64 = 8 * 1024;

/// Start the receiver on its own thread and return immediately.
///
/// A port already in use is a warning, not a crash — the rest of the app is
/// useful without browser tracking, exactly how `voice_note::register` treats
/// a hotkey another app already owns.
pub fn spawn(store: Arc<SnapshotStore>, port: u16) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("humcon-browser-server".into())
        .spawn(move || run(&store, port))
        .expect("failed to spawn browser server thread")
}

fn run(store: &SnapshotStore, port: u16) {
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(server) => server,
        Err(err) => {
            eprintln!(
                "[browser] could not bind 127.0.0.1:{port} ({err}) — browser tab \
                 tracking is disabled this run. Another process may already be \
                 using the port; override with HUMCON_PORT."
            );
            return;
        }
    };
    println!("[browser] listening on http://127.0.0.1:{port} for the browser extension");

    // The last update actually written, kept outside the snapshot itself —
    // same shape as command_log.rs's `last_written`. `incoming_requests()`
    // processes one request at a time on this single thread, so a plain local
    // variable is enough; no lock needed. Comparing against this (rather than
    // reading `snapshot.browser_tab` back out) means a duplicate event skips
    // `store.update` entirely, since `SnapshotStore::update` unconditionally
    // stamps `last_updated` and writes the file even when the edit closure
    // changes nothing — architecture.md's Session 2 note ("prefer
    // change-detection over unconditional writes") means skipping the call,
    // not just skipping the field assignment inside it.
    let mut last_sent: Option<TabUpdate> = None;

    for request in server.incoming_requests() {
        handle_request(store, request, &mut last_sent);
    }
}

fn handle_request(store: &SnapshotStore, mut request: Request, last_sent: &mut Option<TabUpdate>) {
    // OPTIONS (the CORS preflight) is answered with no CORS headers at all —
    // see the module doc. This must come before the method/path check below,
    // since a preflight targets the real method (POST) but arrives as OPTIONS.
    if *request.method() == Method::Options {
        let _ = request.respond(Response::empty(405));
        return;
    }

    if *request.method() != Method::Post || request.url() != "/tab" {
        let _ = request.respond(Response::empty(404));
        return;
    }

    if !has_required_header(request.headers()) {
        let _ = request.respond(Response::empty(400));
        return;
    }

    // Reject an oversized body up front when the client declares one, rather
    // than only bounding the read below — a bit of defense in depth for free.
    if let Some(len) = request.body_length() {
        if len as u64 > MAX_BODY_BYTES {
            let _ = request.respond(Response::empty(400));
            return;
        }
    }

    let mut body = String::new();
    let read_result = request
        .as_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_string(&mut body);

    match read_result {
        Ok(_) if body.len() as u64 > MAX_BODY_BYTES => {
            let _ = request.respond(Response::empty(400));
        }
        Err(err) => {
            eprintln!("[browser] could not read request body: {err}");
            let _ = request.respond(Response::empty(400));
        }
        Ok(_) => match parse_tab_request(&body) {
            Ok(update) => {
                if should_write(last_sent.as_ref(), &update) {
                    if let Err(err) = store.update(|snapshot| {
                        snapshot.browser_tab = BrowserTab {
                            url: Some(update.url.clone()),
                            title: update.title.clone(),
                            captured_at: Some(now_iso8601()),
                        };
                    }) {
                        eprintln!("[browser] could not write snapshot: {err}");
                    } else {
                        println!("[browser] tab -> snapshot");
                        *last_sent = Some(update);
                    }
                }
                let _ = request.respond(Response::empty(204));
            }
            Err(err) => {
                eprintln!("[browser] rejected request: {err}");
                let _ = request.respond(Response::empty(400));
            }
        },
    }
}

fn has_required_header(headers: &[Header]) -> bool {
    headers
        .iter()
        .any(|h| h.field.as_str().as_str().eq_ignore_ascii_case(EXTENSION_HEADER))
}

// ---------------------------------------------------------------------------
// Pure parsing / decision logic — split out so it's testable without a
// running server, matching this codebase's established shape (`parse_log` in
// command_log.rs, `map_capture` in window_capture.rs).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TabRequestBody {
    url: String,
    #[serde(default)]
    title: Option<String>,
}

/// A tab update parsed from the extension's request body, not yet written to
/// the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabUpdate {
    url: String,
    title: Option<String>,
}

#[derive(Debug)]
enum TabError {
    Json(serde_json::Error),
    EmptyUrl,
}

impl std::fmt::Display for TabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(err) => write!(f, "malformed request body: {err}"),
            Self::EmptyUrl => write!(f, "\"url\" is empty"),
        }
    }
}

/// Parses and validates the extension's request body.
///
/// `title` is optional and normalized: `null`, absent, and whitespace-only
/// all collapse to `None` rather than `Some("")`, matching the frozen
/// schema's convention of `null` for "nothing here" (Session 0).
fn parse_tab_request(body: &str) -> Result<TabUpdate, TabError> {
    let parsed: TabRequestBody = serde_json::from_str(body).map_err(TabError::Json)?;

    let url = parsed.url.trim();
    if url.is_empty() {
        return Err(TabError::EmptyUrl);
    }

    let title = parsed.title.and_then(|t| {
        let t = t.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });

    Ok(TabUpdate {
        url: url.to_string(),
        title,
    })
}

/// Whether `incoming` actually differs from the last update we sent.
///
/// architecture.md's Session 2 note is explicit: "prefer change-detection
/// over unconditional writes in any new component" — two pollers already
/// rewrite the whole file every 2-3s, and a page that fires repeated
/// `tabs.onUpdated` events for one navigation shouldn't add a write per event
/// on top of that. Compared against `last_sent` (see `run`) rather than the
/// live snapshot, since `SnapshotStore::update` always writes once called —
/// the point is to skip calling it at all for a duplicate.
fn should_write(last_sent: Option<&TabUpdate>, incoming: &TabUpdate) -> bool {
    last_sent != Some(incoming)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str) -> Header {
        Header::from_bytes(name.as_bytes(), b"1").unwrap()
    }

    #[test]
    fn required_header_is_matched_case_insensitively() {
        assert!(has_required_header(&[header("x-humcon-extension")]));
        assert!(has_required_header(&[header("X-HUMCON-EXTENSION")]));
        assert!(has_required_header(&[header(EXTENSION_HEADER)]));
    }

    #[test]
    fn missing_header_is_rejected() {
        assert!(!has_required_header(&[header("Content-Type")]));
        assert!(!has_required_header(&[]));
    }

    #[test]
    fn parses_url_and_title() {
        let update = parse_tab_request(r#"{"url":"https://example.com","title":"Example"}"#)
            .unwrap();
        assert_eq!(update.url, "https://example.com");
        assert_eq!(update.title.as_deref(), Some("Example"));
    }

    #[test]
    fn missing_title_becomes_none() {
        let update = parse_tab_request(r#"{"url":"https://example.com"}"#).unwrap();
        assert_eq!(update.title, None);
    }

    #[test]
    fn null_title_becomes_none() {
        let update =
            parse_tab_request(r#"{"url":"https://example.com","title":null}"#).unwrap();
        assert_eq!(update.title, None);
    }

    #[test]
    fn whitespace_only_title_becomes_none() {
        let update =
            parse_tab_request(r#"{"url":"https://example.com","title":"   "}"#).unwrap();
        assert_eq!(update.title, None);
    }

    #[test]
    fn empty_url_is_rejected() {
        assert!(matches!(
            parse_tab_request(r#"{"url":""}"#),
            Err(TabError::EmptyUrl)
        ));
        assert!(matches!(
            parse_tab_request(r#"{"url":"   "}"#),
            Err(TabError::EmptyUrl)
        ));
    }

    #[test]
    fn missing_url_field_is_rejected() {
        assert!(matches!(
            parse_tab_request(r#"{"title":"no url here"}"#),
            Err(TabError::Json(_))
        ));
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert!(matches!(
            parse_tab_request("not json"),
            Err(TabError::Json(_))
        ));
    }

    #[test]
    fn url_and_title_are_trimmed() {
        let update =
            parse_tab_request(r#"{"url":"  https://example.com  ","title":"  hi  "}"#).unwrap();
        assert_eq!(update.url, "https://example.com");
        assert_eq!(update.title.as_deref(), Some("hi"));
    }

    fn update(url: &str, title: Option<&str>) -> TabUpdate {
        TabUpdate {
            url: url.to_string(),
            title: title.map(str::to_string),
        }
    }

    #[test]
    fn first_ever_update_should_write() {
        assert!(should_write(None, &update("https://a.com", None)));
    }

    #[test]
    fn identical_update_should_not_write() {
        let last = update("https://a.com", Some("A"));
        assert!(!should_write(Some(&last), &update("https://a.com", Some("A"))));
    }

    #[test]
    fn changed_url_should_write() {
        let last = update("https://a.com", Some("A"));
        assert!(should_write(Some(&last), &update("https://b.com", Some("A"))));
    }

    #[test]
    fn changed_title_same_url_should_write() {
        let last = update("https://a.com", Some("A"));
        assert!(should_write(Some(&last), &update("https://a.com", Some("A2"))));
    }

    #[test]
    fn title_becoming_none_should_write() {
        let last = update("https://a.com", Some("A"));
        assert!(should_write(Some(&last), &update("https://a.com", None)));
    }
}
