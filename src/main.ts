import { invoke } from "@tauri-apps/api/core";
import { parseSnapshot, Snapshot } from "./snapshot";
import { renderCard, renderRefreshError, clearRefreshError } from "./card";

const POLL_MS = 1000;

// Dev-only escape hatch: `?fixture=<name>` loads a checked-in fixture from
// src/fixtures/ instead of invoking Tauri, so the parse/normalize/render
// chain can be exercised for every shape in `npm run dev` in a plain
// browser — no writers to race, no app to run (architecture.md, Session 5).
// Inert in the built app: `new URLSearchParams` only ever sees this window's
// own query string, and there's no UI that ever sets `?fixture=`.
function fixtureName(): string | null {
  return new URLSearchParams(window.location.search).get("fixture");
}

async function readSnapshotText(): Promise<string> {
  const fixture = fixtureName();
  if (fixture) {
    const res = await fetch(`/src/fixtures/${fixture}.json`);
    if (!res.ok) throw new Error(`fixture "${fixture}" not found (${res.status})`);
    return res.text();
  }
  return invoke<string>("read_snapshot_raw");
}

let lastGood: Snapshot | null = null;
let refreshing = false;

async function refresh(root: HTMLElement) {
  // Guards against a slow read overlapping the next tick — reads are cheap
  // (a ~600-byte file) but a stalled network drive or WSL/9p hiccup
  // shouldn't let calls stack up.
  if (refreshing) return;
  refreshing = true;
  try {
    const text = await readSnapshotText();
    const snapshot = parseSnapshot(text);
    lastGood = snapshot;
    renderCard(root, snapshot, new Date());
    clearRefreshError(root);
  } catch (err) {
    // A transient Windows sharing violation against the writer's atomic
    // rename, or (in principle) a read caught mid-write, must not blank the
    // card. In practice the shared writer's temp-file-then-rename means a
    // successful read is always a complete file (architecture.md,
    // snapshot.rs) — but if the read itself fails, keep showing the last
    // good render rather than an empty or broken one.
    console.error("[resume-card] refresh failed:", err);
    if (lastGood) {
      renderRefreshError(root);
    } else {
      root.replaceChildren();
      renderRefreshError(root);
    }
  } finally {
    refreshing = false;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  const root = document.getElementById("card");
  if (!root) return;

  void refresh(root);
  setInterval(() => void refresh(root), POLL_MS);

  // Writers poll every 2-3s (architecture.md); re-read immediately when the
  // card regains focus/visibility so alt-tabbing back shows fresh data
  // rather than waiting out the rest of the interval.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void refresh(root);
  });
  window.addEventListener("focus", () => void refresh(root));
});
