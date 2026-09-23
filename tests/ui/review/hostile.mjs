// The review pane's browser suite, a pull request written to break out of the page.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import fs from "node:fs";
import path from "node:path";
import { check, fx, page, refreshQueue, until } from "./setup.mjs";

// ---------- a pull request written to break out ----------
//
// Two strings on a row are chosen by whoever opened the pull request and reach the page verbatim:
// its TITLE, which the row draws as text, and its HEAD BRANCH, which the expanded row draws as an
// ARGUMENT inside an `onclick`. Nothing narrows either on the way — `prq::node` copies the search
// node's `headRefName` into `head_ref` unchanged — so the page's own escaping is the whole of the
// defence.
//
// This is the tier that can judge it. What is at stake is how a BROWSER reads the attribute: the
// value is entity-decoded *before* the JS parser sees it, so `&#39;` arrives at the parser as a
// quote and `onclick="f('${esc(x)}')"` runs whatever the branch name says, while
// `onclick="f(${esc(JSON.stringify(x))})"` hands the same branch name over as text. No assertion
// over a string a test built can tell those two apart; a click can. The page states that rule
// beside `esc`, and the rule has been broken twice — a bare `JSON.stringify` cut a button's handler
// in half (SKEIN-261) and `javascript:` walked into an `href` (SKEIN-602).
//
// Every character in the branch below is legal in a git ref — no space, none of `~^:?*[\` — so it
// is a branch somebody can really push, and the `"` and the `)` are what would end the argument.
console.log("\na pull request written to break out");
const HOSTILE_REF = `x'+(window.PWNED=1)+'y");window.PWNED=1;("<img/src=x/onerror=window.PWNED=1>`;
const HOSTILE_TITLE = `she said "it's fine" (really) <img src=x onerror="window.PWNED=1">`;
{
  const searches = path.join(fx.root, "search-review-requested.json");
  const nodes = JSON.parse(fs.readFileSync(searches, "utf8"));
  fs.writeFileSync(searches, JSON.stringify([...nodes, {
    ...nodes[0], number: 7, title: HOSTILE_TITLE, headRefName: HOSTILE_REF, headRefOid: "sha7",
    url: "https://github.com/acme/thing/pull/7", updatedAt: "2026-08-07T00:00:00Z",
  }]));
}
await refreshQueue();
const hostileRow = () => page.locator("#revpane .revrow")
  .filter({ has: page.locator(".revtitle", { hasText: "she said" }) });

// Refuses: a title of `<img src=x onerror="window.PWNED=1">` becoming an element instead of words.
await check("a title carrying markup is drawn as text and builds nothing", async () => {
  const title = await page.evaluate(() => {
    const el = [...document.querySelectorAll("#revpane .revtitle")]
      .find(e => e.textContent.includes("she said"));
    return el ? { text: el.textContent, built: [...el.children].map(e => e.tagName) } : null;
  });
  if (!title) throw new Error("the hostile row is not on screen at all");
  // The elements first, because that is the failure: a title that became an `<img>` also reads as a
  // title that was truncated, and being told the wrong one of those sends the reader elsewhere.
  if (title.built.length)
    throw new Error(`the title built ${JSON.stringify(title.built)} instead of saying them`);
  if (title.text !== HOSTILE_TITLE)
    throw new Error(`the title is not the one the pull request carries: ${JSON.stringify(title.text)}`);
});

// The row has to be OPEN for the branch to be drawn at all — `revBody` is what carries the handler.
await hostileRow().first().click();
// And it being open is waited for, because both checks below read the button with `count()`, which
// answers 0 immediately: "the open row draws 0 branch buttons" is what this file would say about a
// row that simply had not opened yet, and it would say it about the escaping rule.
await until(() => [...document.querySelectorAll("#revpane .revrow")]
  .some(r => /she said/.test(r.textContent || "") && r.querySelector(".revbody")), null,
  "the hostile row never opened, so nothing below is about how its branch name is quoted");

// Refuses: a branch name whose `"` ends the attribute early, which is what leaves the rest of the
// handler behind as further attributes — `onclick="…f(" y");window…("` parses as three of them.
// Measured that way rather than by reading the attribute back, because the browser's own parse is
// the fact in question (SKEIN-261 was found by counting attributes on the rendered button).
await check("the branch name stays inside the handler attribute", async () => {
  const box = hostileRow().locator("button:has-text('box on this branch')");
  if (await box.count() !== 1)
    throw new Error(`the open row draws ${await box.count()} branch buttons`);
  const attrs = await box.evaluate(e => [...e.attributes].map(a => a.name).sort());
  if (JSON.stringify(attrs) !== JSON.stringify(["class", "onclick", "type"]))
    throw new Error(`the attribute did not hold: ${JSON.stringify(attrs)}`);
});

// Refuses: `onclick="openNewBoxFor('${esc(repo)}', '${esc(ref)}')"` — the hand-quoted form the page
// forbids. With it, `&#39;` decodes to a quote before the JS parser runs and the argument this
// asserts on is `x` with the payload evaluated beside it; with `esc(JSON.stringify(…))` the handler
// is handed the branch name whole.
//
// `openNewBoxFor` is replaced rather than watched, so the click proves what the handler RECEIVED
// without opening the launch dialog over the pane. Nothing below this point calls it.
await check("and the handler is given the branch name, whole", async () => {
  await page.evaluate(() => { window.SAW = []; window.openNewBoxFor = (...a) => window.SAW.push(a); });
  await hostileRow().locator("button:has-text('box on this branch')").click();
  // The handler having been called is the observable, and the check is about what it was GIVEN — so
  // a beat that expired first would report `[]`, which reads as a handler that was never wired up.
  await until(() => (window.SAW || []).length > 0, null,
    "pressing the branch button called nothing at all");
  const saw = await page.evaluate(() => window.SAW);
  if (JSON.stringify(saw) !== JSON.stringify([["acme", HOSTILE_REF]]))
    throw new Error(`the handler was called with ${JSON.stringify(saw)}`);
});

// The other end of the same rule, one layer down: a link inside the MARKDOWN a row renders — a
// model's brief, a README, a comment — where the string is judged by `safeHref` rather than by
// `esc`. Two payloads, and the second is the one that looks harmless.
//
// `java&Tab;script:` is refused because the guard decodes what a browser decodes: `&Tab;` is a tab,
// the URL parser drops tabs, and `javascript:` is not a scheme this page follows. Decoding only the
// NUMERIC references — which is what the guard did until 2026-09-05 — lets this one through as a
// relative reference, and the browser then runs it.
//
// `https&colon;//example.com/a&Tab;b` is allowed, and what is checked there is that the attribute
// holds the string the guard JUDGED — `https://example.com/ab`, the tab dropped the way the URL
// parser drops it — rather than the one the markdown carried. Judging one spelling and handing the
// browser another is the shape of every bypass this guard has had, so `walkTokens` writes the
// normalised href back onto the token (src/web/index.html).
//
// Driven through the page's own `marked`, configured by the page's own `marked.use`, and read off a
// real anchor: `a.protocol` is the browser's answer to "where would this go", which is the question.
await check("a link in rendered markdown is inert, or is the address the guard read", async () => {
  const seen = await page.evaluate(() => {
    const host = document.createElement("div");
    host.innerHTML = marked.parse("[c](java&Tab;script:alert(1)) and [d](https&colon;//example.com/a&Tab;b)");
    document.body.appendChild(host);
    const out = [...host.querySelectorAll("a")].map(a => ({ href: a.getAttribute("href"), protocol: a.protocol }));
    host.remove();
    return out;
  });
  const [refused, allowed] = seen;
  if (seen.length !== 2) throw new Error(`the markdown made ${seen.length} links: ${JSON.stringify(seen)}`);
  if (refused.protocol === "javascript:" || refused.href !== "")
    throw new Error(`the entity-spelled scheme survived: ${JSON.stringify(refused)}`);
  // The tab is gone because the URL parser drops it, so `https://example.com/ab` is where this
  // anchor really goes — and therefore the only string that belongs in the attribute.
  if (allowed.href !== "https://example.com/ab")
    throw new Error(`the anchor holds a string the guard never judged: ${JSON.stringify(allowed)}`);
});

// The half neither check above would notice on its own: an argument can be mangled AND the payload
// beside it can still run, and a payload that runs quietly leaves every other assertion green.
await check("and nothing the pull request carried has run", async () => {
  const ran = await page.evaluate(() => window.PWNED);
  if (ran !== undefined) throw new Error(`the payload ran: window.PWNED is ${JSON.stringify(ran)}`);
});
