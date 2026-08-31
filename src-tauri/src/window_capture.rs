//! Windows active-window capture.
//!
//! Polls the OS foreground window every few seconds and writes what it finds
//! into `snapshot.json`'s `active_window` key. Nothing else in the file is
//! touched.
//!
//! The behaviour here is shaped by how `active-win-pos-rs` 0.11 actually works
//! on Windows, researched against its source in Session 1:
//!
//! * **`Err(())` is routine, not exceptional.** The crate's error type is the
//!   unit type, so it carries no detail whatsoever. Its first fallible step is
//!   a window-position lookup, which fails whenever `GetForegroundWindow()`
//!   returns NULL — and that happens constantly and harmlessly: while
//!   alt-tabbing, with a menu open, mid window-switch. The same `Err(())` also
//!   covers the lock screen and protected processes (antivirus UIs,
//!   SYSTEM-owned windows). We cannot tell these apart, so `Err` means "skip
//!   this tick" and is deliberately silent — logging it would produce noise
//!   every few seconds during ordinary use.
//! * **An empty title is `Ok("")`, not an error.** The crate's title helper has
//!   no failure path at all, so a genuine `GetWindowTextW` failure and a truly
//!   blank caption are indistinguishable. Both become `window_title: null`.
//! * **Desktop focus looks like an ordinary app** — see `is_shell_desktop`.
//! * **Long titles are silently truncated** at 254 UTF-16 code units by a
//!   fixed-size buffer in the crate, with no retry. Nothing we can do from
//!   here; recorded in architecture.md as a known limitation.
//! * Titles are decoded with `String::from_utf16_lossy`, so unpaired
//!   surrogates become U+FFFD instead of panicking. Non-English and emoji
//!   titles are safe.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::snapshot::{now_iso8601, ActiveWindowSnapshot, SnapshotStore};

/// How often to ask the OS what the foreground window is.
///
/// Each call reads the focused executable's version resource off disk, so this
/// shouldn't become a tight loop. A few seconds is comfortable.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// The title Windows gives its own desktop/shell window.
const SHELL_DESKTOP_TITLE: &str = "Program Manager";

/// The executable that owns the desktop window.
const SHELL_PROCESS_STEM: &str = "explorer";

/// How many consecutive write failures between repeat reports.
///
/// A transient failure is expected now and then (a reader holding the file
/// open is enough), but a permanently unwritable path — an ACL problem, an
/// antivirus lock — would otherwise print on every single poll forever. At a
/// 3-second interval this works out to roughly one line every five minutes.
const WRITE_FAILURE_REPEAT: u32 = 100;

/// Start the capture loop on its own thread and return immediately.
///
/// The thread runs for the life of the process. The crate's Windows path needs
/// no COM initialization, no message pump and no particular thread, and keeps
/// no state between calls, so a plain background thread is all this needs.
pub fn spawn(store: Arc<SnapshotStore>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("humcon-window-capture".into())
        .spawn(move || run(&store))
        .expect("failed to spawn window capture thread")
}

fn run(store: &SnapshotStore) {
    // The last state we printed, so a steady state doesn't spam the console
    // every POLL_INTERVAL. Note we still *write* on every successful poll —
    // only the logging is deduplicated.
    let mut last_logged: Option<ActiveWindowSnapshot> = None;

    // Run of consecutive failed writes, so a persistently unwritable snapshot
    // path can't turn into an error line every POLL_INTERVAL. See
    // WRITE_FAILURE_REPEAT.
    let mut consecutive_write_failures: u32 = 0;

    loop {
        if let Some(captured) = poll_once() {
            let is_new = last_logged
                .as_ref()
                .map(|previous| !previous.same_window_as(&captured))
                .unwrap_or(true);

            if is_new {
                println!("[window] {}", describe(&captured));
                last_logged = Some(captured.clone());
            }

            // Only `active_window` is assigned. Every other key is serialized
            // back out untouched by the store.
            match store.update(|snapshot| snapshot.active_window = Some(captured)) {
                Ok(()) => {
                    if consecutive_write_failures > 0 {
                        println!(
                            "[window] snapshot writes recovered after {consecutive_write_failures} consecutive failure(s)"
                        );
                        consecutive_write_failures = 0;
                    }
                }
                Err(err) => {
                    consecutive_write_failures += 1;
                    // Report the first one immediately, then only every
                    // WRITE_FAILURE_REPEAT-th, so the problem stays visible
                    // without drowning the log.
                    if consecutive_write_failures == 1
                        || consecutive_write_failures % WRITE_FAILURE_REPEAT == 0
                    {
                        eprintln!(
                            "[window] could not write snapshot (consecutive failure #{consecutive_write_failures}): {err}"
                        );
                    }
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
}

/// One capture attempt.
///
/// `None` means "nothing worth recording this tick" — either the crate
/// reported an error, or the desktop has focus. In both cases the caller
/// leaves the previous `active_window` value in place, so a transient blip
/// doesn't erase the context we're trying to preserve.
fn poll_once() -> Option<ActiveWindowSnapshot> {
    // `.ok()?` discards an error that carries no information anyway; see the
    // module docs for why this is expected rather than a problem.
    let window = active_win_pos_rs::get_active_window().ok()?;

    map_capture(&window.app_name, &window.title, &window.process_path)
}

/// Turn raw crate output into the frozen schema's shape, or `None` if this
/// window shouldn't be recorded.
///
/// Split out from `poll_once` so it can be tested without a real desktop.
fn map_capture(app_name: &str, title: &str, process_path: &Path) -> Option<ActiveWindowSnapshot> {
    let title = title.trim();

    if is_shell_desktop(process_path, title) {
        return None;
    }

    Some(ActiveWindowSnapshot {
        app_name: resolve_app_name(app_name, process_path),
        // The schema makes `window_title` nullable, which is exactly where a
        // caption-less window belongs.
        window_title: if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        },
        captured_at: now_iso8601(),
    })
}

/// Is this the Windows desktop itself, rather than an application?
///
/// Pressing Win+D or clicking the wallpaper gives focus to the shell's
/// "Program Manager" window, and the crate reports it as a completely ordinary
/// app: `app_name: "Windows Explorer"`, a real process path, real bounds. We
/// treat it as "no active window" so that minimizing everything leaves the
/// last real app in the snapshot — that's the context the resume card exists
/// to show. (The lock screen ends up the same way for free, since it comes
/// back as `Err`.)
///
/// Both conditions are required: ordinary Explorer *file* windows are also
/// explorer.exe, but their title is the folder name, so they still get
/// recorded normally.
fn is_shell_desktop(process_path: &Path, trimmed_title: &str) -> bool {
    let is_explorer = process_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case(SHELL_PROCESS_STEM))
        .unwrap_or(false);

    is_explorer && trimmed_title == SHELL_DESKTOP_TITLE
}

/// Pick the string for `app_name`, which the schema requires to be present and
/// non-null.
///
/// The crate's `app_name` is the executable's `FileDescription` version
/// resource — "Visual Studio Code" rather than "Code.exe" — which reads well
/// on a resume card. It is localized and version-dependent though, so treat it
/// as a display name, not a stable identifier; `process_path` is the thing to
/// key off if we ever need one.
///
/// It can also come back empty for binaries with no version resource, hence
/// the fallbacks: the executable's own stem, then a last-resort literal so
/// this non-nullable field always holds something.
fn resolve_app_name(app_name: &str, process_path: &Path) -> String {
    let app_name = app_name.trim();
    if !app_name.is_empty() {
        return app_name.to_string();
    }

    if let Some(stem) = process_path.file_stem().and_then(|stem| stem.to_str()) {
        if !stem.is_empty() {
            return stem.to_string();
        }
    }

    "Unknown".to_string()
}

fn describe(captured: &ActiveWindowSnapshot) -> String {
    match &captured.window_title {
        Some(title) => format!("{} — {}", captured.app_name, title),
        None => format!("{} (no title)", captured.app_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(raw: &str) -> PathBuf {
        PathBuf::from(raw)
    }

    #[test]
    fn ordinary_window_maps_across() {
        let captured = map_capture(
            "Visual Studio Code",
            "architecture.md - HumCon",
            &path(r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe"),
        )
        .expect("a normal window should be recorded");

        assert_eq!(captured.app_name, "Visual Studio Code");
        assert_eq!(
            captured.window_title.as_deref(),
            Some("architecture.md - HumCon")
        );
        assert!(captured.captured_at.ends_with('Z'));
    }

    #[test]
    fn empty_title_becomes_null() {
        let captured = map_capture("Some App", "", &path(r"C:\apps\some_app.exe")).unwrap();
        assert_eq!(captured.window_title, None);
        // The non-nullable field still has to hold something real.
        assert_eq!(captured.app_name, "Some App");
    }

    #[test]
    fn whitespace_only_title_becomes_null() {
        let captured = map_capture("Some App", "   \t  ", &path(r"C:\apps\some_app.exe")).unwrap();
        assert_eq!(captured.window_title, None);
    }

    #[test]
    fn title_is_trimmed() {
        let captured = map_capture("Some App", "  Untitled  ", &path(r"C:\apps\a.exe")).unwrap();
        assert_eq!(captured.window_title.as_deref(), Some("Untitled"));
    }

    #[test]
    fn desktop_focus_is_skipped() {
        assert!(map_capture(
            "Windows Explorer",
            "Program Manager",
            &path(r"C:\Windows\explorer.exe"),
        )
        .is_none());
    }

    #[test]
    fn explorer_file_window_is_still_recorded() {
        let captured = map_capture(
            "Windows Explorer",
            "Downloads",
            &path(r"C:\Windows\explorer.exe"),
        )
        .expect("a real Explorer folder window is not the desktop");

        assert_eq!(captured.window_title.as_deref(), Some("Downloads"));
    }

    #[test]
    fn program_manager_title_from_another_process_is_recorded() {
        // Only explorer.exe owns the real desktop; an app that happens to use
        // this title is a normal window.
        assert!(map_capture("Impostor", "Program Manager", &path(r"C:\apps\impostor.exe")).is_some());
    }

    #[test]
    fn missing_app_name_falls_back_to_exe_stem() {
        let captured = map_capture("", "Some window", &path(r"C:\tools\ffprobe.exe")).unwrap();
        assert_eq!(captured.app_name, "ffprobe");
    }

    #[test]
    fn missing_app_name_and_path_falls_back_to_literal() {
        let captured = map_capture("   ", "Some window", &path("")).unwrap();
        assert_eq!(captured.app_name, "Unknown");
    }

    #[test]
    fn non_ascii_and_replacement_chars_survive() {
        // The crate decodes titles lossily, so U+FFFD can legitimately reach
        // us; it must pass through rather than trip anything up.
        let captured = map_capture(
            "メモ帳",
            "設計メモ 📝 \u{FFFD}",
            &path(r"C:\Windows\notepad.exe"),
        )
        .unwrap();

        assert_eq!(captured.app_name, "メモ帳");
        assert_eq!(captured.window_title.as_deref(), Some("設計メモ 📝 \u{FFFD}"));
    }

    #[test]
    fn same_window_as_ignores_timestamp_but_not_title() {
        let a = map_capture("App", "Tab one", &path(r"C:\a.exe")).unwrap();
        let mut b = a.clone();
        b.captured_at = "1999-01-01T00:00:00Z".to_string();
        assert!(a.same_window_as(&b), "captured_at must not affect identity");

        let c = map_capture("App", "Tab two", &path(r"C:\a.exe")).unwrap();
        assert!(!a.same_window_as(&c), "a title change is a change we care about");
    }
}
