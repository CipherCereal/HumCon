// Reader-side view of the frozen snapshot.json contract (architecture.md).
//
// This file has no Tauri import on purpose: it's exercised in a plain
// browser via fixture mode (see main.ts), so the parse/normalize layer is
// provably independent of whether Tauri is even running.

export interface ActiveWindow {
  app_name: string;
  window_title: string | null;
  captured_at: string;
}

export interface RecentCommand {
  command: string;
  ran_at: string;
}

export interface VoiceNote {
  transcript: string | null;
  summary: string | null;
  recorded_at: string | null;
}

export interface BrowserTab {
  url: string | null;
  title: string | null;
  captured_at: string | null;
}

export interface Snapshot {
  schema_version: number;
  last_updated: string | null;
  active_window: ActiveWindow | null;
  recent_commands: RecentCommand[];
  voice_note: VoiceNote;
  browser_tab: BrowserTab;
}

// The schema version this UI was built against (architecture.md, frozen at
// Session 0). Not a parse requirement — `normalizeSnapshot` still renders a
// mismatched file — just what `card.ts` compares against to show the banner.
export const KNOWN_SCHEMA_VERSION = 1;

function asString(v: unknown): string | null {
  return typeof v === "string" ? v : null;
}

function asNonNullString(v: unknown, fallback: string): string {
  return typeof v === "string" ? v : fallback;
}

function normalizeActiveWindow(v: unknown): ActiveWindow | null {
  if (v === null || typeof v !== "object") return null;
  const obj = v as Record<string, unknown>;
  return {
    app_name: asNonNullString(obj.app_name, "Unknown app"),
    window_title: asString(obj.window_title),
    captured_at: asNonNullString(obj.captured_at, ""),
  };
}

function normalizeRecentCommands(v: unknown): RecentCommand[] {
  if (!Array.isArray(v)) return [];
  const out: RecentCommand[] = [];
  for (const item of v) {
    if (item === null || typeof item !== "object") continue;
    const obj = item as Record<string, unknown>;
    if (typeof obj.command !== "string") continue;
    out.push({
      command: obj.command,
      ran_at: asNonNullString(obj.ran_at, ""),
    });
  }
  return out;
}

function normalizeVoiceNote(v: unknown): VoiceNote {
  if (v === null || typeof v !== "object") {
    return { transcript: null, summary: null, recorded_at: null };
  }
  const obj = v as Record<string, unknown>;
  return {
    transcript: asString(obj.transcript),
    summary: asString(obj.summary),
    recorded_at: asString(obj.recorded_at),
  };
}

function normalizeBrowserTab(v: unknown): BrowserTab {
  if (v === null || typeof v !== "object") {
    return { url: null, title: null, captured_at: null };
  }
  const obj = v as Record<string, unknown>;
  return {
    url: asString(obj.url),
    title: asString(obj.title),
    captured_at: asString(obj.captured_at),
  };
}

// Turns arbitrary parsed JSON into a fully-shaped `Snapshot`, tolerating
// missing keys, `null` in place of an object, and wrong types — all of
// which a hand-edited file can produce and none of which should crash the
// card. This is the only place that defensiveness lives; everything past
// this function can assume the shape is exactly as declared above.
export function normalizeSnapshot(raw: unknown): Snapshot {
  const obj = raw !== null && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    schema_version: typeof obj.schema_version === "number" ? obj.schema_version : 0,
    last_updated: asString(obj.last_updated),
    active_window: normalizeActiveWindow(obj.active_window),
    recent_commands: normalizeRecentCommands(obj.recent_commands),
    voice_note: normalizeVoiceNote(obj.voice_note),
    browser_tab: normalizeBrowserTab(obj.browser_tab),
  };
}

// Parses raw JSON text into a normalized `Snapshot`. Text that isn't even
// valid JSON (e.g. caught mid-write, though the shared writer's
// temp-file-then-rename should prevent that) throws — callers keep the last
// good render rather than propagate this into the UI (see main.ts).
export function parseSnapshot(text: string): Snapshot {
  return normalizeSnapshot(JSON.parse(text));
}

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

// Renders an ISO-8601 timestamp relative to `now`. `null`/unparseable input
// (e.g. a field omitted or malformed by hand-editing) returns `null` so the
// caller can show "unknown" instead of "NaNm ago".
export function relativeTime(iso: string | null, now: Date): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;

  const diffMs = now.getTime() - then;
  if (diffMs < 0) return "just now"; // clock skew — don't show a negative age
  if (diffMs < 5 * SECOND) return "just now";
  if (diffMs < MINUTE) return `${Math.floor(diffMs / SECOND)}s ago`;
  if (diffMs < HOUR) return `${Math.floor(diffMs / MINUTE)}m ago`;
  if (diffMs < DAY) return `${Math.floor(diffMs / HOUR)}h ago`;
  return `${Math.floor(diffMs / DAY)}d ago`;
}

// `last_updated` older than this means a writer has stopped (window capture
// polls every 3s, command log every 2s — see architecture.md), which is
// worth surfacing rather than showing a quietly-stale card as if it were live.
const STALE_THRESHOLD_MS = 30 * SECOND;

export function isStale(iso: string | null, now: Date): boolean {
  if (!iso) return true;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return true;
  return now.getTime() - then > STALE_THRESHOLD_MS;
}
