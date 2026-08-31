//! Shell command log reader.
//!
//! The WSL bash hook (`hooks/humcon-log.sh`) appends one JSON object per
//! interactive command to a plain-text log. This module tails that log and
//! merges the last few entries into `snapshot.json`'s `recent_commands` key.
//! Nothing else in the file is touched.
//!
//! Why the split? The hook cannot write `snapshot.json` itself. Session 0
//! settled that all snapshot writes funnel through one `Mutex<Snapshot>`, and
//! the hook runs in a *separate process* where that mutex means nothing — while
//! the window-capture poller is rewriting the whole file every few seconds. A
//! bash-side read-modify-write would clobber `active_window`. So the hook only
//! ever appends to its own file, and this module — inside the Tauri process,
//! going through the same shared writer as every other component — is what
//! actually touches the snapshot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::snapshot::{RecentCommand, SnapshotStore};

/// How often to check the log for new commands.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Session 0's frozen decision: keep the last 20, dropping oldest first.
const MAX_RECENT_COMMANDS: usize = 20;

/// Failures between repeat reports, mirroring `window_capture` so a broken log
/// path can't produce an error line on every single poll.
const READ_FAILURE_REPEAT: u32 = 150;

/// Start the reader on its own thread and return immediately.
pub fn spawn(store: Arc<SnapshotStore>, log_path: PathBuf) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("humcon-command-log".into())
        .spawn(move || run(&store, &log_path))
        .expect("failed to spawn command log thread")
}

fn run(store: &SnapshotStore, log_path: &Path) {
    // The last batch we actually wrote, so a quiet log costs nothing.
    let mut last_written: Vec<RecentCommand> = Vec::new();
    let mut consecutive_read_failures: u32 = 0;

    loop {
        // Deliberately re-read every poll rather than gating on fs::metadata's
        // (mtime, len). That gate was tried first and *silently stopped
        // firing* partway through a soak test: the log is appended from WSL
        // through the 9p server onto NTFS, and Windows directory-entry
        // metadata can lag behind the file's real size while another process
        // holds the handle. Stale metadata meant "no change" and the component
        // quietly stopped updating. The log is bounded to ~64 KB by the hook,
        // so reading it every couple of seconds is far cheaper than a class of
        // bug that looks exactly like the feature not working.
        match fs::read_to_string(log_path) {
            Ok(text) => {
                if consecutive_read_failures > 0 {
                    println!(
                        "[commands] log readable again after {consecutive_read_failures} failure(s)"
                    );
                    consecutive_read_failures = 0;
                }

                let entries = parse_log(&text);

                // Only write when the result actually differs — the window
                // poller is already rewriting this file every few seconds and
                // doesn't need help.
                if entries != last_written {
                    println!(
                        "[commands] {} entr{} -> snapshot",
                        entries.len(),
                        if entries.len() == 1 { "y" } else { "ies" }
                    );
                    last_written = entries.clone();

                    // Only `recent_commands` is assigned; the store serializes
                    // every other key back out unchanged.
                    if let Err(err) = store.update(|snapshot| snapshot.recent_commands = entries) {
                        eprintln!("[commands] could not write snapshot: {err}");
                    }
                }
            }

            // A missing log is the normal state before the hook has ever been
            // sourced — leave `recent_commands` at `[]` and stay quiet.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}

            Err(err) => {
                consecutive_read_failures += 1;
                if consecutive_read_failures == 1
                    || consecutive_read_failures % READ_FAILURE_REPEAT == 0
                {
                    eprintln!(
                        "[commands] could not read {} (consecutive failure #{consecutive_read_failures}): {err}",
                        log_path.display()
                    );
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

/// Parse the raw log into at most `MAX_RECENT_COMMANDS` entries, oldest first.
///
/// Lines that don't parse are skipped rather than failing the batch. That
/// matters in practice: the hook appends from a live shell, so the last line
/// can be torn mid-write, and a command containing invalid UTF-8 would produce
/// a line serde can't read. One bad line must not cost us the other nineteen.
///
/// Split out as a pure function so the cap, the ordering and the
/// skip-malformed behaviour are testable without a shell.
fn parse_log(text: &str) -> Vec<RecentCommand> {
    let all: Vec<RecentCommand> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<RecentCommand>(line).ok())
        .collect();

    // Keep the tail: the most recent commands are the interesting ones, and
    // Session 0 specified drop-oldest.
    let start = all.len().saturating_sub(MAX_RECENT_COMMANDS);
    all[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(cmd: &str) -> String {
        format!(r#"{{"command":"{cmd}","ran_at":"2026-08-29T04:00:00Z"}}"#)
    }

    #[test]
    fn empty_log_yields_nothing() {
        assert!(parse_log("").is_empty());
        assert!(parse_log("\n\n   \n").is_empty());
    }

    #[test]
    fn parses_entries_in_order() {
        let text = format!("{}\n{}\n{}\n", line("first"), line("second"), line("third"));
        let got = parse_log(&text);
        let commands: Vec<&str> = got.iter().map(|e| e.command.as_str()).collect();
        assert_eq!(commands, ["first", "second", "third"]);
        assert_eq!(got[0].ran_at, "2026-08-29T04:00:00Z");
    }

    #[test]
    fn malformed_line_is_skipped_without_losing_the_batch() {
        let text = format!(
            "{}\n{{ not json at all\n{}\n",
            line("before"),
            line("after")
        );
        let got = parse_log(&text);
        let commands: Vec<&str> = got.iter().map(|e| e.command.as_str()).collect();
        assert_eq!(commands, ["before", "after"]);
    }

    #[test]
    fn torn_final_line_is_skipped() {
        // A shell appending concurrently can leave the last line incomplete.
        let text = format!("{}\n{{\"command\":\"half-writ", line("complete"));
        let got = parse_log(&text);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].command, "complete");
    }

    #[test]
    fn caps_at_twenty_keeping_the_most_recent_in_order() {
        let mut text = String::new();
        for i in 1..=25 {
            text.push_str(&line(&format!("cmd{i}")));
            text.push('\n');
        }
        let got = parse_log(&text);
        assert_eq!(got.len(), MAX_RECENT_COMMANDS);
        // Oldest five dropped, order preserved.
        assert_eq!(got.first().unwrap().command, "cmd6");
        assert_eq!(got.last().unwrap().command, "cmd25");
    }

    #[test]
    fn exactly_twenty_is_untouched() {
        let mut text = String::new();
        for i in 1..=20 {
            text.push_str(&line(&format!("cmd{i}")));
            text.push('\n');
        }
        let got = parse_log(&text);
        assert_eq!(got.len(), 20);
        assert_eq!(got.first().unwrap().command, "cmd1");
    }

    #[test]
    fn escaped_quotes_and_backslashes_round_trip() {
        // What the hook produces for:  echo "hi \ there"
        let raw = r#"{"command":"echo \"hi \\ there\"","ran_at":"2026-08-29T04:00:00Z"}"#;
        let got = parse_log(raw);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].command, r#"echo "hi \ there""#);
    }

    #[test]
    fn non_ascii_commands_survive() {
        let raw = r#"{"command":"grep 設計 メモ.txt 📝","ran_at":"2026-08-29T04:00:00Z"}"#;
        let got = parse_log(raw);
        assert_eq!(got[0].command, "grep 設計 メモ.txt 📝");
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        // The log lives on a Windows-visible mount; be forgiving.
        let text = format!("{}\r\n{}\r\n", line("one"), line("two"));
        let got = parse_log(&text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].command, "two");
    }
}
