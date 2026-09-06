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

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::snapshot::{RecentCommand, SnapshotStore};

/// How often to check the log for new commands.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Session 0's frozen decision: keep the last 20, dropping oldest first.
const MAX_RECENT_COMMANDS: usize = 20;

/// Failures between repeat reports, mirroring `window_capture` so a broken log
/// path can't produce an error line on every single poll.
const READ_FAILURE_REPEAT: u32 = 150;

const MAX_LOG_BYTES: u64 = 65536;
const LOG_KEEP_LINES: usize = 100;

/// Resolves PowerShell's PSReadLine history file path on Windows.
fn powershell_history_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("PowerShell")
            .join("PSReadLine")
            .join("ConsoleHost_history.txt")
    })
}

/// Checks if a command matches secret-like patterns and should not be logged.
pub fn is_secret_command(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "bearer",
        "credential",
        "private_key",
        "private-key",
        "--password=",
        "ssh-add",
    ];

    for pat in PATTERNS {
        if lower.contains(pat) {
            return true;
        }
    }
    false
}

fn append_command_to_log(log_path: &Path, cmd: &str) -> io::Result<()> {
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let entry = RecentCommand {
        command: cmd.to_string(),
        ran_at: crate::snapshot::now_iso8601(),
    };
    let json_line = serde_json::to_string(&entry)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", json_line)?;
    file.flush()?;

    // Keep log bounded to ~64KB
    if let Ok(metadata) = fs::metadata(log_path) {
        if metadata.len() > MAX_LOG_BYTES {
            if let Ok(content) = fs::read_to_string(log_path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() > LOG_KEEP_LINES {
                    let keep_from = lines.len() - LOG_KEEP_LINES;
                    let trimmed = lines[keep_from..].join("\n") + "\n";
                    let _ = fs::write(log_path, trimmed);
                }
            }
        }
    }

    Ok(())
}

fn read_new_ps_commands(ps_path: &Path, last_line_count: &mut usize) -> Vec<String> {
    let Ok(content) = fs::read_to_string(ps_path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total <= *last_line_count {
        *last_line_count = total;
        return Vec::new();
    }

    let new_entries: Vec<String> = lines[*last_line_count..]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !is_secret_command(s))
        .map(|s| s.to_string())
        .collect();

    *last_line_count = total;
    new_entries
}

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

    let ps_history = powershell_history_path();
    let mut last_ps_lines = if let Some(ref ps_path) = ps_history {
        fs::read_to_string(ps_path)
            .map(|c| c.lines().count())
            .unwrap_or(0)
    } else {
        0
    };

    // If commands.jsonl is absent or empty, seed the tail of PS history
    if (!log_path.exists() || fs::metadata(log_path).map(|m| m.len() == 0).unwrap_or(true))
        && last_ps_lines > 0
    {
        if let Some(ref ps_path) = ps_history {
            if let Ok(content) = fs::read_to_string(ps_path) {
                let lines: Vec<&str> = content.lines().collect();
                let seed_start = lines.len().saturating_sub(MAX_RECENT_COMMANDS);
                for line in &lines[seed_start..] {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !is_secret_command(trimmed) {
                        let _ = append_command_to_log(log_path, trimmed);
                    }
                }
            }
        }
    }

    loop {
        // Automatically tail commands from PowerShell history
        if let Some(ref ps_path) = ps_history {
            let new_cmds = read_new_ps_commands(ps_path, &mut last_ps_lines);
            for cmd in new_cmds {
                let already_logged_recently = last_written.last().map_or(false, |last| {
                    if last.command == cmd {
                        if let Ok(dt) = DateTime::parse_from_rfc3339(&last.ran_at) {
                            return Utc::now().signed_duration_since(dt).num_seconds() < 2;
                        }
                    }
                    false
                });

                if !already_logged_recently {
                    let _ = append_command_to_log(log_path, &cmd);
                }
            }
        }

        // Re-read commands.jsonl every poll
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

            // A missing log is the normal state before any hook has run —
            // leave `recent_commands` at `[]` and stay quiet.
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

    #[test]
    fn secret_commands_are_classified_correctly() {
        assert!(is_secret_command("export API_KEY=12345"));
        assert!(is_secret_command("mysql --password=secret"));
        assert!(is_secret_command("curl -H 'Authorization: Bearer token123'"));
        assert!(is_secret_command("ssh-add ~/.ssh/id_rsa"));
        assert!(is_secret_command("cat ~/.secret.env"));
        assert!(!is_secret_command("git status"));
        assert!(!is_secret_command("npm run tauri dev"));
        assert!(!is_secret_command("cargo test"));
    }

    #[test]
    fn read_new_ps_commands_only_yields_new_non_secret_lines() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let ps_file = temp_dir.path().join("ConsoleHost_history.txt");

        fs::write(
            &ps_file,
            "git status\nexport API_KEY=123\n\nnpm run dev\n",
        )
        .expect("write ps history");

        let mut line_count = 0;
        let first_read = read_new_ps_commands(&ps_file, &mut line_count);
        assert_eq!(first_read, vec!["git status", "npm run dev"]);
        assert_eq!(line_count, 4);

        // Subsequent read with no changes yields nothing
        let second_read = read_new_ps_commands(&ps_file, &mut line_count);
        assert!(second_read.is_empty());

        // Appending new lines
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&ps_file)
            .expect("open ps history");
        writeln!(f, "cargo build").expect("write line");
        writeln!(f, "ssh-add key").expect("write line");
        writeln!(f, "cargo test").expect("write line");

        let third_read = read_new_ps_commands(&ps_file, &mut line_count);
        assert_eq!(third_read, vec!["cargo build", "cargo test"]);
        assert_eq!(line_count, 7);
    }
}
