//! Voice note capture: global hotkey → microphone recording → whisper.cpp →
//! [`crate::summarize::handle_transcript`].
//!
//! architecture.md flagged this as the highest-risk component, so it is built
//! to fail in pieces rather than all at once:
//!
//! * The hotkey registers even if whisper.cpp is not installed. You get a
//!   clear startup warning, and a recording still produces a WAV file.
//! * Microphone failure is reported at the moment you press the hotkey, not
//!   silently at the end of a recording you thought was working.
//! * [`transcribe`] is a single seam. If whisper.cpp could not be made to
//!   work, only that function's body needs replacing with a fixed string —
//!   every other stage stays real.
//!
//! Only `voice_note.transcript` is written here, through the one shared
//! [`SnapshotStore`] writer (architecture.md, Session 0). `summary` and
//! `recorded_at` belong to `summarize.rs` and are never touched from here.
//!
//! **Threading.** `cpal::Stream` is `Send + Sync` in cpal 0.18 (the WASAPI
//! backend asserts both, and the real audio thread is one cpal spawns
//! internally — the handle we hold is just a control channel), so the stream
//! lives directly in the controller's mutex. Stopping is `drop(stream)`, which
//! terminates and joins that internal thread for us. Only the whisper.cpp call
//! gets its own thread, because it takes seconds and must not block the hotkey
//! handler.

use std::fmt;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SupportedStreamConfig};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::snapshot::SnapshotStore;
use crate::summarize;

/// The toggle hotkey: press once to start recording, again to stop.
///
/// Ctrl+Alt+Space is not claimed by Windows itself or by the apps this project
/// was tested against. If another app has already taken it, registration fails
/// loudly at startup (see [`register`]) rather than silently doing nothing.
pub const HOTKEY: &str = "Ctrl+Alt+Space";

/// Suppresses the console window that would otherwise flash on every
/// `whisper-cli.exe` spawn. `CREATE_NO_WINDOW`, from the Win32 process
/// creation flags.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where the whisper.cpp binary, its model, and the scratch recording live.
///
/// Resolved once at startup from the shared state dir, with the same env-var
/// override convention as `HUMCON_SNAPSHOT_PATH` / `HUMCON_COMMAND_LOG`.
#[derive(Debug, Clone)]
pub struct WhisperConfig {
    pub bin: PathBuf,
    pub model: PathBuf,
    /// Overwritten by every recording — this is scratch space, not history.
    pub wav: PathBuf,
}

impl WhisperConfig {
    /// `HUMCON_WHISPER_BIN` / `HUMCON_WHISPER_MODEL` override the defaults
    /// under `<state_dir>/whisper/`.
    pub fn resolve(state_dir: &Path) -> Self {
        let whisper_dir = state_dir.join("whisper");

        let bin = std::env::var_os("HUMCON_WHISPER_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| whisper_dir.join("whisper-cli.exe"));

        let model = std::env::var_os("HUMCON_WHISPER_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|| whisper_dir.join("ggml-base.en.bin"));

        Self {
            bin,
            model,
            wav: whisper_dir.join("last-recording.wav"),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a recording could not be started or finished.
///
/// Carries `String`s rather than the underlying `cpal` error types, because
/// the only thing we do with them is print them, and this keeps the mic-denied
/// classification (which has to sniff a message string — see
/// [`classify_stream_error`]) in one place.
#[derive(Debug)]
pub enum RecordError {
    NoInputDevice,
    Config(String),
    /// Windows refused microphone access. Worth its own variant because the
    /// fix is a specific Settings page, not anything the user can debug from
    /// a generic backend error.
    MicAccessDenied(String),
    BuildStream(String),
    Play(String),
    UnsupportedSampleFormat(String),
    Wav(String),
}

/// The Windows microphone privacy setting, spelled out. Referenced by both the
/// no-device and access-denied messages.
const MIC_PRIVACY_HINT: &str = "Settings → Privacy & security → Microphone → \
                                'Let desktop apps access your microphone'";

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoInputDevice => write!(
                f,
                "no microphone found. If one is plugged in, check {MIC_PRIVACY_HINT}"
            ),
            Self::Config(detail) => write!(f, "could not read the mic's default config: {detail}"),
            Self::MicAccessDenied(detail) => write!(
                f,
                "Windows denied microphone access ({detail}). Enable it under {MIC_PRIVACY_HINT}"
            ),
            Self::BuildStream(detail) => write!(f, "could not open the mic: {detail}"),
            Self::Play(detail) => write!(f, "could not start the mic stream: {detail}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(f, "mic reports a sample format we cannot record: {format}")
            }
            Self::Wav(detail) => write!(f, "could not write the WAV file: {detail}"),
        }
    }
}

impl std::error::Error for RecordError {}

/// Why transcription did not produce text.
#[derive(Debug)]
pub enum TranscribeError {
    BinaryMissing(PathBuf),
    ModelMissing(PathBuf),
    Spawn(std::io::Error),
    Failed { code: Option<i32>, stderr: String },
}

impl fmt::Display for TranscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BinaryMissing(path) => write!(
                f,
                "whisper-cli.exe not found at {} — see the whisper.cpp setup \
                 note in architecture.md",
                path.display()
            ),
            Self::ModelMissing(path) => write!(
                f,
                "whisper model not found at {} — see the whisper.cpp setup \
                 note in architecture.md",
                path.display()
            ),
            Self::Spawn(err) => write!(f, "could not run whisper-cli.exe: {err}"),
            Self::Failed { code, stderr } => match code {
                Some(code) => write!(f, "whisper-cli.exe exited with {code}: {stderr}"),
                None => write!(f, "whisper-cli.exe was terminated by a signal: {stderr}"),
            },
        }
    }
}

impl std::error::Error for TranscribeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(err) => Some(err),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// A recording in flight.
///
/// Holding the `Stream` here *is* the recording: cpal's internal audio thread
/// feeds `writer` until this struct is dropped.
///
/// Public (with private fields) so `examples/test_record.rs` can drive the
/// same start/stop pair the hotkey does, on a timer instead of a keypress.
pub struct ActiveRecording {
    stream: cpal::Stream,
    writer: WavWriterHandle,
}

/// Owns the idle/recording state that the hotkey toggles between.
pub struct VoiceNoteController {
    recording: Mutex<Option<ActiveRecording>>,
    config: WhisperConfig,
    store: Arc<SnapshotStore>,
}

impl VoiceNoteController {
    /// Returns whether a recording is currently active.
    pub fn is_recording(&self) -> bool {
        self.recording
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// One toggle action (via hotkey or UI button). Starts a recording when idle,
    /// stops and transcribes one when recording. Returns `true` if recording is now active.
    ///
    /// The transcription leg runs on its own thread: whisper.cpp takes seconds
    /// on a real recording, and this is called from the global-shortcut
    /// handler or frontend invoke, which must return promptly.
    pub fn toggle(self: &Arc<Self>) -> bool {
        // Decide and mutate under the lock, but never hold it across the join
        // or the whisper.cpp call.
        let (stopping, is_active) = {
            let mut guard = self
                .recording
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            match guard.take() {
                // Was recording → this press stops it. Taking it above already
                // put us back in the idle state, so a stray press during
                // transcription starts a fresh recording rather than wedging.
                Some(active) => (Some(active), false),
                // Was idle → this press starts a recording.
                None => {
                    match start_recording(&self.config.wav) {
                        Ok(active) => {
                            *guard = Some(active);
                            println!("[voice_note] recording started — press {HOTKEY} or button to stop");
                            (None, true)
                        }
                        Err(err) => {
                            eprintln!("[voice_note] {err}");
                            (None, false)
                        }
                    }
                }
            }
        };

        if let Some(active) = stopping {
            println!("[voice_note] recording stopped, transcribing…");
            let this = Arc::clone(self);
            thread::Builder::new()
                .name("humcon-voice-note".into())
                .spawn(move || this.finish(active))
                .expect("failed to spawn voice note thread");
        }

        is_active
    }

    /// Stop the recorder, transcribe what it captured, and hand the transcript
    /// on to summarization.
    ///
    /// Every failure path still calls [`summarize::handle_transcript`] with an
    /// empty transcript, so `recorded_at` is stamped regardless — a voice note
    /// event happened even if we ended up with no text, which matches the
    /// contract summarize.rs already documents.
    fn finish(&self, active: ActiveRecording) {
        let recorded = match stop_recording(active) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("[voice_note] {err}");
                false
            }
        };

        if !recorded {
            deliver(&self.store, None);
            return;
        }

        transcribe_and_deliver(&self.store, &self.config);
    }
}

/// Transcribe the recording at `config.wav` and push the result downstream.
///
/// Split out from the recording half so `examples/test_transcribe.rs` can
/// exercise the whole whisper.cpp → snapshot → summarize chain against a
/// canned WAV, with no microphone and no running Tauri app — the same
/// motivation as the existing `test_summarize` probe.
pub fn transcribe_and_deliver(store: &Arc<SnapshotStore>, config: &WhisperConfig) {
    match transcribe(config) {
        Ok(text) => {
            if text.is_empty() {
                println!("[voice_note] transcript was empty (no speech detected)");
            } else {
                println!("[voice_note] transcript: {text}");
            }
            deliver(store, Some(text));
        }
        Err(err) => {
            eprintln!("[voice_note] {err}");
            deliver(store, None);
        }
    }
}

/// Write `voice_note.transcript` and hand the text to summarization.
///
/// `None` means "we never got a transcript"; `Some("")` means "we listened and
/// there was no speech" — a successful recording of silence. Those are
/// genuinely different facts, so they get different values.
///
/// Summarization is called either way, so `recorded_at` is stamped even when
/// transcription failed: the voice note event still happened. That matches the
/// contract summarize.rs already documents.
fn deliver(store: &Arc<SnapshotStore>, transcript: Option<String>) {
    let for_summary = transcript.clone().unwrap_or_default();

    if let Err(err) = store.update(|snapshot| snapshot.voice_note.transcript = transcript) {
        eprintln!("[voice_note] could not write snapshot: {err}");
    }

    summarize::handle_transcript(Arc::clone(store), for_summary);
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the toggle hotkey. Called from `lib.rs`'s `setup`.
///
/// A missing binary or model is a warning, not an error: the hotkey and the
/// recording pipeline still work, and you find out at startup instead of
/// discovering it after speaking into the void.
///
/// Hotkey registration failing (usually another app already owns the combo) is
/// also non-fatal — the rest of the app is useful without a voice note.
pub fn register(app: &tauri::App, store: Arc<SnapshotStore>, config: WhisperConfig) -> Arc<VoiceNoteController> {
    if !config.bin.exists() {
        eprintln!(
            "[voice_note] whisper-cli.exe not found at {} — recording will work, \
             transcription will not",
            config.bin.display()
        );
    }
    if !config.model.exists() {
        eprintln!(
            "[voice_note] whisper model not found at {} — recording will work, \
             transcription will not",
            config.model.display()
        );
    }

    let controller = Arc::new(VoiceNoteController {
        recording: Mutex::new(None),
        config,
        store,
    });

    let controller_for_hotkey = Arc::clone(&controller);
    let result = app
        .global_shortcut()
        .on_shortcut(HOTKEY, move |_app, _shortcut, event| {
            // Fire on press only. Acting on both press and release would
            // start and immediately stop the recording on a single tap.
            if event.state == ShortcutState::Pressed {
                controller_for_hotkey.toggle();
            }
        });

    match result {
        Ok(()) => println!("[voice_note] {HOTKEY} toggles voice note recording"),
        Err(err) => eprintln!(
            "[voice_note] could not register {HOTKEY} ({err}) — another app \
             probably owns it. Voice notes are disabled this run."
        ),
    }

    controller
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

type WavWriterHandle = Arc<Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

/// Open the mic and start capturing.
///
/// Any failure surfaces here, synchronously, at the moment the user presses
/// the hotkey — rather than as a silently empty WAV file discovered later.
pub fn start_recording(wav: &Path) -> Result<ActiveRecording, RecordError> {
    let (stream, writer) = open_stream(wav)?;

    // Streams are created stopped; nothing is captured until play().
    stream
        .play()
        .map_err(|err| RecordError::Play(err.to_string()))?;

    Ok(ActiveRecording { stream, writer })
}

/// Stop capturing and close the WAV file properly.
///
/// Dropping the stream first is what makes this safe: it terminates cpal's
/// audio thread, so no callback can be mid-write while we finalize the header.
pub fn stop_recording(active: ActiveRecording) -> Result<(), RecordError> {
    let ActiveRecording { stream, writer } = active;
    drop(stream);

    let writer = writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();

    match writer {
        // finalize() writes the real length into the RIFF header. Skipping it
        // leaves a file whose header claims zero samples.
        Some(writer) => writer
            .finalize()
            .map_err(|err| RecordError::Wav(err.to_string())),
        None => Err(RecordError::Wav("the WAV writer went missing".into())),
    }
}

/// Map a `cpal` stream-build failure onto our error type.
///
/// Windows returns `E_ACCESSDENIED` when microphone privacy is off, and cpal
/// 0.18 has no mapping for that HRESULT — it falls through to the catch-all
/// `BackendError` rather than `ErrorKind::PermissionDenied`, surfacing only as
/// the message text "Access is denied. (os error 5)". So the message is what we
/// have to match on to give the user the one hint that actually helps.
fn classify_stream_error(err: &cpal::Error) -> RecordError {
    let message = err.message().unwrap_or_default().to_string();

    let denied = matches!(err.kind(), ErrorKind::PermissionDenied)
        || (matches!(err.kind(), ErrorKind::BackendError)
            && (message.contains("Access is denied") || message.contains("os error 5")));

    if denied {
        RecordError::MicAccessDenied(err.to_string())
    } else {
        RecordError::BuildStream(err.to_string())
    }
}

/// Open the default input device and wire its callback to a WAV writer.
///
/// We record at whatever format the device natively offers rather than forcing
/// 16 kHz mono. whisper.cpp's prebuilt CLI decodes via miniaudio and resamples
/// internally, which was verified against a 48 kHz stereo file — so pushing
/// resampling onto it removes a whole class of bug from this side.
fn open_stream(path: &Path) -> Result<(cpal::Stream, WavWriterHandle), RecordError> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or(RecordError::NoInputDevice)?;

    let config = device
        .default_input_config()
        .map_err(|err| RecordError::Config(err.to_string()))?;

    let spec = wav_spec(&config)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| RecordError::Wav(err.to_string()))?;
    }

    let writer =
        hound::WavWriter::create(path, spec).map_err(|err| RecordError::Wav(err.to_string()))?;
    let writer: WavWriterHandle = Arc::new(Mutex::new(Some(writer)));
    let writer_for_cb = Arc::clone(&writer);

    let on_error = move |err: cpal::Error| eprintln!("[voice_note] mic stream error: {err}");

    let format = config.sample_format();
    let stream = match format {
        SampleFormat::I8 => device.build_input_stream(
            config.into(),
            move |data, _: &_| write_input_data::<i8, i16>(data, &writer_for_cb),
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data, _: &_| write_input_data::<i16, i16>(data, &writer_for_cb),
            on_error,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            config.into(),
            move |data, _: &_| write_input_data::<i32, i32>(data, &writer_for_cb),
            on_error,
            None,
        ),
        SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data, _: &_| write_input_data::<f32, f32>(data, &writer_for_cb),
            on_error,
            None,
        ),
        other => return Err(RecordError::UnsupportedSampleFormat(other.to_string())),
    }
    .map_err(|err| classify_stream_error(&err))?;

    Ok((stream, writer))
}

/// Translate the device's format into a WAV header spec.
///
/// 8-bit input is widened to 16-bit because WAV stores 8-bit samples as
/// unsigned, which does not match cpal's signed `i8`.
fn wav_spec(config: &SupportedStreamConfig) -> Result<hound::WavSpec, RecordError> {
    let format = config.sample_format();
    if format.is_dsd() {
        return Err(RecordError::UnsupportedSampleFormat(format.to_string()));
    }

    let bits_per_sample = match format {
        SampleFormat::I8 => 16,
        other => (other.sample_size() * 8) as u16,
    };

    Ok(hound::WavSpec {
        channels: config.channels() as _,
        sample_rate: config.sample_rate() as _,
        bits_per_sample,
        sample_format: if format.is_float() {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    })
}

/// Audio callback body: convert and append samples.
///
/// `try_lock` rather than `lock` — this runs on the realtime audio thread, and
/// dropping a buffer is far better than blocking it.
fn write_input_data<T, U>(input: &[T], writer: &WavWriterHandle)
where
    T: Sample,
    U: Sample + hound::Sample + FromSample<T>,
{
    if let Ok(mut guard) = writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for &sample in input.iter() {
                let sample: U = U::from_sample(sample);
                writer.write_sample(sample).ok();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Transcription — the stub seam
// ---------------------------------------------------------------------------

/// Run whisper.cpp over the recorded WAV and return the transcript.
///
/// **This function is the stub seam.** If whisper.cpp ever cannot be made to
/// work on a machine, replacing this body with a fixed string keeps the hotkey,
/// recording, snapshot write and summarization call fully exercised. Nothing
/// else in this module knows how the transcript was produced.
///
/// Verified working against the prebuilt `whisper-bin-x64` release (b4938) with
/// `ggml-base.en.bin`.
fn transcribe(config: &WhisperConfig) -> Result<String, TranscribeError> {
    // Checked explicitly so a missing install is one clear line rather than a
    // raw "program not found" from the OS.
    if !config.bin.exists() {
        return Err(TranscribeError::BinaryMissing(config.bin.clone()));
    }
    if !config.model.exists() {
        return Err(TranscribeError::ModelMissing(config.model.clone()));
    }

    let mut command = Command::new(&config.bin);
    command
        .arg("-m")
        .arg(&config.model)
        .arg("-f")
        .arg(&config.wav)
        // -nt: no timestamps, -np: no banner/progress. Together these leave
        // stdout as just the transcript text; the logs go to stderr.
        .arg("-nt")
        .arg("-np");

    // whisper-cli.exe loads whisper.dll and the ggml-cpu-*.dll backends from
    // its own directory, so run it from there.
    if let Some(dir) = config.bin.parent() {
        command.current_dir(dir);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command.output().map_err(TranscribeError::Spawn)?;

    parse_whisper_output(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

/// Turn a finished `whisper-cli.exe` run into a transcript.
///
/// Pure, so the output shapes that matter — success, silence, non-zero exit —
/// are testable with canned bytes and no binary, matching the
/// `parse_summary_response` / `parse_log` pattern used elsewhere.
fn parse_whisper_output(
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<String, TranscribeError> {
    if !success {
        return Err(TranscribeError::Failed {
            code,
            stderr: String::from_utf8_lossy(stderr).trim().to_string(),
        });
    }

    Ok(clean_transcript(&String::from_utf8_lossy(stdout)))
}

/// Collapse whisper's stdout into one line of speech.
///
/// whisper emits one line per segment, each with a leading space, and marks
/// non-speech with bracketed tokens like `[BLANK_AUDIO]` or `(beep)`. Those are
/// annotations rather than something the user said, so a recording of silence
/// becomes `""` instead of a literal "[BLANK_AUDIO]" reaching the summarizer.
fn clean_transcript(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_annotation(line))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Whether a line is entirely a non-speech marker.
fn is_annotation(line: &str) -> bool {
    (line.starts_with('[') && line.ends_with(']'))
        || (line.starts_with('(') && line.ends_with(')'))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- clean_transcript / is_annotation ---------------------------------

    #[test]
    fn clean_transcript_strips_leading_space_and_newlines() {
        let raw = "\n And so my fellow Americans, ask not what your country can do for you.";
        assert_eq!(
            clean_transcript(raw),
            "And so my fellow Americans, ask not what your country can do for you."
        );
    }

    #[test]
    fn clean_transcript_joins_multiple_segments_with_one_space() {
        let raw = " First segment.\n Second segment.\n Third segment.\n";
        assert_eq!(
            clean_transcript(raw),
            "First segment. Second segment. Third segment."
        );
    }

    #[test]
    fn clean_transcript_treats_blank_audio_as_empty() {
        assert_eq!(clean_transcript("\n[BLANK_AUDIO]\n"), "");
        assert_eq!(clean_transcript(" (silence)"), "");
        assert_eq!(clean_transcript(" (beep)"), "");
    }

    #[test]
    fn clean_transcript_of_no_output_is_empty() {
        assert_eq!(clean_transcript(""), "");
        assert_eq!(clean_transcript("\n\n  \n"), "");
    }

    #[test]
    fn clean_transcript_keeps_speech_next_to_an_annotation() {
        let raw = " [BLANK_AUDIO]\n Actual words here.\n";
        assert_eq!(clean_transcript(raw), "Actual words here.");
    }

    #[test]
    fn clean_transcript_keeps_bracketed_text_inside_a_sentence() {
        // Only a whole line that is an annotation should be dropped.
        let raw = " He said [inaudible] and left.";
        assert_eq!(clean_transcript(raw), "He said [inaudible] and left.");
    }

    #[test]
    fn clean_transcript_preserves_unicode() {
        let raw = " Café — naïve 日本語 🎤";
        assert_eq!(clean_transcript(raw), "Café — naïve 日本語 🎤");
    }

    #[test]
    fn is_annotation_matches_only_whole_line_markers() {
        assert!(is_annotation("[BLANK_AUDIO]"));
        assert!(is_annotation("(silence)"));
        assert!(!is_annotation("Hello there."));
        assert!(!is_annotation("[start] of a sentence"));
    }

    // -- parse_whisper_output ---------------------------------------------

    #[test]
    fn parse_whisper_output_extracts_transcript_on_success() {
        let stdout = b"\n And so my fellow Americans.";
        let result = parse_whisper_output(true, Some(0), stdout, b"whisper log noise").unwrap();
        assert_eq!(result, "And so my fellow Americans.");
    }

    #[test]
    fn parse_whisper_output_returns_empty_string_for_silence() {
        let result = parse_whisper_output(true, Some(0), b"\n[BLANK_AUDIO]\n", b"").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn parse_whisper_output_classifies_non_zero_exit() {
        let stderr = b"error: failed to open model";
        let err = parse_whisper_output(false, Some(1), b"", stderr).unwrap_err();
        match err {
            TranscribeError::Failed { code, stderr } => {
                assert_eq!(code, Some(1));
                assert_eq!(stderr, "error: failed to open model");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn parse_whisper_output_handles_signal_termination() {
        let err = parse_whisper_output(false, None, b"", b"killed").unwrap_err();
        assert!(matches!(
            err,
            TranscribeError::Failed { code: None, .. }
        ));
    }

    #[test]
    fn parse_whisper_output_tolerates_invalid_utf8() {
        // Lossy decoding, never a panic, even on a truncated multi-byte char.
        let stdout = b" caf\xC3\xA9 \xFF\xFE broken";
        let result = parse_whisper_output(true, Some(0), stdout, b"").unwrap();
        assert!(result.starts_with("café"));
    }

    // -- WhisperConfig::resolve -------------------------------------------

    #[test]
    fn resolve_defaults_under_the_state_dir() {
        // No env override is set in this test's process by default; assert the
        // shape of the default paths rather than the overrides, which are
        // process-global and would race other tests.
        let config = WhisperConfig::resolve(Path::new("C:\\Users\\test\\.humcon"));
        assert!(config.wav.ends_with("whisper/last-recording.wav") || config.wav.ends_with("whisper\\last-recording.wav"));
        assert_eq!(config.wav.parent(), config.bin.parent());
    }
}
