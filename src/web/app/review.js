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
// What skein would do to each pull request in this repo, and what it could be told to do —
// `{enabled, defined: [...], prs: {number: standing}}`.
//
// Read separately from the queue, and on purpose. The queue is what GitHub says and is cached for a
// minute; this is skein's own answer about it, and it changes the instant somebody assigns a
// workflow. Folded together, choosing a workflow would not show up until GitHub was read again.
let revFlows = new Map();  // repo_id -> that repo's workflow state (or {error})
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
let revSums = new Map();      // PR number -> summary, or "…" while one is in flight

// **Repos whose readings-on-disk this load has actually heard about**, filled by
// [`loadKnownSummaries`] and read by `revPumpSummaries`.
//
// The pump reads ahead for every row `revSums` does not hold, and "does not hold" is the same
// question as "was never read" only once the bulk payload has landed. Before that it is the same
// question as "the answer is still on its way", and those two have opposite right answers.
//
// `loadReview` starts `loadKnownSummaries` and `loadWorkflows` in that order and they answer in
// whichever order the machine decides. `loadWorkflows` ends by pumping, deliberately: consent to
// read ahead arrives on that payload, and a pump that ran before it found nothing it was allowed
// to read and never asked again. That settled one order and opened the other. With the workflows
// answer first the pump read every row on the queue; each read came back off disk over the stream
// as a FULL reading; and the row shapes in the bulk payload were then dropped on arrival by
// `loadKnownSummaries`'s own guard, which refuses to overwrite a full reading with a thinner one.
// So the page spent one request and one full reading PER ROW where the single row-shaped payload
// SKEIN-287 added was already on its way with all of them — measured against the six-row fixture in
// `tests/ui/review.mjs`, where holding that payload open produced four `POST …/read` and no thin
// reading at all. Which of the two responses landed first is the whole of it (SKEIN-704).
//
// The pump waits on THIS rather than on a duration. `loadKnownSummaries` records the repo whether
// it answered or failed, so there is no order of events in which this stops the pump for good.
let revKnownHeard = new Set();

// **What skein is reading RIGHT NOW**, off `/api/review/reading`, keyed `repo#number` (SKEIN-333).
//
// The page used to keep this knowledge in `revSums` as the string "…", and that is precisely the
// defect: "…" lived in the same map the queue refresh writes into, and the refresh overwrote it
// with the reading on disk — so a row spent most of a 35-second model call displaying the OLD
// answer, and the press looked like it had done nothing. Reported repeatedly, and finally as the
// diagnosis rather than the symptom: "even if it is doing work, I am unable to see."
//
// It is a SEPARATE map for that reason. In-flight is not a kind of summary and must not live where
// summaries are merged; nothing that merges readings may write here, and the row's gist is drawn
// from here rather than from the absence of a summary.
//
// The server owns the truth (`review::readings`), so this survives a reload, a repo switch and a
// second tab, and it carries skein's own background reads as well as pressed ones — the owner's
// answer when asked: any read in flight shows.
let revInFlight = new Map();  // repo#number -> { started_ms, asked }
// Rows whose reading landed and that nobody has looked at since (SKEIN-333). Cleared by OPENING
// the row, not by a timer — the owner's choice, so a read that landed while you were elsewhere is
// still evident when you come back rather than having quietly aged out.
let revUpdated = new Set();
// **What that mark MEANS: a reading was REPLACED**, not "a reading arrived". The two are the same
// thing only for somebody who was already looking at one, and the pane's own first fill is exactly
// where they are not: `revFetchSummary` marked every row its `.finally` touched, so opening the
// pane cold stamped "● updated" on every row the pump filled — a mark reading "this changed while
// you were away" on rows nobody had ever seen, and nothing had changed and nobody was away. A mark
// that fires on everything is the texture this page spends its whole design budget avoiding.
//
// A REAL reading is one with something to say. "…" is a press half a second old, `unread` is skein
// saying it did not look, and `transient` is a failure to reach a reading rather than a reading —
// replacing any of the three tells a reader nothing they were not already watching happen.
function revHasReading(key) {
  const s = revSums.get(key);
  return !!(s && s !== "…" && !s.transient && s.depth !== "unread");
}
// How often the page asks WHAT is in flight. The elapsed seconds are counted locally from
// `started_ms` (`tickInFlight`), so this is not the clock — it is only how quickly a read somebody
// else started, or one that finished, shows up here. The owner's constraint: "for the timer I hope
// you are counting locally and doing github request once in a while only or on refresh."
const REV_INFLIGHT_POLL_MS = 4000;
let revInFlightTimer = null;
// Three at a time. Each summary is a `gh pr diff` plus one or two model calls on the SAME
// subscription the fleet is working on, so this is a deliberate throttle rather than a UI nicety —
// thirty at once would take the rate-limit window away from the boxes doing the real work.
const REV_SUM_PARALLEL = 3;
// **And the width for a read somebody PRESSED, which is a different number for a different
// reason** (SKEIN-353). The two must never be read as one: `REV_SUM_PARALLEL` above is a throttle
// on skein's own initiative, and the owner's rule is that his own presses have none — "Limit is
// only for automatic stuff, manually I can invoke as many as I want", and again, on this code,
// "I thought the 2 slots read is for you to automatically do, not when I ask for it. If I ask for
// it, then it is unlimited."
//
// This is therefore NOT a budget and NOT a throttle on the person. It is a guard against one
// specific consequence: a large simultaneous burst trips GitHub's SECONDARY rate limit, which
// `src/github.rs` answers with a flat fifteen-minute hold on every GitHub read for the whole fleet.
// That is the whole and only reason the number is not infinity, and the owner chose ten knowing it.
//
// What the old shared width cost, measured on his fleet while he waited half an hour for one
// result: three reads pinned at three (#716, #718, #726) ran past their own 180-second budget and
// never ended, so neither stack could advance a single step. A ceiling on something asked for turns
// one slow answer into no answers.
const REV_ASKED_PARALLEL = 10;
let revSumBusy = 0;

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
  // Same poll, same moment: the train's stops arrive on the counts the badge is drawn from, so the
  // banner costs no request of its own and can never disagree with the badge about what is known.
  renderTrainBanner();
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
// The merge train's stops, as a ROW above the app like #healthban — prepended to body's flex
// column so it pushes the app down rather than covering the tab bar it is interrupting. A stopped
// PR is a decision only a person can make, and the repo's segment is the one click that opens it.
// An entry from an older server carries no `stopped` field at all; that reads as "nothing
// stopped", never as an error.
function renderTrainBanner() {
  const stops = revCounts.filter(c => (c.stopped || []).length);
  let ban = document.getElementById("trainban");
  if (!stops.length) { ban?.remove(); return; }
  if (!ban) { ban = document.createElement("div"); ban.id = "trainban"; document.body.prepend(ban); }
  ban.innerHTML = [
    `<span class="trainlead">merge train stopped —</span>`,
    // Reasons kept to their first sentence: the row must say enough to decide whether to look now,
    // and no more — the full story is one click away in the review pane.
    ...stops.map(c => `<button type="button" class="trainrepo" onclick="openReview(${esc(JSON.stringify(c.repo_id))})">${esc(c.repo_id)}: ${
      c.stopped.map(s => `#${esc(s.number)} (${esc(String(s.why || "needs you").split(".")[0])})`).join(" · ")
    }</button>`),
    // Stops with the engine switched off are a different sentence: nothing will retry these, or
    // move anything else, until somebody resumes. Known from payloads already fetched — this
    // banner never earns a request of its own.
    ...(revTrainPaused()
      ? [`<span class="trainlead">· and workflows are paused — nothing will act until resumed</span>`]
      : []),
  ].join("");
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
  const stale = (m.queues || []).filter(q => q.fresh === false);
  return {
    ai: m.ai, prs, blind_spots: blind, queues: m.queues || [],
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

// **Ask what is in flight.** Cheap by construction — `review::readings` is an in-memory map, no
// disk and no GitHub — which is what lets this run on a timer at all while the queue behind it
// stays on its 60s micro-cache (SKEIN-333).
//
// It reschedules ITSELF rather than running off a bare `setInterval`, so a pane nobody is looking
// at stops asking, and two opens cannot leave two timers running.
function revPollInFlight() {
  if (revInFlightTimer) { clearTimeout(revInFlightTimer); revInFlightTimer = null; }
  if (!view || view.mode !== "review") return;
  fetch("/api/review/reading")
    .then(r => r.ok ? r.json() : [])
    .then(list => {
      if (!view || view.mode !== "review") return;
      const was = revInFlight;
      revInFlight = new Map();
      for (const r of list || []) revInFlight.set(`${r.repo_id}#${r.number}`, r);
      // **The server has confirmed this reading is running**, which is what lets the branch below
      // settle a wait when it stops (SKEIN-366). Without this mark the two would race: a press
      // registers its own optimistic in-flight entry on the frame it happens, and a poll landing
      // before the server had begun the reading would find the key "gone" and settle a read that
      // has not started.
      for (const [key, w] of revReadWaits) if (revInFlight.has(key)) w.seen = true;
      // **A reading that was in flight and is not any more has LANDED.** That is the moment the row
      // has something new to say, and the moment the page must fetch what it says: the result went
      // to disk, not to the browser that asked, and a background read has no browser at all. Marked
      // rather than announced with a toast, because the reader may be three rows away.
      let landed = false;
      for (const [key, r] of was) {
        if (revInFlight.has(key)) continue;
        landed = true;
        // Asked BEFORE the delete below, and only then marked: the mark means a reading was
        // replaced, and `revSums` is the only thing that knows whether there was one to replace.
        // Dropping first and marking unconditionally is the same defect as the pump's, one line
        // apart — a row skein read for the first time is not a row that changed while you were
        // away.
        if (revHasReading(key)) revUpdated.add(key);
        // Drop what the page holds so the row re-reads it: the copy in `revSums` is the reading
        // this one REPLACED. Only for a row nobody is mid-sentence in — `revSums` is what an open
        // panel draws from.
        revSums.delete(key);
        const [repo, number] = [key.slice(0, key.lastIndexOf("#")), Number(key.slice(key.lastIndexOf("#") + 1))];
        revFetchHeld(repo, number);
        // **And if this page was waiting for that reading on the stream, it is not coming**
        // (SKEIN-366). The stream is one connection and a client that falls behind it is told, not
        // backfilled — so an event can be lost to a reconnect or a lagged subscriber, and a promise
        // nobody settles is a stack run that never advances and a button that never comes back.
        // `revFetchHeld` above is already fetching what the reading wrote, so this settles the
        // bookkeeping and deliberately writes nothing over it.
        //
        // Only for a wait the server was seen holding: an unseen one may simply not have begun.
        if (revReadWaits.get(key)?.seen) revReadSettle(key, null, "");
      }
      if (landed || was.size !== revInFlight.size) renderReview();
      tickInFlight();
    })
    .catch(() => {})
    .finally(() => {
      if (!view || view.mode !== "review") return;
      revInFlightTimer = setTimeout(revPollInFlight, REV_INFLIGHT_POLL_MS);
    });
}

// The reading skein has just finished, off disk. `held=1` is a request to REMEMBER — it can never
// spend a model call — which is what makes it safe to fire the moment a read lands, including a
// read the pump started that nobody asked for.
function revFetchHeld(repo, number) {
  const key = repo + "#" + number;
  fetch(`/api/repos/${encodeURIComponent(repo)}/review/${number}/summary?held=1`)
    .then(r => r.ok ? r.json() : null)
    .then(full => {
      if (!full) return;
      // Never over a reading that arrived while this was in flight, and never over a row that has
      // gone back into flight since — a fresh press outranks a disk read every time.
      if (revInFlight.has(key)) return;
      const now = revSums.get(key);
      if (now && now !== "…" && !now.thin) return;
      revSums.set(key, full);
      renderReviewNow();
    })
    .catch(() => {});
}

// **The counter ticks LOCALLY**, from the `started_ms` the server reported once — the owner's
// constraint, in their words: "for the timer I hope you are counting locally and doing github
// request once in a while only or on refresh."
//
// It rewrites text in place and never re-renders the pane. A row that repainted once a second
// would take the caret out of a half-typed comment beside it (§6 rule 2), which is the same reason
// `tickAges` works this way — and it carries its own class rather than reusing `.age`, because
// `tickAges` rewrites EVERY `.age` in the document from `data-secs` and a borrowed class is how
// the review rail's age column came to show "?" for ever (`tests/ui/rail.mjs` holds that line).
function tickInFlight() {
  for (const el of document.querySelectorAll(".revflight-secs")) {
    const started = Number(el.dataset.started || 0);
    if (!started) continue;
    el.textContent = revElapsed(Date.now() - started);
  }
}
setInterval(tickInFlight, 1000);

// Elapsed, spelled the way somebody waiting reads it: seconds until a minute, then minutes and
// seconds. Never "0s" — a reading that has just begun has still begun.
function revElapsed(ms) {
  const secs = Math.max(1, Math.round(ms / 1000));
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60), r = secs % 60;
  return r ? `${m}m ${r}s` : `${m}m`;
}

function loadReview(force) {
  // Whatever is already known, on screen before the request goes out.
  revQueue = revSeen.get("*") || null;
  // A load somebody ASKED for starts the chase again: pressing refresh after giving up should not
  // inherit the exhausted counter and give up immediately.
  if (force) revStaleTries = 0;
  // Whatever else this load does, start asking what is in flight — a reading somebody started
  // before this page existed is still running, and the row must say so (SKEIN-333).
  revPollInFlight();
  revLoading = true; renderReview();
  fetch(`/api/review${force ? "?force=1" : ""}`).then(async r => {
    const text = await r.text();
    if (!r.ok) throw new Error(text || `HTTP ${r.status}`);
    return JSON.parse(text);
  }).then(m => {
    revLoading = false;
    if (!revHeld) return;   // you left review while this was in flight
    const q = revMergeQueues(m);
    // A summary is keyed to a head commit; when the branch moves, the old one describes code that
    // is no longer there. Dropping it here is what makes a re-review actually get re-read.
    for (const pr of q.prs || []) {
      const held = revSums.get(rk(pr));
      if (!held || held === "…") continue;
      // A row that could not be REACHED is not a reading at all — asking again is the whole point,
      // and this is the moment to allow it.
      if (held.transient) { revSums.delete(rk(pr)); continue; }
      // A reading of a commit that is no longer the head is KEPT, and marked. It used to be deleted
      // here, which is what made the pump read the PR again on its own: a branch pushed to five
      // times was read five times and none of those readings survived to be compared against.
      //
      // Keeping it is only safe because the row says so loudly — "AI may only add scrutiny, never
      // remove it", and a stale reading shown as current would remove it. Re-reading is a click.
      if (held.head_sha !== pr.head_sha) held.stale = true;
    }
    revDecided.clear();   // the fresh rows carry their own truth; the in-place marks retire
    revQueue = q; revSeen.set("*", q); renderReview();
    // **What skein already knows, before anything is asked for.** One request per repo, no model
    // calls, no rules — see `loadKnownSummaries`. The pump runs after it, so it only ever asks for
    // readings that do not exist yet.
    for (const qq of q.queues) loadKnownSummaries(qq.repo_id);
    loadWorkflows();
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
    // **The remembered queue stays** (docs/review-ux.md §8.4, SKEIN-154). An old queue is worth
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
// absence (docs/review-ux.md §8.5, SKEIN-154).
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
        <button type="button" class="revchip" onclick="openSettings('repos')">Settings → Repos</button></div>`),
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
    }${why.fix ? `<button type="button" class="revchip" onclick="openSettings('repos')">Settings → Repos</button>` : ""}</div>` : ""}
    ${rows.length ? `<div class="revclear-next">${rows.join("")}</div>` : ""}
  </div>`;
}

// Why a PR is in your queue, in the words the filter uses.
function revReasonLabels(pr) {
  return (pr.reasons || []).map(r => typeof r === "string" ? r : (r.team ? `team ${r.team}` : "team"));
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
      body += `<div class="revblind">${blind.map(b =>
        `<div><span class="revblindwhat">incomplete</span> — ${esc(b)}</div>`).join("")}</div>`;
    }
    // 25 red rows is one broken pipeline, not twenty-five decisions: when red is most of the
    // queue it is said once, here, and the row's mark says whose move it is instead. Demoted,
    // not hidden — an opened row still reports its own checks. A lone failure stays row-level.
    const red = shown.filter(p => p.checks === "failing").length;
    if (shown.length >= 3 && red > shown.length / 3) {
      body += `<div class="cired">${red} of ${shown.length} are red — read it as one broken pipeline, not ${red} broken pull requests. Each row still says its own checks when opened.
        <button type="button" class="revchip" onclick="revSnoozeRed()" title="each returns on its own when its author pushes">set the red ones aside until they move</button></div>`;
    }
    // A chip must be true of a MINORITY of rows — the design's own test, applied at render time
    // because the model's tripwire flags turned out to fire on most rows ("behaviour" on 80% of a
    // live queue), and four chips per row squeezed every title into an ellipsis. A kind worn by
    // more than a third of the visible queue is texture, not signal: it leaves the collapsed rows
    // (the expanded brief still carries everything), and what survives is capped at two.
    revCommonChips = (() => {
      const freq = new Map();
      for (const p of shown) {
        const s2 = revSums.get(rk(p));
        const kinds = new Set((s2 && s2 !== "…" ? (s2.flags || []) : []));
        if (revMoved(p)) kinds.add("moved");
        for (const k of kinds) freq.set(k, (freq.get(k) || 0) + 1);
      }
      return new Set([...freq].filter(([, n]) => shown.length >= 3 && n > shown.length / 3).map(([k]) => k));
    })();
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
      // A stack takes the band of the step you would actually open, for the same reason it takes
      // that step's AGE: both are questions about the next decision, not about the tip. It used to
      // be a flat 1 — "unread" — which made the band incomparable across the two kinds of entry:
      // a stack whose next step skein had read and flagged sank below every loose row, and one
      // whose next step was routine floated above them. That also broke the order SKEIN-302 states,
      // where unstacked pull requests sort by age BETWEEN the stacks.
      const entries = [
        ...loose.map(p => ({ band: revReadBand(p), at: sortAt(p), key: rk(p), make: () => revRow(p) })),
        ...laneStacks.map(st => {
          const next = revStackNext(st) || st.steps[0];
          return { band: revReadBand(next), at: sortAt(next), key: revStackKey(st),
                   make: () => revStackRow(st) };
        }),
      ].sort((a, b) => (lane === "yours" && a.band !== b.band)
        ? a.band - b.band
        : lane === "yours"
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
      <button type="button" class="revchip${revEdit ? " on" : ""}" onclick="toggleWorkflowEditor()">workflows</button>
      ${revReadChip()}
      ${revQueue && revQueue.fresh === false
        ? `<span class="revage" title="These repos are showing the pull requests skein last read; it is fetching their current lists. The others are current — one slow repo does not stale the rest.">${
            esc((revQueue.staleRepos || []).map(r => `${r.repo_id} read ${revAgo(r.as_of)}`).join(", "))} · ${
            revStaleTries >= REV_STALE_TRIES
              ? `still fetching — <button type="button" class="revchip" onclick="loadReview(true)">try again</button>`
              : "refreshing"}</span>`
        : ""}
      <button type="button" class="revchip" onclick="loadReview(true)">${revLoading ? "refreshing…" : "refresh"}</button>
    </div>${revModsHtml()}${revEditHtml()}${body}</div>`;
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
    if (!d.ok) { toast(d.error || "could not write that note"); revModsPaint(); return; }
    toast(`note written for ${path}`);
    // The success arm paints through `loadModules`: what changes on screen is the row's state dot
    // and its stamp, and those are the server's answer rather than this one's, so re-reading them
    // IS the paint.
    loadModules();
  }).catch(e => { revWriting = ""; toast(String(e.message || e).split("\n")[0]); revModsPaint(); });
}

// The answer to a press on the notes panel, painted NOW — while the panel is still what the reader
// is looking at (SKEIN-422).
//
// Same defect and same shape as `revComposePaint` (SKEIN-416), which this is the third instance of:
// `writeModule` ended both its arms in `renderReview`, and §6 rule 2 holds that render while the
// pane owns a caret or a live selection. So "writing…" could sit on the chip after the note had
// landed, and the row's state dot stayed on the old answer until the reader's hands moved.
// `loadModules` is here too rather than only in the arm that calls it: `toggleMods` opens the panel
// on "looking at the repo…", and that sentence is replaced by the list on exactly the same render.
// That press already forces, so painting its answer takes no caret the press had not taken.
//
// **What "still on screen" means here, and why it is not the composer's test.** `revComposePaint`
// compares `revComposing === c` because a composer is an object a press captures. A module note has
// no such object — every chip in the panel is drawn from `revMods`, and the answer changes the
// whole panel — so the thing to ask about is the PANEL, and the panel is on screen when two things
// hold:
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

// Summaries are fetched for the lane you are working — Needs you — and nowhere else. A PR you have
// already decided on does not need explaining, and one you set aside explicitly does not either.
// Nothing is fetched at all when AI is off: the answer would be the same "unread" thirty times over,
// and the pane says it once instead.
// Every reading skein already holds for this repo, in one request.
//
// This is the fix for "I can only see 2 PRs with summaries while before there were a bunch". The
// pane used to discover an existing reading only by asking for one pull request at a time, through
// the same call that COMPUTES one — so every limit meant to bound spending also bounded remembering:
// a draft's reading was hidden, an unsettled branch's was hidden, a row past the sixth was hidden
// because the loop stops when the allowance is gone. None of those are reasons to forget something
// skein has already paid for.
//
// Reading from disk is free, so this asks for all of it at once and applies no rules. The pump then
// asks for what is MISSING, and keeps every limit it had.
function loadKnownSummaries(id) {
  // What this page knows about the disk is in flight again, so read-ahead waits for it (SKEIN-704).
  revKnownHeard.delete(id);
  // **`?rows=1`: the row shape.** A collapsed row draws the line, the flags, the depth and whether a
  // review is drafted — and the payload was sending the whole brief, the signals, the ownership and
  // the entire drafted review for every pull request in the repo. Measured on the owner's fleet
  // (SKEIN-287): 153,381 bytes for thirty-nine readings, reproduced locally at 155,167 against
  // 12,055 for the same set. The prose comes back when a row is opened, one row at a time, off disk
  // (`revLoadReading`).
  //
  // It is not only the bytes. That response occupies one of the browser's per-origin connections
  // for as long as it takes, so everything the reader presses meanwhile queues behind it — which is
  // how a four-millisecond disk read came to look like minutes (SKEIN-286).
  fetch(`/api/repos/${encodeURIComponent(id)}/review/summaries?rows=1`)
    .then(async r => { if (!r.ok) throw new Error(await r.text()); return r.json(); })
    .then(known => {
      // Recorded before the guard below, because it is a fact about the request rather than about
      // the pane: this load has heard what is on disk for this repo, whether or not anyone is
      // looking at the answer.
      revKnownHeard.add(id);
      if (!revHeld) return;
      for (const [number, s] of Object.entries(known || {})) {
        s.thin = true;   // the page's own mark: this reading knows less than the one on disk
        const key = id + "#" + Number(number);
        // A row that has already fetched its prose is never replaced by the thin shape for the same
        // head: the thin one knows strictly less about the same reading, and overwriting would
        // empty an expanded row behind the reader.
        // **A row being read RIGHT NOW is never written to.** This is the defect the owner reported
        // again and again as "reread doesn't produce a new review": the guards below were written
        // to skip rows holding real readings, and treated the in-flight marker as worthless data
        // safe to replace — so this refresh, which fires every few seconds, put the OLD reading
        // back four seconds into a thirty-five-second model call and left it there. The press was
        // working the whole time and had nothing on screen to show for it (SKEIN-333).
        //
        // First, and on its own line, because it is not a refinement of the two guards under it:
        // those ask "is what the page holds better than what disk says", and this asks a different
        // question — is skein still buying the answer.
        if (revInFlight.has(key)) continue;
        const has = revSums.get(key);
        if (has && has !== "…" && !has.thin && has.head_sha === s.head_sha) continue;
        // A reading the page already has for THIS head is not replaced, whole: it may be one that
        // has just landed from a forced re-read, and the copy on disk is the older answer.
        //
        // It used to be replaced in PART — the page's reading kept, the disk's `critique` and
        // `has_critique` grafted onto it — because `/review/:n/summary` answered a bare summary and
        // a row read by the pump therefore wore no chip however many reviews were drafted in the
        // same model call (SKEIN-243). The route answers `review::known_at` now (SKEIN-236), so
        // every reading carries the draft that was made with it and the graft has nothing left to
        // add. It is gone rather than left harmless, because two places deciding what "the draft at
        // THIS head" means is how they come to disagree.
        const held = revSums.get(key);
        if (held && held !== "…" && !held.stale && !held.transient) continue;
        // A reading replaced here is a DIFFERENT reading, and this merge cannot say which queue it
        // was built from — the fact rides on `review::ReadingDone`, and nothing on this route
        // carries it. So the note goes with the reading it was about rather than being inherited by
        // the one taking its place: silence means "not known", and a page that let a remembered
        // queue's mark slide onto somebody else's reading would be making the claim up.
        revReadFrom.delete(key);
        revSums.set(key, s);
      }
      renderReview();
      // Only now. Anything still missing is genuinely unread, which is what the limits are for.
      revPumpSummaries();
    })
    // A failure is an answer for this purpose: the page will not learn what is on disk by waiting
    // longer, and a pump held shut for ever is worse than one that reads a row a second time.
    .catch(() => { revKnownHeard.add(id); if (revHeld) revPumpSummaries(); });
}

// The prose behind a thinned row, fetched when the row is opened (SKEIN-287).
//
// **`?held=1`, which is a request to REMEMBER and never to analyse.** The route's default computes
// — it is the read button's route — and expanding a row must not be able to spend a model call, on
// any head, at any hour of the budget. `held=1` answers `review::held`, which is `review::known`
// for one pull request: the reading on disk, the drafted review beside it, and the `stale` mark
// when what skein holds is of an earlier commit. That last case is why this is not a plain
// re-fetch of the summary route — its cache lookup is keyed on the current head, so a deliberately
// kept older reading would come back unread.
//
// In flight the row keeps the thin reading it already has, so the line it was showing does not
// blink; it gains `waiting`, which is what `revDetail` draws its "…" from.
function revLoadReading(repo, number) {
  const key = repo + "#" + number;
  // A reading is being bought for this row; the copy on disk is the one it replaces (SKEIN-333).
  if (revInFlight.has(key)) return;
  const s = revSums.get(key);
  if (!s || s === "…" || !s.thin || s.waiting) return;
  s.waiting = true;
  fetch(`/api/repos/${encodeURIComponent(repo)}/review/${number}/summary?held=1`)
    .then(async r => { if (!r.ok) throw new Error(await r.text()); return r.json(); })
    .then(full => {
      if (!revHeld) return;
      // Only if the row has not moved on: a forced re-read landing while this was in flight is the
      // newer answer, and a reading off disk must never overwrite one somebody just paid for.
      const now = revSums.get(key);
      if (now && now !== "…" && !now.thin) return;
      revSums.set(key, full);
      renderReviewNow();
    })
    .catch(e => {
      if (!revHeld) return;
      const now = revSums.get(key);
      // The row keeps its line and stops claiming a fetch is coming. `waiting` is cleared rather
      // than left set, or the row would sit on "…" for ever with nothing to retry it: opening the
      // row again asks again.
      if (now && now !== "…") { now.waiting = false; now.prose_failed = String(e.message || e).split("\n")[0]; }
      renderReviewNow();
    });
}

// What workflows exist, and what each pull request's is doing. Cheap: it reads the cached queue and
// skein's own files, and touches GitHub only when the queue cache has aged out.
function loadWorkflows() {
  if (!revHeld || !revQueue) return;
  for (const qq of revQueue.queues || []) {
    const id = qq.repo_id;
    fetch(`/api/repos/${encodeURIComponent(id)}/workflows`)
      .then(async r => { if (!r.ok) throw new Error(await r.text()); return r.json(); })
      .then(w => {
        if (!revHeld) return;
        // The payload names the fleet switch too; keeping `revFlowsOn` on the freshest answer is
        // what lets the train panel and banner say "paused" without a request of their own.
        if (w.enabled !== undefined) revFlowsOn = !!w.enabled;
        revFlows.set(id, w);
        renderReview();
        // This payload is where `read_prs` arrives, and the pump reads nothing in a repo whose
        // consent it has not seen (`revReadsAhead`). Without this the ORDER decides: the queue and
        // the known readings land first, the pump runs against an empty `revFlows`, finds nothing
        // it is allowed to read, and never asks again — the whole feature switched off by a race.
        revPumpSummaries();
      })
      // A workflow file with a typo in it answers 502 here, and the row says so — see `revFlowBox`.
      .catch(e => { if (revHeld) { revFlows.set(id, { error: String(e.message || e).split("\n")[0] }); renderReview(); } });
  }
}

// Put a workflow on one pull request, take it off, or let the rules decide again.
function revSetWorkflow(repo, number, choice) {
  // Three answers, not two: a name, the empty string (excluded by hand — no rule may claim it), and
  // "let the rules decide", which forgets the choice. See `prwork::Carries`.
  const body = choice === "__rules" ? { unassign: true } : { name: choice };
  revWorkflowPost(repo, number, body);
}

// Let a stopped workflow run again. Deliberately not bundled with the picker: a person clearing a
// stop is saying "I fixed it", not "change what governs this".
function revClearStop(repo, number) { revWorkflowPost(repo, number, { clear_stop: true }); }

function revWorkflowPost(id, number, body) {
  if (!id) return;
  fetch(`/api/repos/${encodeURIComponent(id)}/review/${number}/workflow`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  })
    .then(r => r.json())
    .then(d => { if (!d.ok) toast(d.error || "that did not stick"); loadWorkflows(); })
    .catch(e => toast(String(e.message || e).split("\n")[0]));
}

// Whether skein reads this repo's pull requests on its own.
//
// Per repo, off until somebody says so, and shown where the queue is rather than in a settings page:
// this is the switch that decides whether skein spends model calls on its own, and the person
// deciding is looking at the queue it would spend them on.
//
// "On its own" covers BOTH readers — the ten-minute background pass and this pane's own pump — and
// nothing else. It used to cover only the background one, so the OFF branch of the sentence below
// was false: open the pane and every row was read anyway, and charged (SKEIN-242). It is one rule
// now, enforced at the model call (`review::unasked_scope`), and the sentence is true again.
// The repos this view is speaking about, and which of them have answered about read-ahead.
//
// `read_prs` is per-repo consent and stays per-repo. What was wrong (SKEIN-282) was not the shape
// of the consent but the shape of the CONTROL: it only existed in a view narrowed to one repo, so
// on the merged queue — where every repo on the owner's fleet had it off — the pane showed a queue
// nothing would ever be read in and offered nothing that would change that, unless you already
// knew to narrow first. A control you have to know about before you can find it is a control the
// person who needs it does not have.
function revReadScope() {
  const one = revScopeRepo();
  const ids = one ? [one] : ((revQueue && revQueue.queues) || []).map(q => q.repo_id);
  const known = ids.filter(id => (revFlows.get(id) || {}).read_prs !== undefined);
  return { known, on: known.filter(id => revFlows.get(id).read_prs) };
}

function revReadChip() {
  const { known, on } = revReadScope();
  if (!known.length) return "";
  const all = on.length === known.length;
  // One repo — the narrowed view, and the sentence can name it.
  if (known.length === 1) {
    return `<button type="button" class="revchip${all ? " on" : ""}"
      title="${all
        ? "skein reads this repo on its own: the pull requests somebody asked you to review, and the ones you opened. One unit of the day's budget each, and nothing else here is read."
        : "skein reads nothing here on its own. Press read it on any row — a read you ask for is never refused."}"
      onclick="revSetReadingFor(${esc(JSON.stringify(known[0]))}, ${!all})">read ahead${all ? " · on" : ""}</button>`;
  }
  // The whole fleet. The COUNT is the point: "0 of 9" is the pane saying, on the screen you are
  // already looking at, that nothing here will ever be read on its own — which is the thing the
  // narrowed-only chip could not say. It is still per-repo consent underneath; this presses it
  // once for each, and says how many it is about to change.
  const off = known.length - on.length;
  return `<button type="button" class="revchip${all ? " on" : ""}"
    title="${all
      ? `skein reads all ${known.length} of these repos on its own: the pull requests somebody asked you to review, and the ones you opened, one unit of the day's budget each. Press to switch every one off.`
      : `skein reads nothing on its own in ${off} of these ${known.length} repos, so those rows stay unread however long you leave this open. Press to switch read-ahead on for all ${known.length} — the pull requests somebody asked you to review, and the ones you opened, one unit of the day's budget each. A read you ask for is never counted.`}"
    onclick="revSetReadingAll(${!all})">read ahead · ${on.length} of ${known.length}</button>`;
}

// **One repo, and the only place this consent is posted.** Every affordance for it — the head
// chip in either form, and the unread row's own sentence — is a caller, so there is one rule for
// what switching it on means and one place a refusal is reported from.
//
// It takes the repo rather than reading the view, which is what let the control exist only where
// the view happened to be narrowed (SKEIN-282). A row knows its own repo; the head chip knows the
// ones it is counting.
function revSetReadingFor(id, on) {
  if (!id) return Promise.resolve(false);
  return fetch(`/api/repos/${encodeURIComponent(id)}/reading`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ on }),
  })
    .then(r => r.json())
    .then(d => {
      if (!d.ok) { toast(d.error || "that did not stick"); return false; }
      toast(on
        ? `skein will read the pull requests waiting on you in ${id}, and the ones you opened`
        : `skein will not read anything in ${id} unless you ask`);
      // `loadWorkflows` carries the new answer back, and pumps: switching read-ahead ON fills the
      // repo in from the pane you are looking at, rather than at the next background tick.
      loadWorkflows();
      return true;
    })
    .catch(e => { toast(String(e.message || e).split("\n")[0]); return false; });
}

// Every repo the merged queue is showing, in one press.
//
// Not a fleet-wide setting: it is the per-repo consent, posted once per repo, and the pane says so
// by counting. A repo that refuses is reported by `revSetReadingFor` on its own, and the ones that
// took still took — a partial answer is the honest one here, and the chip's own count is what says
// where it got to.
function revSetReadingAll(on) {
  const { known } = revReadScope();
  const want = known.filter(id => !!(revFlows.get(id) || {}).read_prs !== on);
  if (!want.length) return;
  toast(on
    ? `switching read-ahead on for ${want.length} repo${want.length === 1 ? "" : "s"}`
    : `switching read-ahead off for ${want.length} repo${want.length === 1 ? "" : "s"}`);
  for (const id of want) revSetReadingFor(id, on);
}

// ---------- the workflow editor ----------
//
// Workflows merge pull requests, so the thing that writes one has two jobs: make the vocabulary
// visible — every word skein knows, offered rather than remembered — and make it impossible to
// build a workflow the parser will refuse. Both come from the same tables the parser reads, served
// by `/api/workflows`, because a picker with its own idea of the vocabulary is a workflow somebody
// builds in the UI and cannot save.
let revEdit = null;        // { workflow: [...] } while open, null while closed
let revVocab = null;       // { conditions: [Word], actions: [Word] }
let revEditSaying = "";    // the server's last refusal, shown as it came

function toggleWorkflowEditor() {
  if (revEdit) { revEdit = null; renderReview(); return; }
  fetch("/api/workflows")
    .then(r => r.json())
    .then(d => {
      revVocab = { conditions: d.conditions || [], actions: d.actions || [] };
      // A file skein cannot read is NOT loaded into the editor as an empty list. Somebody who has
      // not seen what is there cannot mean to replace it, and saving would do exactly that.
      revEditSaying = d.error || "";
      revEdit = d.error ? null : { workflow: d.workflow || [] };
      if (d.error) toast("the workflow file has a problem — fix it before editing here");
      renderReview();
    })
    .catch(e => toast(String(e.message || e).split("\n")[0]));
}

// Every word, from the server, so the page never invents one.
const revWords = kind => (revVocab && revVocab[kind]) || [];

// One condition or action, as a kind and — where the word takes one — its argument.
//
// `where` is the path to the thing being edited: the workflow, the step (-1 for the workflow's own
// rule) and the position (-1 for a step's action). Passing a path rather than binding a closure is
// what lets these be plain inline handlers, which is how the rest of this page is written.
function revAtom(kind, value, where) {
  const words = revWords(kind);
  const [head, ...rest] = String(value || "").split(":");
  const arg = rest.join(":");
  // A word whose argument is part of the word itself (`update-branch:rebase`) is offered whole.
  const whole = words.filter(w => !w.arg && w.spelling.includes(":"));
  const simple = words.filter(w => !w.arg && !w.spelling.includes(":"));
  const taking = words.filter(w => w.arg);
  const opt = (v, label, on) => `<option value="${esc(v)}"${on ? " selected" : ""}>${esc(label)}</option>`;
  const chosen = whole.find(w => w.spelling === value) ? value : head;
  const picker = `<select aria-label="${kind === "conditions" ? "Condition" : "Action"}"
      onchange="revEditWord(${esc(JSON.stringify(where))}, this.value)">${
    [...simple.map(w => opt(w.kind, w.kind, chosen === w.kind)),
     ...whole.map(w => opt(w.spelling, w.spelling, chosen === w.spelling)),
     ...taking.map(w => opt(w.kind, `${w.kind}:…`, chosen === w.kind))].join("")
  }</select>`;
  const word = taking.find(w => w.kind === head);
  if (!word) return picker;
  // A closed set is offered as a set. Free text gets a box with the argument's name in it, so
  // `add-label:` shows "name" rather than an empty rectangle nobody knows what to type into.
  const box = word.choices && word.choices.length
    ? `<select aria-label="Value" onchange="revEditArg(${esc(JSON.stringify(where))}, this.value)">${
        word.choices.map(c => opt(c, c, arg === c)).join("")}</select>`
    : `<input aria-label="Value" placeholder="${esc(word.arg)}" value="${esc(arg)}"
         onchange="revEditArg(${esc(JSON.stringify(where))}, this.value)">`;
  return picker + box;
}

// Where in the file a path points. `w.s.c` — workflow, step (-1 = the workflow's own rule),
// condition (-1 = the step's action).
function revAt(where) {
  const [w, st, c] = String(where).split(".").map(Number);
  const flow = revEdit.workflow[w];
  return { flow, st, c, list: st < 0 ? flow.matches : flow.steps[st].when };
}

function revEditWord(where, kind) {
  const { flow, st, c, list } = revAt(where);
  // Changing the kind drops the old argument rather than carrying it: `label:ci` becoming
  // `checks:ci` is not a thing anybody meant, and it would not parse.
  const word = revWords(c < 0 ? "actions" : "conditions").find(x => x.kind === kind || x.spelling === kind);
  const next = word && word.arg
    ? `${word.kind}:${(word.choices && word.choices[0]) || ""}`
    : (word ? word.spelling.replace(/<.*>/, "") : kind);
  if (c < 0) flow.steps[st].do = next; else list[c] = next;
  renderReview();
}

// **No re-render.** Changing a value does not change the SHAPE of the form: the box already shows
// what was typed, and redrawing it would throw away the caret — and, worse, lose the very click that
// caused the blur that fired this. That is not hypothetical: it is why the first attempt at "type a
// name, then press + step" added no step. Only structural changes redraw.
function revEditArg(where, arg) {
  const { flow, st, c, list } = revAt(where);
  const at = c < 0 ? flow.steps[st].do : list[c];
  const head = String(at).split(":")[0];
  const next = `${head}:${arg}`;
  if (c < 0) flow.steps[st].do = next; else list[c] = next;
}

function revEditAddCond(w, st) {
  const flow = revEdit.workflow[w];
  (st < 0 ? flow.matches : flow.steps[st].when).push("approved");
  renderReview();
}
function revEditDelCond(w, st, c) {
  const flow = revEdit.workflow[w];
  (st < 0 ? flow.matches : flow.steps[st].when).splice(c, 1);
  renderReview();
}
function revEditAddStep(w) {
  revEdit.workflow[w].steps.push({ when: ["approved"], do: "flag:look at this" });
  renderReview();
}
function revEditDelStep(w, st) { revEdit.workflow[w].steps.splice(st, 1); renderReview(); }
// Order is not decoration: the FIRST step that applies is the one that happens, so moving a step is
// changing what the workflow does.
function revEditMoveStep(w, st, by) {
  const steps = revEdit.workflow[w].steps;
  const to = st + by;
  if (to < 0 || to >= steps.length) return;
  [steps[st], steps[to]] = [steps[to], steps[st]];
  renderReview();
}
function revEditAddFlow() {
  revEdit.workflow.push({ name: `workflow-${revEdit.workflow.length + 1}`, matches: [], steps: [] });
  renderReview();
}
// One at a time — `Workflow::serial` (SKEIN-248). No initialiser beside it in `revEditAddFlow`, and
// that is deliberate rather than an omission: `WrittenFlow::serial` is `#[serde(default)]` and is
// always serialised, so a third statement of "a new workflow is not a train" would be a fact kept
// in three places and testable in none. What was missing was never the field — it was this.
function revEditSerial(w, on) { revEdit.workflow[w].serial = !!on; renderReview(); }
function revEditDelFlow(w) { revEdit.workflow.splice(w, 1); renderReview(); }
// Same: the input holds the name, so redrawing it would only take the caret away — see `revEditArg`.
function revEditName(w, name) { revEdit.workflow[w].name = name; }

function revEditSave() {
  fetch("/api/workflows", {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ workflow: revEdit.workflow }),
  })
    .then(r => r.json())
    .then(d => {
      revEditSaying = d.ok ? "" : (d.error || "that did not save");
      if (d.ok) {
        revEdit = { workflow: d.workflow || [] };   // what skein will read back, not what was sent
        toast("workflows saved");
        loadWorkflows();
      }
      renderReview();
    })
    .catch(e => toast(String(e.message || e).split("\n")[0]));
}

function revEditHtml() {
  if (!revEdit) {
    return revEditSaying
      ? `<div class="revmods"><div class="revfail"><b>the workflow file could not be read</b>
          <div>${esc(revEditSaying)}</div>
          <div class="dim">Nothing is running, and this editor will not save over a file it could not show you.</div>
        </div></div>`
      : "";
  }
  const flows = revEdit.workflow.map((flow, w) => {
    const rule = (flow.matches || []).map((c, i) => `<span class="revstep-atom">${revAtom("conditions", c, `${w}.-1.${i}`)}
        <button type="button" class="revx" title="remove" onclick="revEditDelCond(${w}, -1, ${i})">×</button></span>`).join("");
    const steps = (flow.steps || []).map((st, i) => `<div class="revstep">
        <span class="revstep-n">${i + 1}</span>
        <div class="revstep-body">
          <div class="revstep-line"><span class="revstep-lbl">when</span>${
            (st.when || []).map((c, ci) => `<span class="revstep-atom">${revAtom("conditions", c, `${w}.${i}.${ci}`)}
              <button type="button" class="revx" title="remove" onclick="revEditDelCond(${w}, ${i}, ${ci})">×</button></span>`).join("")
          }<button type="button" class="revchip" onclick="revEditAddCond(${w}, ${i})">+ and</button>
          ${(st.when || []).length ? "" : `<span class="dim">anything</span>`}</div>
          <div class="revstep-line"><span class="revstep-lbl">do</span>${revAtom("actions", st.do, `${w}.${i}.-1`)}</div>
        </div>
        <div class="revstep-move">
          <button type="button" class="revx" title="earlier" onclick="revEditMoveStep(${w}, ${i}, -1)">↑</button>
          <button type="button" class="revx" title="later" onclick="revEditMoveStep(${w}, ${i}, 1)">↓</button>
          <button type="button" class="revx" title="remove step" onclick="revEditDelStep(${w}, ${i})">×</button>
        </div>
      </div>`).join("");
    // **One at a time** — `Workflow::serial`, and the whole of what makes a merge train a train
    // (SKEIN-248). It rode the editor's payload and had no control anywhere in the page, so a train
    // could not be built here, or un-set: existing ones survived only because the editor round-trips
    // the object opaquely. Named for what it DOES rather than for the field, because "serial" is
    // the file's word and "one at a time" is the reader's.
    const serial = !!flow.serial;
    return `<div class="revflow-edit">
      <div class="revflow-edit-head">
        <input aria-label="Workflow name" value="${esc(flow.name || "")}" onchange="revEditName(${w}, this.value)">
        <button type="button" class="revchip${serial ? " on" : ""}" onclick="revEditSerial(${w}, ${!serial})"
          title="${serial
            ? "a merge train: this workflow's pull requests act oldest-first, one per pass, and everyone behind the front waits. A stopped front is passed over rather than blocking the rest."
            : "every pull request carrying this workflow acts in the same pass — which means each merge re-runs CI on all of its siblings. Switch on for a merge train: oldest first, one at a time."}"
          >one at a time${serial ? " · on" : ""}</button>
        <span class="revspacer"></span>
        <button type="button" class="revchip" onclick="revEditDelFlow(${w})">delete workflow</button>
      </div>
      <div class="revstep-line"><span class="revstep-lbl">runs on</span>${rule}
        <button type="button" class="revchip" onclick="revEditAddCond(${w}, -1)">+ rule</button>
        ${(flow.matches || []).length ? "" : `<span class="dim">nothing on its own — assign it to a pull request</span>`}</div>
      ${steps}
      <button type="button" class="revchip" onclick="revEditAddStep(${w})">+ step</button>
    </div>`;
  }).join("");
  // Said where the steps are, not in a settings page: the first step that applies is the one that
  // happens, and somebody dragging a step up is changing the workflow's meaning.
  //
  // The train panel rides at the top of the same pane: the person editing what the train DOES and
  // the person watching what it IS DOING are the same person, arriving by the same button.
  return `<div class="revmods">
    ${revTrainHtml()}
    <div class="dim">The first step whose conditions all hold is the one that happens, one per pass.</div>
    ${flows}
    ${revEditSaying ? `<div class="revfail"><b>not saved</b><div>${esc(revEditSaying)}</div></div>` : ""}
    <div class="revflow-edit-foot">
      <button type="button" class="revchip" onclick="revEditAddFlow()">+ workflow</button>
      <span class="revspacer"></span>
      <button type="button" class="revchip go" onclick="revEditSave()">save</button>
    </div>
  </div>`;
}

