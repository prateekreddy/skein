// The expanded row is readable: comments collapse, one read control, and a review that found
// nothing says so (SKEIN-334, SKEIN-335, SKEIN-336).
//
// All three came from the same sitting with the owner, and all three are about a row that is
// telling the truth and cannot be read:
//
//   * "comments have to be expandable instead of showing the list right now" — measured on their
//     own board, #625 carries a 1,600-character comment and #652 two of ~2,000 and ~2,400. In full,
//     they bury the review controls the row exists for.
//   * "reread the code and review the code are still 2 different buttons (they do the same thing,
//     why are they different?)" — they did not do the same thing, which is worse: one spent a model
//     call, the other read a draft off disk for free, and both were verbs.
//   * a redraft of #684 came back with a verdict and zero comments, and rendered as a bare "review
//     ready" chip — after a 35-second wait, indistinguishable from nothing having happened.
//
//   node tests/ui/conversation.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// Two comments at the sizes that caused the report — #625 and #652 on the owner's board carry
// bodies of 1,600 to 2,400 characters. A suite built on one-line comments would pass with the
// collapsing deleted.
// Each body carries a marker only reachable by rendering it WHOLE, past the first line. Without
// that, a "is the body shown" assertion is answered by the peek — the first line is in both — and
// the test passes with the collapsing deleted. The first drafts of this suite did exactly that.
const NEWEST_FIRST_LINE = "Approved per review direction, but this deployment merge needs work:";
const OLDER_FIRST_LINE = "Follow-up in 8976e0e40, closing the two things my previous reply left open.";
const LONG = `${NEWEST_FIRST_LINE}\n\nDEEP-IN-THE-NEWEST ${"detail ".repeat(280)}`;
const ALSO_LONG = `${OLDER_FIRST_LINE}\n\nDEEP-IN-THE-OLDER ${"detail ".repeat(280)}`;

function world() {
  const body = `
    let revConvOpen = new Map();
    let revCrits = new Map();
    let revPending = new Map();
    let revOpen = new Set();
    let revSums = new Map();
    let revInFlight = new Map();
    let revUpdated = new Set();
    const rk = pr => pr.repo_id + "#" + pr.number;
    const revAgo = iso => "17h";
    const threads = () => ({ shown: 0, unseen: 0, open: [] });
    const revThreadHtml = () => "";
    const revApproveWithReviewHtml = () => "";
    const revReceiptHtml = () => "";
    const revNoDraftWhy = () => null;
    const revDraftAtHead = () => true;
    const revCommonChips = new Set();
    ${grab("convKey")}
    ${grab("firstLine")}
    ${grab("revConvToggle")}
    ${grab("revConversation")}
    ${grab("revDraftSection")}
    ${grab("revReadyChip")}
    return {
      convo: pr => revConversation(pr),
      toggle: (key, newest) => revConvToggle(key, newest),
      draft: (pr, k) => { revSums.set(rk(pr), { drafted: { comments: (k.comments || []).length } });
                          return revDraftSection(pr); },
      chip: (pr, n) => { revSums.set(rk(pr), { drafted: { comments: n } }); return revReadyChip(pr); },
      keyOf: (pr, c, i) => convKey(pr, c, i),
    };
  `;
  // revDraftSection reads the row's drafted review through revDraftedReview; stubbed to hand back
  // whatever the case put on the pr, so this suite is about how a draft is DRAWN and not about the
  // head-matching rule (budget.mjs already holds that one).
  const revDraftedReview = pr => pr.draftedReview || null;
  return new Function(
    "esc", "console", "renderReviewNow", "revDraftedReview", body,
  )(String, console, () => {}, revDraftedReview);
}

const PR = {
  repo_id: "acme", number: 625, url: "https://github.com/acme/x/pull/625",
  comments_total: 2,
  comments: [
    { author: "dev-rhea", created_at: "2026-08-24T16:05:20Z", body: ALSO_LONG,
      url: "https://github.com/acme/x/pull/625#issuecomment-1" },
    { author: "dev-sixth", created_at: "2026-08-25T17:11:48Z", body: LONG,
      url: "https://github.com/acme/x/pull/625#issuecomment-2" },
  ],
};

// ── 1. the wall is gone ────────────────────────────────────────────────────────────────────────
{
  const w = world();
  const html = w.convo(PR);

  // The newest comment is open, so its body IS here — that is the point of auto-expanding it.
  t.check("the newest comment is open, and its body is on screen",
    html.includes("DEEP-IN-THE-NEWEST"), true);
  t.check("an older comment is not rendered whole",
    html.includes("DEEP-IN-THE-OLDER"), false);
  t.check("but it is still there, peeking at its first line",
    html.includes(OLDER_FIRST_LINE), true);
  t.check("every comment is its own row",
    (html.match(/class="revcomment(?: open)?"/g) || []).length, 2);
  t.check("and the collapsed one carries a caret to open it", html.includes("▸"), true);
  t.check("while the open one carries the other", html.includes("▾"), true);

  // Newest first: the last comment in the payload is the first in the markup.
  t.check("newest first — the latest turn is where a conversation is scanned from",
    html.indexOf("dev-sixth") < html.indexOf("dev-rhea"), true);
}

// A long first line is still ONE row. The measured case is a comment whose first paragraph runs to
// thousands of characters, so a peek that simply printed it would be the wall again with a caret.
{
  const w = world();
  t.check("a peek is one line, however long the paragraph behind it",
    w.convo({ ...PR, comments: [{ author: "a", created_at: "x", body: ALSO_LONG, url: "u1" },
                                { author: "b", created_at: "y", body: "short", url: "u2" }] })
      .includes("DEEP-IN-THE-OLDER"), false);
  t.check("and a comment that opens with blank lines still peeks at something",
    w.convo({ ...PR, comments: [{ author: "a", created_at: "x", body: "\\n\\n   \\nthe first real line", url: "u3" },
                                { author: "b", created_at: "y", body: "newest", url: "u4" }] })
      .includes("the first real line"), true);
}

// ── 2. each comment opens on its own ───────────────────────────────────────────────────────────
{
  const w = world();
  const older = w.keyOf(PR, PR.comments[0], 0);
  // Opening the older one must not close the newest: they are independent, which is the whole
  // reason the state is per comment rather than "which one is open".
  w.toggle(older, false);
  const html = w.convo(PR);
  t.check("opening an older comment shows it", html.includes("DEEP-IN-THE-OLDER"), true);
  t.check("and does not collapse the newest", html.includes("DEEP-IN-THE-NEWEST"), true);
  t.check("so both are open at once", html.includes("DEEP-IN-THE-OLDER") && html.includes("DEEP-IN-THE-NEWEST"), true);

  // And the newest can be closed, which is what "absent means the default" has to survive.
  w.toggle(w.keyOf(PR, PR.comments[1], 1), true);
  t.check("the newest can be closed like any other",
    w.convo(PR).includes("DEEP-IN-THE-NEWEST"), false);
}

// A comment is identified by its GitHub URL, not its position: the queue re-fetches, and a list
// that gained an older comment would otherwise shift every index and open the wrong row.
{
  const w = world();
  const key = w.keyOf(PR, PR.comments[1], 1);
  t.check("a comment is keyed by its URL where it has one",
    key, "c:https://github.com/acme/x/pull/625#issuecomment-2");
  t.check("and by its row and position only where it has none",
    w.keyOf(PR, { author: "a" }, 3), "acme#625:3");
}

// ── 3. one read control, and no second door to the draft (SKEIN-335) ───────────────────────────
//
// Read from the page's source rather than rendered, because what is being asserted is an ABSENCE
// in one strip, and a render would prove only that this fixture did not happen to draw it.
{
  const row = grab("revBody");
  t.check("the row's own control strip offers one read",
    (row.match(/revReadAgainPress/g) || []).length, 1);
  // `onclick="` and not the bare name: this function's own comment explains why the button went,
  // and a test that searched for the identifier would be answered by the explanation.
  t.check("and no longer offers a second button that only reveals the draft",
    /onclick="revCritiqueOpen/.test(row), false);

  // The counter-case: the panel is still reachable, from the draft itself where a reader is
  // already looking at what they would post. Deleting the button must not have deleted the door.
  const draft = grab("revDraftSection");
  t.check("the drafted review still opens its own panel, in context",
    /onclick="revCritiqueOpen/.test(draft), true);
}

// ── 4. a review that found nothing is a result (SKEIN-336) ─────────────────────────────────────
{
  const w = world();
  const pr = { repo_id: "acme", number: 684, head_sha: "12d1512d" };
  const empty = { head_sha: "12d1512d", comments: [],
                  overall: "Mechanical composition wiring with clear rationale." };

  const html = w.draft({ ...pr, draftedReview: empty }, empty);
  t.check("the heading says what the review found, rather than leaving it to a tooltip",
    /nothing to flag/.test(html), true);
  t.check("and the verdict it did reach is shown",
    html.includes("Mechanical composition wiring"), true);

  t.check("the chip says it too, where the queue is scanned",
    /nothing to flag/.test(w.chip(pr, 0)), true);
  // And a review WITH comments still says how many — the count is the useful thing there, and a
  // fix that made every chip read the same would have lost it.
  t.check("a review with comments still carries its count", /· 3/.test(w.chip(pr, 3)), true);
  t.check("and does not claim it found nothing", /nothing to flag/.test(w.chip(pr, 3)), false);
}

t.done();
