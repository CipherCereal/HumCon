//! Manual verification tool, not part of the app.
//!
//! Records from the default microphone for a few seconds, then runs the same
//! whisper.cpp → snapshot → summarize chain the hotkey does. This is the
//! recording half that `test_transcribe` deliberately skips, driven on a timer
//! instead of a keypress so it can be checked without a running Tauri app.
//!
//! Run with (from a Windows terminal, so the mic and env vars behave):
//!   cargo run --example test_record
//!   cargo run --example test_record -- 10        # record for 10 seconds
//!
//! Writes to a scratch snapshot (`record-probe.json`), never the real one.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use humcon_lib::snapshot::SnapshotStore;
use humcon_lib::voice_note::{start_recording, stop_recording, transcribe_and_deliver, WhisperConfig};

fn main() {
    let seconds: u64 = env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);

    let state_dir = env::var_os("HUMCON_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .expect("no USERPROFILE or HOME set");
            PathBuf::from(home).join(".humcon")
        });

    let config = WhisperConfig::resolve(&state_dir);
    println!("recording to {}", config.wav.display());

    let recording = match start_recording(&config.wav) {
        Ok(recording) => recording,
        Err(err) => {
            eprintln!("could not start recording: {err}");
            return;
        }
    };

    println!("🎤 speak now — recording for {seconds}s…");
    thread::sleep(Duration::from_secs(seconds));

    if let Err(err) = stop_recording(recording) {
        eprintln!("could not finish the recording: {err}");
        return;
    }
    println!("recording stopped, transcribing…");

    let snapshot_path = state_dir.join("record-probe.json");
    let store = Arc::new(
        SnapshotStore::load_or_create(snapshot_path.clone()).expect("could not open the snapshot"),
    );

    transcribe_and_deliver(&store, &config);

    // Summarization runs on its own thread; wait so the printout below is final.
    thread::sleep(Duration::from_secs(6));

    println!();
    match std::fs::read_to_string(&snapshot_path) {
        Ok(contents) => println!("{} =\n{contents}", snapshot_path.display()),
        Err(err) => eprintln!("could not read {}: {err}", snapshot_path.display()),
    }
}
