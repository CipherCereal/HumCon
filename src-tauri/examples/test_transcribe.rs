//! Manual verification tool, not part of the app.
//!
//! Runs the whole post-recording chain — whisper.cpp → `voice_note.transcript`
//! → `summarize::handle_transcript` — against an existing WAV file, with no
//! microphone, no hotkey and no running Tauri app. That isolates whisper.cpp
//! problems from recording problems, which is the split that matters when this
//! component misbehaves.
//!
//! Run with:
//!   cargo run --example test_transcribe                 # uses whisper/jfk.wav
//!   cargo run --example test_transcribe -- path\to.wav
//!
//! Writes to a scratch snapshot (`transcribe-probe.json`) rather than the real
//! one, so a probe run never clobbers live state.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use humcon_lib::snapshot::SnapshotStore;
use humcon_lib::voice_note::{transcribe_and_deliver, WhisperConfig};

fn main() {
    // Same default the app uses: ~/.humcon, overridable with HUMCON_DIR.
    let state_dir = env::var_os("HUMCON_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .expect("no USERPROFILE or HOME set");
            PathBuf::from(home).join(".humcon")
        });

    let mut config = WhisperConfig::resolve(&state_dir);

    // `transcribe` always reads config.wav, so pointing that at the file under
    // test is how we feed it a canned recording.
    if let Some(arg) = env::args().nth(1) {
        config.wav = PathBuf::from(arg);
    } else {
        config.wav = state_dir.join("whisper").join("jfk.wav");
    }

    println!("binary : {}", config.bin.display());
    println!("model  : {}", config.model.display());
    println!("wav    : {}", config.wav.display());
    println!();

    if !config.wav.exists() {
        eprintln!(
            "no WAV at {} — pass one as an argument",
            config.wav.display()
        );
        return;
    }

    let snapshot_path = state_dir.join("transcribe-probe.json");
    let store = Arc::new(
        SnapshotStore::load_or_create(snapshot_path.clone()).expect("could not open the snapshot"),
    );

    transcribe_and_deliver(&store, &config);

    // handle_transcript summarizes on its own thread; give it a moment so the
    // printed snapshot below reflects the finished result.
    thread::sleep(Duration::from_secs(6));

    println!();
    match std::fs::read_to_string(&snapshot_path) {
        Ok(contents) => println!("{} =\n{contents}", snapshot_path.display()),
        Err(err) => eprintln!("could not read {}: {err}", snapshot_path.display()),
    }
}
