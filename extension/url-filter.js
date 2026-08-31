// The only place privacy logic for the browser extension lives. Called from
// background.js *before* anything is sent — a filtered URL never crosses the
// wire at all, unlike the Rust receiver (browser_server.rs), which can only
// reject what has already arrived.
//
// Mirrors the shell command hook's secret filter (hooks/humcon-log.sh, the
// HUMCON_SECRET_RE constant) in spirit, but deliberately narrower in scope.
// That pattern has bare `token`/`secret`/`password` alternatives applied to a
// whole command line, which is fine there — a shell command containing the
// word "token" is almost always about a real credential. Applied to a whole
// URL the same way, it would drop ordinary pages like
// github.com/anthropics/token-counter. Restricting the match to the query
// string and fragment keeps host and path reportable while still dropping
// `?token=...` — same spirit, correct blast radius for this data source.
//
// Matched against a lowercased copy, so the pattern itself is all lowercase —
// the same convention the shell hook uses.
const SECRET_PATTERN =
  /password|passwd|passphrase|secret|token|api[_-]?key|apikey|bearer|credential|private[_-]?key|[a-z_]*(key|token|secret|pass)[a-z_]*=|code=|sig=|signature=|x-amz-/;

/**
 * Whether a tab's URL is safe to report to the local HumCon server.
 *
 * Two independent reasons to say no:
 *  - Not http(s): browser-internal pages (chrome://, brave://, about:), local
 *    files, and extension pages carry no "what was I doing" value, and some
 *    (chrome://settings, a file:// path) are actively private.
 *  - The query string or fragment looks like it carries a credential: a
 *    password-reset link, an OAuth `code=`, a signed S3 URL. Session 2 set
 *    this precedent for shell commands: a dropped entry costs nothing, a
 *    leaked secret cannot be un-leaked.
 *
 * @param {string} rawUrl
 * @returns {boolean}
 */
export function shouldReport(rawUrl) {
  let url;
  try {
    url = new URL(rawUrl);
  } catch {
    return false; // not a parseable URL at all
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return false;
  }

  const sensitive = (url.search + url.hash).toLowerCase();
  if (SECRET_PATTERN.test(sensitive)) {
    return false;
  }

  return true;
}
