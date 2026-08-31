//! Manual verification tool, not part of the app.
//!
//! Runs `summarize::handle_transcript` against a handful of fake transcripts
//! and prints the resulting `voice_note` for each, so failure handling
//! (empty transcript, missing API key, a real API call) can be eyeballed
//! without wiring up whisper.cpp first.
//!
//! Run with: ANTHROPIC_API_KEY=... cargo run --example test_summarize

use std::env;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use humcon_lib::snapshot::SnapshotStore;
use humcon_lib::summarize::handle_transcript;

const TRANSCRIPTS: &[(&str, &str)] = &[
    (
        "normal",
        "Okay so I need to finish the quarterly report before lunch, and then \
         I should follow up with Priya about the invoice she sent last week.",
    ),
    ("very short", "call mom back"),
    ("empty", ""),
    (
        "filler and unusual punctuation",
        "uh... so, like, I guess I'm gonna -- try to fix that bug in the, \
         um, login flow?? before standup I think.",
    ),
];

fn main() {
    if env::var("ANTHROPIC_API_KEY").is_err() {
        println!(
            "[test_summarize] ANTHROPIC_API_KEY is not set — every case below \
             should degrade to summary: null via SummarizeError::MissingApiKey."
        );
    }

    let dir = std::env::temp_dir().join(format!(
        "humcon_test_summarize_{}",
        std::process::id()
    ));
    let snapshot_path = dir.join("snapshot.json");

    for (label, transcript) in TRANSCRIPTS {
        let store = Arc::new(
            SnapshotStore::load_or_create(snapshot_path.clone())
                .expect("failed to create test snapshot store"),
        );

        handle_transcript(Arc::clone(&store), transcript.to_string());

        // handle_transcript runs on its own thread; give it a moment before
        // reading the file back for this simple manual probe.
        thread::sleep(Duration::from_secs(3));

        let raw = std::fs::read_to_string(&snapshot_path).expect("snapshot should exist");
        let json: serde_json::Value =
            serde_json::from_str(&raw).expect("snapshot should be valid json");
        println!("--- {label} ---");
        println!("transcript: {transcript:?}");
        println!("voice_note: {}", json["voice_note"]);
        println!();
    }

    let _ = std::fs::remove_dir_all(&dir);
}
