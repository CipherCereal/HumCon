// Pure DOM rendering for the resume card. No data fetching here — main.ts
// owns reading snapshot.json (or a fixture) and calls renderCard with the
// result, which keeps this file testable via fixture mode in a plain browser.

import {
  KNOWN_SCHEMA_VERSION,
  Snapshot,
  isStale,
  relativeTime,
} from "./snapshot";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  // textContent, never innerHTML: window titles, shell commands and
  // transcripts are arbitrary text from other apps, and this app's CSP is
  // disabled (tauri.conf.json), so nothing here should ever parse as markup.
  if (text !== undefined) node.textContent = text;
  return node;
}

function section(title: string): { section: HTMLElement; body: HTMLElement } {
  const wrap = el("section", "card-section");
  wrap.appendChild(el("h2", "card-section__title", title));
  const body = el("div", "card-section__body");
  wrap.appendChild(body);
  return { section: wrap, body };
}

function empty(body: HTMLElement, message: string, hint?: string) {
  body.appendChild(el("p", "card-empty", message));
  if (hint) body.appendChild(el("p", "card-hint", hint));
}

function renderHeader(root: HTMLElement, snapshot: Snapshot, now: Date) {
  const header = el("header", "card-header");
  header.appendChild(el("h1", "card-title", "Resume"));

  const meta = el("div", "card-header__meta");
  const rel = relativeTime(snapshot.last_updated, now);
  meta.appendChild(
    el("span", "card-updated", rel ? `Updated ${rel}` : "Update time unknown"),
  );
  if (isStale(snapshot.last_updated, now)) {
    meta.appendChild(el("span", "card-badge card-badge--stale", "Stale"));
  }
  header.appendChild(meta);

  if (snapshot.schema_version !== KNOWN_SCHEMA_VERSION) {
    header.appendChild(
      el(
        "p",
        "card-banner",
        `This file was written by schema v${snapshot.schema_version}, ` +
          `but this card was built for v${KNOWN_SCHEMA_VERSION}. Showing what it can.`,
      ),
    );
  }

  root.appendChild(header);
}

function renderActiveWindow(root: HTMLElement, snapshot: Snapshot) {
  const { section: node, body } = section("Active window");
  const win = snapshot.active_window;

  if (win === null) {
    empty(body, "No window captured yet.");
  } else {
    body.appendChild(el("p", "card-window__app", win.app_name));
    if (win.window_title) {
      body.appendChild(el("p", "card-window__title", win.window_title));
    }
  }

  root.appendChild(node);
}

function renderRecentCommands(root: HTMLElement, snapshot: Snapshot) {
  const { section: node, body } = section("Recent commands");
  const commands = snapshot.recent_commands;

  if (commands.length === 0) {
    empty(
      body,
      "No commands logged yet.",
      "Logged from interactive WSL shells sourcing hooks/humcon-log.sh.",
    );
  } else {
    const list = el("ul", "card-commands");
    // Newest first — the log itself is oldest-first FIFO (architecture.md).
    for (const cmd of [...commands].reverse()) {
      const item = el("li", "card-command");
      item.appendChild(el("code", "card-command__text", cmd.command));
      item.appendChild(el("span", "card-command__time", cmd.ran_at || "unknown time"));
      list.appendChild(item);
    }
    body.appendChild(list);
  }

  root.appendChild(node);
}

function renderVoiceNote(root: HTMLElement, snapshot: Snapshot) {
  const { section: node, body } = section("Voice note");
  const note = snapshot.voice_note;

  if (!note.transcript && !note.summary) {
    empty(body, "No voice note yet.", "Press Ctrl+Alt+Space to record one.");
  } else {
    if (note.summary) {
      body.appendChild(el("p", "card-voice__summary", note.summary));
    } else {
      // Real, common state today: transcript exists but summarization
      // hasn't produced a summary (no ANTHROPIC_API_KEY, a failed call, or
      // an empty transcript) — architecture.md, Sessions 3-4. This must
      // read as "not available", not as a broken layout.
      body.appendChild(
        el("p", "card-voice__summary card-voice__summary--unavailable", "Summary unavailable."),
      );
    }
    if (note.transcript) {
      body.appendChild(el("p", "card-voice__transcript", note.transcript));
    }
  }

  root.appendChild(node);
}

function renderBrowserTab(root: HTMLElement, snapshot: Snapshot) {
  const { section: node, body } = section("Browser tab");
  const tab = snapshot.browser_tab;

  if (!tab.url && !tab.title) {
    // Also covers "the browser extension doesn't exist yet" (architecture.md
    // status table) — reads as expected, not as a failure.
    empty(body, "Not connected.");
  } else {
    if (tab.title) body.appendChild(el("p", "card-browser__title", tab.title));
    if (tab.url) body.appendChild(el("p", "card-browser__url", tab.url));
  }

  root.appendChild(node);
}

export function renderCard(root: HTMLElement, snapshot: Snapshot, now: Date) {
  root.replaceChildren();
  renderHeader(root, snapshot, now);
  renderActiveWindow(root, snapshot);
  renderRecentCommands(root, snapshot);
  renderVoiceNote(root, snapshot);
  renderBrowserTab(root, snapshot);
}

export function renderRefreshError(root: HTMLElement) {
  let banner = root.querySelector<HTMLElement>(".card-refresh-error");
  if (!banner) {
    banner = el("p", "card-refresh-error", "Couldn't refresh — showing last known state.");
    root.prepend(banner);
  }
}

export function clearRefreshError(root: HTMLElement) {
  root.querySelector(".card-refresh-error")?.remove();
}
