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
// The third thing from that sitting — a redraft of #684 that came back with a verdict and zero
// comments, rendering as a bare "review ready" chip — is no longer a question this page can get
// wrong: skein keeps no drafted review for a row to summarise. The review is on GitHub.
//
//   node tests/ui/conversation.mjs
import { esc, grab, harness, link } from "./lift.mjs";

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
    let revPending = new Map();
    let revOpen = new Set();
    let revSums = new Map();
    let revInFlight = new Map();
    let revUpdated = new Set();
    const rk = pr => pr.repo_id + "#" + pr.number;
    const revAgo = iso => "17h";
    const revReceiptHtml = () => "";
    let revQueue = null;
    const revCommonChips = new Set();
    ${grab("convKey")}
    ${grab("firstLine")}
    ${grab("revConvToggle")}
    ${grab("revConversation")}
    return {
      convo: pr => revConversation(pr),
      toggle: (key, newest) => revConvToggle(key, newest),
      keyOf: (pr, c, i) => convKey(pr, c, i),
    };
  `;
  return new Function("esc", "link", "console", "renderReviewNow", body)(esc, link, console, () => {});
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

  // **And it says so.** The list has run this way since SKEIN-334 and said it nowhere: a reader who
  // assumes a conversation reads downwards gets the argument backwards, and nothing on screen would
  // tell them. The heading is where an order belongs, beside the count it is an order of.
  t.check("the heading says which way the list runs",
    html.includes("the conversation · 2 · newest first"), true);
}

// One comment has no order to be in, so the heading does not claim one. A label that announces an
// ordering of a single thing is a fact about nothing, and this pane spends its headings carefully.
{
  const w = world();
  t.check("a single comment is not announced as newest first",
    w.convo({ ...PR, comments_total: 1, comments: [PR.comments[1]] }).includes("newest first"), false);
  t.check("and still says what it is", 
    w.convo({ ...PR, comments_total: 1, comments: [PR.comments[1]] }).includes("the conversation · 1"), true);
}

// ── 1b. the truncation is marked where the truncation is ───────────────────────────────────────
//
// "The last 2 of 40 — the rest is on GitHub" used to sit under the heading, which is the place the
// list is most complete. It marks the point the list stops being everything, so it belongs at the
// end you fall off — and the link out with it.
{
  const w = world();
  const html = w.convo({ ...PR, comments_total: 40 });
  t.check("the pane says how much of the conversation it is showing",
    html.includes("The last 2 of 40"), true);
  // The assertion is a POSITION, not the presence of the sentence: it was already present, above,
  // which is exactly the bug. Anchored on the oldest comment rendered — the last row in a
  // newest-first list — so it cannot be satisfied by sitting between two comments either.
  t.check("and says it below the oldest comment it drew, not above the newest",
    html.indexOf("The last 2 of 40") > html.lastIndexOf("dev-rhea"), true);
  t.check("with the way to the rest at that same end",
    html.indexOf("the rest is on GitHub") > html.lastIndexOf("dev-rhea"), true);
  // A conversation that fits says nothing about truncation at all.
  t.check("a conversation with nothing hidden makes no such claim",
    /The last \d+ of/.test(w.convo(PR)), false);
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
  // The strip carries the verdicts a reader gives — those are the row's reason to exist — and a
  // way to the change itself. It does NOT carry a second door to a drafted review skein holds,
  // because skein no longer holds one: the review goes to GitHub, and GitHub is where it is read.
  t.check("the verdicts a reader gives are on the row",
    /revVerdictHtml\(pr\)/.test(row), true);
  t.check("and nothing on it opens a panel over a review skein kept back",
    /revCritique|revDraftSection/.test(row), false);
}

// ── 4. the conversation is the least trusted text on this page, and it is escaped ──────────────
//
// Everything `revConversation` draws comes from GitHub: the author handle, the comment body, and
// the comment's own URL. Anyone who can comment on a pull request skein reviews chooses all three,
// which makes this pane the shortest path from a stranger to the cockpit's DOM — and the cockpit
// runs beside a terminal that starts boxes.
//
// This suite ran with `esc` stubbed as `String` until SKEIN-531 (`new Function("esc", "link", …)(String)`),
// so every assertion above was written against unescaped output and an `esc` that had stopped
// escaping would have left all of them green. `esc` is the page's own now, lifted by `lift.mjs`.
//
// The payload puts all five characters `esc` encodes into the three positions this pane has: a text
// node (author, body, peek), an attribute value (the `href` out to GitHub), and a JavaScript string
// inside a handler attribute (`revConvToggle`'s key, which `convKey` builds out of `c.url`).
//
// Fails on: `esc` returning its argument, or dropping any one of `& < > " '` — each character below
// is asserted by a construct that only survives if that character got through.
{
  const w = world();
  const html = w.convo({
    ...PR,
    comments_total: 2,
    comments: [
      { author: "dev-rhea", created_at: "2026-08-24T16:05:20Z", body: ALSO_LONG,
        url: `https://github.com/acme/x/pull/625#issuecomment-1"onmouseover="alert(1)` },
      { author: `<img src=x onerror="alert(1)">`,
        created_at: "2026-08-25T17:11:48Z",
        body: `</div><script>alert(1)</script> quotes: " and ' and & and <b>bold</b>`,
        url: `https://github.com/acme/x/pull/625#issuecomment-2` },
    ],
  });

  // The three that are executable if they reach the browser as written. Each names the construct
  // that only exists when a character got through UNESCAPED — not the payload string, which is
  // present either way. The first drafts of these three asserted `/onerror\s*=/` and matched the
  // safely-escaped `&lt;img src=x onerror=&quot;…` as readily as the dangerous one: an absence
  // check is only worth what its pattern excludes.
  t.check("a comment body cannot open a tag", /<script/i.test(html), false);
  t.check("an author handle cannot open one either", /<img/i.test(html), false);
  // A raw `"` immediately before an attribute name is the breakout itself: escaped, the payload
  // reads `href="…&quot;onmouseover=&quot;alert(1)"` and there is no bare quote to close on.
  t.check("a comment URL cannot end the attribute it sits in and start another",
    html.includes(`"onmouseover`), false);

  // And the same text IS on screen, escaped — the check above is satisfied by dropping the comment
  // on the floor, and a pane that silently drops hostile comments is its own bug. Spelled out
  // literally rather than built by calling `esc`: an expectation computed with the function under
  // test moves with it and passes however broken it gets, which is the whole of SKEIN-531.
  t.check("the body is shown, with its markup as text",
    html.includes("&lt;/div&gt;&lt;script&gt;alert(1)&lt;/script&gt;"), true);
  t.check("its quotes are shown as text too — both kinds",
    html.includes("quotes: &quot; and &#39; and &amp; and &lt;b&gt;bold&lt;/b&gt;"), true);
  t.check("and the author reads as the characters they typed",
    html.includes("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;"), true);

  // The handler argument is the position the `'` case of `esc` exists for. `convKey` builds the key
  // from `c.url`, the page writes it as `esc(JSON.stringify(k))` (index.html), and the `"` that
  // `JSON.stringify` adds must arrive as an entity or it ends the `onclick` attribute early.
  t.check("the toggle's key is a JSON string with no bare quote left in it",
    html.includes(`revConvToggle(&quot;c:https://github.com/acme/x/pull/625#issuecomment-1\\&quot;onmouseover=\\&quot;alert(1)&quot;`), true);
}

t.done();
