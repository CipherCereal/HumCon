// MV3 service worker. Reports the active tab's URL and title to HumCon's
// local receiver (src-tauri/src/browser_server.rs) whenever it changes.
//
// No content scripts, no chrome.storage, no keepalive hack: an MV3 service
// worker is killed after ~30s idle and re-woken by the events registered
// below, which is exactly the on-tab-change lifecycle this needs — there is
// nothing useful for it to do while idle anyway.
import { shouldReport } from "./url-filter.js";

// Must match src-tauri/src/lib.rs's `browser_port` default. If that ever
// changes via HUMCON_PORT, this needs a matching change (or an options page —
// not built for this first pass).
const ENDPOINT = "http://127.0.0.1:7423/tab";

// Coalesces the burst of tabs.onUpdated events a single page load fires
// (loading -> title set -> complete) into one POST.
const DEBOUNCE_MS = 400;

let lastSent = null; // { url, title } most recently POSTed, for the dedupe below
let debounceTimer = null;

function report(tab) {
  if (!tab || !tab.url || !shouldReport(tab.url)) return;

  const title = tab.title || null;
  if (lastSent && lastSent.url === tab.url && lastSent.title === title) {
    return; // nothing actually changed since the last report
  }

  clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    lastSent = { url: tab.url, title };
    fetch(ENDPOINT, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        // Presence, not value, is what the server checks — see
        // browser_server.rs's module doc for why that's enough to block a
        // web page without needing a shared secret.
        "X-HumCon-Extension": "1",
      },
      body: JSON.stringify({ url: tab.url, title }),
    }).catch(() => {
      // The Tauri app simply not running is the normal case here, not an
      // error worth logging on every tab switch.
    });
  }, DEBOUNCE_MS);
}

function reportActiveTabInWindow(windowId) {
  chrome.tabs.query({ active: true, windowId }, (tabs) => report(tabs[0]));
}

// Switching to a different tab.
chrome.tabs.onActivated.addListener(({ tabId }) => {
  chrome.tabs.get(tabId, report);
});

// In-place navigation and late-arriving titles on the already-active tab.
chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
  if (!tab.active) return;
  if (changeInfo.status === "complete" || changeInfo.title) {
    report(tab);
  }
});

// Switching browser windows. WINDOW_ID_NONE means focus left the browser
// entirely (e.g. to another app) — deliberately not reported, so browser_tab
// keeps showing the last real tab rather than being cleared, matching how
// active_window treats a failed poll as "skip", not "erase".
chrome.windows.onFocusChanged.addListener((windowId) => {
  if (windowId === chrome.windows.WINDOW_ID_NONE) return;
  reportActiveTabInWindow(windowId);
});
