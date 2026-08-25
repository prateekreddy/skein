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
// `merge_state`, `review_decision` — is already on the payload, so nothing about it needs a
// request.
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
// Ordered most-actionable first, because the row shows one reason and it should be the one you
// would act on: somebody asking for changes outranks a thread, and a thread outranks a rebase.
export function authorBlock(pr) {
  if (!authored(pr)) return null;
  if ((pr.review_decision || "") === "CHANGES_REQUESTED") {
    return { kind: "changes", why: "changes requested" };
  }
  const open = threads(pr).open.length;
  if (open) return { kind: "threads", why: `${open} thread${open === 1 ? "" : "s"} unresolved` };
  const state = (pr.merge_state || "").toUpperCase();
  // `mergeable` is three-valued on purpose (`Pr::mergeable`): `null` is "GitHub has not worked it
  // out yet", which is not a conflict and must never be reported as one.
  if (pr.mergeable === false || state === "DIRTY") {
    return { kind: "conflict", why: `conflicts with ${pr.base_ref || "its base"}` };
  }
  if (state === "BEHIND") return { kind: "behind", why: `behind ${pr.base_ref || "its base"}` };
  // Approved and mergeable is its own quiet state, not a nag: it belongs in the group you go
  // looking at, and `approvalsLine` is what it has to say there.
  return null;
}

// Have YOU decided this one, against the head that is there now? A comment is deliberately not a
// decision — leaving a note is not clearing the pull request — which is the same line `prq`'s lane
// rule and `my_review_state` both draw.
export const decided = pr =>
  (pr.my_review === "approved" || pr.my_review === "changes-requested") && !!pr.review_is_current;

// The pane's top-level bucket for one row: "yours", "theirs", "not-ready" or "archived".
//
// Archived is a HUMAN act and outranks everything — you set it aside, so it does not get to claim
// you back. Not-ready keeps the lane's meaning (a draft or a conflicted pull request somebody else
// is still going to change); an authored one never reaches it, because `prq`'s own lane rule puts
// authorship first.
//
// `decided` is asked HERE rather than left to the next refresh because the pane marks a row done in
// place and does not reload (`revMarkDone`, SKEIN-162): between the verdict landing and the next
// queue coming back, `lane` still says `needs-you`, and a list that kept claiming you for a pull
// request you had just approved would be the surface disagreeing with the act you watched it take.
export function moveOf(pr) {
  if (pr.lane === "archived") return "archived";
  if (authorBlock(pr)) return "yours";
  if (pr.lane === "needs-you") return decided(pr) ? "theirs" : "yours";
  return pr.lane === "not-ready" ? "not-ready" : "theirs";
}

// Why this row is in the your-move list, in words — "" for a row that is not in it.
//
// The list mixes both roles, which is what makes "review not given" worth saying: on a single-role
// lane it was true of every row and said nothing, and beside "2 threads unresolved" it is the
// difference between work you owe somebody and work somebody owes you. The AGE is deliberately not
// in this sentence — the rail's first cell is the sort key and already carries it (§4).
export function moveWhy(pr) {
  const block = authorBlock(pr);
  if (block) return block.why;
  if (pr.lane !== "needs-you") return "";
  return pr.my_review && pr.my_review !== "none" && !pr.review_is_current
    ? "your review is out of date"
    : "review not given";
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
