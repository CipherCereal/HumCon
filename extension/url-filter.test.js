// Run with: node --test extension/url-filter.test.js
// No test framework dependency needed — node:test is built in, and this is
// the only test file in the extension, so pulling one in isn't worth it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldReport } from "./url-filter.js";

test("ordinary http(s) pages are reported", () => {
  assert.equal(shouldReport("https://github.com/anthropics/humcon"), true);
  assert.equal(shouldReport("http://example.com/"), true);
  assert.equal(shouldReport("https://google.com/search?q=rust"), true);
});

// The regression the narrowed (query+fragment-only) scope exists to prevent:
// applying the shell hook's bare "token"/"secret" alternatives to a whole URL
// would have dropped this.
test("the word token/secret/key in the host or path is not enough to drop a page", () => {
  assert.equal(shouldReport("https://github.com/anthropics/token-counter"), true);
  assert.equal(shouldReport("https://example.com/api-keys/docs"), true);
  assert.equal(shouldReport("https://example.com/secret-santa"), true);
});

test("query strings that look like credentials are dropped", () => {
  assert.equal(shouldReport("https://example.com/reset?token=abc123"), false);
  assert.equal(shouldReport("https://example.com/login?password=hunter2"), false);
  assert.equal(shouldReport("https://example.com/oauth/callback?code=xyz"), false);
  assert.equal(
    shouldReport("https://s3.amazonaws.com/bucket/file?X-Amz-Signature=abc"),
    false,
  );
  assert.equal(shouldReport("https://example.com/download?sig=abc123"), false);
  assert.equal(shouldReport("https://example.com/verify?api_key=abc"), false);
});

test("credentials in the fragment are dropped too", () => {
  assert.equal(shouldReport("https://example.com/page#access_token=abc123"), false);
});

test("non-http(s) schemes are never reported", () => {
  assert.equal(shouldReport("chrome://extensions"), false);
  assert.equal(shouldReport("brave://settings"), false);
  assert.equal(shouldReport("about:blank"), false);
  assert.equal(shouldReport("file:///C:/secrets.txt"), false);
  assert.equal(shouldReport("chrome-extension://abcdefg/options.html"), false);
});

test("unparseable input is rejected, not thrown", () => {
  assert.equal(shouldReport("not a url"), false);
  assert.equal(shouldReport(""), false);
});
