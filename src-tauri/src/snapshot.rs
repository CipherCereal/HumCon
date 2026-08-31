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

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::Value;
    use tempfile::TempDir;

    /// A snapshot path inside a fresh temp dir — tests never touch the real
    /// state dir. The `TempDir` is returned so it lives (and cleans up) with
    /// the test.
    fn temp_store_path() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("snapshot.json");
        (dir, path)
    }

    /// What a reader outside this process sees: the file itself, as JSON.
    fn read_json(path: &Path) -> Value {
        let text = fs::read_to_string(path).expect("read snapshot file");
        serde_json::from_str(&text).expect("snapshot file holds valid json")
    }

    fn sample_window(app: &str, title: &str) -> ActiveWindowSnapshot {
        ActiveWindowSnapshot {
            app_name: app.to_string(),
            window_title: Some(title.to_string()),
            captured_at: "2026-08-29T08:00:00Z".to_string(),
        }
    }

    /// Every path in `dir` whose name matches `snapshot.corrupt-*.json`.
    fn quarantine_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("read temp dir")
            .map(|entry| entry.expect("dir entry").path())
            .filter(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                name.starts_with("snapshot.corrupt-") && name.ends_with(".json")
            })
            .collect()
    }

    #[test]
    fn load_or_create_without_a_file_writes_a_complete_default() {
        let (_dir, path) = temp_store_path();
        let store = SnapshotStore::load_or_create(path.clone()).expect("load_or_create");
        assert_eq!(store.path(), path.as_path());

        // The startup no-op write must leave a file carrying every key of the
        // frozen schema — absent data is null (or [] for recent_commands),
        // never a missing field.
        let json = read_json(&path);
        assert_eq!(json["schema_version"], 1);
        assert!(json["last_updated"].is_string());
        assert!(json["active_window"].is_null());
        assert_eq!(json["recent_commands"], Value::Array(Vec::new()));
        assert_eq!(json["voice_note"]["transcript"], Value::Null);
        assert_eq!(json["voice_note"]["summary"], Value::Null);
        assert_eq!(json["voice_note"]["recorded_at"], Value::Null);
        assert_eq!(json["browser_tab"]["url"], Value::Null);
        assert_eq!(json["browser_tab"]["title"], Value::Null);
        assert_eq!(json["browser_tab"]["captured_at"], Value::Null);
        // Exactly the six top-level keys — nothing extra, nothing dropped.
        assert_eq!(json.as_object().expect("top-level object").len(), 6);
    }

    #[test]
    fn load_or_create_creates_missing_parent_directories() {
        // First run on a fresh machine: the state dir doesn't exist yet.
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("nested").join("state").join("snapshot.json");
        SnapshotStore::load_or_create(path.clone()).expect("load_or_create");
        assert!(path.exists());
    }

    #[test]
    fn load_or_create_round_trips_an_existing_valid_file() {
        let (_dir, path) = temp_store_path();
        {
            let store = SnapshotStore::load_or_create(path.clone()).expect("first store");
            store
                .update(|s| {
                    s.active_window = Some(sample_window("Code.exe", "snapshot.rs"));
                    s.recent_commands = vec![RecentCommand {
                        command: "git status".to_string(),
                        ran_at: "2026-08-29T08:01:00Z".to_string(),
                    }];
                    s.voice_note.transcript = Some("remember the milk".to_string());
                    s.browser_tab.url = Some("https://example.com".to_string());
                })
                .expect("update");
        }

        // A second store — conceptually a fresh process — loads the same data
        // and writes it back out intact.
        let _store = SnapshotStore::load_or_create(path.clone()).expect("second store");
        let json = read_json(&path);
        assert_eq!(json["active_window"]["app_name"], "Code.exe");
        assert_eq!(json["active_window"]["window_title"], "snapshot.rs");
        assert_eq!(json["recent_commands"][0]["command"], "git status");
        assert_eq!(json["voice_note"]["transcript"], "remember the milk");
        assert_eq!(json["browser_tab"]["url"], "https://example.com");
    }

    #[test]
    fn corrupt_file_is_quarantined_with_its_original_bytes() {
        let (dir, path) = temp_store_path();
        let garbage = "{ \"schema_version\": 1, this is not json";
        fs::write(&path, garbage).expect("plant corrupt file");

        SnapshotStore::load_or_create(path.clone()).expect("load_or_create");

        // Exactly one snapshot.corrupt-<timestamp>.json appears alongside,
        // holding the unparseable original byte for byte.
        let quarantined = quarantine_files(dir.path());
        assert_eq!(quarantined.len(), 1, "expected exactly one quarantine file");
        assert_eq!(
            fs::read(&quarantined[0]).expect("read quarantine file"),
            garbage.as_bytes()
        );

        // And snapshot.json itself starts over as a complete default.
        let json = read_json(&path);
        assert_eq!(json["schema_version"], 1);
        assert!(json["active_window"].is_null());
        assert_eq!(json["recent_commands"], Value::Array(Vec::new()));
    }

    #[test]
    fn valid_json_of_the_wrong_shape_is_also_quarantined() {
        // Parseable JSON that isn't a Snapshot (missing required keys) must
        // get the same move-aside treatment as syntactic garbage.
        let (dir, path) = temp_store_path();
        let wrong_shape = "[1, 2, 3]";
        fs::write(&path, wrong_shape).expect("plant wrong-shape file");

        SnapshotStore::load_or_create(path.clone()).expect("load_or_create");

        let quarantined = quarantine_files(dir.path());
        assert_eq!(quarantined.len(), 1);
        assert_eq!(
            fs::read(&quarantined[0]).expect("read quarantine file"),
            wrong_shape.as_bytes()
        );
        assert_eq!(read_json(&path)["schema_version"], 1);
    }

    #[test]
    fn update_persists_the_edit_and_leaves_other_keys_untouched() {
        let (_dir, path) = temp_store_path();
        let store = SnapshotStore::load_or_create(path.clone()).expect("load_or_create");
        store
            .update(|s| {
                s.recent_commands = vec![RecentCommand {
                    command: "cargo build".to_string(),
                    ran_at: "2026-08-29T08:02:00Z".to_string(),
                }];
                s.voice_note = VoiceNote {
                    transcript: Some("transcript".to_string()),
                    summary: Some("summary".to_string()),
                    recorded_at: Some("2026-08-29T08:03:00Z".to_string()),
                };
                s.browser_tab = BrowserTab {
                    url: Some("https://example.com/docs".to_string()),
                    title: Some("Docs".to_string()),
                    captured_at: Some("2026-08-29T08:04:00Z".to_string()),
                };
            })
            .expect("seed other components' keys");
        let before = read_json(&path);

        // One component touches only its own key...
        store
            .update(|s| s.active_window = Some(sample_window("notepad.exe", "todo.txt")))
            .expect("update active_window");

        // ...and on disk that key changed while everyone else's survived.
        let after = read_json(&path);
        assert_eq!(after["active_window"]["app_name"], "notepad.exe");
        assert_eq!(after["active_window"]["window_title"], "todo.txt");
        for key in ["schema_version", "recent_commands", "voice_note", "browser_tab"] {
            assert_eq!(after[key], before[key], "`{key}` should be unchanged");
        }
    }

    #[test]
    fn update_restamps_last_updated_and_schema_version_on_every_write() {
        let (_dir, path) = temp_store_path();
        let store = SnapshotStore::load_or_create(path.clone()).expect("load_or_create");

        // Even an edit that plants its own values gets overwritten — update()
        // owns these two fields, whichever key the caller meant to change.
        store
            .update(|s| {
                s.last_updated = "not-a-timestamp".to_string();
                s.schema_version = 999;
            })
            .expect("update");

        let json = read_json(&path);
        assert_eq!(json["schema_version"], 1);
        let stamped = json["last_updated"].as_str().expect("last_updated is a string");
        assert_ne!(stamped, "not-a-timestamp");
        // The contract format: RFC 3339, UTC ("Z"), whole seconds.
        chrono::DateTime::parse_from_rfc3339(stamped).expect("last_updated parses as rfc3339");
        assert!(stamped.ends_with('Z'), "timestamp should be UTC: {stamped}");
        assert_eq!(
            stamped.len(),
            "2026-08-29T08:20:14Z".len(),
            "timestamp should have whole-second precision: {stamped}"
        );
    }

    #[test]
    fn successful_write_leaves_no_temp_file_behind() {
        let (dir, path) = temp_store_path();
        let store = SnapshotStore::load_or_create(path.clone()).expect("load_or_create");
        store
            .update(|s| s.voice_note.transcript = Some("x".to_string()))
            .expect("update");

        assert!(path.exists());
        assert!(
            !dir.path().join("snapshot.json.tmp").exists(),
            "temp file should be renamed away after a successful write"
        );
    }
}
