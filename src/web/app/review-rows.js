// The name of the act, as the control that sends it is labelled. NOT `revReceiptHtml`'s map, which
// is the past tense of an act that HAPPENED ("merged", "changes requested") and reads as a lie in
// front of a refusal — "merged refused" says the opposite of what took place. Anything missing
// falls through to the kind itself, which is already the word the wire uses.
const REV_ACT_NAME = { approve: "approve", "request-changes": "request changes", comment: "comment",
                       archive: "set aside", merge: "merge" };

// **An act on this row was refused, and the row says so** (SKEIN-385). The receipt carrying the
// reason, `try again` and the link to GitHub lives in the row's own strip — which is inside the
// BODY, drawn only while the row is open. So a merge GitHub had refused left a collapsed row
// looking exactly like a row nothing had been asked of, and folding the row was enough to take the
// only answer on screen away with it. Reported as "no toast, no receipt, no mark on the row".
//
// On the LINE, because that is what a reader scanning the queue sees, and it earns its place under
// the minority rule above: an act that failed is true of almost no rows and changes what you do
// next. It states the refusal rather than the reason — a queue line has no room for GitHub's
// sentence — and opening the row, which is what clicking the line does, is where the sentence is.
// It does NOT clear when the row is opened, the way `● updated` does: that mark is "something
// happened while you were away" and is answered by looking, where this one is a thing still owed
// an answer. The receipt inside the row and this are one fact at two zoom levels, and both last
// until the act is retried or taken back.
function revRefusedChip(pr) {
  const p = revPending.get(rk(pr));
  if (!p || p.state !== "failed") return "";
  const did = REV_ACT_NAME[p.kind] || p.kind;
  return `<span class="revtag refused" title="${esc(p.error || "")}

open the row for the whole answer, and for try again">✗ ${esc(did)} refused</span>`;
}

function revRow(pr) {
  const open = revOpen.has(rk(pr));
  // WHY this row needs you, in words, on the collapsed line (SKEIN-300/302). It is exempt from the
  // minority rule the other chips obey, and deliberately: that rule is about marks which compete
  // with the title for attention, and this is the row's reason for being in the list at all. It is
  // what makes one mixed list readable — "2 threads unresolved" and "review not given" beside each
  // other say which of your two roles is being asked for, which the lane split used to say by
  // living in different sections and cannot say once they share one. Absent everywhere else, so it
  // never appears on a row that is not yours to act on.
  const why = moveWhy(pr);
  const whyTag = why ? `<span class="revwhy">${esc(why)}</span>` : "";
  const chips = `${revRefusedChip(pr)}${whyTag}${pr.draft ? `<span class="revtag draft">draft</span>` : ""}${
    revMoved(pr) && !revCommonChips.has("moved") ? `<span class="revtag moved" title="you decided on an earlier commit — this is back with you because the branch moved">new commits</span>` : ""
  }`;
  const line = `<div class="revline" onclick="toggleRevRow(${esc(JSON.stringify(rk(pr)))})">
      <span class="mv ${revMove(pr)}" title="${esc(REV_MOVE_WORDS[revMove(pr)] || "")}"></span>
      <span class="revnum" title="${esc(pr.repo_id || "")}">#${pr.number}</span>
      <span class="tcell"><span class="revtitle">${esc(pr.title || "")}</span>${chips}</span>
      ${revRail(pr)}
    </div>`;
  // data-rk is the repaint hook: SKEIN-162 marks a row done in place, and a row you cannot find
  // again is a row you can only repaint by rebuilding the pane around it. `sel` is the keyboard's
  // selection (an rk, so a re-render finding it again IS the survival §6 rule 1 asks for), `flash`
  // the one-render hand-off mark, and `held` an act still inside its undo window — the "greys in
  // place" of §6's `e`, painted by the same one-row repaint the receipt rides.
  return `<div class="revrow${open ? " open" : ""}${revDecided.has(rk(pr)) ? " done" : ""}${
    revSel === rk(pr) ? " sel" : ""}${revFlash === rk(pr) ? " flash" : ""}${
    (revPending.get(rk(pr)) || {}).state === "waiting" ? " held" : ""}" data-rk="${esc(rk(pr))}">${line}${open ? revBody(pr) : ""}</div>`;
}

// When this pull request started waiting on YOU — the sort key of the your-move lane.
//
// Not createdAt, and not updatedAt-when-avoidable: waiting starts when the head moved after you
// had reviewed (the commit that invalidated your approval is the moment it came back), and
// updatedAt is the honest fallback where the request time is not knowable from the search payload.
// updatedAt DESC was the old order, and it was upside down for a review queue: the PR waiting
// longest sank to the bottom, and any push — a bot's included — lifted a row to the top.
function revWaitedSince(p) {
  const moved = p.my_review && p.my_review !== "none" && !p.review_is_current;
  return (moved && p.committed_at) || p.updated_at || "";
}

// Does this row answer the search? Substring, case-insensitive, over everything a person remembers
// about a pull request: its number, its words, whose it is, and which branches it spans.
function revMatchesSearch(p) {
  const q = revSearch.trim().toLowerCase();
  if (!q) return true;
  return [String(p.number), p.title, p.author, p.head_ref, p.base_ref, p.repo_id]
    .some(v => (v || "").toLowerCase().includes(q));
}

function revSearchSet(v) {
  revSearch = v;
  // Forced past the focus deferral: this is the ONE caller typing into the pane whose render is
  // the point — the queue filtering under the keystroke — and it restores the caret itself below.
  renderReview(true);
  // innerHTML replaced the input mid-keystroke: put the caret back where the typist left it.
  const box = revpane && revpane.querySelector && revpane.querySelector(".revsearch");
  if (box) { box.focus(); box.setSelectionRange(box.value.length, box.value.length); }
}

// Whose move — the one field that changes what you do next, in the row's highest-value position.
// Exactly one state is lit, and it is the one that is yours; "done" is a decision that still
// holds; "blocked" is drawn by the stack for a step whose base is unreviewed.
const REV_MOVE_WORDS = {
  yours: "your move",
  theirs: "waiting on someone else",
  done: "you decided, and it holds",
  blocked: "waits on an unreviewed base",
};
//
// It reads `moveOf` rather than the lane, so the lit mark and the list a row is IN can never
// disagree — a row sitting under "your move" with a hollow ring is the pane contradicting itself
// on the one field the design gave the row's highest-value pixel to.
function revMove(pr) {
  const m = moveOf(pr);
  if (m === "archived") return "done";
  // `replied` reads as yours, and it has to: the ring and the list a row sits in must not disagree,
  // and a row under "replied to you" showing the solid "done" mark — which is where a decided pull
  // request would otherwise fall two lines below — would be the pane saying the verdict still
  // stands about the one row where somebody has answered it.
  if (m === "yours" || m === "replied") return "yours";
  // `decided`, not a second reading of `review_is_current` (SKEIN-354). A verdict stands until
  // GitHub asks you again, so a push no longer demotes "you decided, and it holds" to the hollow
  // ring — and asking the same function `moveOf` asked is what stops the lit mark and the list a
  // row is in from ever disagreeing.
  if (decided(pr)) return "done";
  return "theirs";
}

// Age, then size, then who. Age is first because it is the sort key, and a sort key you cannot see
// is a sort you cannot audit; amber past three days. Size renders only once the queue carries the
// fields — the empty cell still occupies its column, so nothing shifts when the numbers arrive.
//
// The age cell is `revage`, never the fleet's `age`: `tickAges` rewrites EVERY `.age` in the
// document from its `data-secs` once a second, and a review row carries none — so the true age
// painted here was stomped to ageNow's "?" one tick after render, for the life of the page.
//
// It renders `revSortAt`, which IS what the lane sorted on (SKEIN-251). It used to render
// `pr.updated_at` while the your-move lane sorted on `revWaitedSince`, so a pull request you
// approved, that was force-pushed eight days ago and commented on thirty seconds ago, sat near the
// top of oldest-first reading `1m` above rows reading `4d`. The one column this design put on the
// row so the order could be AUDITED was the one that made it unauditable, and it took the amber
// three-day mark with it.
function revRail(pr) {
  const a = revAge(revSortAt(pr));
  const sz = revSize(pr);
  return `<span class="rail"><span class="revage${a.old ? " old" : ""}" title="${
    esc(revSortWord(pr))}">${esc(a.label)}</span><span class="size"${sz ? ` title="${esc(sz.title)}"` : ""}>${sz ? esc(sz.label) : ""}</span><span class="who">${esc(pr.author || "")}</span></span>`;
}

// **The one order, asked once.** The lane decides which clock a row is on, and both the sort and
// the column it is audited by read it from here — because "how long has this waited" computed twice
// is how the two came apart in the first place (`review::waited_since`, whose doc already says
// "Do not invent a second order here", mirrors `revWaitedSince` field for field; the column was the
// third implementation and the only one that disagreed).
//
// Your move is how long it has been waiting on YOU: a head that landed after your approval is the
// moment it came back, and a comment since then does not restart it. Every other lane is recency —
// for your own pull requests, "what moved most recently" is the right question, and nothing there
// is waiting on you at all.
//
// Asked of `moveOf` rather than of the lane (SKEIN-302), because the list is what the column
// audits and the list mixes both roles now. On a pull request you opened, `revWaitedSince` falls
// through to `updated_at` — your own PR has no request time, and the last touch is the comment or
// the review that put it back in your hands. That fallthrough is why this stays one function:
// `review::waited_since` mirrors it field for field to decide what skein reads ahead, and a second
// clock invented here would be the third implementation of an order that already came apart once.
function revSortAt(p) {
  return moveOf(p) === "yours" ? revWaitedSince(p) : (p.updated_at || "");
}
// What the number in that cell MEANS, on the cell — the groups measure different things and the
// column cannot say which in 34 px.
function revSortWord(p) {
  if (moveOf(p) !== "yours") return "last moved this long ago";
  if (authored(p)) return "waiting on you since it was last touched";
  return revMoved(p)
    ? "waiting on you since the commit that came after your review"
    : "waiting on you this long";
}
// `7 files ±782`, with the whole story on hover — the old `7f 782±` read as a cipher to the person
// the column exists for. Null while the queue does not carry the numbers; the caller renders an
// empty cell that still holds its column.
function revSize(pr) {
  if (pr.changed_files == null) return null;
  const files = `${pr.changed_files} file${pr.changed_files === 1 ? "" : "s"}`;
  return { label: `${files} ±${(pr.additions || 0) + (pr.deletions || 0)}`,
           title: `+${pr.additions || 0} −${pr.deletions || 0} across ${files}` };
}

// `4.2d`, `20h`, `12m` — compact because the cell is 34px, tabular because it is a column.
function revAge(iso) {
  const at = Date.parse(iso || "");
  if (!at) return { label: "", old: false };
  const d = Math.max(0, Date.now() - at) / 86400000;
  const label = d >= 10 ? `${Math.round(d)}d`
    : d >= 1 ? `${Math.round(d * 10) / 10}d`
    : d * 24 >= 1 ? `${Math.round(d * 24)}h`
    : `${Math.max(1, Math.round(d * 1440))}m`;
  return { label, old: d > 3 };
}

// `revrowacts` on the row's own control strip: an expanded row grew sections with `.revacts` of
// their own — the drafted review's, the budget refusal's — and a bare `.revacts` selector reads
// whichever one the document reaches first. The class is what lets a reader (and a test) name the
// row's own controls. Kept as a JS comment: an HTML comment inside this inline script would put the
// parser into script-data-escaped state and take the rest of the page with it.
// **The verdict controls, on the ROW** (SKEIN-449).
//
// They used to live in a reading view of skein's own, gated on the diff being on screen —
// `docs/parity.md` §3, "no verdict from a surface that is not showing you the change". The
// change is read on GitHub now and a session does the reviewing, so the gate went with the surface
// that justified it, and the row is where you say what you think. It is still a surface that has to
// be OPENED: these are drawn in the row's body, and `a` on a collapsed row refuses out loud.
//
// The four are unchanged from that bar, deliberately — this was a move, not a redesign: approve,
// post comments of your own, request changes, or just comment. `merge` stays separate from
// approving for the reason its own title says.
function revVerdictHtml(pr) {
  const repo = pr.repo_id, number = pr.number;
  // **A merge GitHub has already said no to** (SKEIN-415). `=== false` and the identity matters:
  // `mergeable` is `Option<bool>` all the way from `prq::Pr`, where unknown is not "no" — GitHub
  // reports UNKNOWN for a while after every push, and reading that as a conflict would take merge
  // away from a pull request that merges perfectly well.
  const dead = pr.mergeable === false
    ? `conflicts with ${pr.base_ref || "its base"} — GitHub will not merge it until they are resolved`
    : "";
  return `<button type="button" class="revchip go" onclick="revAct(${esc(JSON.stringify(repo))}, ${number}, 'approve')">approve</button>
      <button type="button" class="revchip" onclick="revCompose(${esc(JSON.stringify(repo))}, ${number}, 'request-changes')">request changes…</button>
      <button type="button" class="revchip" onclick="revCompose(${esc(JSON.stringify(repo))}, ${number}, 'comment')">comment…</button>
      ${dead
        ? `<button type="button" class="revchip" disabled title="${esc(dead)}">merge</button>
      <span class="dim revcannot">${esc(dead)}</span>`
        : `<button type="button" class="revchip" onclick="revAct(${esc(JSON.stringify(repo))}, ${number}, 'merge')" title="separate from approving: with a protected base your approval is one of several">merge</button>`}`;
}

function revBody(pr) {
  const archived = pr.lane === "archived";
  // Your review state is worth spelling out rather than badging: "approved, but they have not
  // re-asked" is the single most useful sentence on a PR you already looked at.
  //
  // It is `moveNote`'s sentence rather than a second copy of it (SKEIN-354). This line used to
  // hand-roll "you approved — and new commits have landed since", which is precisely the claim the
  // rule stopped making: a push no longer takes your approval off you, a re-request does. Two
  // places saying that in their own words is how they come to disagree, which is the reason the
  // whose-move rule lives in one module at all.
  //
  // `moveNote` also carries the half the owner asked for about somebody else's problem — "you can
  // still say that conflicts or whatever as info but it is not mine to fix" — so a conflicted pull
  // request that is not yours says so here without being handed to you. It returns "" for a row
  // that IS your move, where the row's own `revwhy` has already said why.
  const note = moveNote(pr);
  const mine = note ? esc(note)
    : pr.my_review && pr.my_review !== "none" ? `you ${esc(pr.my_review.replace("-", " "))}`
    : "you have not reviewed this";
  return `<div class="revbody">
    <div class="revmeta">
      <span>${esc(pr.head_ref || "")} → ${esc(pr.base_ref || "")}</span>
      <span>checks ${esc(pr.checks || "none")}${(pr.failing_checks || []).length
        ? ` — ${(pr.failing_checks).map(c => c.url
            ? link(c.url, esc(c.name))
            : esc(c.name)).join(", ")}`
        : ""}</span>
      <span>${mine}</span>
      ${link(pr.url, "open on GitHub ↗")}
    </div>
    ${revApprovals(pr)}
    ${revConversation(pr)}
    <div class="revacts revrowacts">${revPending.has(rk(pr))
      // SKEIN-162: an act on this row is held, in flight, landed or refused — the strip you
      // pressed IS where that story shows, not a toast in the opposite corner.
      ? revReceiptHtml(rk(pr), revPending.get(rk(pr)))
      : `${revVerdictHtml(pr)}
      ${link(pr.url, "read on GitHub", 'class="revchip" target="_blank" rel="noopener"')}
      <span class="revspacer"></span>
      <button type="button" class="revchip" onclick="openNewBoxFor(${esc(JSON.stringify(pr.repo_id))}, ${esc(JSON.stringify(pr.head_ref || ""))})">box on this branch</button>
      ${pr.snoozed
        ? `<button type="button" class="revchip" onclick="revSnooze(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, '')" title="it would come back on its own at the author's next push">bring back now</button>`
        : `<button type="button" class="revchip" onclick="revSnooze(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, ${esc(JSON.stringify(pr.head_sha || ""))})" title="out of the queue until the author pushes — the push is what brings it back">until it moves</button>
      <button type="button" class="revchip" onclick="archivePr(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, ${archived ? "false" : "true"})">${archived ? "bring back" : "set aside"}</button>`}`}
    </div>
    ${revComposeHtml(pr)}
  </div>`;
}

// Could this repo's queue see your TEAMS? `gh` without `read:org` cannot list them, and a team
// asked to review then arrives from GitHub without a slug and is dropped on the floor
// (`prq::normalise`) — so the roster below can be short without anything having failed. The queue
// already reports the gap as a standing blind spot; this is the one question that has to ask it
// per row, because a list of names is exactly where a silent omission does harm (SKEIN-262/306).
function revTeamsBlind(repoId) {
  return ((revQueue && revQueue.blind_spots) || [])
    .some(b => (!repoId || String(b).startsWith(repoId + ":")) && /read:org/.test(b));
}

// **Which approvals are still outstanding, and from whom** (SKEIN-306). The owner: "May be I am the
// author and I want to ensure that all approvals are done on it."
//
// `review_decision` answers whether the repository is satisfied; it cannot answer WHO, and who is
// the question somebody chasing an approval actually has. Drawn only on a pull request you opened,
// and only in the expanded row: this is information you go looking for, not something that should
// nag — an authored PR whose approvals are outstanding is not, by itself, your move (SKEIN-303).
function revApprovals(pr) {
  if (!authored(pr)) return "";
  const line = approvalsLine(pr);
  const decision = pr.review_decision || "";
  const said = line
    ? line
    : decision === "APPROVED" ? "every approval this repository asks for is in"
    : decision === "CHANGES_REQUESTED" ? "changes were requested, and nobody is queued to look again"
    : decision === "REVIEW_REQUIRED" ? "this repository requires a review, and nobody is queued"
    : "GitHub is not waiting on anybody";
  // The roster's own honesty, and it is not decoration: a short list read as a whole one is how
  // somebody concludes an approval has landed that never will.
  const short = revTeamsBlind(pr.repo_id)
    ? `<span class="revblindwhat">incomplete</span> — a team asked to review would not be listed:
       <code>gh</code> cannot see your teams. Fix: <code>gh auth refresh -s read:org</code>`
    : "";
  return `<div class="revapprovals"><b>approvals</b> ${esc(said)}${short ? `<div>${short}</div>` : ""}</div>`;
}

// **The conversation ABOUT the pull request** (SKEIN-300, SKEIN-304). The owner's own words: "if
// they are normal comments then just show it here and also link out."
//
// Only the PR-level ones. An inline review thread is a conversation about a line of code and is
// worth nothing away from the line — drawn here it was a stack of decontextualised fragments — so
// the panel that drew them is gone, and a thread is answered where it lives. What is left of them
// on this page is `moveNote`'s "N threads unresolved", which is a fact about WHOSE MOVE it is and
// not an attempt to hold the conversation here.
//
// Bodies go through `esc` and are laid out with `white-space: pre-wrap`, never `marked.parse`: the
// brief is skein's own prose and a comment is somebody else's text off the internet, and the one
// place on this page that renders markdown must not be pointed at it.
// Which comments are open, keyed per comment rather than per row (SKEIN-334): opening one must not
// collapse another, and the state has to survive the re-render that drawing it causes.
//
// A Map of key -> bool rather than a Set of open keys, because the DEFAULT is not the same for
// every comment — the newest starts open — so "absent" has to mean "whatever the default is here"
// and not "closed".
let revConvOpen = new Map();

// A comment's identity. Its GitHub URL when it has one, which is stable across re-fetches and
// across a reordering; the index only where there is none, which is a fixture and never the live
// queue. Keyed by PR too, so two pull requests cannot share an entry.
function convKey(pr, c, i) {
  return c.url ? `c:${c.url}` : `${pr.repo_id}#${pr.number}:${i}`;
}

// Enough of a comment to decide whether to open it: its first line with anything in it. Blank
// leading lines are common — a comment that opens with a heading's blank line would otherwise peek
// as nothing at all, which is the one thing a collapsed row must never look like.
function firstLine(body) {
  const line = String(body || "").split("\n").map(l => l.trim()).find(Boolean) || "";
  return line.length > 120 ? line.slice(0, 119) + "…" : line;
}

function revConvToggle(key, isNewest) {
  const open = revConvOpen.has(key) ? revConvOpen.get(key) : isNewest;
  revConvOpen.set(key, !open);
  renderReviewNow();
}

function revConversation(pr) {
  const seen = pr.comments || [];
  const total = pr.comments_total;
  const older = total != null && total > seen.length ? total - seen.length : 0;
  if (!seen.length && !older) return "";
  const when = iso => (iso ? revAgo(iso) : "");
  // **One row per comment, and the wall is gone** (SKEIN-334). The owner: "comments have to be
  // expandable instead of showing the list right now."
  //
  // Measured on their own board, which is why this is not a tidiness question: #625 carries a
  // 1,600-character comment, #652 two of ~2,000 and ~2,400. Rendered in full, three of those bury
  // the row's review controls under several screens of text — and the controls are what the row is
  // FOR. What a reader needs at a glance is who spoke, when, and enough of the first line to know
  // whether to open it.
  //
  // Newest first, and the newest open: a conversation is scanned from its latest turn, and the
  // comment that changes what you do is almost always the last one. The rest are one click each,
  // and each opens on its own — `revConvOpen` remembers per comment, so opening one does not
  // collapse another.
  //
  // PR-LEVEL comments only, unchanged and deliberate (SKEIN-300): review threads on lines are
  // never rendered here and link out instead.
  const newest = seen.length - 1;
  const commentRows = seen.map((c, i) => {
    const k = convKey(pr, c, i);
    const open = revConvOpen.has(k) ? revConvOpen.get(k) : i === newest;
    const head = `<div class="revcomment-head" onclick="revConvToggle(${esc(JSON.stringify(k))}, ${i === newest})">
        <span class="revconv-caret">${open ? "▾" : "▸"}</span>
        <span class="who">${esc(c.author || "somebody")}</span>
        <span class="dim">${esc(when(c.created_at))}</span>
        ${c.url ? link(c.url, "on GitHub ↗", 'target="_blank" rel="noopener" onclick="event.stopPropagation()"') : ""}</div>`;
    return `<div class="revcomment${open ? " open" : ""}">
      ${head}
      ${open
        ? `<div class="revcomment-body">${esc(c.body || "")}</div>`
        : `<div class="revcomment-peek">${esc(firstLine(c.body))}</div>`}
    </div>`;
  }).reverse().join("");
  // **The order is in the heading, and the truncation is where the truncation is.** This list has
  // run newest-first since SKEIN-334 and said so nowhere — a reader who assumes a conversation
  // reads downwards gets the argument backwards and has no way to find out. It is claimed only with
  // two or more comments on screen, because one comment has no order to be in and a heading that
  // announces one would be a fact about nothing.
  //
  // "The last N of M" moved to the BOTTOM for the same reason it is worth saying at all: it marks
  // where the list stops being everything, and at the top it marked the place the list is most
  // complete. The rest is on GitHub, and the link belongs at the end you fell off.
  const commentsBlock = (seen.length || older)
    ? `<div class="revconv-part"><b>the conversation${total != null ? ` · ${total}` : ""}${
        seen.length > 1 ? " · newest first" : ""}</b>
      ${commentRows}
      ${older ? `<div class="dim">The last ${seen.length} of ${total}${
        pr.url ? ` — ${link(pr.url, "the rest is on GitHub ↗")}` : ""}.</div>` : ""}
    </div>`
    : "";
  return `<div class="revconv">${commentsBlock}</div>`;
}

// The composer: one box for the three things that need words. `ask` stays private; the other two
// post under your name, which is why the button that sends them says so.
let revComposing = null;   // { repo, number, kind, text, answer, busy }

let revSearch = "";            // the queue's search text — matches number, title, author, branch, repo
let revCommonChips = new Set(); // chip kinds worn by most of the queue this render — wallpaper, not signal

