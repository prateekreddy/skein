import { test } from "node:test";
import assert from "node:assert/strict";
import { safeHref, asBrowserSeesIt, SAFE_SCHEMES } from "../src/links.mjs";

const BASE = "http://127.0.0.1:7878/";

// Each payload was run through the real vendored `marked` bundle before it was written down here;
// they are the ones that actually rendered as live hrefs, not ones imagined for the test. What this
// file can and cannot see is worth being exact about: it imports `links.mjs`, so the change that
// fails it is an edit to `safeHref`, `asBrowserSeesIt` or `SAFE_SCHEMES` — a page that stopped
// calling the guard would leave every assertion here green. The concrete changes that fail each
// test below are named above it.
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

// The same trick spelled with NAMED references, which is how it walked through the fix for the
// numeric one: that decoded `&#9;` and `&#x09;` and nothing else, so `java&Tab;script:` and
// `javascript&colon;` were emitted verbatim and rendered as live hrefs. Every payload here was run
// through the vendored marked v12.0.2 with the page's own `marked.use` block; before the named
// references were decoded, each of these came back as `<a href="…">`.
//
// Fails on: taking `Tab`, `NewLine` or `colon` out of `ASCII_NAMED` in `links.mjs`.
test("a scheme hidden behind a named character reference is refused", () => {
  for (const hostile of [
    "java&Tab;script:alert(1)",
    "java&NewLine;script:alert(1)",
    "javascript&colon;alert(1)",
    "JaVa&Tab;ScRiPt&colon;alert(1)",
    "data&colon;text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
    "&Tab;javascript:alert(1)",
  ]) {
    assert.equal(safeHref(hostile, BASE), null, `${hostile} must not be followed`);
  }
});

// The bypass the fix for the one above would have introduced on its own, and the reason the decoding
// runs to a fixed point.
//
// `safeHref` hands its answer back to be written into the document, so the document gets a string
// that has been decoded once already — and the browser decodes what it is given. Decode `&amp;#9;`
// once and the result is `&#9;`: inert in the browser that produced it, a tab in the browser that
// reads it back. Through the real pipeline, a one-pass decoder plus the write-back renders
// `[c](java&amp;#9;script:alert(1))` as `href="java&#9;script:alert(1)"`, which runs on click —
// a payload that is harmless with no fix at all.
//
// Fails on: `MAX_DECODE_PASSES = 1` *and* deleting the settle check from `safeHref` — which
// together are the one-pass fix described above. Either one alone still refuses these, which is the
// point of having both: the loop decides what the document is given, the settle check decides what
// is refused outright.
test("a reference that decodes into another reference is refused", () => {
  for (const hostile of [
    "java&amp;#9;script:alert(1)",
    "java&amp;Tab;script:alert(1)",
    "javascript&amp;colon;alert(1)",
    "java&amp;amp;#9;script:alert(1)",
  ]) {
    assert.equal(safeHref(hostile, BASE), null, `${hostile} must not be followed`);
  }
});

// Nested past the point where it is a link somebody wrote. Bounded rather than looped to exhaustion
// because a README is under no obligation to be sane and each pass rescans the string; unsettled at
// the bound is refused rather than half-decoded, since a half-decoded href is exactly the case
// above.
//
// Fails on: deleting the `decodeOnce(seen) !== seen` line from `safeHref`.
test("a string still decoding at the bound is refused rather than guessed at", () => {
  const nested = `java&${"amp;".repeat(40)}#9;script:alert(1)`;
  assert.equal(safeHref(nested, BASE), null);
});

// Fails on: `return href` instead of `return seen` at the end of `safeHref`.
test("what comes back is the string the browser will parse, not the one that came in", () => {
  assert.equal(safeHref("https://example.com/a?b=1&amp;c=2", BASE), "https://example.com/a?b=1&c=2");
  assert.equal(safeHref("https://example.com/&Tab;a", BASE), "https://example.com/a");
});

// And the price of settling it, which is worth stating rather than discovering: a destination that
// was escaped twice comes back with one layer taken off rather than being refused. The browser would
// have decoded `&amp;amp;` to `&amp;` and this hands it `&`. Nothing here can change a scheme —
// every named reference that expands to an ASCII character is in the table, so what survives
// undecoded cannot become a `:` — but it is a rewritten address, and refusing an ordinary URL over
// a doubled ampersand would be the worse of the two.
//
// Fails on: `MAX_DECODE_PASSES = 1`, which refuses this instead.
test("a destination escaped twice is still a link", () => {
  assert.equal(safeHref("https://example.com/?q=&amp;amp;x", BASE), "https://example.com/?q=&x");
});

// Fails on: adding `nbsp` (or any non-ASCII name) to `ASCII_NAMED`; widening the `[\t\n\r]` strip to
// every C0 character and space.
test("what the browser sees is what is checked", () => {
  assert.equal(asBrowserSeesIt("java&#9;script:x"), "javascript:x");
  assert.equal(asBrowserSeesIt("java&#x09;script:x"), "javascript:x");
  assert.equal(asBrowserSeesIt("java&Tab;script:x"), "javascript:x");
  assert.equal(asBrowserSeesIt("javascript&colon;x"), "javascript:x");
  // An interior space is NOT closed up. The URL parser percent-encodes it; a normaliser that
  // removed it would rewrite `http://e.com/a b` into a different address than the one written.
  assert.equal(asBrowserSeesIt(" a b "), "a b");
  // Decoding more than the browser needs is its own bug: a reference that cannot contribute a
  // scheme character is left exactly as it was found.
  assert.equal(asBrowserSeesIt("a&nbsp;b"), "a&nbsp;b");
  assert.equal(asBrowserSeesIt("a&colonb"), "a&colonb");
});

// The other half of the property, and the half a too-strict guard breaks: the ordinary links in
// every README the file viewer opens must still work, or this fix would have made the feature
// useless rather than safe. A guard that refuses everything passes the tests above.
//
// The last five carry the text the entity decoder is looking for and are not entities — a query
// string of its own, a name the table does not hold, a name with no semicolon after it, an ampersand
// in a file name, and a space. Each of them comes back byte for byte, because the answer is written
// into the document and an address is not the guard's to rewrite.
//
// Fails on: decoding a named reference without its `;`; putting a non-ASCII name in `ASCII_NAMED`;
// stripping interior spaces in `asBrowserSeesIt`.
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
    "https://example.com/search?q=tabs&sort=new",
    "https://example.com/?q=a&nbsp;b",
    "https://example.com/?q=colon&colon",
    "docs/a&b.md",
    "http://example.com/a b",
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
