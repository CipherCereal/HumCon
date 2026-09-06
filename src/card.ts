// Pure DOM rendering for the resume card. No data fetching here — main.ts
// owns reading snapshot.json (or a fixture) and calls renderCard with the
// result, which keeps this file testable via fixture mode in a plain browser.

import {
  BrowserTabEntry,
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

function section(
  title: string,
  actionEl?: HTMLElement,
): { section: HTMLElement; body: HTMLElement } {
  const wrap = el("section", "card-section");
  const header = el("div", "card-section__header");
  header.appendChild(el("h2", "card-section__title", title));
  if (actionEl) {
    header.appendChild(actionEl);
  }
  wrap.appendChild(header);
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

  const titleRow = el("div", "card-header__top");
  const stale = isStale(snapshot.last_updated, now);
  const dot = el(
    "span",
    `card-status-dot ${stale ? "card-status-dot--stale" : "card-status-dot--live"}`,
  );
  dot.setAttribute("title", stale ? "State is stale" : "Live & capturing");
  titleRow.appendChild(dot);
  titleRow.appendChild(el("h1", "card-title", "Resume"));
  header.appendChild(titleRow);

  const meta = el("div", "card-header__meta");
  const rel = relativeTime(snapshot.last_updated, now);
  meta.appendChild(
    el("span", "card-updated", rel ? `Updated ${rel}` : "Update time unknown"),
  );
  if (stale) {
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

function renderRecentCommands(root: HTMLElement, snapshot: Snapshot, now: Date) {
  const { section: node, body } = section("Recent commands");
  const commands = snapshot.recent_commands;

  if (commands.length === 0) {
    empty(
      body,
      "No commands logged yet.",
      "Interactive shell commands will appear here automatically.",
    );
  } else {
    const list = el("ul", "card-commands");
    // Newest first — the log itself is oldest-first FIFO (architecture.md).
    for (const cmd of [...commands].reverse()) {
      const item = el("li", "card-command");
      item.setAttribute("title", "Click to copy command");

      const codeEl = el("code", "card-command__text", cmd.command);
      const timeRel = cmd.ran_at ? relativeTime(cmd.ran_at, now) : null;
      const timeEl = el("span", "card-command__time", timeRel || cmd.ran_at || "unknown time");
      if (cmd.ran_at) {
        timeEl.setAttribute("title", cmd.ran_at);
      }

      item.appendChild(codeEl);
      item.appendChild(timeEl);

      // Micro-interaction: copy command on click
      item.addEventListener("click", () => {
        try {
          void navigator.clipboard.writeText(cmd.command);
          const origText = timeEl.textContent;
          timeEl.textContent = "copied";
          timeEl.classList.add("card-command__time--copied");
          setTimeout(() => {
            timeEl.textContent = origText;
            timeEl.classList.remove("card-command__time--copied");
          }, 1200);
        } catch {
          // Fallback if clipboard API unavailable
        }
      });

      list.appendChild(item);
    }
    body.appendChild(list);
  }

  root.appendChild(node);
}

function createRecordButton(isRecording: boolean, onToggle?: () => void): HTMLElement {
  const btn = el(
    "button",
    `card-record-btn ${isRecording ? "card-record-btn--recording" : ""}`,
  );
  btn.type = "button";
  btn.setAttribute(
    "title",
    isRecording ? "Stop recording (Ctrl+Alt+Space)" : "Record voice note (Ctrl+Alt+Space)",
  );

  const dot = el("span", "card-record-btn__dot");
  const label = el(
    "span",
    "card-record-btn__label",
    isRecording ? "Stop" : "Record",
  );

  btn.appendChild(dot);
  btn.appendChild(label);

  if (onToggle) {
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      onToggle();
    });
  }

  return btn;
}

function renderVoiceNote(
  root: HTMLElement,
  snapshot: Snapshot,
  isRecording: boolean,
  onToggleVoice?: () => void,
) {
  const recordBtn = createRecordButton(isRecording, onToggleVoice);
  const { section: node, body } = section("Voice note", recordBtn);
  const note = snapshot.voice_note;

  if (isRecording) {
    const banner = el("div", "card-voice__recording-banner");
    const pulse = el("span", "card-record-btn__dot");
    banner.appendChild(pulse);
    banner.appendChild(
      el(
        "span",
        undefined,
        "Listening... speak clearly. Click Stop or press Ctrl+Alt+Space when done.",
      ),
    );
    body.appendChild(banner);
  }

  if (!note.transcript && !note.summary) {
    if (!isRecording) {
      empty(body, "No voice note yet.", "Click Record or press Ctrl+Alt+Space to record one.");
    }
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

function renderBrowserTabItem(tab: BrowserTabEntry): HTMLElement {
  const item = el("li", "card-browser-tab");

  const info = el("div", "card-browser-tab__info");

  if (tab.title) {
    info.appendChild(el("p", "card-browser-tab__title", tab.title));
  }
  const urlLink = el("a", "card-browser-tab__url", tab.url);
  urlLink.setAttribute("href", tab.url);
  urlLink.setAttribute("target", "_blank");
  urlLink.setAttribute("rel", "noreferrer noopener");
  info.appendChild(urlLink);

  item.appendChild(info);

  const badge = el("span", "card-browser-tab__freq", `${tab.frequency}×`);
  badge.setAttribute("title", `Visited ${tab.frequency} time${tab.frequency === 1 ? "" : "s"}`);
  item.appendChild(badge);

  return item;
}

function renderBrowserTabs(root: HTMLElement, snapshot: Snapshot) {
  const { section: node, body } = section("Browser tabs");
  const tabs = snapshot.browser_tabs;

  if (tabs.length === 0) {
    // Covers: extension not installed, app not running, or no tab switch yet.
    empty(body, "Not connected.", "Install the HumCon browser extension to track tabs.");
  } else {
    const list = el("ul", "card-browser-tabs");
    // tabs arrive sorted ascending by frequency (least first) from the backend.
    for (const tab of tabs) {
      list.appendChild(renderBrowserTabItem(tab));
    }
    body.appendChild(list);
  }

  root.appendChild(node);
}

export function updateHeaderMeta(root: HTMLElement, snapshot: Snapshot, now: Date) {
  const updatedEl = root.querySelector<HTMLElement>(".card-updated");
  if (updatedEl) {
    const rel = relativeTime(snapshot.last_updated, now);
    updatedEl.textContent = rel ? `Updated ${rel}` : "Update time unknown";
  }

  const stale = isStale(snapshot.last_updated, now);
  const dot = root.querySelector<HTMLElement>(".card-status-dot");
  if (dot) {
    dot.className = `card-status-dot ${stale ? "card-status-dot--stale" : "card-status-dot--live"}`;
    dot.setAttribute("title", stale ? "State is stale" : "Live & capturing");
  }

  const meta = root.querySelector<HTMLElement>(".card-header__meta");
  if (meta) {
    const staleBadge = meta.querySelector<HTMLElement>(".card-badge--stale");
    if (stale && !staleBadge) {
      meta.appendChild(el("span", "card-badge card-badge--stale", "Stale"));
    } else if (!stale && staleBadge) {
      staleBadge.remove();
    }
  }
}

export function updateVoiceRecordingState(root: HTMLElement, isRecording: boolean) {
  const btn = root.querySelector<HTMLButtonElement>(".card-record-btn");
  if (btn) {
    btn.className = `card-record-btn ${isRecording ? "card-record-btn--recording" : ""}`;
    btn.setAttribute(
      "title",
      isRecording ? "Stop recording (Ctrl+Alt+Space)" : "Record voice note (Ctrl+Alt+Space)",
    );
    const label = btn.querySelector<HTMLElement>(".card-record-btn__label");
    if (label) {
      label.textContent = isRecording ? "Stop" : "Record";
    }
  }

  const voiceSection = btn?.closest(".card-section");
  const voiceBody = voiceSection?.querySelector<HTMLElement>(".card-section__body");
  const existingBanner = voiceBody?.querySelector<HTMLElement>(".card-voice__recording-banner");

  if (isRecording && !existingBanner && voiceBody) {
    const banner = el("div", "card-voice__recording-banner");
    const pulse = el("span", "card-record-btn__dot");
    banner.appendChild(pulse);
    banner.appendChild(
      el(
        "span",
        undefined,
        "Listening... speak clearly. Click Stop or press Ctrl+Alt+Space when done.",
      ),
    );
    voiceBody.prepend(banner);
  } else if (!isRecording && existingBanner) {
    existingBanner.remove();
  }
}

export function renderCard(
  root: HTMLElement,
  snapshot: Snapshot,
  now: Date,
  isRecording: boolean = false,
  onToggleVoice?: () => void,
) {
  const prevCommands = root.querySelector<HTMLElement>(".card-commands");
  const prevScrollTop = prevCommands ? prevCommands.scrollTop : null;
  const prevWindowY = window.scrollY;

  root.replaceChildren();
  renderHeader(root, snapshot, now);
  renderActiveWindow(root, snapshot);
  renderRecentCommands(root, snapshot, now);
  renderVoiceNote(root, snapshot, isRecording, onToggleVoice);
  renderBrowserTabs(root, snapshot);

  if (prevScrollTop !== null) {
    const nextCommands = root.querySelector<HTMLElement>(".card-commands");
    if (nextCommands) {
      nextCommands.scrollTop = prevScrollTop;
    }
  }
  if (prevWindowY !== 0) {
    window.scrollTo(0, prevWindowY);
  }
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
