import { test } from "node:test";
import assert from "node:assert/strict";
import { summaryOf, noteOf, mentionsLabel, shapeUrl } from "../src/change.mjs";

test("the summary is the modules added up, not a field to trust", () => {
  const s = summaryOf([{ added: 400, removed: 100 }, { added: 12, removed: 80 }, {}]);
  assert.deepEqual(s, { modules: 3, added: 412, removed: 180 });
  assert.deepEqual(summaryOf(null), { modules: 0, added: 0, removed: 0 });
});

test("a stale note is not shown — it reads exactly like a current one", () => {
  assert.deepEqual(noteOf({ note_state: "fresh", note: "the host side of create" }),
    { state: "fresh", text: "the host side of create" });
  const stale = noteOf({ note_state: "stale", note: "what it used to be" });
  assert.equal(stale.state, "stale");
  assert.ok(!stale.text.includes("what it used to be"), "the stale text leaked onto the screen");
  assert.equal(noteOf({ note_state: "absent" }).state, "absent");
  assert.equal(noteOf(null).state, "absent");
});

test("no count and a count of zero are different answers", () => {
  // The dangerous one is the reassuring one: "0 mentions" beside a signal nobody counted reads as
  // "nothing uses this".
  assert.equal(mentionsLabel({ what: "src/old.rs was deleted" }), "");
  assert.equal(mentionsLabel({ mentions: 0 }), "0 mentions");
  assert.equal(mentionsLabel({ mentions: 1 }), "1 mention");
  assert.equal(mentionsLabel({ mentions: 12 }), "12 mentions");
  assert.equal(mentionsLabel(null), "");
});

// Both URLs are checked against the router's real table by
// `cockpit::tests::the_change_view_asks_a_url_this_router_answers`, which runs this function and
// matches its answer against the `.route(…)` entries in `src/bin/skein-server/main.rs`. What is asserted
// here is the shape of the transform — the `#` comes off, a missing repo yields no URL — because
// this file can call the function with inputs the Rust test would have to build a fleet to produce.
test("a branch and a pull request ask the same question of different routes", () => {
  assert.equal(shapeUrl({ source: "box", name: "web-main" }), "/api/boxes/web-main/shape");
  // `/api/repos/:id/review/:number/shape`, the route the server registers — a pull request's shape
  // is one of the review routes. It read `/api/pr/…` here until SKEIN-246, and this line asserting
  // that spelling was one of the three things that kept the view 404ing.
  assert.equal(shapeUrl({ source: "pull-request", repo: "web", name: "#412" }), "/api/repos/web/review/412/shape");
  // A setup fault has no shape, and a pull request with no repo cannot be asked about.
  assert.equal(shapeUrl({ source: "setup", name: "sbx" }), "");
  assert.equal(shapeUrl({ source: "pull-request", name: "#412" }), "");
  assert.equal(shapeUrl(null), "");
});

test("a box whose name would change the path is escaped, not interpolated", () => {
  assert.equal(shapeUrl({ source: "box", name: "a/b" }), "/api/boxes/a%2Fb/shape");
});
