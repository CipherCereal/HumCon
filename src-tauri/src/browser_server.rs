//! Loopback HTTP receiver for the browser extension (`extension/`).
//!
//! Session 0's schema froze `browser_tab` with no producer, and nothing built
//! in Sessions 1-5 defines how one would reach it — this module and the
//! extension define that contract together (architecture.md, Session 6).
//!
//! Session 7 overhauled the contract: `browser_tab` (single object) is
//! replaced by `browser_tabs` (Vec), and this server now accumulates a
//! per-URL frequency counter. On every tab switch:
//!   1. The URL's entry (keyed by URL) gets its `frequency` incremented and
//!      its `last_seen` updated.
//!   2. Entries older than 1 hour are pruned.
//!   3. The remaining entries are sorted ascending by frequency (least-visited
//!      first, most-visited last) and written to `snapshot.browser_tabs`.
//!
//! The extension's POST body is unchanged — still `{url, title}` — so no
//! extension update is needed for the schema change.
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
//! `browser_tabs` in a local file, not a leak. A shared token would close that
//! gap but was declined as setup friction (see the plan).

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::thread;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::snapshot::{BrowserTabEntry, SnapshotStore};

/// Header every legitimate request must carry. Its mere presence, not its
/// value, is what matters — see the module doc for why that's enough to stop
/// a web page without needing a secret.
const EXTENSION_HEADER: &str = "X-HumCon-Extension";

/// Requests larger than this are rejected before being read into memory. A
/// tab's URL and title are a few hundred bytes at most; this leaves generous
/// headroom without letting a malformed or hostile client allocate freely.
const MAX_BODY_BYTES: u64 = 8 * 1024;

/// Tabs not seen for longer than this are pruned from the tracked set.
const MAX_IDLE_SECS: i64 = 3600; // 1 hour

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

    // In-memory tab store: URL → entry. Kept on this thread so no lock is
    // needed — `incoming_requests()` processes one request at a time.
    let mut tab_map: HashMap<String, TabState> = HashMap::new();

    for request in server.incoming_requests() {
        handle_request(store, request, &mut tab_map);
    }
}

/// Per-URL state kept in the in-memory map. Differs from `BrowserTabEntry`
/// only in that `last_seen` is stored as a parsed `DateTime` here for cheap
/// age comparisons, rather than as an ISO-8601 string.
struct TabState {
    title: Option<String>,
    frequency: u32,
    last_seen: DateTime<Utc>,
}

fn handle_request(
    store: &SnapshotStore,
    mut request: Request,
    tab_map: &mut HashMap<String, TabState>,
) {
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
                let entries = record_tab_switch(tab_map, update.url.clone(), update.title.clone(), Utc::now());

                if let Err(err) = store.update(|snapshot| {
                    snapshot.browser_tabs = entries;
                }) {
                    eprintln!("[browser] could not write snapshot: {err}");
                } else {
                    println!(
                        "[browser] tab switch → {} (freq {})",
                        update.url,
                        tab_map.get(&update.url).map_or(0, |s| s.frequency)
                    );
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

/// Updates tab map with the incoming switch, prunes entries older than 1 hour,
/// and returns the entries sorted ascending by frequency.
fn record_tab_switch(
    tab_map: &mut HashMap<String, TabState>,
    url: String,
    title: Option<String>,
    now: DateTime<Utc>,
) -> Vec<BrowserTabEntry> {
    let entry = tab_map.entry(url).or_insert(TabState {
        title: title.clone(),
        frequency: 0,
        last_seen: now,
    });
    entry.frequency += 1;
    entry.last_seen = now;
    if title.is_some() {
        entry.title = title;
    }

    let cutoff = now - Duration::seconds(MAX_IDLE_SECS);
    tab_map.retain(|_, v| v.last_seen > cutoff);

    let mut entries: Vec<BrowserTabEntry> = tab_map
        .iter()
        .map(|(url, state)| BrowserTabEntry {
            url: url.clone(),
            title: state.title.clone(),
            frequency: state.frequency,
            last_seen: state.last_seen.to_rfc3339(),
        })
        .collect();
    entries.sort_by_key(|e| e.frequency);
    entries
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

    #[test]
    fn tab_frequency_and_ascending_sort_order() {
        let mut map = HashMap::new();
        let now = Utc::now();

        // Switch to Tab A 3 times, Tab B 1 time, Tab C 2 times
        record_tab_switch(&mut map, "https://a.com".into(), Some("A".into()), now);
        record_tab_switch(&mut map, "https://a.com".into(), Some("A".into()), now);
        record_tab_switch(&mut map, "https://a.com".into(), Some("A".into()), now);
        record_tab_switch(&mut map, "https://b.com".into(), Some("B".into()), now);
        record_tab_switch(&mut map, "https://c.com".into(), Some("C".into()), now);
        let entries = record_tab_switch(&mut map, "https://c.com".into(), Some("C".into()), now);

        assert_eq!(entries.len(), 3);
        // Ascending by frequency: B (1), C (2), A (3)
        assert_eq!(entries[0].url, "https://b.com");
        assert_eq!(entries[0].frequency, 1);
        assert_eq!(entries[1].url, "https://c.com");
        assert_eq!(entries[1].frequency, 2);
        assert_eq!(entries[2].url, "https://a.com");
        assert_eq!(entries[2].frequency, 3);
    }

    #[test]
    fn tabs_idle_over_one_hour_are_pruned() {
        let mut map = HashMap::new();
        let t0 = Utc::now();

        record_tab_switch(&mut map, "https://old.com".into(), Some("Old".into()), t0);
        assert_eq!(map.len(), 1);

        // Advance time by 3601 seconds (> 1 hour)
        let t1 = t0 + Duration::seconds(3601);
        let entries = record_tab_switch(&mut map, "https://new.com".into(), Some("New".into()), t1);

        // Old tab must be pruned, only new tab remains
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://new.com");
        assert_eq!(entries[0].frequency, 1);
    }
}
