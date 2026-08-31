//! Shared owner of `snapshot.json` — the single source of truth between
//! HumCon's components (see architecture.md).
//!
//! Session 0 froze the schema and also settled *how* the file gets written:
//! every backend component runs inside this one Tauri process, so all writes
//! funnel through a single `Mutex<Snapshot>`. Whoever holds the lock
//! serializes the whole struct and writes it atomically — temp file first,
//! then rename over the real one.
//!
//! Why not let each component read-modify-write just its own key? Because a
//! write serializes the *entire* file. Two components doing that concurrently
//! would race, and one could silently clobber the other's key even though each
//! "owns" a different one. One mutex plus an atomic rename makes
//! last-write-wins actually true, and needs no cross-process file lock.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// Bumped only by a deliberate schema change, which CLAUDE.md requires to be
/// recorded in architecture.md's session log in the same session as the code.
const SCHEMA_VERSION: u32 = 1;

/// Timestamps in `snapshot.json` are ISO-8601 / RFC 3339, UTC, whole seconds —
/// e.g. `2026-08-29T08:20:14Z`.
pub fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The frozen `snapshot.json` schema, field for field.
///
/// Two rules from Session 0 are load-bearing here:
///
/// * **Never omit a key.** Absent data is `null` (or `[]` for
///   `recent_commands`), never a missing field, so every reader — serde here,
///   the TS interfaces, the resume-card UI — has one single shape to handle.
///   This is why no `Option` field below carries
///   `#[serde(skip_serializing_if = "Option::is_none")]`: that attribute drops
///   the key entirely, which is exactly what we don't want.
/// * **`last_updated` means "most recent write to anything"**, not to a
///   particular section. `SnapshotStore::update` stamps it on every write,
///   whichever key changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub last_updated: String,

    /// `None` — serialized as `null` — until the first successful window
    /// capture.
    ///
    /// The frozen schema types the *inner* fields as non-nullable (`app_name`
    /// and `captured_at` are plain strings), which left no legal value for
    /// "nothing captured yet". Session 1 resolved that by reading Session 0's
    /// "use `null` for scalar/object fields" as covering the object as a
    /// whole. So this is either absent or fully populated — never half-filled
    /// with placeholder strings.
    pub active_window: Option<ActiveWindowSnapshot>,

    pub recent_commands: Vec<RecentCommand>,
    pub voice_note: VoiceNote,
    pub browser_tab: BrowserTab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveWindowSnapshot {
    pub app_name: String,
    /// `None` when the focused window has no readable caption.
    pub window_title: Option<String>,
    pub captured_at: String,
}

impl ActiveWindowSnapshot {
    /// Do these describe the same app *and* the same title?
    ///
    /// Deliberately ignores `captured_at`, which changes on every poll.
    ///
    /// This is hand-written rather than using the capture crate's own
    /// `PartialEq`, which compares only the process and window ids and
    /// *ignores the title on purpose* ("even if the title changes it's still
    /// the same window", per its maintainer). For HumCon the title is the
    /// interesting part — a different browser tab, a different file in the
    /// editor — so comparing the crate's way would hide exactly what we want
    /// to notice.
    pub fn same_window_as(&self, other: &Self) -> bool {
        self.app_name == other.app_name && self.window_title == other.window_title
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentCommand {
    pub command: String,
    pub ran_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceNote {
    pub transcript: Option<String>,
    pub summary: Option<String>,
    pub recorded_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrowserTab {
    pub url: Option<String>,
    pub title: Option<String>,
    pub captured_at: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            last_updated: now_iso8601(),
            active_window: None,
            recent_commands: Vec::new(),
            voice_note: VoiceNote::default(),
            browser_tab: BrowserTab::default(),
        }
    }
}

#[derive(Debug)]
pub enum SnapshotError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "snapshot file i/o failed: {err}"),
            Self::Json(err) => write!(f, "snapshot json failed: {err}"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
        }
    }
}

impl From<io::Error> for SnapshotError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for SnapshotError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Holds the one in-memory `Snapshot` and owns writing it to disk.
pub struct SnapshotStore {
    path: PathBuf,
    inner: Mutex<Snapshot>,
}

impl SnapshotStore {
    /// Load the snapshot at `path`, or start from defaults if it isn't there.
    ///
    /// A file that exists but doesn't parse is *not* silently discarded — it
    /// may hold other components' data. It gets moved aside to
    /// `snapshot.corrupt-<timestamp>.json` and we continue from defaults, so
    /// the bad file survives for inspection instead of being overwritten.
    pub fn load_or_create(path: PathBuf) -> Result<Self, SnapshotError> {
        let snapshot = match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Snapshot>(&text) {
                Ok(loaded) => loaded,
                Err(err) => {
                    let aside = path.with_file_name(format!(
                        "snapshot.corrupt-{}.json",
                        Utc::now().format("%Y%m%dT%H%M%SZ")
                    ));
                    eprintln!(
                        "[snapshot] {} did not parse ({err}); moving it to {} and starting from defaults",
                        path.display(),
                        aside.display()
                    );
                    fs::rename(&path, &aside)?;
                    Snapshot::default()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Snapshot::default(),
            Err(err) => return Err(err.into()),
        };

        let store = Self {
            path,
            inner: Mutex::new(snapshot),
        };

        // Write once at startup with a no-op edit, so the file always exists
        // and always has every key for the other components and the UI to
        // read.
        store.update(|_| {})?;

        Ok(store)
    }

    /// Where this store's snapshot.json lives. Read-only: the resume card UI
    /// reads the file directly (see architecture.md, Session 5) rather than
    /// this process's in-memory copy, so it also observes a hand-edited file
    /// and any future out-of-process writer. Exposed rather than duplicating
    /// the `HUMCON_SNAPSHOT_PATH` / `HUMCON_DIR` resolution in `lib.rs`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mutate the snapshot, then write the whole file atomically.
    ///
    /// Callers should touch only their own component's field inside `edit`.
    /// Everything around it — locking, stamping `last_updated`, serializing
    /// every other key back out unchanged — is handled here.
    pub fn update<F>(&self, edit: F) -> Result<(), SnapshotError>
    where
        F: FnOnce(&mut Snapshot),
    {
        // If a thread panicked while holding this lock the mutex is
        // "poisoned". The data behind it is still structurally sound (a
        // half-applied edit at worst), and refusing to ever write again would
        // be worse than carrying on, so recover the value rather than
        // propagating the panic.
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        edit(&mut snapshot);
        snapshot.schema_version = SCHEMA_VERSION;
        snapshot.last_updated = now_iso8601();

        self.write_atomically(&snapshot)
    }

    fn write_atomically(&self, snapshot: &Snapshot) -> Result<(), SnapshotError> {
        let json = serde_json::to_string_pretty(snapshot)?;

        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;

        // The temp file must sit in the same directory as the target, so the
        // rename stays on one volume — a cross-volume rename is not atomic and
        // on Windows fails outright.
        let temp = dir.join("snapshot.json.tmp");
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(json.as_bytes())?;
            // Flush to the disk before renaming, so a crash can't leave a
            // renamed-but-empty file behind.
            file.sync_all()?;
        }

        // On Windows this becomes MoveFileExW with MOVEFILE_REPLACE_EXISTING,
        // so it replaces the existing file rather than failing. A reader
        // holding the file open without FILE_SHARE_DELETE can still make it
        // fail transiently; the caller just tries again on the next poll.
        fs::rename(&temp, &self.path)?;

        Ok(())
    }
}
