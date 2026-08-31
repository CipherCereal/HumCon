mod browser_server;
mod command_log;
// `pub` so `examples/test_summarize.rs` can build a `SnapshotStore` and call
// `summarize::handle_transcript` directly, without a running Tauri app.
pub mod snapshot;
pub mod summarize;
// `pub` for the same reason as `snapshot`/`summarize` above:
// `examples/test_transcribe.rs` drives the whisper.cpp → snapshot → summarize
// chain directly, without a microphone or a running Tauri app.
pub mod voice_note;
mod window_capture;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Manager, State};

use snapshot::SnapshotStore;

/// Returns snapshot.json's raw text so the resume card UI can render it.
///
/// Deliberately raw text, not a parsed `Snapshot` — a hand-edited file
/// missing a key would fail to deserialize entirely (serde errors on a
/// missing non-`Option` field), collapsing the whole card instead of
/// degrading gracefully, which is the opposite of what the UI needs to do
/// (architecture.md, Session 5). Normalizing tolerant-of-missing-keys shape
/// happens on the TS side, in `snapshot.ts`.
///
/// Read-only: never writes, never moves a corrupt file aside — that's
/// `SnapshotStore::load_or_create`'s job, untouched here.
#[tauri::command]
async fn read_snapshot_raw(store: State<'_, Arc<SnapshotStore>>) -> Result<String, String> {
    std::fs::read_to_string(store.path()).map_err(|e| e.to_string())
}

/// Directory holding HumCon's runtime state: `snapshot.json` and the shell
/// hook's `commands.jsonl`.
///
/// This process and the WSL bash hook must agree on one location, and the hook
/// has to be able to name it as a `/mnt/c/...` path, so state lives in the
/// user's home rather than Tauri's app-data dir. `~/.humcon` is
/// `C:\Users\<you>\.humcon` here and `/mnt/c/Users/<you>/.humcon` from WSL —
/// the same directory from both sides, which is the whole point.
///
/// `HUMCON_DIR` overrides it; keep that in sync with the hook's own default.
fn state_dir(app: &tauri::App) -> PathBuf {
    if let Some(raw) = std::env::var_os("HUMCON_DIR") {
        return PathBuf::from(raw);
    }

    match app.path().home_dir() {
        Ok(home) => home.join(".humcon"),
        Err(err) => {
            // Not ideal — the hook's hardcoded default won't match — but better
            // than refusing to start.
            eprintln!("[state] no home dir ({err}); falling back to the app data dir");
            app.path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
        }
    }
}

/// Where `snapshot.json` lives. `HUMCON_SNAPSHOT_PATH` overrides it, which is
/// handy in development for watching the file somewhere convenient.
///
/// Either way this is a plain Windows path. The app runs as a native Windows
/// process, so a WSL-only path would simply be invisible to it (CLAUDE.md).
fn snapshot_path(app: &tauri::App) -> PathBuf {
    if let Some(raw) = std::env::var_os("HUMCON_SNAPSHOT_PATH") {
        return PathBuf::from(raw);
    }

    state_dir(app).join("snapshot.json")
}

/// Where the bash hook appends commands. `HUMCON_COMMAND_LOG` overrides it.
///
/// Absent is normal: it simply means the hook has never run, and
/// `recent_commands` stays `[]`.
fn command_log_path(app: &tauri::App) -> PathBuf {
    if let Some(raw) = std::env::var_os("HUMCON_COMMAND_LOG") {
        return PathBuf::from(raw);
    }

    state_dir(app).join("commands.jsonl")
}

/// Port the browser extension's local HTTP receiver listens on.
/// `HUMCON_PORT` overrides it; keep that in sync with `extension/background.js`'s
/// own default if it's ever changed there.
///
/// Not wired to `app` today, but takes it for symmetry with the other
/// resolvers above and in case a future override needs app state (e.g. a
/// per-install random port persisted to disk).
fn browser_port(_app: &tauri::App) -> u16 {
    std::env::var("HUMCON_PORT")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(7423)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![read_snapshot_raw])
        .setup(|app| {
            let snapshot_file = snapshot_path(app);
            let command_log_file = command_log_path(app);
            let whisper = voice_note::WhisperConfig::resolve(&state_dir(app));
            println!("[snapshot] using {}", snapshot_file.display());
            println!("[commands] watching {}", command_log_file.display());
            println!("[voice_note] whisper binary {}", whisper.bin.display());

            let store = Arc::new(SnapshotStore::load_or_create(snapshot_file)?);

            // Every producer writes through the same store, so the single
            // Mutex<Snapshot> serializes them (architecture.md, Session 0).
            window_capture::spawn(Arc::clone(&store));
            command_log::spawn(Arc::clone(&store), command_log_file);
            browser_server::spawn(Arc::clone(&store), browser_port(app));

            // Event-driven rather than a polling thread: this one only does
            // work when the hotkey is pressed, so it adds no snapshot churn.
            voice_note::register(app, Arc::clone(&store), whisper);

            // Keep the store alive and reachable: read_snapshot_raw (above) uses
            // it to resolve the resume card UI's read path, and it's how any
            // future component would reach the shared writer.
            app.manage(store);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
