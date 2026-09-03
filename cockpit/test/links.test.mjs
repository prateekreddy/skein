import { test } from "node:test";
import assert from "node:assert/strict";
import { safeHref, asBrowserSeesIt, SAFE_SCHEMES } from "../src/links.mjs";

const BASE = "http://127.0.0.1:7878/";

// The concrete change that makes every assertion below fail is removing `safeHref` from
// `walkTokens` in `index.html`, which is what this guards. Each payload was run through the real
// vendored `marked` bundle before this test existed; they are the ones that actually rendered as
// live hrefs, not ones imagined for the test.
test("a scheme a document must not navigate to is refused", () => {
  for (const hostile of [
    "javascript:alert(1)",
    "JaVaScript:alert(1)",
    "JAVASCRIPT:alert(1)",
    "vbscript:msgbox(1)",
    "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
    "file:///etc/passwd",
    "blob:http://127.0.0.1:7878/abc",
  ]) {
    assert.equal(safeHref(hostile, BASE), null, `${hostile} must not be followed`);
  }
});

// The bypass that got through the first version of this guard, kept as its own test because reading
// the code does not find it: `new URL()` refuses `java&#9;script` as a scheme, so the string parsed
// as a RELATIVE reference, resolved to `http:` against the page, and passed — and the browser then
// decoded the attribute and ran it.
test("a scheme hidden behind an entity or a control character is refused", () => {
  for (const hostile of [
    "java&#9;script:alert(1)",
    "java&#10;script:alert(1)",
    "java&#x09;script:alert(1)",
    "java\tscript:alert(1)",
    "java\nscript:alert(1)",
    "java script:alert(1)",
    " javascript:alert(1)",
    "\u0000javascript:alert(1)",
  ]) {
    assert.equal(safeHref(hostile, BASE), null, `${JSON.stringify(hostile)} must not be followed`);
  }
});

test("what the browser sees is what is checked", () => {
  assert.equal(asBrowserSeesIt("java&#9;script:x"), "javascript:x");
  assert.equal(asBrowserSeesIt("java&#x09;script:x"), "javascript:x");
  assert.equal(asBrowserSeesIt(" a b "), "ab");
});

// The other half of the property, and the half a too-strict guard breaks: the ordinary links in
// every README the file viewer opens must still work, or this fix would have made the feature
// useless rather than safe.
test("the links a document is written with still work", () => {
  for (const fine of [
    "./README.md",
    "docs/architecture.md",
    "#a-heading",
    "https://example.com/a?b=1#c",
    "http://example.com",
    "mailto:someone@example.com",
    "img/diagram.png",
    "../sibling/file.md",
  ]) {
    assert.equal(safeHref(fine, BASE), fine, `${fine} is an ordinary link`);
  }
});

test("nothing at all is not a link", () => {
  assert.equal(safeHref("", BASE), null);
  assert.equal(safeHref("   ", BASE), null);
  assert.equal(safeHref(null, BASE), null);
  assert.equal(safeHref(undefined, BASE), null);
});

// An allow-list is the point; if this list ever grows, the growth should be a deliberate diff.
test("the allowed schemes are the three a rendered document needs", () => {
  assert.deepEqual(SAFE_SCHEMES, ["http:", "https:", "mailto:"]);
});
