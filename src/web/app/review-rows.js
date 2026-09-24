// ---------- the merge train, watched ----------
//
// The owner is automating merges and asked for exactly two things: "I need to know exactly what is
// it working on, which step is it on, status of previous steps" — and "I should be able to pause it
// or resume it at any point". So the workflows pane opens with this panel: the engine's one switch
// with its one button, and per repo each serial workflow's train — the front PR with its step
// spelled out, the whole line in train order, every stop with its reason and the same
// let-it-run-again button the row has, and each PR's journal of what already happened.
//
// Drawn from what the pane already fetches: `revFlows`, one GET /api/repos/:id/workflows per repo,
// whose payload a new-enough server extends with `trains` per repo and `journal` per PR. An older
// server sends neither, and the panel says what standing alone can — no crash, no invented front.
let revTrainOpen = new Set();   // "repo#number" whose journal timeline is unfolded in the panel
// The fleet's workflow switch as the server last SAID it — from GET /api/workflows after a
// pause/resume, or from the per-repo payloads as they land. null = never heard. The pause button
// paints from this, never from its own click.
let revFlowsOn = null;

// Is the switch known to be off? Read from what is already on hand, never a request of its own:
// the train banner asks this on every counts poll.
function revTrainPaused() {
  if (revFlowsOn !== null) return !revFlowsOn;
  for (const f of revFlows.values()) if (f && f.enabled !== undefined) return f.enabled === false;
  return false;
}

// Pause or resume everything. One fleet-wide switch (`pr_workflows`) rather than per-train levers:
// the engine re-reads it every sweep and there is no mid-step to interrupt, so flipping it takes
// effect at once. The POST carries ONLY the flipped field — /api/settings merges — and the repaint
// comes from re-reading GET /api/workflows: the panel shows what the server says the switch is,
// never what the click hoped.
function revTrainToggle(on) {
  fetch("/api/settings", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ pr_workflows: !!on }),
  })
    .then(async r => { if (!r.ok) throw new Error(await r.text()); return r.json(); })
    .then(() => fetch("/api/workflows"))
    .then(r => r.json())
    .then(d => {
      revFlowsOn = !!d.enabled;
      toast(revFlowsOn ? "workflows resumed" : "workflows paused — nothing will act");
      loadWorkflows();          // the per-repo payloads carry `enabled` too; keep them agreeing
      renderReview();
    })
    .catch(e => toast(String(e.message || e).split("\n")[0]));
}

// A number in the panel is one click into that pull request's row in the queue — the same open the
// banner's repo click uses, plus the row expansion the row's own click would do.
function revTrainGo(id, number) {
  openReview(id);
  const key = id + "#" + number;
  if (!revOpen.has(key)) toggleRevRow(key);
}

function revTrainLog(key) {
  if (!revTrainOpen.delete(key)) revTrainOpen.add(key);
  renderReviewNow();
}

// `#123`, clickable. Same esc-into-onclick pattern as the banner's repo button.
function revTrainNumBtn(id, n) {
  return `<button type="button" class="trainnum" onclick="revTrainGo(${esc(JSON.stringify(id))}, ${Number(n)})" title="open #${Number(n)} in the queue">#${Number(n)}</button>`;
}

// The queue's title for a PR, when the queue has it loaded; the panel must not fetch for one.
function revTrainTitle(id, n) {
  const pr = ((revQueue && revQueue.prs) || []).find(p => p.repo_id === id && p.number === Number(n));
  return (pr && pr.title) || "";
}

// When, in the queue's own relative vocabulary — `revAge` is the pane's age helper, and the
// journal's epoch stamp only needs dressing as an ISO date to use it.
function revTrainWhen(ms) {
  return revAge(new Date(ms || 0).toISOString()).label || "now";
}

// One pull request's timeline, oldest first exactly as the server sends it. A "cleared" entry is a
// person's act — the hand that let the train move again — and reads as one.
function revTrainJournal(log) {
  if (!log || !log.length) return `<div class="revtrain-log"><span class="dim">no history recorded yet</span></div>`;
  return `<div class="revtrain-log">${log.map(e => `<div class="${esc(e.kind)}">
      <span class="when">${esc(revTrainWhen(e.at_ms))}</span>
      <span class="kind">${esc(e.kind === "cleared" ? "cleared by hand" : e.kind)}</span>
      <span>${esc(e.what || "")}</span>
    </div>`).join("")}</div>`;
}

// An older server sends no `trains`. Standing still says which pull requests each workflow carries,
// and the train's own order is lowest-number-first — so the panel degrades to that list, claiming
// no front it was not told (no `front` key at all, which is how `revTrainLine` knows not to guess).
function revTrainsFromStanding(prs) {
  const byFlow = new Map();
  for (const [n, st] of Object.entries(prs || {})) {
    if (!st || !st.workflow) continue;
    // A HELD pull request keeps its `workflow` on purpose — the row must still say which workflow
    // somebody chose — and is deliberately excluded from ACTING (`Carries::acting` against
    // `Carries::name`), so the tick skips it. Drawn as a car it would be a promise of an act that
    // will never be taken, and if it held the lowest number it would be drawn as the front while
    // the tick's front was somebody else (SKEIN-279/326). This is the page's own fallback for a
    // server that sends no `trains`; the route makes the same exclusion.
    if (st.holding) continue;
    if (!byFlow.has(st.workflow)) byFlow.set(st.workflow, []);
    byFlow.get(st.workflow).push(Number(n));
  }
  return [...byFlow.entries()].map(([flow, line]) => ({
    flow,
    line: line.sort((a, b) => a - b),
    stopped: line.filter(n => (prs[n] || {}).stopped).map(n => ({ number: n, why: prs[n].stopped })),
  }));
}

// One train: what it is working on NOW, spelled out, then every car in order.
function revTrainLine(id, tr, f) {
  const prs = f.prs || {};
  const st = n => prs[n] || { workflow: "", how: "", next: "", step: 0, stopped: "" };
  const journalOf = n => st(n).journal || (f.journals || {})[n];
  const front = "front" in tr ? tr.front : undefined;
  const fs = front == null ? null : st(front);
  const title = n => { const s = revTrainTitle(id, n); return s ? `<span class="revtrain-title">${esc(s)}</span>` : ""; };
  // Three honest now-lines: a front (its step, from the same standing the row shows), a train
  // whose every PR is stopped (the server said `front: null`), and an older server that never said
  // (no `front` key — the panel claims nothing it was not told).
  const now = front === undefined
    ? ""
    : front === null
      ? `<div class="revtrain-now dim">nobody can move — the line is empty or everyone in it is stopped</div>`
      : `<div class="revtrain-now">now: ${revTrainNumBtn(id, front)} ${title(front)} — ${
          fs.next ? `step ${fs.step} — <code>${esc(fs.next)}</code>` : `waiting — nothing applies right now`}</div>`;
  const stopsBy = new Map((tr.stopped || []).map(s => [Number(s.number), s.why]));
  const cars = (tr.line || []).map((n, i) => {
    const s = st(n);
    const key = id + "#" + n;
    const why = stopsBy.has(Number(n)) ? stopsBy.get(Number(n)) : s.stopped;
    const doing = why ? `stopped — ${why}` : s.next ? String(s.next).split(":")[0] : "waiting";
    const open = revTrainOpen.has(key);
    return `<div class="revtrain-car${n === front ? " front" : ""}${why ? " stopped" : ""}">
        <span class="revtrain-pos">${i + 1}</span>
        ${revTrainNumBtn(id, n)}${n === front ? `<span class="revtag flow">front</span>` : ""}
        ${title(n)}
        <span class="revtrain-doing">${esc(doing)}</span>
        ${why ? `<button type="button" class="revchip" onclick="revClearStop(${esc(JSON.stringify(id))}, ${Number(n)})">let it run again</button>` : ""}
        <button type="button" class="revchip${open ? " on" : ""}" onclick="revTrainLog(${esc(JSON.stringify(key))})">history</button>
      </div>${open ? revTrainJournal(journalOf(n)) : ""}`;
  }).join("");
  return `<div class="revtrain-flow">${esc(tr.flow)}</div>${now}${cars}`;
}

// One repo's trains. Nothing to show — no serial workflow carrying anything — is no section.
function revTrainRepo(id, f) {
  const trains = (f.trains !== undefined ? f.trains || [] : revTrainsFromStanding(f.prs))
    .filter(tr => (tr.line || []).length);
  if (!trains.length) return "";
  return `<div class="revtrain-repo"><h5>${esc(id)}</h5>${trains.map(tr => revTrainLine(id, tr, f)).join("")}</div>`;
}

// The panel itself. Empty when no repo has a train to show — the editor is then the whole pane.
function revTrainHtml() {
  const sections = [...revFlows.entries()]
    .filter(([, f]) => f && !f.error)
    .map(([id, f]) => revTrainRepo(id, f))
    .filter(Boolean);
  if (!sections.length) return "";
  const paused = revTrainPaused();
  return `<div class="revtrain${paused ? " off" : ""}">
    <div class="revtrain-state">
      <span>merge train — ${paused ? "paused" : "running"}</span>
      <button type="button" class="revchip${paused ? " go" : ""}" onclick="revTrainToggle(${paused})">${paused ? "resume" : "pause"}</button>
      ${paused ? `<span class="revtrain-off">paused — nothing will act</span>` : ""}
    </div>
    <div class="revtrain-body">${sections.join("")}</div>
  </div>`;
}

// What this pull request's workflow is, in one word for the collapsed line.
function revFlowChip(pr) {
  const flows = revFlows.get(pr.repo_id);
  const st = flows && flows.prs && flows.prs[pr.number];
  if (!st || !st.workflow) return "";
  if (st.stopped) {
    return `<span class="revtag stopped" title="${esc(st.stopped)}">${esc(st.workflow)} · stopped</span>`;
  }
  // The verb only — "add-label", "merge" — because the row is scanned and the argument is detail.
  const doing = st.next ? String(st.next).split(":")[0] : "";
  // A held workflow has no next step and is not idle either (SKEIN-326): "nothing to do right now"
  // on the hover of a pull request whose workflow is waiting on its own conditions is the same
  // wrong sentence the expanded row used to print, one surface up.
  const title = st.next ? `step ${st.step}: ${st.next}` : st.holding || "nothing to do right now";
  return `<span class="revtag flow" title="${esc(title)}">${esc(st.workflow)}${doing ? ` · ${esc(doing)}` : ""}</span>`;
}

// Does skein read this repo's pull requests on its own? The owner's per-repo consent — off until
// they switch it on, the "read ahead" chip beside the queue (`revReadChip`), `repos::Repo::read_prs`
// on disk — carried on the workflows payload the pane already fetches for every repo in the queue
// (`loadWorkflows`, answered by `api_workflows` in `src/bin/skein-server/workflows.rs`).
//
// Not answered yet, or the fetch failed, reads as OFF. The server refuses either way
// (`review::unasked_scope`), so asking would buy nothing but a refusal painted on every row; and
// `loadWorkflows` pumps again the moment an answer lands, so "off" here is never the last word.
function revReadsAhead(id) {
  const flows = revFlows.get(id);
  return !!(flows && flows.read_prs);
}

// Is this a pull request skein reads on ITS OWN? The page's copy of `review::worth_reading` over
// `review::worth_a_visit`, and the server is the authority: it asks the same question at the model
// call, so a copy that drifts can only make the pane ask for a refusal — never spend anything the
// server would not have spent.
//
// Two ways in, and being **mentioned** is not one of them — somebody talking about you is not a
// request to read (SKEIN-242, which is how a mention came to cost the day's budget):
//
//   * somebody asked you to review it, personally or through a team, and it is your move;
//   * **you opened it**, which puts it in the waiting lane and nowhere else (the `author == login`
//     arm of the lane `if` in `build_pr`, `src/prq/node.rs`). This is SKEIN-277: the server has
//     read and drafted authored rows since f69c611, on ONE merged model call, but the pane's own
//     pump still said `lane === "needs-you"` — so the owner's own stack filled in only when the
//     ten-minute background tick reached it, never from having the pane open in front of them.
//
// **Drafts are never read unasked**, whichever lane and whoever wrote them. A draft is the author
// saying it is not finished, and spending a model call to describe something nobody has proposed
// yet is the clearest case of work that was not asked for. The button in the expanded row still
// reads one on request — the rule is about what happens on its own, not about what you can have.
function revSkeinsToRead(p) {
  if (p.draft) return false;
  const rs = new Set((p.reasons || []).map(r => typeof r === "string" ? r : "team"));
  const yours = rs.has("author");
  const asked = rs.has("reviewer") || rs.has("reviewed") || rs.has("team");
  if (p.lane === "needs-you") return asked || yours;
  if (p.lane === "waiting") return yours;
  // not-ready is its author still changing the answer; archived is you saying it will not move.
  return false;
}

// Fill in the readings skein is allowed to make on its own.
//
// **The whole rule, in one sentence: if you pressed it, it is free and unconditional; if skein
// decided to read it, that happens only in a repo you switched read-ahead on for, and it is
// counted against the day.** This pump is the second half of that sentence — it is skein's
// initiative, not yours, however present you are — so it obeys the same scope and pays from the
// same ledger as the ten-minute background pass. Anything you touch is the first half: expanding a
// row (`toggleRevRow`, `asked`), "read it" and "re-read" (`force`), none of them scoped and none of
// them counted.
function revPumpSummaries() {
  if (!revQueue || !revQueue.ai) return;
  // `revKnownHeard` first: skein reads ahead only where it has already heard what it holds
  // (SKEIN-704). Nothing is lost by waiting: `loadKnownSummaries` pumps the moment its answer
  // lands, so a row that really is unread is asked for in the same turn as it was before.
  const want = (revQueue.prs || [])
    .filter(p => revKnownHeard.has(p.repo_id) && revReadsAhead(p.repo_id)
                 && revSkeinsToRead(p) && !revSums.has(rk(p)));
  // Three at a time, and no total: the total is the server's, spent where the model call is
  // (`review::over_budget`), and a row it refuses comes back saying so with a button — which is
  // worth vastly more than a row the page silently never asked about.
  while (revSumBusy < REV_SUM_PARALLEL && want.length) {
    const p = want.shift();
    revFetchSummary(p.repo_id, p.number);
  }
}

// Ask for a reading. `how` says WHO is asking, which is the whole boundary the day's budget draws
// (`review::Trigger`): "force" is a person pressing re-read — ignore the cached answer AND do not
// budget it; "asked" is a person revealing they want this one — a cache hit is still welcome, but
// the ceiling on skein's own initiative does not apply, per the owner: "Limit is only for automatic
// stuff, manually I can invoke as many as I want." Anything else is the pump, which pays.
//
// The marker travels on the URL because the server defaults the safe way round: a request without
// it is treated as unasked, so forgetting it gates a button rather than un-gating a sweep.
function revFetchSummary(id, number, how) {
  // The repo the ROW belongs to, passed by the row itself — never the one the view happens to
  // name. The merged queue holds every repo at once, so there is no "current repo" to fall back
  // on, which is what used to send the pump to `/api/repos/undefined/…`.
  if (!id || !revHeld) return;
  // "here" is "read it here instead" (SKEIN-818): a forced read, outside the pull request's box.
  const force = how === "force" || how === true || how === "here";
  // **`redraft=1`: read it again, and draft a NEW review from that reading** (SKEIN-293, the
  // owner's "go with one control"). It implies a forced read — a cached reading returns from
  // `review::visit` before anything is drafted, so a redraft honouring the cache would be a press
  // that did nothing — and it outranks `held=1`, so a press that asked for a new reading is never
  // quietly downgraded into a disk read.
  //
  // **Nothing is warned about first, and this sentence used to say there was** (SKEIN-564): it read
  // "a reader who has just been warned and said yes", which is a claim about a SAFETY interaction —
  // we ask before destroying your work — and the page contradicted it in the same file.
  // `revReadAgainPress` has no confirm step and says why it does not: skein stores no review, so a
  // re-read has nothing of the reader's to throw away. A confirmation described but never shown is
  // worse than one that was never promised, because it is trusted.
  //
  // This is where "re-read" stopped being the weaker of two buttons. On a row that already had a
  // draft it used to fall through to the cheap two-stage summary path — different prompt, weaker
  // model — because a draft check said no to a head it had already drafted, while "review the
  // code" was one merged call on the stronger model. Same press, two qualities of reading, and
  // neither label said so. Both the check and the drafting are gone (skein stores no review), so
  // there is one press buying one reading; `?redraft=1` is what is left of the distinction, and it
  // now means only `review::Review::Always` — read it even where skein would not read unasked.
  const q = how === "here" ? "?redraft=1&here=1" : force ? "?redraft=1" : how === "asked" ? "?asked=1" : "";
  const key = id + "#" + number;
  // **A second read of the same row, started before the first landed** (SKEIN-366). `revReadWaits`
  // is keyed by pull request, so the entry about to be written would replace an outstanding one and
  // leave its promise unresolved for ever — and `revStackPump` books a step in that promise's
  // `.finally`, so an orphan is a stack run that stops advancing with a "stop" control that never
  // goes away. Settled here rather than refused: the press still buys the reading it asked for,
  // exactly as it did when each read carried its own request. Writes nothing over `revSums`, so the
  // question two lines below still has the reading it is asking about.
  revReadSettle(key, null, "");
  // Was there a reading here to REPLACE? Read now, because the next line writes "…" over it — and
  // the answer is the whole meaning of the "● updated" mark this request will leave behind
  // (`revHasReading`).
  const replacing = revHasReading(key);
  // How long this one takes, so the stack read can say how long the REST will (SKEIN-337). Measured
  // rather than assumed: the only honest source for "~10m" is what reads have actually cost on this
  // fleet, on this model, today.
  //
  // **What counts as a measurement is `computed`** — the server's own word for "a model call
  // happened" (`review::Summary::computed`) — and not `force`, which is who asked. Those came apart
  // in both directions and each way was wrong. A first press of "read all 18" on a fresh page is
  // eighteen forced reads that have taught the estimate nothing yet, so the press that most needs a
  // number is the one that has none; and a forced read the server answers off disk returns in
  // milliseconds, so counting it would drag the median toward a speed no model call ever runs at.
  // The measurement is taken where the answer lands (`revReadSettle`), which is no longer here.
  const startedAt = Date.now();
  revSumBusy++;
  // **In flight from this instant**, not from whenever the poll next runs. The server is the
  // authority on what is being read (`/api/review/reading`) and the poll will confirm this within
  // `REV_INFLIGHT_POLL_MS`, but a press must show its counter on the frame it happens — four
  // silent seconds is exactly the gap the reader reads as "nothing happened" (SKEIN-333).
  //
  // `started_ms` is this browser's clock, and the server's reply carries its own; they differ by
  // whatever the two clocks differ by, which is a second at worst on one machine and does not
  // matter for a number rounded to seconds.
  if (!revInFlight.has(key)) revInFlight.set(key, { repo_id: id, number, started_ms: Date.now(), asked: true });
  revSums.set(key, "…");
  // A person pressing "read it" / "re-read" gets the "…" now; the PUMP's own reads keep the
  // deferral, because a row filling itself in behind a half-typed comment is exactly the
  // background render §6 rule 2 protects against. `how` already draws that line for the budget.
  if (force || how === "asked") renderReviewNow(); else renderReview();
  // **The reading is STARTED here, and lands on the stream** (SKEIN-366).
  //
  // This used to be one `fetch` held open for the whole model call. That is a browser connection
  // held for tens of seconds, and browsers allow six per origin over HTTP/1.1 — which is all the
  // cockpit speaks (`curl --http2` against it still answers `HTTP/1.1 200 OK`). `REV_ASKED_PARALLEL`
  // is ten, so ONE pressed stack read took every connection the page had, and everything else it
  // does — the image upload, the health tick, the queue refresh, a second stack's progress — sat in
  // the browser's own queue behind a 35-second model call. Measured in a real browser against a
  // build of `d48a4ce`: an unrelated `GET /api/health` from this page took 12 ms with three
  // readings in flight, 12,814 ms with six, and 34,438 ms with ten.
  //
  // **The width is not the fix and must not become it.** A read the owner asks for is not rationed
  // — his instruction, twice — and a smaller number moves the cliff rather than removing it. What
  // changed is where the answer travels: this POST returns in milliseconds, and the reading itself
  // arrives on the one `EventSource` the page already holds (`connect`, `reading`). Ten readings
  // now cost one connection between them, so the number of connections this page holds does not
  // grow with the number of readings in flight — which is the invariant, and what the test asserts.
  //
  // Returned so a caller reading a whole stack can wait for this one before starting the next
  // (`revStackPump`). Every other caller ignores it, as they did before.
  const waiting = new Promise(resolve => revReadWaits.set(key, {
    id, number, key, force, replacing, startedAt, resolve,
    // Has the SERVER said it is reading this? `revPollInFlight` sets it, and only a wait that has
    // been seen may be settled by that poll noticing the reading is gone — see `revReadSettle`.
    seen: false,
  }));
  fetch(`/api/repos/${encodeURIComponent(id)}/review/${number}/read${q}`, { method: "POST" })
    .then(async r => { if (!r.ok) throw new Error((await r.text()) || `HTTP ${r.status}`); })
    // The START failed, which is a different thing from a reading that failed — and the row must
    // say so either way rather than sitting on "…" for ever.
    .catch(e => revReadSettle(key, null, String(e.message || e).split("\n")[0]));
  return waiting;
}

// Readings this page is waiting for, keyed `repo#number` — see `revFetchSummary`.
//
// It is what makes a reading's ANSWER separable from the request that asked for one: the request is
// over in milliseconds and this holds the promise the caller is waiting on until the stream, or the
// in-flight poll, says what happened.
let revReadWaits = new Map();

// **Which queue a landed reading was built from**, keyed `repo#number` — the `as_of` stamp when it
// was a remembered one, absent when it was fresh or when this page never heard.
//
// `review::ReadingDone` carries the same distinction `x-skein-queue` draws on the request-shaped
// route, and it is here because a reading built from a remembered queue is the one case where
// `Known::stale` cannot be trusted to be the whole answer: `stale` is computed by comparing the
// reading's head against the head the QUEUE holds, so a queue that has not asked GitHub since
// before the branch moved reports a stale reading as current. Absent is not a claim of freshness —
// see `revReadSettle`, which only writes what it was told.
let revReadFrom = new Map();

// A reading that arrived with nobody waiting for it — see `revReadSettle`, which is its only
// caller. True when it was applied.
//
// It writes the answer and nothing else: no `revSumBusy`, no promise, no "● updated". Those belong
// to a press, and this is the case where the press's bookkeeping has already been done by whatever
// settled its wait.
//
// The two refusals are `revFetchHeld`'s, for `revFetchHeld`'s reasons — this is the same act, with
// the answer arriving rather than being fetched. A row being read RIGHT NOW is never written to
// (SKEIN-333): a landed answer is the older one the instant somebody presses again. And a row
// already holding a real reading keeps it — "…", an `unread` row and a `transient` failure are each
// weaker than an answer, and a thin row knows strictly less about the same one.
function revApplyLanded(key, s) {
  if (!revHeld || !s) return false;
  if (revInFlight.has(key)) return false;
  const now = revSums.get(key);
  if (now && now !== "…" && !now.thin && !now.transient && now.depth !== "unread") return false;
  revSums.set(key, s);
  return true;
}

// A reading this page asked for has landed. The one place its answer is applied.
//
// `s` is the summary (the same body `/review/:n/summary` answers with), or null; `error` is the
// sentence to show when there is no summary. Both absent means "it ended and this page did not hear
// how" — the poll's recovery path, where `revFetchHeld` is already fetching what is on disk, so
// this settles the bookkeeping and writes nothing over it.
//
// `from` is `{queue, as_of}` off `review::ReadingDone` when the stream delivered it, and undefined
// on every other path. Undefined means UNKNOWN and leaves whatever is remembered alone: a poll
// recovering a reading it did not watch arrive must not be able to say it came from a fresh queue.
//
// Guarded by the map for its BOOKKEEPING: a reading nobody here is waiting for must not
// double-count `revSumBusy`, resolve a promise twice, or advance a stack run twice — which is what
// makes settling idempotent when the stream and the poll race.
//
// **That guard used to take the reading itself with it, and that is how one went missing**
// (SKEIN-766). It read "not this page's to apply, and the in-flight poll already brings those in",
// and the second half is untrue of the case that matters. The poll brings in what is on DISK
// (`revFetchHeld`), and a reading whose ownership could not be consulted is served and deliberately
// never written down (`review::ownership` answers `Unreadable`, and `review::summarise` stores only
// when `ownership_unknown` is empty) — so for those there is nothing to bring in, and `?held=1`
// answers that skein holds no reading of this pull request yet.
//
// And the poll is what takes the wait away. `revPollInFlight` settles a wait the moment the server
// stops listing the reading, and the server stops listing it BEFORE it announces the answer:
// `api_review_read` drops the reading's registration when the read returns and calls
// `announce_reading` afterwards. So under load the frame carrying the answer landed a moment after
// the poll had closed the door, found no wait, and was dropped — leaving the row holding the poll's
// "not read", which is the opposite of what had just happened, permanently. Measured as one reading
// of ten missing with the server reporting nothing in flight: 1 of 12 four-lane single-core runs, 0
// of 12 unloaded (SKEIN-754, SKEIN-766).
//
// So the fallback no longer outranks the thing it is a fallback for.
function revReadSettle(key, s, error, from) {
  const w = revReadWaits.get(key);
  // Nobody waiting means no bookkeeping to do — but there may still be an answer to apply.
  // `revApplyLanded` says whether it took it, so the provenance below is written for the readings
  // that actually landed and for no others (SKEIN-239's rule; `tests/ui/provenance.mjs` case 5).
  if (!w && !revApplyLanded(key, s)) return;
  if (from && from.queue) {
    if (from.queue === "remembered") revReadFrom.set(key, from.as_of || "");
    else revReadFrom.delete(key);
  }
  if (!w) { renderReview(); return; }
  revReadWaits.delete(key);
  revSumBusy--;
  if (revHeld) {
    if (s) {
      revSums.set(key, s);
    } else if (error) {
      // A failed reading is not "nothing to see here". It becomes an unread summary carrying the
      // reason, exactly as the server's own failures do — the page must not be a place where a
      // network error looks like a clear PR.
      // `transient`: this is a failure to REACH the reading, not a reading. It has to be visible
      // but must not become the permanent answer for that row. The pump skips anything already in
      // `revSums`, so without this the row showed a transport error for ever and nothing ever
      // asked again. Dropped on the next queue load, which is where the stale-head drop lives.
      // Carries the head it was asked about, like a real reading does. Without it the entry is
      // dropped by the stale-head rule — by ACCIDENT, because it happens to have no head to
      // compare — and the rule that is supposed to govern this ("a failure to reach is not an
      // answer") would be dead code that nothing could test.
      revSums.set(key, {
        number: w.number, depth: "unread", line: "", detail: "", flags: [], yours: [], others: 0,
        transient: true,
        head_sha: ((revQueue && revQueue.prs || []).find(p => p.repo_id === w.id && p.number === w.number) || {}).head_sha,
        unread_because: "skein could not reach its own summary for this PR: " + error,
      });
    }
  }
  // **What counts as a measurement is `computed`** — the server's own word for "a model call
  // happened" — and not `force`, which is who asked. A forced read the server answers off disk
  // returns in milliseconds, and counting it would drag the median toward a speed no model call
  // ever runs at.
  if (s && s.computed) revNoteReadMs(Date.now() - w.startedAt);
  // Out of flight the moment the answer is here. Left to the poll, the row would keep its counter
  // for up to `REV_INFLIGHT_POLL_MS` after the reading it was waiting for had arrived — a spinner
  // over a finished answer, which is the same lie as a stale answer over a running one, told the
  // other way round.
  revInFlight.delete(key);
  // Landed, and nobody has looked at it yet. Cleared by opening the row (`toggleRevRow`) — never by
  // a timer, so a reading that finished while the reader was elsewhere is still evident when they
  // come back. Only where this read REPLACED a reading (`replacing`, captured before the "…" went
  // in): a row skein is filling in for the first time has nothing to come back to and nothing to
  // compare, and marking it made the pane's cold open a wall of "● updated".
  if (w.replacing) revUpdated.add(key);
  renderReview();
  revPumpSummaries();
  w.resolve(error ? { ok: false, error } : { ok: true });
}

// One reading, off the live stream. The shape is `review::ReadingDone`.
//
// Separate from the stream plumbing that receives it because this is review's rule, not the
// stream's: which readings this page may apply, and what a reading with no summary means.
function revReadArrived(d) {
  if (!d || !d.repo_id || !d.number) return;
  revReadSettle(`${d.repo_id}#${d.number}`, d.summary || null, d.error || "",
    { queue: d.queue || "", as_of: d.as_of || "" });
}

// **The one control** (SKEIN-293, the owner's "go with one control"). Read it again, and draft a new
// review from that reading — always both halves, which is what the two old buttons only sometimes
// each did.
//
// It goes straight away. There is nothing on this page a re-read can now destroy — the vetting
// panel that held a reader's kept/dropped decisions is gone, and a reading replacing a reading
// costs them nothing. Making somebody confirm something that costs them nothing is how a
// confirmation becomes noise and stops being read.
function revReadAgainPress(repo, number) {
  revFetchSummary(repo, number, "force");
}

// **A reading landed here and you have not looked at it yet** (SKEIN-333).
//
// It clears when the row is OPENED, never on a timer — the owner's choice. A stamp that ages out
// on its own is no use for the case it exists to serve: a read takes most of a minute, so the
// reader is somewhere else when it lands, and "updated 90 seconds ago, fading" is a mark designed
// to be missed by exactly the person it is for.
//
// Deliberately not a count and not a diff of the two readings: the row's own line already says
// what skein now thinks, and the only thing this adds is that it CHANGED while you were away.
function revUpdatedChip(pr) {
  if (!revUpdated.has(rk(pr))) return "";
  // A row already open is a row being looked at, so there is nothing to come back to.
  if (revOpen.has(rk(pr))) return "";
  return `<span class="revtag updated" title="skein finished reading this while you were elsewhere — open the row and this mark clears">● updated</span>`;
}

// The name of the act, as the control that sends it is labelled. NOT `revReceiptHtml`'s map, which
// is the past tense of an act that HAPPENED ("merged", "changes requested") and reads as a lie in
// front of a refusal — "merged refused" says the opposite of what took place. Anything missing
// falls through to the kind itself, which is already the word the wire uses.
const REV_ACT_NAME = { approve: "approve", "request-changes": "request changes", comment: "comment",
                       archive: "set aside", merge: "merge", reread: "re-read" };

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

// Read this one, from the collapsed line (SKEIN-228).
//
// Re-analysis existed only behind the fold, as `re-read` among eight other chips — which the owner
// read as its not existing at all, and from the outside that is the same thing. So it comes out to
// the line, and it is RATIONED the way every mark on this row is rationed: a control on every row
// is a texture, and most rows do not need one because the pump reads the lane for you.
//
// The three states that do need one, and nothing else:
//   * a reading of an EARLIER commit — the row already says "read before the latest commits", and
//     re-analysing is the answer to that sentence (law 1: never state a problem without its move);
//   * the day's automatic budget spent — the refusal invites exactly this press;
//   * unread with nothing coming — a draft, a lane the pump does not read, or a reading that
//     failed. A row in flight says "reading it…" and offers nothing, because it is already coming.
//
// **What it says about cost is nothing, and that is a change.** It used to end "a reading you ask
// for is never counted against the day's budget" — true of the BUDGET, and read on a control as
// "this is free". It is not: the press downloads the diff and spends a model call that takes most
// of a minute. The ceiling is on skein's own initiative (`review::Trigger`) and a reader pressing
// here is not spending against it, which is worth knowing exactly once, in the one place the budget
// is the subject — the panel that says the day's automatic reading has stopped (`revDetail`). The
// owner's call, having been offered the alternative: fix the wording, leave the control alone, and
// do not print a duration on a button.
// **Does this pull request want reading again, and why** — the one question every read control on
// the row is asking, asked once (SKEIN-372). It used to be asked twice: `revReadAgain` decided it
// for the line, and `revDetail`'s branches decided it again for the body, so an expanded row could
// draw two doors to the same act. Now the line asks this, and the strip below draws its own control
// only when the answer is "no" — because when it is anything else, the body already carries the
// move beside the sentence that explains it, which is where law 1 wants it.
function revWantsRead(pr) {
  const s = revSums.get(rk(pr));
  if (s === "…") return "";
  if (!s) return "unread";
  if (s.stale) return "stale";
  if (s.depth === "unread") return "unread";
  return "";
}

function revReadAgain(pr, open) {
  const s = revSums.get(rk(pr));
  if (s === "…") return "";
  // The line's control is the COLLAPSED row's: SKEIN-371 put it there so buying the missing half
  // would not require opening the row. An open row draws the same act in its body, beside the
  // sentence that says why it is needed, so drawing it here as well is the two-doors defect.
  // `open` is passed rather than read from `revOpen`, because a step inside a stack is opened by
  // `revStackStep` and the caller is the only one that knows which of the two it is.
  if (open) return "";
  const stale = s && s !== "…" && s.stale;
  const unread = !s || (s !== "…" && s.depth === "unread");
  // A third state used to be decided here — read, and no review came back (SKEIN-371) — and it is
  // gone with the field that was its only evidence. See the note where `revNoReviewCameBack` was.
  if (!stale && !unread) return "";
  // A row the pump is about to read anyway offers nothing: the control would be gone a second
  // later, which is a flicker rather than an affordance. "About to be read" is the PUMP's own
  // question, asked of the pump — not a second copy of its rule, which is how this line came to
  // promise a reading that never arrived for a repo with read-ahead switched off (SKEIN-242) and
  // to offer a button on a pull request of yours the pump now reads (SKEIN-277).
  const coming = !s && revQueue && revQueue.ai
    && revReadsAhead(pr.repo_id) && revSkeinsToRead(pr);
  if (coming) return "";
  return `<button type="button" class="revread" title="${esc(stale
      ? "read this again, against the commit that is there now"
      : "have skein read this one")}"
    onclick="event.stopPropagation(); revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">${
    stale ? "re-read" : "read it"}</button>`;
}

function revRow(pr) {
  const open = revOpen.has(rk(pr));
  const s = revSums.get(rk(pr));
  // The tripwire marks ride on the collapsed line, because they are the reason to stop scrolling.
  // A chip must be true of a minority of rows AND change what you do. The reason chips failed
  // both and are gone; a flag kind the whole queue wears this render (revCommonChips) is dropped
  // too, and what survives is capped at two so the title keeps its column — the expanded brief
  // still lists every flag. The moved chip obeys the same rule: when a whole wave of heads moved,
  // the wave is the story, and the stale-reading note in the gist still marks each row.
  const rawFlags = (s && s !== "…" ? (s.flags || []) : []).filter(f => !revCommonChips.has(f));
  const flags = rawFlags.slice(0, 2)
    .map(f => `<span class="revtag flag">${esc(f)}</span>`).join("")
    + (rawFlags.length > 2 ? `<span class="revtag flag" title="${esc(rawFlags.slice(2).join(", "))}">+${rawFlags.length - 2}</span>` : "");
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
  }${revUpdatedChip(pr)}${revFlowChip(pr)}${flags}${revReadAgain(pr, open)}`;
  const line = `<div class="revline" onclick="toggleRevRow(${esc(JSON.stringify(rk(pr)))})">
      <span class="mv ${revMove(pr)}" title="${esc(REV_MOVE_WORDS[revMove(pr)] || "")}"></span>
      <span class="revnum" title="${esc(pr.repo_id || "")}">#${pr.number}</span>
      <span class="tcell"><span class="revtitle">${esc(pr.title || "")}</span>${chips}</span>
      ${revGist(s, rk(pr), open)}
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

// The most expensive artefact in the product is a reading, and an unused judgement is a cost with
// no return: a tripwire flag lifts a row (band 0), the unread hold the middle (band 1), and a
// routine verdict sinks (band 2) — always inside the lane readiness chose, per the module's own
// rule that AI only adds scrutiny.
function revReadBand(p) {
  const s = revSums.get(rk(p));
  if (!s || s === "…" || s.depth === "unread") return 1;
  if ((s.flags || []).length) return 0;
  return 2;
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

// The gist cell — what this PR is, on the line you actually scan at thirty a day. It is NEVER
// empty: the module's rule is that AI only adds scrutiny, so "skein read this and it is routine"
// and "skein never looked at this" must not render alike — and the empty string was exactly that,
// on 23 of 29 live rows. All four states are one row high, which is the other half of the
// contract: a summary landing may change this cell's text, never any row's position.
function revGist(s, key, open) {
  // **Being read right now**, and it outranks every other thing this cell could say — including a
  // perfectly good reading, which is exactly the case that was broken: the row went on showing the
  // OLD line for most of a 35-second call and the press looked inert (SKEIN-333).
  //
  // The old line is kept, DIMMED and labelled, rather than blanked. The owner's choice, and the
  // right one: a row that empties itself has thrown away the only thing it knew in exchange for
  // saying it is busy, and a reader scanning the list loses their place.
  //
  // **The chrome is as short as it can be said**, because every character of it is taken off the
  // line it is supposed to be protecting. It used to read "reading again…" whenever there was an
  // old line — which is to say precisely when the cell could least afford six more characters —
  // and the word earned nothing: an old line sitting beside a running counter IS the "again", and
  // it carries "the reading being replaced" on its own title. Measured in the real 256px column
  // (`measure-gist.mjs`): 124.8px of the old reading before, 159.3px after.
  const r = key && revInFlight.get(key);
  if (r) {
    const had = s && s !== "…" && s.depth !== "unread" && (s.line || "");
    return `<span class="gist reading">
      <span class="revflight">⟳ reading…
        <span class="revflight-secs" data-started="${Number(r.started_ms) || 0}">${
          revElapsed(Date.now() - (Number(r.started_ms) || Date.now()))}</span>${
        r.asked === false ? `<span class="revflight-who" title="skein started this one itself — you did not press for it">skein's own</span>` : ""}</span>${
      had ? `<span class="revflight-was" title="the reading being replaced">${esc(had)}</span>` : ""}
    </span>`;
  }
  if (!s) return `<span class="gist unknown">not read</span>`;
  if (s === "…") return `<span class="gist reading">reading it…</span>`;
  // **The failure is stated at whichever surface can hold it, and at one of them at a time**
  // (SKEIN-400). An open row drew `unread_because` three times over: cut to this column's width
  // here, in full again thirty pixels below in `revDetail`'s "Not summarised — …" beside the button
  // that acts on it, and a third time as this span's own `title`, repeating the text it sat on.
  // That is the two-doors defect `revReadAgain` already refuses on this same line — "an open row
  // draws the same act in its body, so drawing it here as well" — one field along: the sentence
  // rather than the button. `open` is threaded from the caller for the same reason it is threaded
  // there, and only from the two callers that draw a body: a step inside a stack is opened by
  // `revStackStep`, and nothing here can tell which of the two it is.
  //
  // **Open**, the body has the whole sentence and the move, so this cell steps back to the state
  // mark alone — still `not read`, still `.gist.unknown`'s dotted "stated absence" underline, so
  // the row reads down the column exactly as its neighbours do and the §10 rule that nothing may
  // change a row's height is untouched (it is one nowrap run either way).
  //
  // **Collapsed**, this cell is the only place the sentence is, and the title is not a second copy
  // of it in any sense a reader would recognise: `ai::Unread::say` writes 150–250 characters whose
  // CURE is at the end — "It is on the PATH of the process running skein-server… or set
  // SKEIN_CLAUDE_BIN to its full path" — and about 45 of them fit a 256px column. An error cut
  // there keeps its complaint and loses its fix, so hover is what makes the rest reachable without
  // opening the row. That is this page's one job for a `title`: say what the visible text could
  // not, as `.mv` does for a glyph, `.revnum` for the repo behind a bare number, and the `+N` chip
  // for the flags it hid.
  if (s.depth === "unread") {
    return open
      ? `<span class="gist unknown">not read</span>`
      : `<span class="gist unknown" title="${esc(s.unread_because || "")}">not read — ${esc(s.unread_because || "")}</span>`;
  }
  // A reading of an earlier commit says so where you scan, not only when expanded — the whole risk
  // of keeping it is that it gets mistaken for a reading of what is there now.
  return `<span class="gist${s.stale ? " stale" : ""}">${s.stale ? "read before the latest commits — " : ""}${esc(s.line || "")}</span>`;
}

// `revrowacts` on the row's own control strip: an expanded row grew sections with `.revacts` of
// their own — the drafted review's, the budget refusal's — and a bare `.revacts` selector reads
// whichever one the document reaches first. The class is what lets a reader (and a test) name the
// row's own controls. Kept as a JS comment: an HTML comment inside this inline script would put the
// parser into script-data-escaped state and take the rest of the page with it.
// **The verdict controls, on the ROW** (SKEIN-449).
//
// They used to live in a reading view of skein's own, gated on the diff being on screen —
// `docs/review-ux.md` §6, "no verdict from a surface that is not showing you the change". The
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
    ${revDetail(pr)}
    <div class="revacts revrowacts">${revPending.has(rk(pr))
      // **One read control, and the drafted review shows itself** (SKEIN-335). There was a second
      // button here — "review the code…" — and the owner's report was the whole of the case:
      // "reread the code and review the code are still 2 different buttons (they do the same
      // thing, why are they different?)".
      //
      // They did not do the same thing, which is worse: one spent a model call and took 35
      // seconds, the other read a drafted review off disk for free and generated nothing. Nothing
      // on screen said which, and both were verbs. On a pull request with no draft the second
      // opened an empty panel, so the honest reading of it was "the button is broken".
      //
      // SKEIN-162: an act on this row is held, in flight, landed or refused — the strip you
      // pressed IS where that story shows, not a toast in the opposite corner.
      ? revReceiptHtml(rk(pr), revPending.get(rk(pr)))
      : `${revVerdictHtml(pr)}
      ${link(pr.url, "read on GitHub", 'class="revchip" target="_blank" rel="noopener"')}
      <button type="button" class="revchip" onclick="revCompose(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, 'ask')">ask…</button>
      <span class="revspacer"></span>
      <button type="button" class="revchip" onclick="openNewBoxFor(${esc(JSON.stringify(pr.repo_id))}, ${esc(JSON.stringify(pr.head_ref || ""))})">box on this branch</button>
      ${pr.snoozed
        ? `<button type="button" class="revchip" onclick="revSnooze(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, '')" title="it would come back on its own at the author's next push">bring back now</button>`
        : `<button type="button" class="revchip" onclick="revSnooze(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, ${esc(JSON.stringify(pr.head_sha || ""))})" title="out of the queue until the author pushes — the push is what brings it back">until it moves</button>
      <button type="button" class="revchip" onclick="archivePr(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, ${archived ? "false" : "true"})">${archived ? "bring back" : "set aside"}</button>`}
      ${revWantsRead(pr) ? "" : `<button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it again</button>`}`}
    </div>
    ${revFlowBox(pr)}
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

// The expanded half: the brief when there is one, and an unmissable statement when there is not.
//
// A PR skein has not read must never render as an empty, calm space — that reads as "nothing here",
// which is the one thing it must not mean. So the absence is drawn as loudly as the presence.
function revDetail(pr) {
  const s = revSums.get(rk(pr));
  if (!s) {
    // Which reason, because three different ones land here and only one of them is a setting to
    // change. "Not read yet" alone reads as a failure on a draft that skein is deliberately leaving
    // alone, and as a limit on a fleet where the feature is simply switched off.
    const why = revQueue && !revQueue.ai
      ? " Reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes. Until then every PR stays at full attention."
      : pr.draft
        ? " It is a draft, so skein leaves it alone until it is marked ready."
        // Which of the two scope rules left it unread, because they have different answers: one is
        // a switch on this repo, the other is what skein reads at all on its own. "Nothing has
        // asked for this one yet" was the whole of what this said, on a row that would never be
        // read however long you waited (SKEIN-242).
        // And the switch it names is RIGHT HERE (SKEIN-282). This sentence named a control that
        // existed only in a view narrowed to one repo, so on the merged queue it told the reader
        // about something they could not reach from where they were standing — law 1's failure
        // one step removed: the move was stated but not offered.
        : !revReadsAhead(pr.repo_id)
          ? ` skein does not read ${esc(pr.repo_id)} on its own — <button type="button" class="revchip" onclick="revSetReadingFor(${
              esc(JSON.stringify(pr.repo_id))}, true)" title="the pull requests somebody asked you to review, and the ones you opened, one unit of the day's budget each — nothing else here is read">read ahead in ${
              esc(pr.repo_id)}</button> or read this one now.`
          : !revSkeinsToRead(pr)
            ? " skein reads what somebody asked you to review and what you opened; this one is neither."
            // No settle clause any more: the hour is gone from the server, so the page must not
            // claim a branch is being waited on when nothing is waiting.
            : " Nothing has asked for this one yet.";
    return `<div class="revnosum">Not read yet.${why}${revQueue && !revQueue.ai ? ""
      : ` <button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it</button>`}</div>`;
  }
  if (s === "…") return `<div class="revnosum">reading it…</div>`;
  // **A row shape cannot draw a brief** (SKEIN-287). The queue payload carries the line, the flags
  // and whether a review is drafted; the prose, the signals and the ownership are fetched when the
  // row opens (`revLoadReading`). Drawing the brief from a thin reading would print "yours: none"
  // and "no signals" as FACTS about a pull request skein has read and attributed — the exact
  // failure §8.6 exists to prevent, one field along.
  //
  // Not "reading it…", because nothing is being read: skein has the answer and the page is
  // fetching it off disk. An unread row falls through — its reason (`unread_because`,
  // `budget_stopped`) rides on the row shape and is the whole of what there is to say.
  if (s.thin && s.depth !== "unread") {
    return `<div class="revnosum">${s.prose_failed
      ? `The brief could not be fetched — ${esc(s.prose_failed)} Close the row and open it again to retry.`
      : `<span class="dim">fetching the brief…</span>`}</div>`;
  }
  // The day's AUTOMATIC budget is spent, and that is the one refusal with a move in it: a read you
  // ask for is never budgeted (`review::Trigger::Asked`), so the button is the answer rather than a
  // footnote — the owner's ask, verbatim: "When limit is hit, surface and ask me to manually trigger
  // these. Limit is only for automatic stuff, manually I can invoke as many as I want."
  if (s.depth === "unread" && s.budget_stopped) {
    return `<div class="revnosum budget"><b>skein stopped reading for today.</b>
      ${esc(s.unread_because || "")}
      <div class="revacts"><button type="button" class="revchip go" onclick="revFetchSummary(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, 'asked')">read this one now</button>
      <span class="dim">a read you ask for is never counted against the day</span></div></div>`;
  }
  // **The one row where the reader is actually stuck was the one with no move in it** (SKEIN-392).
  // Both neighbours carry a control — the budget panel above offers "read this one now", "Not read
  // yet" below offers "read it" — and this branch, reached only when skein TRIED and could not,
  // ended "Read this one yourself.". That is what a person is given after waiting for a reading,
  // and it told them to give up while the collapsed row was still drawing them a button
  // (`revReadAgain` counts depth === "unread"): the pane and the row disagreeing about whether
  // there was anything left to do. §law 1 — never a statement without the move it implies.
  // **Its box did not answer in time, and skein stopped there** (SKEIN-818). The owner's wording,
  // and the owner's decision: spending the budget again is the reader's call, so the row offers
  // both ways on — the box again, or here instead — and presses neither by itself.
  if (s.depth === "unread" && s.stopped_at_box) return `<div class="revnosum boxslow">Not read — ${esc(s.unread_because || "")}
    <div class="revacts"><button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it again</button>
      <button type="button" class="revchip" onclick="revFetchSummary(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, 'here')">read it here instead</button></div></div>`;
  if (s.depth === "unread") return `<div class="revnosum">Not summarised — ${esc(s.unread_because || "")}
    <div class="revacts"><button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it again</button></div></div>`;
  // What this is a reading OF. Kept deliberately (see `loadReview`), so it has to be unmistakable
  // about which code it describes — and the way out is right here rather than somewhere else.
  // `not_reread` (SKEIN-444) makes this the same sentence with the missing half filled in: skein
  // did not merely fail to read the newest commit — nobody has asked it to. A round runs when the
  // author re-requests your review on GitHub, or when you press this button. That is a different
  // thing to tell somebody, and leaving it out would make a deliberate choice read as neglect. The
  // way out is the same button either way, because asking is exactly what it is for.
  const stale = s.stale
    ? `<div class="revstale">${s.not_reread
        ? `<b>${esc(s.not_reread)}</b> The reading below is of
           <code>${esc(String(s.head_sha || "").slice(0, 7))}</code>.`
        : `<b>This was read before the latest commits.</b> It describes
           <code>${esc(String(s.head_sha || "").slice(0, 7))}</code>, and the branch has moved since.`}
        <button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it again</button></div>`
    : "";
  // **The reading was built from a queue skein remembered**, so the line above cannot be trusted to
  // be the whole answer. `stale` is "this reading's head is not the head the QUEUE holds" — and a
  // queue handed over from disk while a fresh one is being fetched (`prq::Queue::fresh`) holds
  // whatever head it held when it was written. A branch that moved since is a stale reading that
  // reports itself current, which is the one direction that rule must not fail in.
  //
  // Drawn only where `stale` is silent: where it has already fired, the reader has the stronger
  // sentence and the same way out, and a hedge under it would be arguing with it. Drawn at all
  // because it is rare — the remembered queue is the fallback path, not the usual one — so this is
  // a note that means something on the row it appears on rather than a disclaimer on every row.
  //
  // The move it implies is the queue's, not the reading's: reading again would spend a model call
  // and ask the same remembered queue. `loadReview(true)` is the one that asks GitHub.
  const from = revReadFrom.get(rk(pr));
  const blind = !s.stale && from !== undefined
    ? `<div class="revstale blind"><b>This was read against a queue skein remembered${
        from ? `, taken ${esc(revAgo(from))}` : ""}.</b> GitHub was not asked, so skein cannot say
        whether the branch has moved since — a reading of an earlier commit would not be marked.
        <button type="button" class="revchip" onclick="loadReview(true)">ask GitHub now</button></div>`
    : "";
  // **This reading did not run in its box** (SKEIN-799). The server composes the whole sentence
  // (`review::Summary::read_outside_box`) and this draws it verbatim, for the reason that field's
  // own doc gives: the second half is about whether a sweep spoke for the reading, and `swept`
  // reaches this page only when it is TRUE — a page composing that half itself could not tell "no
  // sweep accounted for it" from "this skein is too old to say".
  //
  // **The open row and not the collapsed gist** — SKEIN-400's one-surface rule, which the `gist`
  // above is already written to: a sentence this long in a 256px column keeps its complaint and
  // loses everything after it, and drawing it in both places is the defect that rule was written
  // for. The body is where the reader is when they are deciding what this reading is worth.
  //
  // The move is the same one `stale` offers and for the same reason — nothing here is fixable by
  // reading harder, and the box may well be back — so the statement is not left without one
  // (SKEIN-392, one field along).
  const outside = s.read_outside_box
    ? `<div class="revstale outsidebox"><b>${esc(s.read_outside_box)}</b>
        <button type="button" class="revchip" onclick="revReadAgainPress(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">read it again</button></div>`
    : "";
  // Three ways, exactly as `review::Ownership` answers (SKEIN-117): attributed, could-not-look,
  // or the repo has no CODEOWNERS. Silence is the right rendering for the third and the WRONG one
  // for the second — an empty `yours` used to draw as nothing either way, so a brief written
  // while skein could not read the repo looked like a brief about a repo nobody owns. The
  // not-kept half is said too, because it is the reader's answer to "then why is it still
  // offering to read this one": the server deliberately does not cache a blind reading.
  const owned = s.ownership_unknown
    ? `<div class="revowned">skein could not read this repo to see which of this is yours — ${
        esc(s.ownership_unknown)}. The whole change is in scope below, and this reading is not being kept.</div>`
    : (s.yours || []).length
    ? `<div class="revowned">yours: ${(s.yours || []).map(p => `<code>${esc(p)}</code>`).join(" ")}${
        s.others ? ` <span class="dim">· and ${s.others} path${s.others === 1 ? "" : "s"} you do not own</span>` : ""}</div>`
    : "";
  // Kept visually apart from the brief on purpose: "the diff says so" and "a model thinks so" are
  // different kinds of claim, and collapsing them into one paragraph would make the weaker one
  // borrow the authority of the stronger.
  const signals = (s.signals || []).length
    ? `<div class="revsignals"><b>found in the diff</b>${(s.signals || []).map(g =>
        `<div><span class="revtag flag">${esc(g.kind)}</span> ${esc(g.what)} <span class="dim">${esc(g.file)}</span></div>`).join("")}</div>`
    : "";
  const detail = s.detail
    ? `<div class="revbrief">${marked.parse(s.detail)}</div>`
    : `<div class="revbrief"><p>${esc(s.line || "")}</p></div>`;
  return stale + blind + outside + owned + signals + detail;
}

// What governs this pull request, what it would do next, and how to change either.
//
// Everything here is about a thing that acts on its own, so it says three things a person needs
// before they can trust it: what it WOULD do (the same evaluator the tick uses, so the preview
// cannot disagree with what happens), who decided it should — a rule or you — and how to stop it.
function revFlowBox(pr) {
  const flows = revFlows.get(pr.repo_id);
  if (!flows) return "";
  if (flows.error) {
    // A workflow file with a typo in it loads none of them, deliberately (half an automation is
    // worse than none), and the pull request is the place somebody will notice.
    return `<div class="revflow"><b>Workflows are not running.</b> ${esc(flows.error)}</div>`;
  }
  const defined = flows.defined || [];
  const st = (flows.prs || {})[pr.number]
    || { workflow: "", how: "none", next: "", step: 0, stopped: "", holding: "" };
  const chosen = st.how === "assigned" ? st.workflow : st.how === "excluded" ? "" : "__rules";
  const options = [
    `<option value="__rules"${chosen === "__rules" ? " selected" : ""}>let the rules decide</option>`,
    ...defined.map(f => `<option value="${esc(f.name)}"${chosen === f.name ? " selected" : ""}>${esc(f.name)}</option>`),
    `<option value=""${st.how === "excluded" ? " selected" : ""}>no workflow — leave this one alone</option>`,
  ].join("");

  // Where it stands, in a sentence rather than a badge: this is the part somebody reads when they
  // are deciding whether to let it run.
  const where = st.stopped
    ? `<div class="revflow-stop"><b>Stopped.</b> ${esc(st.stopped)}
        <button type="button" class="revchip" onclick="revClearStop(${esc(JSON.stringify(pr.repo_id))}, ${pr.number})">let it run again</button></div>`
    : !st.workflow
      ? `<div class="dim">Nothing governs this pull request${st.how === "excluded" ? " — you left it out" : ""}.</div>`
      // **HELD** (SKEIN-279/326): somebody put a workflow on this pull request and the workflow's
      // own `matches` are not true yet. The row fell through to "waiting for something to change"
      // — true, and wrong in the way that matters: it reads as a clock running when none is, and
      // it does not say what is missing. The sentence the server builds names the unmet conditions
      // as the FILE spells them, so it can be checked against the workflow somebody is reading.
      //
      // Drawn in the "not a stop" register, deliberately: no button, nothing to clear, and neither
      // amber nor red. Nothing has failed and nothing is waiting on a clock — this is a workflow
      // doing exactly what it was configured to do, and the owner is reading this pane as a dry run
      // with the train switched off. "Put it on the train, it goes when it is approved" is now
      // literally what happens, and this is the line that says so.
      : st.holding
        ? `<div class="revflow-holding">${esc(st.holding)}</div>`
      : st.next
        ? `<div>Next: <code>${esc(st.next)}</code> <span class="dim">(step ${st.step}${
            st.how === "matched" ? `, from ${esc(st.workflow)}'s own rule` : ""})</span></div>`
        : `<div class="dim">Nothing to do right now — ${esc(st.workflow)} is waiting for something to change.</div>`;

  // Said once, here, rather than on every row: with the switch off none of this acts, and a person
  // reading "next: merge" deserves to know whether that is a plan or a prediction.
  const off = flows.enabled === false && st.workflow
    ? `<div class="revflow-off">Workflows are switched off for this fleet, so nothing will happen on its own.</div>`
    : "";

  return `<div class="revflow">
    <div class="revflow-head"><span>workflow</span>
      <select aria-label="Workflow for this pull request" onchange="revSetWorkflow(${esc(JSON.stringify(pr.repo_id))}, ${pr.number}, this.value)">${options}</select>
    </div>${where}${off}
  </div>`;
}

// The composer: one box for the three things that need words. `ask` stays private; the other two
// post under your name, which is why the button that sends them says so.
let revComposing = null;   // { repo, number, kind, text, answer, busy }

let revSearch = "";            // the queue's search text — matches number, title, author, branch, repo
let revCommonChips = new Set(); // chip kinds worn by most of the queue this render — wallpaper, not signal

