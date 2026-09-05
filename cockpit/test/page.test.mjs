// Three properties of `src/web/index.html` that nothing else is in a position to check.
//
// The cockpit's testable code lives in `cockpit/src` and is imported by the suites beside this one.
// These three are not about a function: they are about the 10,000-line inline script as a text, and
// each of them is a bug that shipped. `tests/page_scripts.rs` holds the page properties that are
// about *declarations*; these are about what the page builds and what it calls, which is JavaScript
// reasoning, so they are written in JavaScript.
//
// Reading the page from here is deliberate. The suites that only import `cockpit/src` cannot observe
// the page at all — a page that stopped calling `safeHref` would leave every assertion in
// `links.test.mjs` green — so a claim about the page has to open the page.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const PAGE = readFileSync(new URL("../../src/web/index.html", import.meta.url), "utf8");
const BUNDLE = readFileSync(new URL("../../src/web/vendor/cockpit.js", import.meta.url), "utf8");

// `onclick="f('${esc(x)}')"` is an XSS site, and the `'` that `esc` encodes does not close it: an
// attribute value is entity-decoded *before* the JS parser reads it, so `&#39;` reaches the parser
// as a quote and ends the string. Measured in Chromium with `x` = `x'+(window.PWNED=1)+'y`: the
// hand-quoted button ran the payload, the `esc(JSON.stringify(x))` button received the text.
//
// Forty-six sites were written that way. `m.path` at one of them is a top-level directory name from
// the repository being reviewed, so landing a directory on a base branch was enough to reach it.
//
// Fails on: writing one back — `onclick="f('${esc(x)}')"` anywhere in the page.
test("no handler attribute builds a JavaScript string by quoting an interpolation", () => {
  // Whole-line comments dropped first: the rule is written down beside `esc`, and writing it down
  // means spelling out the shape it forbids.
  const code = PAGE.split("\n").filter(l => !l.trimStart().startsWith("//")).join("\n");
  const sites = [...code.matchAll(/'\$\{/g)].map(m => code.slice(m.index, m.index + 60));
  assert.deepEqual(sites, [], "a string argument in a handler is esc(JSON.stringify(x))");
});

// Fails on: dropping any of the five characters from `esc`.
test("esc encodes everything that can end an attribute or a tag", () => {
  const line = PAGE.match(/^const esc = .*$/m);
  assert.ok(line, "the page still defines esc on one line");
  const esc = new Function(`${line[0]}; return esc;`)();
  assert.equal(esc(`&<>"'`), "&amp;&lt;&gt;&quot;&#39;");
  assert.equal(esc(null), "null", "coercion is unchanged — callers pass it anything");
});

// A syntax error anywhere in the inline script fails the WHOLE script before a line of it runs, and
// nothing else here would see it: `tests/page_scripts.rs` reads declarations out of the text, the
// suites beside this one import modules the page does not contain, and `cargo build` never opens the
// page. The 46 handler rewrites above are 46 chances to unbalance a template literal.
//
// Parsed, not run: `new Function` compiles the body and stops.
//
// Fails on: any unbalanced brace, quote or backtick in the page's inline script.
test("the page's inline script parses", () => {
  const blocks = [...PAGE.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)];
  assert.equal(blocks.length, 1, "the page is one inline script — see tests/page_scripts.rs");
  new Function(blocks[0][1]);
});

// `refresh()` was called on the success path of a fleet rebuild and of every box-settings save, and
// was never declared. Both calls sit inside a `.then` whose `.catch` reports `e.message`, so the
// ReferenceError was rendered as the operation failing: a rebuild that worked toasted "resize
// failed: refresh is not defined".
//
// Zero-argument calls only, which is what makes this cheap and exact rather than a parser: a bare
// `name()` statement is a call on a function of the page's own, and there are 119 of them.
//
// Fails on: deleting `function refresh`, or adding a call to anything undeclared.
test("the page declares every function it calls by bare name", () => {
  const declared = new Set();
  for (const src of [PAGE, BUNDLE]) {
    for (const m of src.matchAll(/(?:^|\s)(?:function|const|let|var|class)\s+([A-Za-z_$][\w$]*)/g)) {
      declared.add(m[1]);
    }
    for (const m of src.matchAll(/([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?(?:function|\()/g)) {
      declared.add(m[1]);
    }
  }
  const called = new Set();
  for (const m of PAGE.matchAll(/(?:^|[;{}\s])(?<!new\s)([A-Za-z_$][\w$]*)\(\)\s*;/g)) {
    if (!/\bnew\s+$/.test(PAGE.slice(Math.max(0, m.index - 5), m.index + m[0].indexOf(m[1])))) {
      called.add(m[1]);
    }
  }
  assert.deepEqual([...called].filter(n => !declared.has(n)).sort(), []);
});
