//! Verification tool, not part of the app.
//!
//! Prints exactly what `active-win-pos-rs` returns for the foreground window,
//! unfiltered and unmapped, once a second. Its purpose is to let a test tell
//! "our filter correctly dropped this window" apart from "this window never
//! actually had focus" — the two look identical if you only inspect
//! snapshot.json.
//!
//! Run with: cargo run --example raw_active_window

use std::thread;
use std::time::Duration;

fn main() {
    println!("secs\tresult");
    for tick in 0.. {
        match active_win_pos_rs::get_active_window() {
            Ok(w) => println!(
                "{tick}\tOK   app_name={:?} title={:?} title_utf16_len={} exe={:?}",
                w.app_name,
                w.title,
                w.title.encode_utf16().count(),
                w.process_path.file_name().unwrap_or_default()
            ),
            // The unit error carries no detail; that is the crate's API.
            Err(()) => println!("{tick}\tERR  (no active window / inaccessible)"),
        }
        thread::sleep(Duration::from_secs(1));
    }
}
