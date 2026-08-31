# HumCon browser tab reporter

Reports the active tab's URL and title to HumCon's local receiver
(`http://127.0.0.1:7423/tab`, `src-tauri/src/browser_server.rs`) whenever it
changes, so the resume card can show `browser_tab`. See `.claude/architecture.md`
for the full contract and design rationale.

## Install (load unpacked)

The Tauri app must already be running (`cargo tauri dev` or the built app) —
the extension has nothing to talk to otherwise, and simply stays quiet.

1. Open `brave://extensions` (or `chrome://extensions` — either browser works
   identically for MV3).
2. Enable **Developer mode** (top right).
3. **Load unpacked** → select this `extension/` folder
   (`C:\Users\Vaishnav\HumCon\extension`).
4. Switch tabs. Within about half a second, `browser_tab` in
   `C:\Users\Vaishnav\.humcon\snapshot.json` should show the new tab, and the
   resume card should update.

## What it does and doesn't do

- Only `http://` and `https://` tabs are reported. Browser-internal pages
  (`chrome://`, `brave://`, `about:`), local files, and extension pages never
  are — see `url-filter.js`.
- A URL whose query string or fragment looks like it carries a credential
  (`?token=`, `?code=`, a signed S3 link, etc.) is dropped entirely rather
  than sent and redacted, matching the shell command hook's precedent
  (`hooks/humcon-log.sh`): a dropped entry costs nothing, a leaked secret
  cannot be un-leaked.
- Losing focus to another app does **not** clear `browser_tab` — it keeps
  showing the last real tab, the same way `active_window` treats a failed
  poll as "skip this tick," not "erase what we had."
- Does not work in Incognito/private windows unless you explicitly enable
  that for the extension (MV3's `not_allowed` default) — left as-is rather
  than configured, since that's a deliberate privacy boundary, not an
  oversight.
- Requests only the `tabs` permission plus one `host_permissions` entry for
  the local endpoint. `activeTab` was considered and rejected: it only grants
  access after you click the extension's toolbar icon, and is revoked on the
  next navigation — useless for passive background reporting with no click.

## Known limitation

The endpoint has no shared secret — it only checks that a request carries the
`X-HumCon-Extension` header, which blocks a hostile *web page* (see
`browser_server.rs`'s module doc for why) but not another local process on
the same machine, which could send the same header. Accepted for a first
pass: the impact is a wrong `browser_tab` in a local file.
