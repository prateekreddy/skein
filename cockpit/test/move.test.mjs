import { strict as assert } from "node:assert";
import { test } from "node:test";
import { andList, approvalsLine, authorBlock, authored, moveNote, moveOf, moveWhy, threads,
  yourMoveCount } from "../src/move.mjs";

// A pull request as the queue serialises one, with only the fields these rules read.
const pr = over => ({
  number: 7, lane: "waiting", reasons: ["author"], base_ref: "main",
  my_review: "none", review_is_current: false, my_review_requested: false,
  review_decision: "", mergeable: null,
  merge_state: "", checks: "none", review_threads: [], review_threads_total: null,
  review_requests: [], ...over,
});
// The one somebody else opened and asked you to look at — the shape all three of the owner's
// corrections below are about.
const asked = over => pr({ lane: "needs-you", reasons: ["reviewer"], ...over });
const thread = over => ({ id: "t", resolved: false, outdated: false, ...over });

test("a pull request awaiting your review is your move, and says which kind", () => {
  const waiting = asked();
  assert.equal(moveOf(waiting), "yours");
  assert.equal(moveWhy(waiting), "review not given");
  // A note you left is not a verdict, so the row is still yours and still says so plainly.
  assert.equal(moveWhy(asked({ my_review: "commented" })), "review not given");
});

// ---- SKEIN-354: WHOSE MOVE AN APPROVAL IS ----
//
// The owner, on a pull request he had approved that was still showing as needing him: "approved
// should come only if my review status on the PR is approved rn, if I approved and then some file I
// own changed, so github asks me to review again then it should show that."
//
// The rule these hold: your verdict is GitHub's answer to "is it standing", and only GitHub asking
// you again takes it back. Nothing here may reach for `review_is_current` — it was false on all 26
// rows of his live queue, including the two he had approved himself, which is how the old rule came
// to say "your review is out of date" about everything.
test("an approval stands after the branch moves, and comes back when GitHub asks again", () => {
  const stands = asked({ my_review: "approved", review_is_current: false });
  assert.equal(moveOf(stands), "theirs", "a push threw away an approval GitHub still holds");
  assert.equal(moveWhy(stands), "", "a row that is not in the your-move list gave a reason for being in it");
  // THE COUNTER-CASE, and it is the half he asked for by name: a file he owns changed, CODEOWNERS
  // re-requested him, and that lands in `review_requests` as him. Then it IS his again.
  const again = asked({ my_review: "approved", review_is_current: false, my_review_requested: true,
                        review_requests: [{ name: "you", team: false }] });
  assert.equal(moveOf(again), "yours", "GitHub asked again and the row did not come back");
  assert.equal(moveWhy(again), "asked to review again");
  // Requesting changes is a verdict too, and it behaves the same on both sides.
  assert.equal(moveOf(asked({ my_review: "changes-requested" })), "theirs");
  assert.equal(moveWhy(asked({ my_review: "changes-requested", my_review_requested: true })),
    "asked to review again");
  // And a comment is still not a verdict, so a re-request on one is an ordinary unreviewed row.
  assert.equal(moveWhy(asked({ my_review: "commented", my_review_requested: true })), "review not given");
});

// The sentence he chose for the row, over "back to you the moment anything lands":
// WAITING ON OTHERS · you approved · moved since — they have not re-asked.
test("a row that is not yours still says what became of your review", () => {
  assert.equal(moveNote(asked({ my_review: "approved", review_is_current: false })),
    "you approved · moved since — they have not re-asked");
  // Nothing has moved: the note is the verdict alone, with no claim about commits.
  assert.equal(moveNote(asked({ my_review: "approved", review_is_current: true })), "you approved");
  assert.equal(moveNote(asked({ my_review: "changes-requested", review_is_current: false })),
    "you asked for changes · moved since — they have not re-asked");
  // A row that IS yours says why it is yours, and says it in the your-move list — never both.
  assert.equal(moveNote(asked({ my_review: "approved", my_review_requested: true })), "");
  assert.equal(moveNote(asked()), "");
  // Archived is a decision to stop hearing about it, so it hears nothing.
  assert.equal(moveNote(pr({ lane: "archived", my_review: "approved" })), "");
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

// ---- SKEIN-354: A CONFLICT IS NOT A CLAIM ON YOU UNLESS THE BRANCH IS YOURS ----
//
// The owner, shown the conflicted root of somebody else's stack: "why is it my move at all, it is
// not PR I created, so if there are conflicts that's PR owner problem, not mine. So as far as I am
// concerned my work there is done. You can still say that conflicts or whatever as info but it is
// not mine to fix."
//
// Both halves are asserted here, because implementing one and forgetting the other is how this
// lands wrong: the row must not be in the your-move list, and it must still SAY the conflict.
test("somebody else's conflict is information, and yours is still your move", () => {
  const lane = { lane: "not-ready", reasons: ["reviewer"] };
  const dirty = pr({ ...lane, mergeable: false, merge_state: "DIRTY" });
  assert.equal(moveOf(dirty), "not-ready", "a conflict on a branch you did not open claimed you");
  assert.equal(moveWhy(dirty), "", "a conflict you cannot fix was given as your reason to act");
  assert.equal(moveNote(dirty), "conflicts with main — theirs to fix");
  // THE COUNTER-CASE: the branch is yours, so the conflict is the one thing on this list you can
  // actually do something about, and it is your move exactly as it always was.
  const mine = pr({ reasons: ["author"], mergeable: false, merge_state: "DIRTY" });
  assert.equal(moveOf(mine), "yours", "a conflict on your OWN branch stopped being your move");
  assert.equal(moveWhy(mine), "conflicts with main");
  // `null` is "GitHub has not worked it out yet" — 17 of the owner's 26 live rows — and it must
  // never be reported as a conflict on either side of that line.
  assert.equal(moveNote(pr({ ...lane, mergeable: null, merge_state: "UNKNOWN" })), "");
  // And a decided row says what became of your review rather than the merge state: one sentence,
  // and the one about YOU is the one worth the pixels.
  assert.equal(moveNote(pr({ ...lane, mergeable: false, merge_state: "DIRTY", my_review: "approved" })),
    "you approved · moved since — they have not re-asked");
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

// ---- SKEIN-323: THE NUMBER ON THE BUTTON IS THIS LIST ----
//
// The badge used to be `Lane::NeedsYou` counted on the server until somebody opened the pane, and
// the pane's own count then replaced it — so the two rows below are the difference between the two
// answers, and they are the rows a person is most likely to be waiting on: their own.
//
// If this ever passes while `moveOf` and the badge disagree, the count has been written twice.
test("the badge's number is the your-move list, not the reviewer's lane", () => {
  const rows = [
    // In the reviewer's lane and in the list: nobody has your verdict yet.
    asked({ number: 1 }),
    // In the reviewer's lane and NOT in the list: you decided and nobody asked again.
    asked({ number: 2, my_review: "approved" }),
    // NOT in the reviewer's lane and IN the list — the whole of the bug. You opened it, so the
    // lane calls it waiting; changes were requested on it, so it is waiting on nobody but you.
    pr({ number: 3, review_decision: "CHANGES_REQUESTED" }),
    // Also yours, also outside the lane: a thread nobody has answered.
    pr({ number: 4, review_threads: [thread({ id: "a" })], review_threads_total: 1 }),
    // Yours, and nothing outstanding: their move, whatever its checks say (SKEIN-303).
    pr({ number: 5, review_decision: "APPROVED", mergeable: true, merge_state: "CLEAN" }),
    // Set aside by hand, and a draft: neither is claiming you.
    pr({ number: 6, lane: "archived" }),
    pr({ number: 7, lane: "not-ready", reasons: ["reviewer"] }),
  ];
  // **The fixture has to be a case where the two answers DIFFER, or this test cannot fail.** A
  // `yourMoveCount` that counted the lane instead passes every assertion below on a list where the
  // lane and the list happen to be the same size, which is most lists.
  const lane = rows.filter(p => p.lane === "needs-you").length;
  assert.equal(lane, 2);
  assert.equal(yourMoveCount(rows), 3);
  assert.notEqual(yourMoveCount(rows), lane,
    "the fixture stopped being a case where the lane and the list disagree, so this test would now "
    + "pass against the bug it is here for");
  // Which three, said out loud — a count that is right for the wrong rows is the failure this test
  // would otherwise be blind to.
  assert.deepEqual(rows.filter(p => moveOf(p) === "yours").map(p => p.number), [1, 3, 4]);
  // A repo with nothing open, and one whose count could not be taken at all: the server sends an
  // empty list for both, and the badge must read them as nothing rather than as unknown.
  assert.equal(yourMoveCount([]), 0);
  assert.equal(yourMoveCount(undefined), 0);
});
