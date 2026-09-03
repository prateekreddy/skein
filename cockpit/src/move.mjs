// Whose move is a pull request — across BOTH of your roles — and why, in words.
//
// The queue's `lane` (src/prq.rs) is the REVIEWER's answer: somebody asked you to review this and
// nothing stops you giving it. It is a good answer and it stays. It is not the whole of the
// question the owner asked — "I want to know what needs me very clearly" — because a pull request
// YOU opened is `Lane::Waiting` by definition, however stuck it is (SKEIN-300).
//
// **Where this rule lives, decided rather than defaulted** (SKEIN-302). It is the PAGE's, not the
// server's, for two reasons. The lane answers "is this review work for you" and is a fact about one
// pull request; this answers "does this need your hands", which mixes the roles and is a claim
// about the pane's TOP LEVEL. And every field it reads — `reasons`, `review_threads`, `mergeable`,
// `merge_state`, `review_decision`, `my_review`, `my_review_requested` — is already on the payload,
// so nothing about it needs a request. The last of those is the newest and was added for this rule
// (SKEIN-354): the queue carries GitHub's answer to "is skein still being asked", because the page
// cannot work that out and must not guess at it.
//
// It is a cockpit module rather than a function in the page for the reason `Pr::settled` and
// SKEIN-243 are both records of: **a rule written twice drifts**. Two things read it — the pane's
// grouping and the count the badge is folded from — and neither may be able to disagree with the
// other about what needs you. One function, tested in node.

// A pull request you opened. `reasons` is GitHub's answer to why this is in your queue at all
// (`prq::Reason`), and `author` is the membership rule that put it there; a team reason arrives as
// an object rather than a string, which is why this compares only strings.
export const authored = pr => (pr.reasons || []).some(r => r === "author");

// The review threads on a pull request, split into what is open and what could not be seen.
//
// `review_threads` is capped (`REVIEW_THREADS_FETCHED`), so `open` is a FLOOR and `unseen` says by
// how much. That asymmetry is deliberate everywhere it is read: a thread skein can see and that is
// unresolved is evidence, and a thread it never fetched is not — so `unseen` may make a roster say
// it is short, and may never promote a row on a guess.
export function threads(pr) {
  const list = pr.review_threads || [];
  const total = pr.review_threads_total;
  return {
    open: list.filter(t => !t.resolved),
    shown: list.length,
    unseen: total != null && total > list.length ? total - list.length : 0,
  };
}

// Does GitHub say this cannot merge as it stands?
//
// `mergeable` is three-valued on purpose (`Pr::mergeable`): `null` is "GitHub has not worked it out
// yet", which is not a conflict and must never be reported as one. It is its own function because
// two callers ask it for OPPOSITE purposes — the author's block below, where a conflict is your
// move, and `moveNote`, where the same conflict on somebody else's branch is only information — and
// a conflict test written twice is how those two come to disagree about what `DIRTY` means.
export const conflicted = pr =>
  pr.mergeable === false || (pr.merge_state || "").toUpperCase() === "DIRTY";

// What an authored pull request is waiting on YOU for — `null` when it is waiting on somebody else.
//
// **Checks are not here, and adding them is the change this comment exists to stop.** The owner,
// verbatim (SKEIN-303): "CI pass isn't your responsibility, that is of whoever merges — unless
// ci-queue tag is attached and it fails then. So that differentiation is to be made. But this
// ci-queue thing is very specific to this repo. So I don't want to include that in generic
// workflow." So the generic rule ignores `checks` and `failing_checks` entirely. If the ci-queue
// behaviour is ever wanted it arrives as repo CONFIGURATION handed to this function, never as a
// branch inside it — a red pull request that is nobody's move is the ordinary state of this fleet,
// and a rule that nags about it turns the whole list back into wallpaper.
//
// This does not contradict `Lane::NeedsYou`'s own note about failing checks (src/prq.rs). That one
// is about the REVIEWER's lane — red is the ordinary state of a pull request awaiting review here,
// because CI runs after approval — and it stays exactly as it is. Two different questions about
// one field.
//
// **A pull request you did NOT open never gets here, and a conflict is the case that proves why
// the guard on the first line is load-bearing rather than tidy** (SKEIN-354). The owner, shown
// somebody else's conflicted pull request that skein had put in front of him: "why is it my move at
// all, it is not PR I created, so if there are conflicts that's PR owner problem, not mine. So as
// far as I am concerned my work there is done. You can still say that conflicts or whatever as info
// but it is not mine to fix." Both halves of that are the design. Every reason below is an AUTHOR's
// reason — a conflict is yours to rebase only where the branch is yours — and the same `DIRTY` that
// blocks you here is worth SAYING on a row you are only reading, which is what `moveNote` is for.
// Saying it and claiming him are different pixels.
//
// Ordered most-actionable first, because the row shows one reason and it should be the one you
// would act on: somebody asking for changes outranks a thread, and a thread outranks a rebase.
export function authorBlock(pr) {
  if (!authored(pr)) return null;
  if ((pr.review_decision || "") === "CHANGES_REQUESTED") {
    return { kind: "changes", why: "changes requested" };
  }
  const open = threads(pr).open.length;
  if (open) return { kind: "threads", why: `${open} thread${open === 1 ? "" : "s"} unresolved` };
  if (conflicted(pr)) {
    return { kind: "conflict", why: `conflicts with ${pr.base_ref || "its base"}` };
  }
  if ((pr.merge_state || "").toUpperCase() === "BEHIND") {
    return { kind: "behind", why: `behind ${pr.base_ref || "its base"}` };
  }
  // Approved and mergeable is its own quiet state, not a nag: it belongs in the group you go
  // looking at, and `approvalsLine` is what it has to say there.
  return null;
}

// Does your verdict STAND — and has GitHub asked you again since you gave it?
//
// **This asks GitHub, where it used to work the answer out from the commits, and that swap is the
// whole of SKEIN-354.** The old rule also required `review_is_current`: skein compared the sha your
// review was left against with the head that is there now, so any push — a rebase, a typo fix, a
// commit in a file you never look at — took your approval off you and handed the row back. The
// owner, verbatim, on a pull request he had approved that was still claiming him: "approved should
// come only if my review status on the PR is approved rn, if I approved and then some file I own
// changed, so github asks me to review again then it should show that. Can refresh this once in a
// while in background or when I manually refresh."
//
// So there are exactly two questions, and GitHub answers both — skein infers neither:
//
//   * **is your review standing right now** — `my_review`, which `prq::my_review_state` reads from
//     GraphQL's `latestOpinionatedReviews`, so a note you left afterwards does not demote your own
//     approval and an approval GitHub has DISMISSED is gone rather than remembered;
//   * **has GitHub asked you again** — `my_review_requested`, which is you, by name, in
//     `reviewRequests`. That is where a CODEOWNERS re-request lands when a file you own changes,
//     and it is the only re-ask skein acts on. It is a floor and not a census: a request made of a
//     TEAM you are in arrives without your name (and without `read:org`, without the team's either
//     — the roster already reports that blind spot), so the error can only fall on the side of not
//     claiming you, which is the side he asked for.
//
// Why the commit comparison had to go rather than be tuned: measured on the owner's live queue on
// 2026-08-26 (`GET /api/review`, 26 rows), `review_is_current` was false on **all 26** — including
// the two he had approved himself. So this function was false everywhere, no verdict he gave ever
// cleared a row, and the 8 rows where he had said anything at all read "your review is out of
// date" — 7 of them about notes he had left, not verdicts he had given.
//
// A comment is still deliberately not a decision — leaving a note is not clearing the pull request
// — which is the same line `prq`'s lane rule and `my_review_state` both draw.
export const decided = pr =>
  (pr.my_review === "approved" || pr.my_review === "changes-requested") && !pr.my_review_requested;

// The pane's top-level bucket for one row: "yours", "theirs", "not-ready" or "archived".
//
// Archived is a HUMAN act and outranks everything — you set it aside, so it does not get to claim
// you back. Not-ready keeps the lane's meaning (a draft or a conflicted pull request somebody else
// is still going to change); an authored one never reaches it, because `prq`'s own lane rule puts
// authorship first. Somebody else's conflicted pull request is exactly what that bucket is for, and
// it is where `moveNote` says so — a row can be told about without being handed to you.
//
// `decided` is asked HERE rather than left to the next refresh because the pane marks a row done in
// place and does not reload (`revMarkDone`, SKEIN-162): between the verdict landing and the next
// queue coming back, `lane` still says `needs-you`, and a list that kept claiming you for a pull
// request you had just approved would be the surface disagreeing with the act you watched it take.
export function moveOf(pr) {
  if (pr.lane === "archived") return "archived";
  if (authorBlock(pr)) return "yours";
  if (answered(pr)) return "replied";
  if (pr.lane === "needs-you") return decided(pr) ? "theirs" : "yours";
  return pr.lane === "not-ready" ? "not-ready" : "theirs";
}

// Has somebody answered a finding you left, since you left it? — `docs/pr-review.md` §10's `reply`.
//
// `replied_to_me` is `Pr::replied_to`'s answer, computed server-side where the viewer's login is in
// scope, and it is three-valued on purpose: `true` a reply is there, `false` skein saw the whole
// thread list and there is none, `null`/absent it could not say. Only `true` moves a row, which is
// the same fail-closed rule the engine's facts use — a row must not be pulled back into your hands
// by a fact nobody looked up.
//
// **Above `decided`, and that IS the feature.** A pull request you have approved or asked changes on
// is `theirs` by definition, and it stays there however much the author says to you — which is what
// §10 calls the reply trigger and what the queue could not show. A reply is somebody waiting on you
// again, so it outranks the verdict that sent the row away.
//
// Below `authorBlock`, because a pull request YOU opened that is blocked is already yours and does
// not need a second reason; and below `archived`, because archiving is a human act and nothing
// automatic gets to undo it.
export const answered = pr => pr.replied_to_me === true;

// How many of these are your move — the badge's whole number.
//
// It is a function rather than the same filter written at each call site because there are two of
// them and they must not be able to disagree: `loadReview` folds the pane's own fetch into the
// badge the moment the pane loads, and `pollReviewCounts` folds the three-minute poll's rows in
// when nobody has opened it (SKEIN-323). Those two disagreeing IS the bug — the badge counted
// `Lane::NeedsYou` until the pane was opened and then jumped — so the count has one home, next to
// the rule it counts.
// **`replied` counts too**, and it has to. A lane that claims you and is missing from the badge is
// the exact failure the two call sites below were unified to prevent: the surface saying one number
// while the list shows another. A reply you have not answered is work you owe somebody.
export const YOUR_MOVE = ["yours", "replied"];
export const yourMoveCount = prs =>
  (prs || []).filter(pr => YOUR_MOVE.includes(moveOf(pr))).length;

// Why this row is in the your-move list, in words — "" for a row that is not in it.
//
// The list mixes both roles, which is what makes "review not given" worth saying: on a single-role
// lane it was true of every row and said nothing, and beside "2 threads unresolved" it is the
// difference between work you owe somebody and work somebody owes you. The AGE is deliberately not
// in this sentence — the rail's first cell is the sort key and already carries it (§4).
//
// **"Your review is out of date" is gone, and its absence is the point** (SKEIN-354). That sentence
// was skein's own inference — it fired whenever the head had moved past the sha you reviewed, which
// on the owner's live queue was every row — and it said it about 8 of his 26, seven of them for
// notes he had left rather than verdicts he had given. What replaces it is the only thing that puts
// a decided pull request back in your hands: GitHub asking you again. Reaching that line with a
// verdict on record can mean nothing else, because a standing verdict with no fresh request is
// exactly what `decided` sends to the other list.
export function moveWhy(pr) {
  const block = authorBlock(pr);
  if (block) return block.why;
  // A verdict you have given and nobody has asked you to revisit is not a reason to be in this
  // list, and the row must not carry a reason it is not there for: `moveOf` has already sent it to
  // "theirs", and between an approval landing and the next queue arriving `lane` still says
  // `needs-you` (SKEIN-162).
  // Said before the lane is consulted, because a replied row is routinely one `moveOf` has already
  // taken off `needs-you` — a verdict given and answered is the ordinary shape of this.
  if (answered(pr)) return "answered your review";
  if (pr.lane !== "needs-you" || decided(pr)) return "";
  return pr.my_review === "approved" || pr.my_review === "changes-requested"
    ? "asked to review again"
    : "review not given";
}

// What a row that is NOT your move still has to say for itself — "" when it has nothing to add.
//
// Two sentences live here, both of them the owner correcting the pane from live use (SKEIN-354),
// and what they have in common is that a row can carry a fact without the fact carrying a claim.
//
// **A conflict on a pull request somebody else opened**: "You can still say that conflicts or
// whatever as info but it is not mine to fix." So it is said here, on a row in the waiting list,
// and `authorBlock` — the only thing that can put a row in the your-move list for a merge state —
// never sees it, because it is not his branch to rebase.
//
// **An approval whose branch has moved since.** Asked to choose between the row coming back to him
// on the next push and the row staying theirs, he chose theirs until they ask again, in these
// words: WAITING ON OTHERS · you approved · moved since — they have not re-asked. The row is not
// claiming him, and is visibly not having forgotten what he did either — which is what makes the
// quiet answer trustworthy rather than merely quiet.
//
// `review_is_current` is read HERE and nowhere else in this module, and only to say "moved since".
// That is all it is still good for: it is evidence about the CODE — the sha you reviewed against
// the head that is there now — and no longer a verdict about you. Where GitHub named no commit for
// your review it reads the same way, which is the same reading the pane's own "new commits" chip
// has always made of the field.
export function moveNote(pr) {
  const move = moveOf(pr);
  if (move === "yours" || move === "archived") return "";
  if (decided(pr)) {
    const said = pr.my_review === "approved" ? "you approved" : "you asked for changes";
    return pr.review_is_current ? said : `${said} · moved since — they have not re-asked`;
  }
  return conflicted(pr) ? `conflicts with ${pr.base_ref || "its base"} — theirs to fix` : "";
}

// Who still owes an approval — "waiting on @dana and @sam", "waiting on the acme/core team".
//
// `review_decision` answers "does this still need somebody"; it cannot answer "who", and who is the
// question an author actually has (SKEIN-306). A team stays a team rather than being flattened to a
// login, because "waiting on @alice" and "waiting on acme/core" are not interchangeable sentences.
export function approvalsLine(pr) {
  const asked = (pr.review_requests || []).map(r => (r.team ? `the ${r.name} team` : `@${r.name}`));
  if (!asked.length) return "";
  return `waiting on ${andList(asked)}`;
}

// `a`, `a and b`, `a, b and c`. Its own function because it is read out loud in three places and a
// list joined with commas alone reads as a fragment.
export function andList(words) {
  if (words.length <= 1) return words[0] || "";
  return `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
}
