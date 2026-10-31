import { test } from "node:test";
import assert from "node:assert/strict";
import { decisionAction, decisionRows, decisionsHtml, panelForHash } from "../src/decisions.mjs";

// Rows spelled the way `queue::Row` serialises them.
const row = (source, extra = {}) => ({
  source, need: "you", repo: "web", name: "web-main", headline: "keep the table?",
  state: "waiting", waiting_secs: 60, url: "", ...extra,
});

test("every kind of decision gets a card, and its button opens the place that answers it", () => {
  const html = decisionsHtml([
    row("question"),
    row("package", { headline: "apt: libnss3", why: "the browser tests need it" }),
    row("git-write", { headline: "acme/docs" }),
    row("box", { blocked: "question" }),
  ], 0, () => "1m ago");
  const opens = [...html.matchAll(/data-open="([^"]+)"/g)].map(m => m[1]);
  assert.deepEqual(opens, ["askq", "subq", "gitq", "terminal"]);
  assert.match(html, /Open its terminal to answer/);
  assert.match(html, /<span class="n">4<\/span>/);
});

// What would make it fail: a card drawn for a row the page has no action for — a setup fault, a
// pull request, a box that only stopped — which reads as a decision the cockpit lost.
test("no card is drawn without an action", () => {
  for (const r of [
    row("setup"), row("pull-request"), row("box"), row("box", { blocked: "permission" }),
    row("something-new"), row("question", { name: "" }),
  ]) {
    assert.equal(decisionAction(r), null, JSON.stringify(r));
  }
  assert.equal(decisionsHtml([row("setup"), row("box")], 0, null), "", "an empty strip is drawn");
  // Every card that is drawn carries exactly one button.
  const html = decisionsHtml([row("question"), row("setup"), row("package")], 0, null);
  assert.equal((html.match(/class="dcard"/g) || []).length, 2);
  assert.equal((html.match(/<button /g) || []).length, 2);
});

// What would make it fail: any box-written field interpolated without `decisionEsc`.
test("every word a box wrote is escaped", () => {
  const evil = "<b>x</b>\"'&";
  const html = decisionsHtml([
    row("package", { name: evil, headline: evil, why: evil }),
    row("box", { blocked: "question", name: evil, headline: evil }),
  ], 0, () => "now");
  assert.ok(!html.includes("<b>x</b>"), html);
  assert.ok(html.includes("&lt;b&gt;x&lt;/b&gt;&quot;&#39;&amp;"), html);
});

test("the server's order is kept", () => {
  const rows = [row("git-write", { name: "a" }), row("setup"), row("question", { name: "b" })];
  assert.deepEqual(decisionRows(rows).map(r => r.name), ["a", "b"]);
  assert.deepEqual(decisionRows(undefined), []);
});

// What would make it fail: a hash that names no panel opening one, or a decision's panel missing.
test("a hash opens the panel it names, and nothing else", () => {
  for (const p of ["askq", "subq", "gitq"]) assert.equal(panelForHash("#" + p), p);
  for (const h of ["", "#", "#box=web-main", "#health", "#askqx", "#constructor", null, undefined]) {
    assert.equal(panelForHash(h), null, String(h));
  }
});

// What would make it fail: the disk card opening a panel instead of carrying its own buttons, the
// buttons not sending back the ask the card was drawn from, a card drawn for a disk row with no
// ask to send back, or the box's name or reason not escaped.
test("a disk ask is decided on its card, with the ask it was drawn from", () => {
  const disk = { id: "d-0000aaaa", mb: 20480 };
  const html = decisionsHtml([row("disk", {
    name: "web-main", headline: "asks for 20 GB disk (now 10 GB, using 12.6 GB)",
    why: "the index is <14G>", disk,
  })], 0, () => "1m ago");
  assert.match(html, /<b>web-main<\/b> asks for 20 GB disk \(now 10 GB, using 12\.6 GB\): the index is &lt;14G&gt;/);
  const buttons = [...html.matchAll(/data-disk="(approve|deny)" data-box="web-main" data-id="d-0000aaaa" data-mb="20480">(\w+)</g)]
    .map(m => [m[1], m[2]]);
  assert.deepEqual(buttons, [["approve", "Approve"], ["deny", "Deny"]]);
  assert.ok(!html.includes("data-open"), html);
  assert.equal(decisionAction(row("disk")), null, "a disk row with no ask drew a card");
});

