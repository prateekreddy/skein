import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  DISMISS_CHOICES, dismissUntil, healthProblems, loginProblems, staleProblems, stillDismissed,
  trainProblems,
} from "../src/dismiss.mjs";

const HOUR = 60 * 60 * 1000;
const NOW = Date.UTC(2026, 9, 21, 9, 30);

test("the menu offers the three lengths the owner chose, an hour first", () => {
  // Fails if a choice is renamed, dropped or reordered: the first is what a plain ✕ means.
  assert.deepEqual(DISMISS_CHOICES.map(([, label]) => label), ["for 1 hour", "for 4 hours", "until tomorrow"]);
});

test("each choice ends when it says", () => {
  // Fails if the four-hour choice is computed as one, or an unknown choice hides for longer.
  assert.equal(dismissUntil("hour", NOW), NOW + HOUR);
  assert.equal(dismissUntil("hours4", NOW), NOW + 4 * HOUR);
  assert.equal(dismissUntil("anything else", NOW), NOW + HOUR);
  // "until tomorrow" is the next local midnight, whatever zone this runs in: later today never,
  // more than a day never, and on the stroke of a new day.
  const t = dismissUntil("tomorrow", NOW);
  const at = new Date(t), was = new Date(NOW);
  assert.ok(t > NOW && t - NOW <= 24 * HOUR, `${t - NOW}ms ahead`);
  assert.deepEqual([at.getHours(), at.getMinutes(), at.getSeconds()], [0, 0, 0]);
  assert.notEqual(at.getDate(), was.getDate());
});

test("a dismissal covers its problems until it runs out", () => {
  const rec = { ids: ["login:claude"], until: NOW + HOUR };
  assert.equal(stillDismissed(rec, ["login:claude"], NOW), true);
  // Fails if the expiry is ignored: a dismissal would last for ever.
  assert.equal(stillDismissed(rec, ["login:claude"], NOW + HOUR), false);
});

test("a problem the dismissal did not name shows the row", () => {
  const rec = { ids: ["login:claude"], until: NOW + HOUR };
  // Fails if the ids are not compared at all, or only the first is.
  assert.equal(stillDismissed(rec, ["login:codex"], NOW), false);
  assert.equal(stillDismissed(rec, ["login:claude", "login:codex"], NOW), false);
});

test("one of the dismissed problems going away is not a new problem", () => {
  // Fails if the comparison is equality rather than "every current id was dismissed".
  const rec = { ids: ["check:registry", "check:sbx"], until: NOW + HOUR };
  assert.equal(stillDismissed(rec, ["check:sbx"], NOW), true);
});

test("every doubt shows the row", () => {
  const ids = ["x"];
  for (const rec of [null, undefined, "x", {}, { ids: "x", until: NOW + HOUR }, { ids: ["x"] },
                     { ids: ["x"], until: String(NOW + HOUR) }]) {
    assert.equal(stillDismissed(rec, ids, NOW), false, JSON.stringify(rec));
  }
  // A row naming nothing cannot be covered by a dismissal of nothing.
  assert.equal(stillDismissed({ ids: [], until: NOW + HOUR }, [], NOW), false);
  // Further ahead than "until tomorrow" can reach: written by a clock that was wrong.
  assert.equal(stillDismissed({ ids, until: NOW + 30 * HOUR }, ids, NOW), false);
});

test("the environment row names its counted failures and its boxes, never their sentences", () => {
  const labels = ["registry", "sbx", "ai", "disk"].map(key => ({ key, label: key }));
  const health = {
    ok: false, labels, counted: ["registry", "sbx", "disk"],
    registry: { level: "unsatisfied", detail: "unreachable for 31s. check the network" },
    sbx: { level: "unknown", detail: "timed out" },
    ai: { level: "unsatisfied", detail: "off" },
    disk: { level: "satisfied" },
    dark_boxes: ["example-one"], stale_boxes: ["example-two"],
  };
  // Fails if an unknown or uncounted check is named, or the detail is part of the id.
  assert.deepEqual(healthProblems(health), ["check:registry", "dark:example-one", "stale:example-two"]);
  const later = { ...health, registry: { level: "unsatisfied", detail: "unreachable for 46s. check the network" } };
  assert.deepEqual(healthProblems(later), healthProblems(health));
  assert.deepEqual(healthProblems(null), []);
});

test("the login and train rows name what is stopped, not when or why", () => {
  assert.deepEqual(
    loginProblems([{ runtime: "claude", witness: "refusal", expired_at: "09:00", said: "401" },
                   { runtime: "codex", witness: "file", expired_at: "yesterday" }]),
    ["login:claude", "login:codex"]);
  assert.deepEqual(
    trainProblems([{ repo_id: "alpha", stopped: [{ number: 7, why: "CI failed" }, { number: 9, why: "conflict" }] },
                   { repo_id: "beta" }]),
    ["alpha#7", "alpha#9"]);
});

test("one outage is one problem, and the next outage is another", () => {
  // Fails if the id is built from the seconds count rather than the moment the server last spoke.
  assert.deepEqual(staleProblems(NOW), staleProblems(NOW));
  assert.notDeepEqual(staleProblems(NOW), staleProblems(NOW + 40_000));
});
