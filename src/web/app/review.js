// ---------- review: a repo's PR queue ----------
//
// The inversion that makes this a review surface rather than a shipping one: the repo owns the
// list, a PR is the object, and a box is something you summon onto a branch when one turns out to
// need hands. Everything else in the dock is keyed by box; this is keyed by repo, which is why it
// carries `view.repo` and why `applyView` docks on it.
//
// Nothing here polls. The queue costs three `gh` round trips, so it loads when you open it and when
// you ask — never on the fleet's 2s tick.
let revQueue = null;          // last fetched merged queue (every repo, rows tagged repo_id)
let revRepoFilter = "";       // "" = all repos; a repo id narrows the view, it never refetches
// One key for every per-PR map. PR numbers collide across repos, and the merged queue holds all of
// them at once — keying by number alone would open #12 of one repo when #12 of another was clicked.
const rk = pr => (pr.repo_id || "") + "#" + pr.number;
let revFilter = "all";        // all | author | reviewer | mentioned
let revOpen = new Set();      // PR numbers expanded in place
let revLoading = false;
// SKEIN-162: a pressed verdict is held HERE, client-side, for eight seconds before anything
// reaches GitHub. The control you pressed collapses to a receipt with a live countdown; `undo`
// inside the window cancels a request that was never made. Keyed by rk; each entry carries the
// payload exactly as assembled at press time, and walks waiting → posting → posted | failed.
const REV_UNDO_MS = 8000;
const revPending = new Map();
// Rows acted on THIS session, marked done in place (green dot, struck title) without the full
// loadReview(true) rebuild that used to reflow the pane under the reader. Cleared when the next
// real queue arrives — by then the server's own fields carry the truth and the row leaves or
// re-lanes on its own.
const revDecided = new Set();
// The last queue seen for each repo, so switching to one you have already looked at paints now
// rather than after three GraphQL searches. The server keeps its own copy on disk for a cold start;
// this is the same idea one layer up, for the switch that happens twenty times an hour.
const revSeen = new Map();
// The repo the in-memory state below belongs to — the queue on screen, which rows are expanded, and
// every summary read for it. NOT the same question as "what does the view name": leaving review for
// a box names no repo at all, and that is not a repo switch.
let revHeld = null;
let revStaleTimer = null;
// How many times the pane has asked again for a queue that came back remembered rather than fresh.
//
// It used to ask ONCE, four seconds later, on the reasoning that "hammering it would not make it
// faster". True, and it left the pane on the older copy whenever the real fetch took longer than
// four seconds — with the badge, which polls separately, already showing the newer number. Reported
// as "the list shows 34 while the PR button shows 44 and it suddenly shows up later": the "later"
// being whenever something else happened to reload it.
//
// So it keeps asking, with growing gaps, and gives up out loud rather than silently.
let revStaleTries = 0;
const REV_STALE_TRIES = 5;

// The day's reading budget lives on the SERVER, and only there (`review::over_budget`). The page
// used to keep its own — six unasked readings per repo — and it was wrong twice over: it counted
// requests rather than model calls, so a reload spent the whole allowance on free cache hits and
// every row past the sixth went unread for ever; and every approve reloaded the pane, which handed
// out a fresh six. A budget the client holds is a budget any client action can refill. What is left
// here is REV_SUM_PARALLEL, which is a throttle on the burst and not a limit on the total.
//
// The one-hour settle gate is gone with it: the server stopped applying it (`prq::settled` is no
// longer consulted by `review::worth_reading`) when re-anchoring by line text made a reading of a
// moving head worth having, so a copy of the rule here would only refuse rows skein itself reads.

// The badge on the review button. Polled slowly and never as part of the 2s fleet tick: each count
// is three `gh` round trips per repo, and this is a number you act on within minutes, not seconds.
//
// Only repos with the queue switched on are asked, and the server skips any with no GitHub remote —
// so a fleet of scratch clones costs nothing at all.
let revCounts = [];
const REV_POLL_MS = 3 * 60 * 1000;
function pollReviewCounts() {
  fetch("/api/review/counts").then(r => r.json()).then(cs => {
    revCounts = cs || [];
    // **The badge counts the your-move list and nothing else** (SKEIN-300), and this is where it
    // starts doing so before anybody opens the pane (SKEIN-323). `needs_you` off the wire is
    // `Lane::NeedsYou` — the reviewer's lane — and a pull request YOU opened with changes requested
    // or a thread open on it is not in it however stuck it is. The rows the count was taken over
    // ride along for exactly this, and `yourMoveCount` is the one rule both this and `loadReview`
    // fold in, so the number cannot change meaning when the pane happens to be open.
    //
    // An entry with no `prs` at all is an older server answering a tab that outlived it — the same
    // reading `stopped`'s absence gets below. Its lane count is the best it can say, and a badge
    // that is sometimes short is worth more than one that suddenly reads zero.
    for (const c of revCounts) if (c.prs) c.needs_you = yourMoveCount(c.prs);
    renderRevBadge();
  }).catch(() => {});
}
function renderRevBadge() {
  const btn = document.getElementById("revbtn");
  if (!btn) return;
  const total = revCounts.reduce((n, c) => n + (c.needs_you || 0), 0);
  const broken = revCounts.filter(c => c.error);
  // Repos skein did not ask about. Named in the tooltip even though they are not a fault: "no PRs
  // need you" and "your only repo was never looked at" were the same empty badge, and the second one
  // is the state someone stares at while their queue fills up on GitHub.
  const unasked = revCounts.filter(c => c.skipped);
  let badge = btn.querySelector(".revbadge");
  if (!total && !broken.length) { if (badge) badge.remove(); btn.removeAttribute("data-count"); return; }
  if (!badge) { badge = document.createElement("span"); badge.className = "revbadge"; btn.append(badge); }
  // A repo whose count failed shows as "!" rather than as nothing: a badge that quietly reads zero
  // because `gh` is broken is indistinguishable from an empty queue, which is the one thing this
  // must never be.
  badge.textContent = broken.length && !total ? "!" : String(total);
  badge.classList.toggle("bad", broken.length > 0);
  btn.title = [
    total ? `${total} pull request${total === 1 ? "" : "s"} need you` : "Review a repo's pull requests",
    ...revCounts.filter(c => c.needs_you).map(c => `  ${c.repo_id}: ${c.needs_you}`),
    ...broken.map(c => `  ${c.repo_id}: ${c.error}`),
    ...(unasked.length ? ["", "not looked at:"] : []),
    ...unasked.map(c => `  ${c.repo_id}: ${c.skipped}`),
  ].join("\n");
}

function openReview(repoId) {
  if (!repos.length) { toast("add a repo first — review lists the PRs of the repos skein manages"); return; }
  document.body.classList.remove("show-fleet");
  // One queue across every repo (SKEIN-146). A repo id here is a FILTER over the merged list, not a
  // mode: choosing one never refetches and never clears, so what was read for one repo survives
  // looking at another. PR-number collisions are handled by `rk`, not by forgetting.
  //
  // A bare press of the review button keeps the filter you last chose; the picker's own "" is an
  // explicit "all repos" and does reset it.
  //
  // `"*"` arrives here and means EVERY repo, not a repo named `*`. It is `view.repo`'s word for
  // "the review pane is somewhere you are" while no repo is chosen (`revRepoFilter || "*"`,
  // below), and `restoreSessions` hands that saved view straight back on a reload. Read as a repo
  // id it matches nothing, so the restored queue paints a header, an empty lane and no rows —
  // measured in the browser suite, which had never reloaded with the pane open before the
  // keyboard work made it happen. The filter's own word for every repo is `""`.
  const asked = repoId === "*" ? "" : repoId;
  revRepoFilter = asked !== undefined ? asked : (localStorage.getItem("skein.reviewRepo") || "");
  if (revRepoFilter === "*") revRepoFilter = "";   // and a store poisoned before this line is fixed
  try { localStorage.setItem("skein.reviewRepo", revRepoFilter); } catch {}
  revHeld = "*";
  view = { box: null, mode: "review", kind: "agent", repo: revRepoFilter || "*" };
  applyView();
  loadReview();
}

// **move it aside** (SKEIN-552): rename an unreadable archive or snooze file to
// `<file>.unreadable-<date>` beside it, then ask for the queue again — the line goes because the
// file is no longer there to fail, and the set-aside list starts empty.
function revMoveSetAsideAside(repoId, file) {
  fetch(`/api/repos/${encodeURIComponent(repoId)}/set-aside/${encodeURIComponent(file)}/move-aside`,
        { method: "POST" })
    .then(r => r.json())
    .then(d => {
      if (!d.ok) { notice(`could not move it aside: ${d.error || "no reason given"}`); return; }
      toast(`moved to ${d.moved_to} — your set-aside list starts empty; nothing was deleted`);
      loadReview(true);
    })
    .catch(e => notice(`could not move it aside: ${failText(e)}`));
}

// One list from many queues. Per-repo `fresh`/`as_of` survive the merge — one slow repo must not
// stale the others — and every blind spot and failure is attributed to the repo it came from,
// because "a repo failed" hides exactly the information that decides whether you care.
function revMergeQueues(m) {
  const prs = [];
  for (const qq of m.queues || []) for (const pr of qq.prs || []) prs.push({ ...pr, repo_id: qq.repo_id });
  // Recency within a lane: one repo's numbers-descending, made comparable across repos.
  prs.sort((a, b) => String(b.updated_at || "").localeCompare(String(a.updated_at || "")));
  const blind = [];
  for (const qq of m.queues || []) for (const b of qq.blind_spots || []) blind.push(`${qq.repo_id}: ${b}`);
  // The blind-spot lines that name a set-aside file skein could not read, keyed by the line as it
  // is drawn, so the line can carry `move it aside` without the page re-parsing a sentence
  // (SKEIN-552). The server says which line; the page only puts the chip on it.
  const setAside = new Map();
  for (const qq of m.queues || []) for (const u of qq.unreadable_set_aside || [])
    setAside.set(`${qq.repo_id}: ${u.blind_spot}`, { repo_id: qq.repo_id, file: u.file });
  const stale = (m.queues || []).filter(q => q.fresh === false);
  return {
    ai: m.ai, prs, blind_spots: blind, queues: m.queues || [], set_aside: setAside,
    // A repo whose queue could not be built is NOT folded in with the blind spots (SKEIN-164): a
    // gap inside a queue that is here and a queue that is missing entirely are different
    // conditions, and the pane draws them differently — amber for what stands, orange for what
    // failed. Structured, because the repo dropdown marks a broken repo with "!" and a mark needs
    // data rather than a sentence to re-parse.
    failed: m.failed || [],
    // Repos skein deliberately did NOT ask about — review queue switched off, or no GitHub remote.
    // `prq::merged` reports them rather than omitting them, on the rule that "never looked" and
    // "nothing waiting" must not be the same silence; dropping the field here made the pane break
    // that rule at the one screen built to keep it, and print "acme is clear." about pull requests
    // it never asked GitHub for (SKEIN-245).
    skipped: m.skipped || [],
    fresh: stale.length === 0,
    staleRepos: stale.map(q => ({ repo_id: q.repo_id, as_of: q.as_of })),
  };
}

function loadReview(force) {
  // Whatever is already known, on screen before the request goes out.
  revQueue = revSeen.get("*") || null;
  // A load somebody ASKED for starts the chase again: pressing refresh after giving up should not
  // inherit the exhausted counter and give up immediately.
  if (force) revStaleTries = 0;
  revLoading = true; renderReview();
  fetch(`/api/review${force ? "?force=1" : ""}`).then(async r => {
    const text = await r.text();
    if (!r.ok) throw new Error(text || `HTTP ${r.status}`);
    return JSON.parse(text);
  }).then(m => {
    revLoading = false;
    if (!revHeld) return;   // you left review while this was in flight
    const q = revMergeQueues(m);
    revDecided.clear();   // the fresh rows carry their own truth; the in-place marks retire
    revQueue = q; revSeen.set("*", q); renderReview();
    // `fresh: false` means the server handed over its remembered copy and went to fetch the real
    // one. Ask again shortly — by then it is a cache hit, not another wait. Once: if the second ask
    // is still stale the fetch is genuinely slow, and hammering it would not make it faster.
    clearTimeout(revStaleTimer);
    if (q.fresh === false && revStaleTries < REV_STALE_TRIES) {
      // 4s, 8s, 16s, 32s, 64s — five tries over two minutes. Growing, because the reason it was
      // stale the first time is that GitHub is slow, and asking at the same interval would just ask
      // more often for the same reason.
      const wait = 4000 * 2 ** revStaleTries;
      revStaleTries++;
      revStaleTimer = setTimeout(() => { if (revHeld) loadReview(); }, wait);
    }
    // Fresh: the chase is over, and the next stale answer starts its own.
    if (q.fresh !== false) revStaleTries = 0;
    // The pane just fetched every repo for real; fold that into the badge rather than waiting up
    // to three minutes for the poller to rediscover numbers we already have.
    for (const qq of q.queues) {
      // **The badge counts the your-move list and nothing else** (SKEIN-300). It counts `moveOf`
      // rather than `Lane::NeedsYou` for the same reason the pane groups on it: a pull request you
      // opened with changes requested on it needs you, and a badge that says otherwise is the
      // disagreement between the badge and the pane that §2.7 is about, one field along.
      //
      // The three-minute poll folds the same rule over the same rows now (SKEIN-323), so this is
      // no longer the only place the badge is right — it is the fresher of two answers that agree,
      // which is what lets the pane's fetch land on the button without the number jumping.
      const n = yourMoveCount(qq.prs);
      const row = revCounts.find(c => c.repo_id === qq.repo_id);
      if (row) { row.needs_you = n; row.error = ""; } else revCounts.push({ repo_id: qq.repo_id, needs_you: n, error: "" });
    }
    renderRevBadge();
  }).catch(e => {
    revLoading = false;
    if (!revHeld) return;
    // **The remembered queue stays** (docs/parity.md §3, SKEIN-154). An old queue is worth
    // vastly more than an empty one, skein already has it on screen, and the failure replacing it
    // threw away rows that were there a second ago — and are still on disk in `prq::remember`. It
    // is NOT written back into `revSeen`: this is not an answer, it is the last answer plus a
    // reason, and the pane dims it so "not live" is legible without reading anything.
    const held = revSeen.get("*");
    revQueue = { ...(held || { prs: [], blind_spots: [] }),
                 error: String(e.message || e).split("\n")[0], remembered: !!held };
    renderReview();
  });
}

// **The pane's top level is ONE "your move" list, and these keys are `moveOf`'s words rather than
// `Pr::lane`'s** (SKEIN-300, SKEIN-302). They are different questions. The lane answers "is this
// review work for you", per role; this list mixes the roles, because the owner's ask was "I want to
// know what needs me very clearly" and under the lanes a pull request YOU opened was filed as
// `waiting` by definition, however stuck it was. `cockpit/src/move.mjs` holds the rule; nothing
// here re-decides it.
//
// The two groups under it are places you GO LOOKING, not lists that claim you, so both fold to a
// count that states its own composition. Archived keeps its own place at the bottom: you put it
// there by hand, so it is neither a claim nor a question.
const REV_LANES = [
  ["yours",     "your move"],
  // §10's `reply` trigger, given somewhere to land. A pull request you decided on is `theirs` by
  // definition and stays there however much its author answers you — which is the thing the queue
  // could not show. Its own group rather than folded into "your move" because the two are different
  // asks: one is a review you have not given, the other is an answer you have not read. Unfolded,
  // like your move, because it claims you; `moveOf` puts a row here only on `replied_to_me === true`.
  ["replied",   "replied to you"],
  ["theirs",    "waiting on others"],
  ["not-ready", "not ready"],
  ["archived",  "archived"],
];
// Both groups below your move start folded to a count with their reasons. Nothing is hidden — a
// count is not hiding — they are just not claiming to be your problem until you ask.
let revNotReadyOpen = false;
function toggleNotReady() { revNotReadyOpen = !revNotReadyOpen; renderReviewNow(); }
let revTheirsOpen = false;
function toggleTheirs() { revTheirsOpen = !revTheirsOpen; renderReviewNow(); }
const REV_FOLDS = { theirs: () => revTheirsOpen, "not-ready": () => revNotReadyOpen };
const revFolds = lane => Object.prototype.hasOwnProperty.call(REV_FOLDS, lane);
const revFoldOpen = lane => (revFolds(lane) ? REV_FOLDS[lane]() : true);
// What the group is made of, said on its own heading — the same job `revNotReadyWhy` does one
// group down, for the same reason: a number you cannot decompose is a number you have to open.
function revTheirsWhy(rows) {
  const n = { mine: 0, decided: 0 };
  for (const p of rows) {
    if (authored(p)) n.mine++;
    else if (p.my_review && p.my_review !== "none") n.decided++;
  }
  const parts = [];
  if (n.mine) parts.push(`${n.mine} you opened`);
  if (n.decided) parts.push(`${n.decided} you signed off`);
  return parts.join(", ");
}
// The lane's own composition: why these are not ready, tallied. A PR can be more than one of
// these at once, so the tallies are independent — they name the reasons, they do not sum to the
// count.
function revNotReadyWhy(rows) {
  const n = { draft: 0, conflicted: 0 };
  for (const p of rows) {
    if (p.draft) n.draft++;
    if (p.mergeable === false) n.conflicted++;
  }
  const parts = [];
  if (n.draft) parts.push(`${n.draft} draft${n.draft === 1 ? "" : "s"}`);
  if (n.conflicted) parts.push(`${n.conflicted} conflicted`);
  return parts.join(", ");
}
// A lane with nothing in it — and for the lane you came here for, that is an ANSWER rather than an
// absence (docs/parity.md §3, SKEIN-154).
//
// "nothing here." in the corner of a 1400px page, while the badge in the same window reads 39, is
// the shape this replaces. A cleared queue is the best moment this product has and it should read
// like one; and then the honest next thing, which is usually that another repo has ten.
//
// Narrowed by a filter or a search is NOT that moment: nothing matching what you typed says nothing
// about whether anything needs you, so those keep the plain line.
function revLaneEmpty(lane) {
  const narrowed = revFilter !== "all" || revSearch.trim();
  if (lane !== "yours" || narrowed || (revQueue && revQueue.error)) {
    return `<div class="revempty">nothing here${
      revSearch.trim() ? " matching what you typed" : revFilter !== "all" ? " under this filter" : ""}.</div>`;
  }
  return revClearHtml();
}

// What the rest of the fleet holds, when this queue holds nothing.
//
// Every number here comes off the queue already on screen — one merged answer covers every repo —
// so the calm screen costs no request. A repo that could not be READ is listed beside the repos
// that have work: "empty" and "not looked at" must never be the same screen, which is the whole
// blind-spot rule one level up.
// Why the pane cannot say the scope is CLEAR — or null when it can.
//
// A repo whose review queue is switched off, or that has no GitHub remote, is reported in
// `skipped` rather than left out (`prq::merged`), precisely so it is not mistaken for a repo with
// nothing waiting. The pane dropped the field, so the calm screen printed "acme is clear." — a
// positive claim about pull requests skein never asked GitHub for. A repo whose queue FAILED is
// the same lie facing the other way: what is in it is unknown, not empty (SKEIN-245).
//
// Answered for the SCOPE, because the scope is what the headline claims. With no repo chosen the
// claim is about the whole fleet, and one queue that WAS read earns it — "nothing is waiting on
// you" is only false when nothing was read at all. That is why a partial failure keeps the calm
// screen: the repos that answered are a real reading, and the ones that did not are rows on it.
function revUnasked() {
  const q = revQueue || {};
  const scope = revRepoFilter;
  if (scope) {
    const off = (q.skipped || []).find(x => x.repo_id === scope);
    if (off) return {
      head: `skein did not ask about ${esc(scope)}.`,
      sub: `${esc(off.skipped || "its review queue is switched off")} — so this is not "nothing is
        waiting", it is nothing looked for.`,
      fix: true,
    };
    const bad = (q.failed || []).find(x => x.repo_id === scope);
    if (bad) return {
      head: `skein could not read ${esc(scope)}.`,
      sub: `${esc(bad.error || "its queue could not be built")} — what is waiting in this repo is
        unknown, not nothing.`,
      retry: true,
    };
    return null;
  }
  if ((q.queues || []).length) return null;
  const nf = (q.failed || []).length, ns = (q.skipped || []).length;
  if (!nf && !ns) return null;
  const bits = [];
  if (nf) bits.push(`${nf} could not be read`);
  if (ns) bits.push(`${ns} ${ns === 1 ? "was" : "were"} not asked about`);
  return {
    head: "skein has not read any queue.",
    sub: `${bits.join(", and ")} — so this screen is about what skein asked for, not about what is
      waiting for you.`,
    retry: !!nf,
    fix: !!ns,
  };
}

function revClearHtml() {
  const scope = revRepoFilter;
  const elsewhere = new Map();      // repo -> { n, oldest }
  let setAside = 0;
  for (const p of (revQueue && revQueue.prs) || []) {
    if (p.lane === "archived" || p.snoozed) { if (!scope || p.repo_id === scope) setAside++; continue; }
    if (p.lane !== "needs-you" || p.repo_id === scope) continue;
    const at = revWaitedSince(p);
    const cur = elsewhere.get(p.repo_id) || { n: 0, oldest: "" };
    cur.n++;
    // Oldest first is the order the lane itself uses, and an ISO timestamp compares as a string.
    if (at && (!cur.oldest || String(at) < cur.oldest)) cur.oldest = at;
    elsewhere.set(p.repo_id, cur);
  }
  const failed = ((revQueue && revQueue.failed) || []).filter(f => f.repo_id !== scope);
  const unasked = ((revQueue && revQueue.skipped) || []).filter(u => u.repo_id !== scope);
  const rows = [
    ...[...elsewhere].sort((a, b) => b[1].n - a[1].n).map(([id, c]) => {
      const age = revAge(c.oldest).label;
      return `<button type="button" class="revclear-row" onclick="openReview(${esc(JSON.stringify(id))})">
        <span class="revclear-n">${c.n}</span>
        <span>${esc(id)}${age ? ` — oldest has waited ${esc(age)}` : ""}</span></button>`;
    }),
    // A repo skein could not read is not a repo with nothing in it, and this screen is exactly
    // where the two would otherwise be confused.
    ...failed.map(f => `<div class="revclear-row bad">
        <span class="revclear-n">!</span>
        <span>${esc(f.repo_id)} — skein could not read this queue: ${esc(f.error)}</span>
        <button type="button" class="revchip" onclick="loadReview(true)">try again</button></div>`),
    // And a repo skein did not ASK about, for the same reason and in a different colour: it is not
    // a failure, it is an absence of a question. The dash stands where a count would be, because a
    // 0 here is exactly the confusion this screen exists to prevent (SKEIN-245).
    ...unasked.map(u => `<div class="revclear-row off">
        <span class="revclear-n">—</span>
        <span>${esc(u.repo_id)} — skein did not ask: ${esc(u.skipped || "its review queue is switched off")}</span>
        <button type="button" class="revchip" onclick="openSettings('repos')">Settings → Repositories</button></div>`),
  ];
  // The headline is a CLAIM, so it is only made when the scope was read. `revUnasked` decides that
  // in one place; everything below it — the other repos' counts, the failures, the unasked — is
  // true either way and stays.
  const why = revUnasked();
  return `<div class="revclear">
    <div class="revclear-head">${why ? why.head : scope ? `${esc(scope)} is clear.` : "Nothing is waiting on you."}</div>
    <div class="revclear-sub">${why ? why.sub : `Nothing ${scope ? "here" : "in any repo skein watches"} needs your review${
      setAside ? `, and ${setAside} ${setAside === 1 ? "is" : "are"} set aside${
        scope ? "" : " across the fleet"} — they are in the lanes below` : ""}.`}</div>
    ${why && (why.retry || why.fix) ? `<div class="revclear-acts">${
      why.retry ? `<button type="button" class="revchip go" onclick="loadReview(true)">try again</button>` : ""
    }${why.fix ? `<button type="button" class="revchip" onclick="openSettings('repos')">Settings → Repositories</button>` : ""}</div>` : ""}
    ${rows.length ? `<div class="revclear-next">${rows.join("")}</div>` : ""}
  </div>`;
}

// A PR you already decided on, brought back because the branch moved under you. On the collapsed
// line this is the difference between "you have never seen this" and "you have, and it changed" —
// and without it the two are indistinguishable at exactly the row you would skip.
function revMoved(pr) {
  return pr.my_review && pr.my_review !== "none" && !pr.review_is_current;
}
function revMatchesFilter(pr) {
  if (revFilter === "all") return true;
  const rs = (pr.reasons || []).map(r => typeof r === "string" ? r : "team");
  // A team review request IS a review request — filtering to "reviewer" and hiding the PRs a team
  // of yours was asked to review would drop exactly the rows the team query was added to find.
  if (revFilter === "reviewer") return rs.includes("reviewer") || rs.includes("reviewed") || rs.includes("team");
  return rs.includes(revFilter);
}

// §6 focus rule 2: renderReview may NEVER replace a subtree containing document.activeElement.
// Measured before this existed: a half-typed comment went focused:"rev-compose",caret:4 →
// focused:BODY,caret:0 on every summary landing, the 4s stale re-poll and every filter change.
// The render is therefore DEFERRED while a composer or the search input is focused — or while a
// text selection is being made over the pane (§6 rule 3's sibling: a selection is as killable as
// a caret) — and coalesced: one render when the reader lets go, not zero and not five. Deferring
// rather than diffing keeps renderReview's contract for its ~40 callers (call it whenever state
// moved), and the surgical paths that must land WHILE a composer is open already exist beside it:
// revRepaintRow and revPendingPaint touch one row, never the pane.
// Settle the selection against the queue's NEW order, before any row paints. The selection is an
// rk key, so surviving a re-sort, a summary landing or a filter change is the default; only when
// the selected key has LEFT the list does position matter — the row that took its place by
// position inherits the selection, and flashes once to say so (§6 focus rule 1). A selection on a
// step of the open stack is on screen even though steps are not in nav, and holds as itself.
function revNavSettle(nav) {
  revNav = nav;
  if (revSel) {
    const open = revStackOpenKey && revStacks.get(revStackOpenKey);
    const inSteps = open && open.steps.some(p => rk(p) === revSel);
    if (!nav.includes(revSel) && !inSteps) {
      revSel = nav.length ? nav[Math.max(0, Math.min(revSelAt, nav.length - 1))] : null;
      if (revSel) revFlash = revSel;
    }
  }
  revSelAt = Math.max(0, nav.indexOf(revSel));
}

let revRenderQueued = false;
function revRenderHeld() {
  if (!revpane || !revpane.contains) return false;
  const ae = typeof document !== "undefined" && document.activeElement;
  if (ae && revpane.contains(ae) && /^(INPUT|TEXTAREA)$/.test(ae.tagName || "")) return true;
  const s = typeof window !== "undefined" && window.getSelection ? window.getSelection() : null;
  if (s && !s.isCollapsed && s.anchorNode && revpane.contains(s.anchorNode)) return true;
  return false;
}
function revRenderFlush() {
  if (revRenderQueued && !revRenderHeld()) renderReview();
}

// A render the READER just caused, painted now instead of queued.
//
// §6 focus rule 2 defers a render while the pane holds a caret or an uncollapsed selection, and it
// is right to: a render nobody asked for — a summary landing, the 4s re-poll, a workflow answering
// — must not move the caret out of a half-typed comment. A render caused by the reader's own press
// is the opposite case, and deferring it is how a press comes to look like a broken button: the
// critique panel is a stack of textareas, so selecting a phrase in a drafted comment and then
// pressing post left NOTHING on screen — no "posting…", no disabled chip — while the request was
// genuinely in flight. Reported in exactly those words: "posting comments button doesn't work,
// they aren't responsive even if something is happening in the background" (SKEIN-264).
//
// Nothing is lost by painting. Every field in this pane writes through to state on `oninput`, so
// the rebuilt panel carries what was typed; what a forced render costs is the caret's POSITION.
//
// **The ANSWER to a press forces too** (SKEIN-385, SKEIN-416). This comment used to end "which is
// why the press-time call forces and the answer-time call in the same handler does not", and that
// sentence was the defect rather than the design. A press is over in a frame; its answer lands
// seconds later, by which time the reader's hands are back in the pane and the deferral is on. So
// `revPendingPaint` left a merge GitHub refused with no receipt at all, and `revDraft` left a
// drafted review that existed only in state — the button, in both cases, having visibly done
// nothing. Every callback that resolves a press paints now, and each forces only while the thing
// it is answering is still on screen; a render for something nobody is looking at is the render
// rule 2 is actually about.
//
// The `oninput` handlers are the deliberate exception and keep `renderReview`: a keystroke is not
// a press, and rebuilding the field under the typing hand is the exact harm rule 2 exists for.
function renderReviewNow() { renderReview(true); }

// A confirmation the browser REFUSED TO SHOW is not a "no".
//
// Tick "prevent this page from creating additional dialogs" once — a browser offers it after the
// second dialog — and every later `confirm` returns false without asking anybody. At the call site
// that is indistinguishable from Cancel, so the press vanishes with nothing said, for the rest of
// the tab's life. It was one of three ways this pane could look like a dead button (SKEIN-264).
//
// The tell is the clock, not the answer: a dialog a person read and dismissed took them time to
// dismiss; one that was never drawn returns within the same frame. Below that floor the `false` is
// not trusted as a refusal and the reader is told why nothing happened. It cannot tell a suppressed
// dialog from an impossibly fast Cancel, and does not need to — being wrong costs one sentence,
// never an action, because the answer is still treated as no.
const CONFIRM_FLOOR_MS = 12;
function confirmed(question) {
  const at = Date.now();
  let said = false;
  try { said = confirm(question); } catch { said = false; }
  if (said) return true;
  if (Date.now() - at < CONFIRM_FLOOR_MS) {
    toast("this browser is blocking skein's confirmations, so nothing was sent — reload the tab and allow dialogs for it");
  }
  return false;
}

// An exception is a ROW's problem, not the queue's (SKEIN-268).
//
// Reported by the owner: "any small error anywhere in the review page just blanks the entire page
// and gives the error." The pane is built as one string and assigned in one shot, so a throw in any
// of the thirty-odd helpers that string calls means the assignment never runs: the pane is blank if
// it throws before the first paint, frozen at whatever it last drew if later, and either way the
// reason went to devtools, where nobody is looking, and the way out went with the surface.
//
// So the paint is guarded here, at the top. Beneath it, `revRowSafe` guards each ROW — the layer
// that matters, because everything touching model output, a diff, a draft or a workflow is
// per-row, and that is where malformed data actually arrives. This one is the backstop for
// everything that is not: the picker, the lane headings, the failure boxes, `revUnasked`.
function renderReview(force) {
  try { revRenderPane(force); }
  catch (e) { revRenderFailed(e); }
}

// The pane could not be built. KEEP WHAT IS ON SCREEN — `revpane.innerHTML` was never reached, so
// the last good queue is still there, and an old queue is worth vastly more than nothing: it is
// also the only way out still on the page. The reason goes in a strip above it, in the page,
// selectable, because a reason in devtools is a reason nobody reports.
//
// Prepended rather than rendered, deliberately: this runs because a render just failed, and asking
// the same render to draw the notice about its own failure is one throw from a blank pane again.
function revRenderFailed(e) {
  const why = String((e && e.message) || e || "unknown").split("\n")[0];
  reportPageError(why);
  if (!revpane || !revpane.querySelector) return;
  let strip = revpane.querySelector(".revcrash");
  if (!strip) {
    strip = document.createElement("div");
    strip.className = "revfail revcrash";
    if (revpane.prepend) revpane.prepend(strip);
    else revpane.insertBefore(strip, revpane.firstChild);
  }
  strip.innerHTML = `<b>the queue could not be drawn</b>
    <div>${esc(why)}</div>
    <div>What is below this is the last thing skein drew — it may be out of date.</div>
    <div class="revacts"><button type="button" class="revchip go" onclick="loadReview(true)">read it again</button></div>`;
}

// One row, drawn inside its own guard. A row that throws becomes a row SAYING it could not be
// drawn — with which pull request it is and why — while the other twenty-nine draw.
//
// It keeps its `data-rk`, so the keyboard's navigation and every `[data-rk]` query still find it: a
// row that cannot be drawn is still a row that exists, and dropping it would silently shorten j/k
// for as long as the fault lasted, which is a second failure hiding behind the first.
function revRowSafe(en) {
  try { return en.make(); }
  catch (e) { return revRowBrokenHtml(en.key, e); }
}

function revRowBrokenHtml(key, e) {
  const why = String((e && e.message) || e || "unknown").split("\n")[0];
  reportPageError(`${key}: ${why}`);
  // The way out is GitHub. Skein cannot draw this pull request, and the one thing it still knows
  // about it — which one it is — is exactly what is needed to go and look at it somewhere else.
  const at = String(key).lastIndexOf("#");
  const url = at > 0
    ? `https://github.com/${String(key).slice(0, at)}/pull/${String(key).slice(at + 1)}`
    : "";
  return `<div class="revrow broken" data-rk="${esc(key)}">
    <span class="mv"></span>
    <div class="revmain">
      <div class="revline"><span class="revtitle">${esc(key)} — skein could not draw this row</span></div>
      <div class="gist">${esc(why)}</div>
    </div>
    ${url ? link(url, "open on GitHub ↗", 'class="revopen" target="_blank" rel="noopener"') : ""}
  </div>`;
}

function revRenderPane(force) {
  if (!revpane) return;
  // `force` is the reader's own press (`renderReviewNow`), and the one caller that repaints the
  // very field being typed in and restores its caret itself (revSearchSet); a render nobody asked
  // for waits for the reader's hands.
  if (!force && revRenderHeld()) { revRenderQueued = true; return; }
  revRenderQueued = false;
  // The notes panel is about a repo, and this is where which repo that is gets decided. Called
  // before anything below is built so `revModsCount` and `revModsHtml` read a panel that is already
  // about the repo this render is drawing, rather than one the NEXT render would correct — a repo
  // filter is changed with the mouse, and the press that follows it can be in the same second.
  revModsSync();
  // Nine repositories were a strip with counts, until nine of them wrapped into six ragged rows
  // that pushed the queue below the fold (SKEIN-211). Now they are one dropdown — but SKEIN-163's
  // point survives the change of form: every count and every failure mark lives in the option
  // TEXT, never in a tooltip, and the closed control names the current choice with its number.
  // A zero-count repo dims but stays choosable; a repo whose queue could not be built wears "!"
  // instead of a number it does not have, and its option's title carries the error. Repo and
  // audience stay orthogonal axes, which is why this stays apart from the reason chips: merged
  // into one group, either picking a repo deselects "to review" or the group stops reading as a
  // radio. Choosing routes through `openReview` exactly as the strip's click did — a filter over
  // the merged queue, never a refetch.
  const repoCounts = new Map();
  for (const p of (revQueue && revQueue.prs) || []) {
    if (p.lane === "archived") continue;
    repoCounts.set(p.repo_id, (repoCounts.get(p.repo_id) || 0) + 1);
  }
  const brokenRepos = new Map(((revQueue && revQueue.failed) || [])
    .map(f => [f.repo_id, f.error || "this repo's queue could not be built"]));
  // A repo skein never asked about wore `acme · 0` — the same option, to the byte, as a repo with a
  // clean queue, which is the choice that led to the headline this whole item is about. A dash is
  // not a count, and the option says why it has none (SKEIN-245).
  const unaskedRepos = new Map(((revQueue && revQueue.skipped) || [])
    .map(u => [u.repo_id, u.skipped || "skein did not ask about this repo"]));
  const allCount = [...repoCounts.values()].reduce((a, b) => a + b, 0);
  const picker = repos.length > 1
    ? `<select class="revrepo" aria-label="Repository" onchange="openReview(this.value)">${[
        `<option value=""${revRepoFilter ? "" : " selected"}>all repos · ${allCount}</option>`,
        ...repos.map(r => {
          const n = repoCounts.get(r.id) || 0;
          const broken = brokenRepos.has(r.id);
          const off = !broken && unaskedRepos.has(r.id);
          return `<option value="${esc(r.id)}"${r.id === revRepoFilter ? " selected" : ""}${
            n || broken ? "" : ` class="none"`}${broken ? ` title="${esc(brokenRepos.get(r.id))}"` : ""}${
            off ? ` title="${esc(unaskedRepos.get(r.id))}"` : ""}>${
            esc(r.id)} · ${broken ? "!" : off ? "—" : n}</option>`;
        }),
      ].join("")}</select>`
    : `<span>${esc(revRepoFilter || "all repos")}</span>`;
  const chips = [["all","all"],["author","mine"],["reviewer","to review"],["mentioned","mentioned"]]
    .map(([k, label]) => `<button type="button" class="revchip${revFilter === k ? " on" : ""}" onclick="setRevFilter(${esc(JSON.stringify(k))})">${esc(label)}</button>`)
    .join("");
  let body = "";
  if (revLoading && !revQueue) {
    body = `<div class="revempty">asking GitHub…</div>`;
  } else if (revQueue) {
    // The whole queue failed. The box says what happened AND what to do about it — a report with no
    // move in it is where this pane used to stop (SKEIN-154). Three affordances: ask again, the
    // setting that holds the credential it failed on, and — when there is one — the copy skein
    // already had, which stays on screen below rather than being replaced by this.
    if (revQueue.error) {
      body += `<div class="revfail"><b>the queue could not be built</b>
        <div>GitHub said: ${esc(revQueue.error)}</div>
        ${revQueue.remembered
          ? `<div>These are the pull requests skein last read${
              (revQueue.staleRepos || []).length ? ` — ${esc((revQueue.staleRepos || []).map(r => `${r.repo_id} ${revAgo(r.as_of)}`).join(", "))}` : ""
            }. Check states and new commits are not in them.</div>`
          : ""}
        <div class="revacts">
          <button type="button" class="revchip go" onclick="loadReview(true)">${revLoading ? "trying…" : "try again"}</button>
          <button type="button" class="revchip" onclick="openSettings('github')">Settings → GitHub &amp; keys</button>
        </div></div>`;
    }
    const shown = (revQueue.prs || [])
      .filter(p => !revRepoFilter || p.repo_id === revRepoFilter)
      .filter(revMatchesFilter)
      .filter(revMatchesSearch);
    // A repo whose queue could not be built at all — skein failed, which is what the orange box is
    // for. Above the blind spots because a repo that is missing entirely outranks a gap inside one
    // that is here, and separate from them because they are different conditions: this one may be
    // over by the next refresh (SKEIN-164).
    const failed = (revQueue.failed || [])
      .filter(f => !revRepoFilter || f.repo_id === revRepoFilter);
    if (failed.length) {
      body += `<div class="revfail"><b>${failed.length === 1
        ? "a repo's queue could not be built" : `${failed.length} repos' queues could not be built`
        }</b>${failed.map(f => `<div>${esc(f.repo_id)} — ${esc(f.error)}</div>`).join("")}</div>`;
    }
    // Blind spots are stated and never swallowed — a queue that under-reports silently is worse than
    // no queue — but they are STANDING conditions, and amber says so: `gh` without read:org is true
    // of every load until somebody runs the command in the sentence. It used to wear the failure's
    // box, which is how an alarm stops being read.
    const blind = (revQueue.blind_spots || [])
      .filter(b => !revRepoFilter || b.startsWith(revRepoFilter + ":"));
    if (blind.length) {
      body += `<div class="revblind">${blind.map(b => {
        const u = revQueue.set_aside && revQueue.set_aside.get(b);
        const chip = u ? ` <button type="button" class="revchip" data-set-aside="${esc(u.file)}"
          onclick="revMoveSetAsideAside(${esc(JSON.stringify(u.repo_id))}, ${esc(JSON.stringify(u.file))})"
          title="renames the file beside itself; nothing is deleted">move it aside</button>` : "";
        return `<div><span class="revblindwhat">incomplete</span> — ${esc(b)}${chip}</div>`;
      }).join("")}</div>`;
    }
    // 25 red rows is one broken pipeline, not twenty-five decisions: when red is most of the
    // queue it is said once, here, and the row's mark says whose move it is instead. Demoted,
    // not hidden — an opened row still reports its own checks. A lone failure stays row-level.
    const red = shown.filter(p => p.checks === "failing").length;
    if (shown.length >= 3 && red > shown.length / 3) {
      body += `<div class="cired">${red} of ${shown.length} are red — read it as one broken pipeline, not ${red} broken pull requests. Each row still says its own checks when opened.
        <button type="button" class="revchip" onclick="revSnoozeRed()" title="each returns on its own when its author pushes">set the red ones aside until they move</button></div>`;
    }
    // A chip must be true of a MINORITY of rows: when a whole wave of heads moved, the wave is the
    // story, so `moved` leaves the collapsed rows once more than a third of the queue wears it.
    revCommonChips = shown.length >= 3 && shown.filter(revMoved).length > shown.length / 3
      ? new Set(["moved"]) : new Set();
    // Dependent chains collapse into one row each; their members never appear loose anywhere —
    // fifteen scattered rows that are one change is the wrong-review generator this exists for.
    // A live search dissolves the stacks: their aggregation hides exactly the row the searcher is
    // after, and a hit inside "15 pull requests, one change" is a hit you cannot see.
    const stacks = revSearch.trim() ? [] : revChains(shown);
    const inStack = new Set(stacks.flatMap(st => st.steps.map(rk)));
    revStacks = new Map(stacks.map(st => [revStackKey(st), st]));
    // Two passes, and the split is the keyboard's (§6 focus rule 1): the FIRST computes what each
    // lane will show, in its final order, so the selection can be settled against the NEW order
    // before any row paints itself — a row renders its own `sel` class, so a selection reconciled
    // after rendering would be a render behind. The SECOND paints.
    const lanes = [];
    const nav = [];
    for (const [lane, label] of REV_LANES) {
      const loose = shown.filter(p => moveOf(p) === lane && !inStack.has(rk(p)));
      const laneStacks = stacks.filter(st => revStackLane(st) === lane);
      // Your move always shows its heading, even at zero — "nothing needs you" is an answer you
      // came here for, and a missing section reads as a page that failed to load.
      if (!loose.length && !laneStacks.length && lane !== "yours") continue;
      // A stack sorts by its next actionable step, not its tip — the tip is the one PR you
      // cannot review yet, and it is exactly what floats to the top of a recency sort.
      //
      // Your move is ordered by how long it has waited on you, OLDEST first: working from the top
      // clears what is most likely blocking a colleague. The other lanes keep recency — for your
      // own PRs, "what moved most recently" is the right question.
      //
      // `revSortAt` is the same function the AGE CELL renders (SKEIN-251), and that is the whole
      // point of asking it here rather than spelling the rule out a second time: the column exists
      // so this order can be audited, and a column computing the order for itself audits nothing.
      const sortAt = revSortAt;
      // What skein read moves the row WITHIN its lane, never across lanes: a tripwire flag lifts a
      // row above the unread, a routine verdict sinks below them, and nothing the model says can
      // take a PR out of the lane its readiness put it in — AI adds scrutiny, never removes it. A
      // wrong promotion costs a glance; a wrong demotion costs a missed regression.
      //
      // A stack sorts by the step you would actually open: its age is a question about the next
      // decision, not about the tip.
      const entries = [
        ...loose.map(p => ({ at: sortAt(p), key: rk(p), make: () => revRow(p) })),
        ...laneStacks.map(st => {
          const next = revStackNext(st) || st.steps[0];
          return { at: sortAt(next), key: revStackKey(st), make: () => revStackRow(st) };
        }),
      ].sort((a, b) => lane === "yours"
        ? String(a.at).localeCompare(String(b.at))
        : String(b.at).localeCompare(String(a.at)));
      // "N decisions from M pull requests": a stack is one entry point and several decisions in a
      // forced order — the heading says both, which is also how the badge (counting PRs) and the
      // pane (counting starts) stop disagreeing.
      const total = loose.length + laneStacks.reduce((n, st) => n + st.steps.length, 0);
      // A folded group is computed in full and DRAWN as nothing. Its rows are navigable exactly
      // when they are visible, so a selection can never sit on a row nobody can see; the count on
      // the heading is the whole group either way, because a fold that also under-counted would be
      // hiding rather than folding.
      const drawn = revFoldOpen(lane) ? entries : [];
      lanes.push({ lane, label, loose, entries: drawn, count: entries.length, total });
      for (const en of drawn) nav.push(en.key);
    }
    revNavSettle(nav);
    for (const { lane, label, loose, entries, count, total } of lanes) {
      if (revFolds(lane)) {
        const why = lane === "not-ready" ? revNotReadyWhy(loose) : revTheirsWhy(loose);
        body += `<div class="revlane" data-lane="${esc(lane)}"><h4 class="revfold" onclick="${lane === "not-ready" ? "toggleNotReady()" : "toggleTheirs()"}">${esc(label)}
            <span class="revn">${count}</span>
            <span class="dim">${why ? `· ${esc(why)} ` : ""}— ${revFoldOpen(lane) ? "click to fold" : "click to expand"}</span></h4>${
          entries.map(revRowSafe).join("")
        }</div>`;
        continue;
      }
      body += `<div class="revlane" data-lane="${esc(lane)}"><h4>${esc(label)} <span class="revn">${entries.length}</span>${
          entries.length !== total ? ` <span class="dim">from ${total} pull requests</span>` : ""
        }</h4>${
        entries.length ? entries.map(revRowSafe).join("")
                    : revLaneEmpty(lane)
      }</div>`;
    }
  }
  // `notlive` dims the rows, and the dimming is load-bearing: it is how you tell at a glance that
  // what you are looking at is the copy skein remembers rather than what GitHub says now, without
  // reading a word (§8.4). The head keeps full strength — the way out lives there.
  revpane.innerHTML = `<div class="revwrap${revQueue && revQueue.error && revQueue.remembered ? " notlive" : ""}"><div class="revhead">${picker}<div class="revchips">${chips}</div>
      <input class="revsearch" type="search" placeholder="find a pull request…" value="${esc(revSearch)}"
        oninput="revSearchSet(this.value)"
        onkeydown="if (event.key === 'Escape') { event.stopPropagation(); if (this.value) revSearchSet(''); else this.blur(); }">
      <span class="revspacer"></span>
      ${revScopeRepo() ? `<button type="button" class="revchip${revModsOpen ? " on" : ""}" onclick="toggleMods()">${revModsCount()}</button>` : ""}
      ${revQueue && revQueue.fresh === false
        ? `<span class="revage" title="These repos are showing the pull requests skein last read; it is fetching their current lists. The others are current — one slow repo does not stale the rest.">${
            esc((revQueue.staleRepos || []).map(r => `${r.repo_id} read ${revAgo(r.as_of)}`).join(", "))} · ${
            revStaleTries >= REV_STALE_TRIES
              ? `still fetching — <button type="button" class="revchip" onclick="loadReview(true)">try again</button>`
              : "refreshing"}</span>`
        : ""}
      <button type="button" class="revchip" onclick="loadReview(true)">${revLoading ? "refreshing…" : "refresh"}</button>
    </div>${revModsHtml()}${body}</div>`;
  revFlash = "";   // the hand-off flash is one render's worth of saying so, not a state
}

// How old the queue on screen is, in the shortest true form. Its own function rather than a shared
// one because the shared ages on this page are driven by a ticking clock over `age_secs`; this is a
// single RFC 3339 stamp rendered once per fetch, and borrowing that machinery for it would be more
// moving parts than the sentence needs.
function revAgo(iso) {
  const at = Date.parse(iso || "");
  if (!at) return "a while ago";
  const s = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function setRevFilter(k) { revFilter = k; renderReviewNow(); }

// The repo that repo-scoped chrome (the notes pane, the read-ahead switch) belongs to: the filter
// you chose, or the only repo on screen. With several repos and no filter there is no honest
// answer, and the chrome waits rather than guessing.
function revScopeRepo() {
  if (revRepoFilter) return revRepoFilter;
  const qs = revQueue && revQueue.queues;
  return qs && qs.length === 1 ? qs[0].repo_id : "";
}

// ---------- standing notes on the repo's modules ----------
//
// These answer *questions*, not summaries: a diff carries its own before-state, but a question is
// usually about the thing the diff landed in, which the diff cannot show. Each note records the
// commit its module was at when it was written, so "out of date" is a fact here rather than a
// worry — and a stale note is never used to answer anything.
let revMods = null;        // {modules:[{path, owners, state, written}], unread_because}
let revModsRepo = "";      // the repo `revMods` is the answer FOR — read it through `revModsShown`
let revModsOpen = false;
let revWriting = "";       // the module being written right now

// **What is loaded is one repo's answer, and a module path does not say whose** (SKEIN-427).
//
// `src`, `docs` and `tests` are modules of half the repos in a fleet, so a row drawn from repo A's
// list is a perfectly plausible row of repo B's — on screen, and in a request. `revMods` used to be
// read directly and was never cleared when the pane's repo changed, so with the panel open you
// could switch the filter from A to B and press "re-write" on a row that was still A's: the note
// went to `/api/repos/B/modules/write` carrying A's path, and nothing on the page said so.
//
// This is the only way in now. It answers with the list only while the repo it was loaded for is
// still the repo the pane is scoped to, so every reader of it — the count on the chip, the panel,
// and the press — is about one repo or about none.
function revModsShown() {
  const repo = revScopeRepo();
  return repo && repo === revModsRepo ? revMods : null;
}

// Keep the panel about the repo on screen: forget another repo's answer, and ask for this one's
// while the panel is open.
//
// Derived from `revScopeRepo()` rather than cleared where the filter moves, because the scope
// changes in more ways than one press: `openReview` sets the filter, and a queue that lands with a
// single repo in it makes THAT repo the scope with nobody pressing anything. A fix that listed the
// ways would be one `revQueue = …` away from being wrong again.
//
// Cheap and idempotent — it returns at once as soon as the two agree — which is what lets the
// pane's own render call it, the one place that runs on every way the scope can change. It cannot
// ask twice for the same repo either: `loadModules` stamps `revModsRepo` at the REQUEST, so a load
// that fails leaves the two agreeing and is not retried on every render.
function revModsSync() {
  const repo = revScopeRepo();
  if (repo === revModsRepo) return;
  revMods = null;
  revModsRepo = "";
  if (revModsOpen && repo) loadModules();
}

function loadModules() {
  const repo = revScopeRepo();
  // Nothing to ask about, and nothing on screen to ask for: `revModsHtml` draws no panel without a
  // scope either, so this early return no longer strands a panel that cannot refresh itself.
  if (!repo) return;
  // Another repo's answer is not this one's, and must not be on screen while this one is fetched.
  // The same-repo case is left alone on purpose: re-reading after a note is written would otherwise
  // blank the list it is about to redraw.
  if (revModsRepo !== repo) revMods = null;
  revModsRepo = repo;
  fetch(`/api/repos/${encodeURIComponent(repo)}/modules`).then(r => r.json())
    .then(ms => {
      // Overtaken in flight by a change of repo. This answer is about a repo the pane is no longer
      // showing, and storing it would put exactly the wrong list back under exactly the wrong
      // queue — the defect above, arriving a second late instead of a second early.
      if (revModsRepo !== repo) return;
      revMods = ms && ms.modules ? ms : { modules: [], unread_because: (ms && ms.unread_because) || "" };
      revModsPaint();
    })
    .catch(() => {});
}
function toggleMods() {
  revModsOpen = !revModsOpen;
  if (revModsOpen && !revModsShown()) loadModules();
  renderReviewNow();
}
function writeModule(path) {
  if (revWriting) { toast("one at a time — each note takes a minute"); return; }
  // **A press writes about the repo whose list this row came from, or it writes nothing**
  // (SKEIN-427). The press used to be unconditional, on the argument that the chip dispatching it
  // is drawn by `revModsHtml` and so the panel is open by construction. The panel being open was
  // never the question: the question is which repo it is open ON, and a row can outlive the answer
  // it was drawn from by exactly as long as it takes to change the filter.
  const shown = revModsShown();
  const repo = revScopeRepo();
  if (!shown || !(shown.modules || []).some(m => m.path === path)) {
    // What is said is what is KNOWN: the panel is not showing that row for this repo. Whether the
    // path is a module of it is a different question, and one this pane cannot answer while it is
    // holding another repo's answer or none.
    toast(`nothing was written — the notes panel is ${repo ? `on ${repo}` : "not on one repository"} now, and ${path} is not one of the modules it is showing`);
    revModsPaint();
    return;
  }
  // A press is over in a frame — see `renderReviewNow` — and what it has to put on screen is the
  // chip going to "writing…" and its siblings going disabled.
  revWriting = path; renderReviewNow();
  fetch(`/api/repos/${encodeURIComponent(repo)}/modules/write`, {
    method: "POST", headers: { "content-type": "application/json" },
    // The repo travels in the body as well as in the path, and the server refuses the two when
    // they disagree (`api_write_module`). It is not the same fact twice: the path is where the
    // PANE is pointing and the body is where the ROW came from, two variables whose disagreement
    // was the whole of this bug. The guard above makes them equal here — and a page that has lost
    // track of which repo it is drawing is precisely the thing that cannot notice it has.
    body: JSON.stringify({ path, repo: revModsRepo }),
  }).then(r => r.json()).then(d => {
    revWriting = "";
    if (!d.ok) { notice(d.error || "could not write that note"); revModsPaint(); return; }
    toast(`note written for ${path}`);
    // The success arm paints through `loadModules`: what changes on screen is the row's state dot
    // and its stamp, and those are the server's answer rather than this one's, so re-reading them
    // IS the paint.
    loadModules();
  }).catch(e => { revWriting = ""; notice(`could not write that note: ${failText(e)}`); revModsPaint(); });
}

// The answer to a press on the notes panel, painted NOW — while the panel is still what the reader
// is looking at (SKEIN-422).
//
// `writeModule` ended both its arms in `renderReview`, and §6 rule 2 holds that render while the
// pane owns a caret or a live selection. So "writing…" could sit on the chip after the note had
// landed, and the row's state dot stayed on the old answer until the reader's hands moved.
// `loadModules` is here too rather than only in the arm that calls it: `toggleMods` opens the panel
// on "looking at the repo…", and that sentence is replaced by the list on exactly the same render.
// That press already forces, so painting its answer takes no caret the press had not taken.
//
// `revModsOpen` is that question: `revModsHtml` returns "" when it is false, so with the panel shut
// there is no element anywhere on the page that this answer would change, and forcing a render
// would take a caret for nothing.
function revModsPaint() {
  if (revModsOpen) renderReviewNow(); else renderReview();
}
// The chip's own label carries the state, so opening the panel is a choice rather than the only way
// to find out whether there is anything to do in it.
function revModsCount() {
  const mods = revModsShown();
  if (!mods) return "notes";
  // A repo skein could not read has no count to give. "0/0" would be a claim about a repo nobody
  // looked at — the same conflation SKEIN-117 fixed on the server side.
  if (mods.unread_because) return "notes ?";
  const fresh = mods.modules.filter(m => m.state === "fresh").length;
  return `notes ${fresh}/${mods.modules.length}`;
}
function revModsHtml() {
  // Drawn under exactly the condition the chip that shuts it is drawn under — `revScopeRepo()`, in
  // the header above. They were two conditions, and the pair had a gap in it: clear the filter with
  // several repos in the fleet and there is no honest scope, so the chip went and the panel stayed,
  // on screen with nothing anywhere on the page to close it (SKEIN-427).
  if (!revModsOpen || !revScopeRepo()) return "";
  const mods = revModsShown();
  if (!mods) return `<div class="revmods"><div class="revempty">looking at the repo…</div></div>`;
  // Three states, and the pane says which: still looking, could not look, looked and found nothing.
  // The middle one used to wear the last one's sentence.
  if (mods.unread_because) return `<div class="revmods"><div class="revempty">skein could not read this repo — ${esc(mods.unread_because)}</div></div>`;
  if (!mods.modules.length) return `<div class="revmods"><div class="revempty">skein read this repo and found no modules in it.</div></div>`;
  return `<div class="revmods">
    <p class="revmodsnote">What each part of the repo is, written once and re-written when that part
      changes. Used to answer your questions about a PR — never to write its summary, and a note
      whose module has moved since is skipped rather than trusted.</p>
    ${mods.modules.map(m => `<div class="revmod">
      <span class="revmodstate ${esc(m.state)}" title="${m.state === "fresh" ? "current" : m.state === "stale" ? "the module has changed since this was written" : "no note yet"}"></span>
      <code>${esc(m.path)}</code>
      ${(m.owners || []).length ? `<span class="dim">${esc((m.owners || []).join(" "))}</span>` : ""}
      <span class="revspacer"></span>
      <button type="button" class="revchip" onclick="writeModule(${esc(JSON.stringify(m.path))})"${revWriting ? " disabled" : ""}>${
        revWriting === m.path ? "writing…" : m.state === "absent" ? "write" : "re-write"}</button>
    </div>`).join("")}
  </div>`;
}

