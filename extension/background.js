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
const DEBOUNCE_MS = 150;

let lastSent = null; // { url, title } most recently POSTed
let debounceTimer = null;

function sendTab(url, title) {
  lastSent = { url, title };
  fetch(ENDPOINT, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-HumCon-Extension": "1",
    },
    body: JSON.stringify({ url, title }),
  }).catch(() => {
    // App not running is normal, silent catch
  });
}

function report(tab, immediate = false) {
  if (!tab || !tab.url || !shouldReport(tab.url)) return;

  const title = tab.title || null;
  if (lastSent && lastSent.url === tab.url && lastSent.title === title) {
    return; // identical to last report
  }

  clearTimeout(debounceTimer);

  if (immediate) {
    sendTab(tab.url, title);
  } else {
    debounceTimer = setTimeout(() => {
      sendTab(tab.url, title);
    }, DEBOUNCE_MS);
  }
}

function reportActiveTabInWindow(windowId, immediate = false) {
  chrome.tabs.query({ active: true, windowId }, (tabs) => {
    if (tabs && tabs[0]) report(tabs[0], immediate);
  });
}

// Switching to a different tab — user action, report immediately.
chrome.tabs.onActivated.addListener(({ tabId }) => {
  chrome.tabs.get(tabId, (tab) => report(tab, true));
});

// In-place navigation and late-arriving titles on the already-active tab.
chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
  if (!tab.active) return;
  if (changeInfo.status === "complete" || changeInfo.title) {
    report(tab, false);
  }
});

// Switching browser windows — report immediately.
chrome.windows.onFocusChanged.addListener((windowId) => {
  if (windowId === chrome.windows.WINDOW_ID_NONE) return;
  reportActiveTabInWindow(windowId, true);
});

// Report currently active tab on extension startup / wake-up.
chrome.tabs.query({ active: true, lastFocusedWindow: true }, (tabs) => {
  if (tabs && tabs[0]) report(tabs[0], true);
});

