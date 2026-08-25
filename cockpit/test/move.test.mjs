import { strict as assert } from "node:assert";
import { test } from "node:test";
import { andList, approvalsLine, authorBlock, authored, moveOf, moveWhy, threads }
  from "../src/move.mjs";

// A pull request as the queue serialises one, with only the fields these rules read.
const pr = over => ({
  number: 7, lane: "waiting", reasons: ["author"], base_ref: "main",
  my_review: "none", review_is_current: false, review_decision: "", mergeable: null,
  merge_state: "", checks: "none", review_threads: [], review_threads_total: null,
  review_requests: [], ...over,
});
const thread = over => ({ id: "t", resolved: false, outdated: false, ...over });

test("a pull request awaiting your review is your move, and says which kind", () => {
  const asked = pr({ lane: "needs-you", reasons: ["reviewer"] });
  assert.equal(moveOf(asked), "yours");
  assert.equal(moveWhy(asked), "review not given");
  // The case the head-sha design exists for: you decided, and the branch moved past your decision.
  const back = pr({ lane: "needs-you", reasons: ["reviewer"], my_review: "approved", review_is_current: false });
  assert.equal(moveWhy(back), "your review is out of date");
});

test("an authored pull request with nothing outstanding is NOT your move", () => {
  const clean = pr({ review_decision: "APPROVED", mergeable: true, merge_state: "CLEAN" });
  assert.equal(authorBlock(clean), null);
  assert.equal(moveOf(clean), "theirs");
  assert.equal(moveWhy(clean), "");
});

// ---- SKEIN-303: THE CI RULE ----
//
// This is the one that will be "fixed" later by somebody who has not read it. The owner, verbatim:
// "CI pass isn't your responsibility, that is of whoever merges — unless ci-queue tag is attached
// and it fails then. So that differentiation is to be made. But this ci-queue thing is very
// specific to this repo. So I don't want to include that in generic workflow."
//
// If this assertion fails and the change that broke it added a `checks` branch to `authorBlock`,
// the change is wrong and the rule is right: the ci-queue behaviour arrives as repo configuration
// handed in, never as a branch inside the generic rule.
test("checks NEVER make an authored pull request your move", () => {
  for (const state of ["failing", "pending", "passing", "none"]) {
    const red = pr({ checks: state, failing_checks: [{ name: "build (nightly)", url: "u" }],
                     review_decision: "APPROVED", mergeable: true, merge_state: "CLEAN" });
    assert.equal(authorBlock(red), null, `checks:${state} moved an authored PR into your-move`);
    assert.equal(moveOf(red), "theirs", `checks:${state} moved an authored PR into your-move`);
  }
  // And red plus a real reason still names the REAL reason — never the checks.
  const both = pr({ checks: "failing", review_decision: "CHANGES_REQUESTED" });
  assert.equal(moveWhy(both), "changes requested");
});

test("changes requested on your own pull request is your move", () => {
  const p = pr({ review_decision: "CHANGES_REQUESTED" });
  assert.deepEqual(authorBlock(p), { kind: "changes", why: "changes requested" });
  assert.equal(moveOf(p), "yours");
});

test("unresolved threads on your own pull request are your move, and are counted", () => {
  const one = pr({ review_threads: [thread({ id: "a" })], review_threads_total: 1 });
  assert.equal(moveWhy(one), "1 thread unresolved");
  const some = pr({
    review_threads: [thread({ id: "a" }), thread({ id: "b", resolved: true }), thread({ id: "c" })],
    review_threads_total: 3,
  });
  assert.equal(moveWhy(some), "2 threads unresolved");
  // An OUTDATED thread is still open and still yours to answer — a different sentence, not a
  // resolved one (`prq::ReviewThread`).
  const old = pr({ review_threads: [thread({ id: "a", outdated: true })], review_threads_total: 1 });
  assert.equal(moveOf(old), "yours");
  // Every thread resolved is nothing outstanding.
  const done = pr({ review_threads: [thread({ id: "a", resolved: true })], review_threads_total: 1 });
  assert.equal(authorBlock(done), null);
});

test("a thread skein never fetched is counted as unseen, and never as open", () => {
  // The list is capped, so "open" is a floor and the shortfall is stated rather than guessed at.
  const p = pr({ review_threads: [thread({ id: "a", resolved: true })], review_threads_total: 4 });
  const t = threads(p);
  assert.equal(t.open.length, 0);
  assert.equal(t.unseen, 3);
  // And the shortfall does not promote the row: a thread nobody has seen is not evidence.
  assert.equal(moveOf(p), "theirs");
});

test("a conflict or a moved base is your move; an unknown mergeable is not", () => {
  assert.equal(moveWhy(pr({ mergeable: false })), "conflicts with main");
  assert.equal(moveWhy(pr({ merge_state: "DIRTY" })), "conflicts with main");
  assert.equal(moveWhy(pr({ merge_state: "BEHIND" })), "behind main");
  // `null` is "GitHub has not worked it out yet" and must never be reported as a conflict.
  assert.equal(authorBlock(pr({ mergeable: null, merge_state: "UNKNOWN" })), null);
});

test("the rule only applies to pull requests you opened", () => {
  // Somebody else's conflicted pull request, that you already decided on, is still their move: the
  // author rules answer an AUTHOR's question and this is not your branch to rebase.
  const theirs = pr({ reasons: ["reviewer"], mergeable: false, review_decision: "CHANGES_REQUESTED" });
  assert.equal(authored(theirs), false);
  assert.equal(authorBlock(theirs), null);
  assert.equal(moveOf(theirs), "theirs");
});

test("a verdict you just gave takes the row out of your move, before any refetch", () => {
  // The pane marks a row done in place and does not reload (SKEIN-162), so `lane` still reads
  // `needs-you` for the seconds after an approval lands. A list that kept claiming you for it would
  // be contradicting the act the reader just watched it take.
  const done = pr({ lane: "needs-you", reasons: ["reviewer"], my_review: "approved", review_is_current: true });
  assert.equal(moveOf(done), "theirs");
  // A COMMENT is not a decision — the same line `prq::my_review_state` draws.
  const noted = pr({ lane: "needs-you", reasons: ["reviewer"], my_review: "commented", review_is_current: true });
  assert.equal(moveOf(noted), "yours");
});

test("archived and not-ready keep their own buckets", () => {
  // You set it aside; that decision outranks everything, including a conflict on your own PR.
  assert.equal(moveOf(pr({ lane: "archived", mergeable: false })), "archived");
  assert.equal(moveOf(pr({ lane: "not-ready", reasons: ["reviewer"], draft: true })), "not-ready");
});

test("outstanding approvals name people and teams differently", () => {
  assert.equal(approvalsLine(pr({ review_requests: [] })), "");
  assert.equal(approvalsLine(pr({ review_requests: [{ name: "dana", team: false }] })),
    "waiting on @dana");
  assert.equal(approvalsLine(pr({ review_requests: [
    { name: "dana", team: false }, { name: "sam", team: false },
  ] })), "waiting on @dana and @sam");
  assert.equal(approvalsLine(pr({ review_requests: [
    { name: "acme/core", team: true }, { name: "dana", team: false },
  ] })), "waiting on the acme/core team and @dana");
  assert.equal(andList(["a", "b", "c"]), "a, b and c");
});
