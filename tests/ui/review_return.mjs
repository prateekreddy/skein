// What survives a trip to a box and back to the review queue, and what gets asked again?
//
// Two live reports, one root:
//
//   "it worked first time but when I clicked a box and went back to PRs the entire thing
//    disappears" … "and it starts reading again"
//   "for a bunch of PRs I am getting — not summarised — skein could not reach its own summary for
//    this PR: no such repo"
//
// Both are the same mistake made twice: asking `view.repo` — the repo the VIEW happens to name —
// for a question about the repo the pane's state BELONGS to. A box view names no repo, so
//
//   * `openReview` read a return from a box as a repo switch and threw away every summary read and
//     every row expanded, then asked for all of them again; and
//   * `revFetchSummary` built `/api/repos/undefined/review/N/summary`, whose honest 404 — "no such
//     repo" — was then written into the row as if it were a reading of the pull request.
//
// The second was permanent: the pump skips any PR already in `revSums`, so a row that failed to be
// fetched showed a transport error for ever and nothing ever asked again.
//
// The real `openReview`/`loadReview`/`renderReview`/`revFetchSummary` run here against a stubbed
// fetch that answers the way the server does, including its 404.
//
//   node tests/ui/review_return.mjs
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { grab, harness, page, pure } from "./lift.mjs";

// The owner's real queue, captured 2026-08-25 — 54 open pull requests on `acme/thing`, trunk
// `develop`. See tests/ui/fixtures/README.md for why it is kept whole.
const THING = JSON.parse(readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "fixtures", "thing-queue.json"), "utf8"));

const t = harness();

// The page's world, stubbed down to what these functions touch.
function board() {
  const asked = [];
  const body = `
    let view = { box: null, mode: "term", kind: "agent" };
    const repos = [{ id: "alpha" }, { id: "beta" }];
    let revQueue = null, revOpen = new Set(), revSums = new Map(), revMods = null;
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    let revReading = null, revReturnScroll = 0, revSearch = "";
    const revDiffs = new Map(), revNotes = new Map();
    let revFilter = "all", revLoading = false, revStaleTimer = null;
    let revRepoFilter = "";
    let revCrits = new Map();
    // Read-ahead ON for both fixture repos: it is the pump's SCOPE (review::unasked_scope,
    // SKEIN-242), so an empty map here would make every assertion about what the pane reads on its
    // own vacuous.
    const revFlows = new Map([["alpha", { read_prs: true }], ["beta", { read_prs: true }]]);   // read-ahead ON by default; readAhead() below moves it
    let revModsOpen = false, revCounts = [];
    let revSumBusy = 0;
    const REV_SUM_PARALLEL = 3;
    // Whose move a pull request is, from cockpit/src/move.mjs — the one place that rule lives
    // (SKEIN-302). The pane groups on it, the badge counts it and the row's mark paints it, so a
    // world that stubbed it would be proving a second copy of the rule rather than the rule.
    ${pure("move")}
    ${grab("revSeen")}
    ${grab("rk")}
    ${grab("revSel")}
    ${grab("revSelAt")}
    ${grab("revNav")}
    ${grab("revStacks")}
    ${grab("revFlash")}
    ${grab("revChord")}
    ${grab("revLastActKey")}
    ${grab("revHunkAt")}
    ${grab("revFileAt")}
    ${grab("revRenderQueued")}
    ${grab("revNavSettle")}
    ${grab("revRenderHeld")}
    ${grab("revKeyHunks")}
    ${grab("revKeyMark")}
    ${grab("revRenderFlush")}
    ${grab("revStackKey")}
    ${grab("revRkQuery")}
    ${grab("revKeySelect")}
    ${grab("revKeyShowSel")}
    ${grab("revKeyShow")}
    ${grab("revMergeQueues")}
    ${grab("revScopeRepo")}
    ${grab("revTrunkOf")}
    ${grab("revChains")}
    ${grab("revStackName")}
    ${grab("REV_UNROOTED_WHY")}
    ${grab("revStepNo")}
    ${grab("revMisnamed")}
    ${grab("revStackNext")}
    ${grab("revStackLane")}
    ${grab("revStackOpenKey")}
    ${grab("revStackStep")}
    ${grab("toggleRevStack")}
    // Opening a row or a step fetches the prose the row shape left behind (SKEIN-287).
    ${grab("revLoadReading")}
    ${grab("toggleStackStep")}
    ${grab("revRail")}
    // One order, asked once (SKEIN-251): the lane sorts on this and the age cell renders it, so a
    // world that had only one of the two could not see them disagree.
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revStackRow")}
    ${grab("revStackSteps")}
    // Expanding a row asks for a stored draft only when the bulk payload did not already carry one
    // (SKEIN-216), so opening a row runs this.
    ${grab("revDraftedReview")}
    // The chip itself, not a proxy for it: SKEIN-243 was a queue where six reviews had been drafted
    // and paid for and not one row said so, so the assertion has to be the mark on the row.
    ${grab("revDraftAtHead")}
    ${grab("revReadyChip")}
    // Counted in the same minority tally as the drafted-review chip (SKEIN-275), so the pane's own
    // render needs it even though this world stubs the row that draws it. No backticks: see above.
    ${grab("revNoDraftWhy")}
    ${grab("toggleRevRow")}
    ${grab("revStaleTries")}
    ${grab("REV_STALE_TRIES")}
    ${grab("revHeld")}
    ${grab("REV_LANES")}
    ${grab("revNotReadyOpen")}
    ${grab("toggleNotReady")}
    ${grab("revNotReadyWhy")}
    // The two groups below your move both fold now (SKEIN-302), so the pane needs both switches
    // and the sentence each heading states its composition with.
    ${grab("revTheirsOpen")}
    ${grab("toggleTheirs")}
    ${grab("REV_FOLDS")}
    ${grab("revFolds")}
    ${grab("revFoldOpen")}
    ${grab("revTheirsWhy")}
    // The two empty states the pane can be in: nothing here, and nothing anywhere (SKEIN-154).
    ${grab("revLaneEmpty")}
    // The one place that decides whether the calm headline is a claim the pane has earned.
    ${grab("revUnasked")}
    ${grab("revClearHtml")}
    ${grab("loadKnownSummaries")}
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    ${grab("revPumpSummaries")}
    ${grab("revFetchSummary")}
    ${grab("revMatchesFilter")}
    ${grab("openReview")}
    // loadReview starts the in-flight poll (SKEIN-333). Stubbed: this suite is about what the
    // queue load does, and a real poll would ask the network on every case.
    const revPollInFlight = () => {};
    ${grab("loadReview")}
    ${grab("revSnooze")}
    ${grab("revSnoozeRed")}
    ${grab("renderReview")}
    // SKEIN-268: the paint is guarded, each row is guarded, and a fault is said out loud.
    ${grab("lastPageError")}
    ${grab("reportPageError")}
    ${grab("revRowBrokenHtml")}
    ${grab("revRowSafe")}
    ${grab("revRenderFailed")}
    ${grab("revRenderPane")}
    // The press's own render, which is not deferred (SKEIN-264).
    ${grab("renderReviewNow")}
    ${grab("renderReading")}
    ${grab("revMovedNotice")}
    ${grab("revWaitedSince")}
    ${grab("revReadBand")}
    ${grab("revMoved")}
    ${grab("revMatchesSearch")}
    ${grab("revSearchSet")}
    // The reading view is pointed at a COMMIT (SKEIN-254), so it can be missed by a moved head
    // instead of showing one commit's code for the life of the tab.
    ${grab("revDiffKey")}
    ${grab("revReadingKey")}
    ${grab("revDiffRead")}
    ${grab("revDiffBusy")}
    ${grab("revReadingLoad")}
    ${grab("revReloadReading")}
    ${grab("readingFiles")}
    ${grab("openReading")}
    ${grab("closeReading")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    ${grab("REV_UNDO_MS")}
    ${grab("revPending")}
    ${grab("revDecided")}
    ${grab("revReceiptHtml")}
    ${grab("revBarHtml")}
    const revRenderNotes = () => {};
    const revComposeHtml = () => "";
    const renderDiff = txt => '<div class="diff">' + txt.split(String.fromCharCode(10)).map(l => '<span class="ln">' + l + '</span>').join('') + '</div>';
    // Stubbed: this suite asks WHAT is on screen, not how a row is drawn.
    // One row can be made to throw, which is the only way to prove that a row's fault stays a row's
    // (SKEIN-268). Real data does it through marked.parse on model prose, a malformed draft, a
    // workflow with a shape nobody expected — all per-row, all inside this one call. No backticks
    // in here: this whole body is a template literal and one would end it mid-world.
    let breaks = [];
    const revRow = pr => {
      if (breaks.includes(pr.number)) throw new TypeError("cannot read properties of undefined (reading 'map')");
      return "<row n=" + pr.number + ">";
    };
    const revBody = () => "";
    const revModsCount = () => "notes";
    const revModsHtml = () => "";
    const revAgo = () => "just now";
    const renderRevBadge = () => {};
    const applyView = () => {};
    const persistView = () => {};
    const toasts = [];
    const toast = m => toasts.push(m);
    // Stubbed: this world sets revFlows directly, so the answer is already here. What matters is
    // that switching read-ahead ASKS again, which the request log below shows.
    let workflowReloads = 0;
    const loadWorkflows = () => { workflowReloads++; };
    let revEdit = null;
    const revEditHtml = () => "";
    // Real, not stubbed (SKEIN-282): the control has to exist on the queue a person is actually
    // looking at, and a stub returning "" is exactly the state this item is about.
    ${grab("revReadScope")}
    ${grab("revReadChip")}
    ${grab("revSetReadingFor")}
    ${grab("revSetReadingAll")}
    // The page's own scheduler, so the test can see WHEN it would ask again rather than waiting.
    
    return {
      open: id => openReview(id),
      // Clicking a box: the dock's own view change, verbatim from \`showBox\`.
      box: name => { view = { box: name, mode: "term", kind: "agent" }; },
      rows: () => (revpane.innerHTML.match(/<row /g) || []).length,
      breakRow: ns => { breaks = ns; },
      // NOT \`broken\` — board() already returns that name for repos whose queue failed, and the
      // outer spread would silently shadow this one into undefined.
      brokenRows: () => (revpane.innerHTML.match(/class="revrow broken"/g) || []).length,
      sums: () => [...revSums.values()].filter(s => s !== "…").length,
      got: (n, repo) => revSums.get((repo || "alpha") + "#" + n),
      open_rows: () => revOpen.size,
      openKeys: () => [...revOpen],
      expand: (n, repo) => { revOpen.add((repo || "alpha") + "#" + n); },
      tries: () => revStaleTries,
      fetchOne: (n, repo) => revFetchSummary(repo || "alpha", n, "force"),
      toggleNR: () => toggleNotReady(),
      toggleTheirs: () => toggleTheirs(),
      // What j/k walks, which IS the drawn list in its drawn order — a folded group contributes
      // nothing to it, because a selection must never sit on a row nobody can see.
      nav: () => revNav.slice(),
      // The whose-move headings as a reader sees them, with the count each one claims. Built with
      // RegExp rather than a literal: this whole world is a template literal, so a backslash here
      // is eaten before the regex ever exists.
      heads: () => [...revpane.innerHTML.matchAll(
        new RegExp('<h4[^>]*>([^<]*)<span class="revn">([0-9]+)</span>', "g"))]
        .map(m => [m[1].trim(), Number(m[2])]),
      stack: key => toggleRevStack(key),
      chains: () => revChains(revQueue.prs || []),
      row: key => toggleRevRow(key),
      openRow: () => [...revOpen],
      search: q => revSearchSet(q),
      common: () => [...revCommonChips],
      chip: (n, repo) => revReadyChip(((revQueue || {}).prs || [])
        .find(p => p.repo_id === (repo || "alpha") && p.number === n)),
      bands: () => ((revQueue && revQueue.prs) || []).map(p => [p.number, revReadBand(p)]),
      snoozeRed: () => revSnoozeRed(),
      read: (repo, n) => openReading(repo, n),
      back: () => closeReading(),
      reading: () => revReading,
      // "show the new code" — the moved notice's own press (SKEIN-254).
      reload: sha => revReloadReading(sha),
      note: (key, path, line, body) => { revNotesFor(key).push({ path, line, body }); revNotesSave(key); },
      notes: key => revNotesFor(key).slice(),
      // The owner's per-repo consent as the workflows payload carries it (SKEIN-282).
      readAhead: map => {
        for (const [id, on] of Object.entries(map)) revFlows.set(id, { ...(revFlows.get(id) || {}), read_prs: on });
        renderReviewNow();
      },
      readChip: () => revReadChip(),
      readAll: on => revSetReadingAll(on),
      readOne: (id, on) => revSetReadingFor(id, on),
      toasts: () => toasts.slice(),
      // A push landing on a row the pane already holds — what a queue refresh does to it, without
      // a refetch this world would have to fake anyway (SKEIN-254).
      moveHead: (n, sha) => {
        const p = ((revQueue || {}).prs || []).find(x => x.number === n);
        if (p) p.head_sha = sha;
        renderReviewNow();
      },
    };
  `;
  // Settled by default — two hours since the head commit. A pull request skein has no commit date
  // for, or one pushed to minutes ago, is not read on its own, so a fixture without this reads as
  // an empty queue and every assertion about reading would be vacuous.
  const SETTLED = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  let hot = [];            // numbers whose head commit landed just now
  let moved = [];          // numbers whose head has moved since it was read
  const pushed = {};       // numbers whose branch has been pushed to since the pane read them
  let fresh = true;          // whether the server has the current list yet
  let laneRows = null;       // when set, the queue serves exactly these rows
  let blindSpots = [];       // what each served repo reports it could not see
  const queue = id => ({
    repo_id: id,
    ai: true,
    fresh,
    // The branch everything here is ultimately for. `revChains` severs the stack at it BY NAME
    // (SKEIN-288), so a fixture without it exercises the no-trunk fallback instead of the rule.
    trunk,
    prs: laneRows || [1, 2, 3, 4, 5, 6].map(n => ({
      number: n,
      lane: "needs-you",
      draft: false,
      head_sha: id + n + (moved.includes(n) ? "-moved" : ""),
      committed_at: hot.includes(n) ? new Date().toISOString() : SETTLED,
      // The server's answer, not the page's. `prq::settled` decides; the page renders it, so a
      // fixture that only carried the timestamp would be testing a rule the page no longer has.
      settled: !hot.includes(n),
      reasons: ["reviewer"],
    })),
    // What this repo's queue could not SEE — a standing gap, distinct from a queue that could not
    // be built at all (`broken` below).
    blind_spots: blindSpots,
  });
  const lanes = rows => { laneRows = rows; };
  let trunk = "";            // what the queue reports as this repo's trunk
  let served = ["alpha"];    // which repos the merged answer carries
  // Answers on demand rather than immediately, so a test can look at the pane between a request and
  // its answer — which is where both defects lived.
  let pending = [];
  let refuse = null;              // a repo whose summaries the server will not serve
  let outage = null;              // when set, the merged queue route answers with this failure
  let brokenRepos = [];           // repos whose queue the merged answer reports as failed
  let unaskedRepos = [];          // repos the merged answer says it deliberately did not ask about
  let known = {};                 // readings already on disk, as the bulk route answers them
  let drafted = [];               // numbers the merged model call drafted a review for, as it read
  const fetch = (url) => {
    asked.push(url);
    // The merged queue: every repo in one answer, the shape the pane opens on (SKEIN-146).
    if (/^\/api\/review($|\?)/.test(url)) {
      if (outage) {
        return new Promise(resolve => pending.push(() => resolve({
          ok: false, status: 502, text: () => Promise.resolve(outage),
        })));
      }
      return new Promise(resolve => pending.push(() => resolve({
        ok: true,
        text: () => Promise.resolve(JSON.stringify({
          ai: true, queues: served.map(queue),
          failed: brokenRepos.map(id => ({ repo_id: id, needs_you: 0, error: "boom", skipped: "" })),
          // `MergedQueue::skipped` — a repo skein deliberately did not ask about, carried rather
          // than omitted so the pane can tell it from a repo with nothing waiting (src/prq.rs).
          skipped: unaskedRepos,
        })),
      })));
    }
    // Per-repo workflow state and stored critiques: nothing, immediately — these worlds are about
    // queues and readings.
    if (/\/workflows$/.test(url) || /\/critique$/.test(url)) {
      return Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
    }
    if (/\/snooze$/.test(url)) {
      return Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true }) });
    }
    // Per-repo read-ahead consent (SKEIN-282). One POST per repo, whichever affordance pressed it.
    if (/\/reading$/.test(url)) {
      return Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true }) });
    }
    // The change itself, the shape /api/repos/:id/review/:n/diff serves (SKEIN-148).
    const rd = url.match(/review\/(\d+)\/diff$/);
    if (rd) {
      const diff = [
        "diff --git a/src/lib.rs b/src/lib.rs",
        "--- a/src/lib.rs",
        "+++ b/src/lib.rs",
        "@@ -1,2 +1,3 @@",
        " fn keep() {}",
        "+fn added() {}",
        " fn tail() {}",
        "diff --git a/docs/note.md b/docs/note.md",
        "--- a/docs/note.md",
        "+++ b/docs/note.md",
        "@@ -1 +1 @@",
        "-old line",
        "+new line",
      ].join(String.fromCharCode(10));
      // The route has no sha argument: it serves whatever is at the head NOW, which is exactly why
      // the page files the answer under the answer's OWN head (SKEIN-254).
      const at = pushed[rd[1]];
      return Promise.resolve({ ok: true, json: () => Promise.resolve({
        head_sha: at || ("sha" + rd[1]),
        diff: at ? diff.replace("fn added() {}", "fn added_again() {}") : diff,
        cut: false,
      }) });
    }
    const id = decodeURIComponent(url.match(/repos\/([^/]+)\//)[1]);
    // The bulk read: what skein already holds. This fixture holds nothing — every scenario here is
    // about what the pane ASKS for, so an empty answer keeps the pump as the only source and the
    // assertions about it meaningful.
    // `?rows=1` since SKEIN-287 — the row shape. Matched with the query, or the stub answers a
    // call the page does not make and the pump becomes the only source.
    if (/review\/summaries(\?|$)/.test(url)) {
      return new Promise(resolve => pending.push(() => resolve({
        ok: true,
        // This route is read with `r.json()`, like the workflow one — the queue and the per-PR
        // summaries are read with `r.text()`. Both shapes, or the stub answers a call the page does
        // not make.
        json: () => Promise.resolve(known),
        text: () => Promise.resolve(JSON.stringify(known)),
      })));
    }
    const sum = url.match(/review\/(\d+)\/summary/);
    return new Promise(resolve => pending.push(() => {
      // Word for word what the server answers for a repo id it does not know.
      if (sum && (refuse === id || id === "undefined")) {
        return resolve({ ok: false, status: 404, text: () => Promise.resolve("no such repo") });
      }
      if (sum) {
        const n = Number(sum[1]), head = id + sum[1];
        return resolve({ ok: true, text: () => Promise.resolve(JSON.stringify({
          number: n, head_sha: head, depth: "line", line: "x", computed: true,
          // `review::known_at` — the summary flattened, with the review drafted at THIS head beside
          // it. One model call produces both, so the route that answers one answers both
          // (SKEIN-236); a fixture answering a bare summary would be testing a server that is gone.
          ...(drafted.includes(n)
            ? { has_critique: true,
                // `Known::drafted` — the row's own vocabulary for "there is a review, at this head,
                // with this many comments". It survives `Known::thin`, and `revReadyChip` reads it
                // rather than the critique object (SKEIN-287).
                drafted: { head_sha: head, comments: 0 },
                critique: { number: n, head_sha: head, overall: "one thing", comments: [] } }
            : {}),
        })) });
      }
      resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(queue(id))) });
    }));
  };
  const revpane = { innerHTML: "", classList: { toggle() {} } };
  const document = { body: { classList: { remove() {}, toggle() {} } } };
  const localStorage = {
    store: {}, getItem(k) { return this.store[k] ?? null; }, setItem(k, v) { this.store[k] = v; },
  };
  // Timers the test drives, so "it asks again in four seconds, then eight" is observable without
  // any waiting — and so a page that STOPPED asking is visibly different from one that is waiting.
  const waits = [];
  const made = new Function(
    "revpane", "document", "localStorage", "fetch", "esc", "encodeURIComponent",
    "decodeURIComponent", "setTimeout", "clearTimeout", "console", body,
  )(
    revpane, document, localStorage, fetch, String, encodeURIComponent,
    decodeURIComponent,
    (fn, ms) => { waits.push({ fn, ms }); return waits.length; },
    () => {},
    console,
  );
  // One round of answers: what is outstanding right now, and not what those answers go on to ask
  // for. The pump refills itself, so the two are different moments and both matter here.
  const settle = async () => {
    const round = pending;
    pending = [];
    round.forEach(f => f());
    await new Promise(r => setTimeout(r, 0));
  };
  return {
    ...made,
    settle,
    lanes,
    trunk: t => { trunk = t; },
    pane: () => revpane.innerHTML,
    // Everything, until it stops asking.
    drain: async () => { for (let i = 0; i < 20 && pending.length; i++) await settle(); },
    refuse: id => { refuse = id; },
    serves: ids => { served = ids; },
    // The server has not caught up yet: it hands over the copy it remembers.
    stale: on => { fresh = !on; },
    broken: ids => { brokenRepos = ids; },
    // Repos skein never asked GitHub about — review queue switched off, or no remote.
    unasked: rows => { unaskedRepos = rows; },
    // GitHub, or skein's own route, refusing the whole queue — not one repo of nine.
    outage: why => { outage = why; },
    // A standing gap inside a queue that did arrive — `gh` without read:org is the live one.
    blinds: bs => { blindSpots = bs; },
    // What the page asked to be woken for, and how long it wanted to wait.
    waits: () => waits.slice(),
    fire: () => { const due = waits.map(w => w.fn); waits.length = 0; due.forEach(f => f()); },
    // A pull request somebody has just pushed to.
    hot: ns => { hot = ns; },
    // Readings skein already holds, which the pane must show without asking for any of them.
    holds: map => { known = map; },
    // Which pull requests the reading itself comes back with a drafted review for — the merged
    // model call's other half, on the answer to the very request that triggered it.
    drafts: ns => { drafted = ns; },
    // The branch moving under a reading that has already been made.
    moved: ns => { moved = ns; },
    // Somebody pushed: the queue learns the new head AND the diff route starts serving it, which
    // is the pair that makes a per-PR diff cache go stale silently (SKEIN-254).
    push: (n, sha) => { pushed[n] = sha; made.moveHead(n, sha); },
    // Summary requests only — the queue's own fetches are not what these counts are about.
    reads: () => asked.filter(u => /review\/\d+\/summary/.test(u)),
    diffs: () => asked.filter(u => /review\/\d+\/diff$/.test(u)),
    posts: () => asked.filter(u => u.includes("/snooze")),
    // NOT `reading` — the world already returns that name for the view, and the outer spread would
    // shadow it into the request log (the same trap `brokenRows` is named around).
    readingPosts: () => asked.filter(u => /\/reading$/.test(u))
      .map(u => decodeURIComponent(u.match(/repos\/([^/]+)\//)[1])),
  };
}

// ---- a trip to a box and back: everything the pane knew is still known ----
{
  const b = board();
  b.open("alpha");
  await b.drain();
  t.check("the queue paints on the first visit", b.rows(), 6);
  t.check("and its rows are read", b.sums(), 6);

  b.expand(2);
  // What the FIRST visit asked for is legitimate — those are the reads that filled the pane. The
  // question is whether coming back asks for any of it a second time, so the count is taken here.
  const before = b.reads().length;

  b.box("some-box");
  b.open();   // the PR button, which passes no repo id

  // BEFORE the queue comes back. This is the moment the pane was empty of everything it knew.
  t.check("coming back paints immediately, from what is already known", b.rows(), 6);
  t.check("the summaries that were read are still read", b.sums(), 6);
  t.check("and a row you had expanded is still expanded", b.open_rows(), 1);
  t.check("nothing is read again", b.reads().length, before);

  await b.drain();
  t.check("and the refetch leaves it that way", b.rows(), 6);
  t.check("summaries survive the refetch too", b.sums(), 6);
  t.check("which asked for nothing further", b.reads().length, before);
}

// ---- two repos, one queue: a repo is a filter, and filtering forgets nothing ----
//
// The old rule — "switching repos drops the previous repo's summaries" — existed because PR
// numbers collide and the maps were keyed by number alone. The merged queue keys every row by
// `repo#number` instead, so alpha's #2 can never open as beta's #2 — and with the collision gone,
// forgetting stops being a safety measure and becomes a bug (SKEIN-175). What was read for one
// repo stays read while you look at another.
{
  const b = board();
  b.serves(["alpha", "beta"]);
  // Every reading already on disk, in both repos — free, so the counts here are about keeping,
  // not spending.
  b.holds(Object.fromEntries([1, 2, 3, 4, 5, 6].map(n =>
    [n, { number: n, head_sha: "x" + n, depth: "line", line: "read " + n }])));
  b.open("alpha");
  await b.drain();
  t.check("the filtered view shows one repo's rows", b.rows(), 6);
  t.check("while both repos' readings are held", b.sums(), 12);
  b.expand(2);

  b.box("some-box");
  b.open("beta");
  t.check("filtering to another repo keeps every reading", b.sums(), 12);
  t.check("and alpha's expansion is alpha's, not beta's #2", b.openKeys(), ["alpha#2"]);
  await b.drain();
  t.check("beta's six rows are what is shown", b.rows(), 6);
}

// ---- a summary that lands while you are looking at a box ----
//
// The reported one: reading `view.repo` at fetch time asked for a repo called "undefined", and the
// server's honest 404 was stored as that row's reading.
{
  const b = board();
  b.open("alpha");
  await b.settle();          // the queue lands; the pump starts asking for summaries
  b.box("some-box");         // and you go to look at a box while they are in flight
  await b.drain();

  t.check("summaries ask for the repo they belong to",
    b.reads().filter(u => u.includes("undefined")), []);
  t.check("and they are kept when they land", b.sums(), 6);
  t.check("none of them reads as a failure", b.got(1).unread_because, undefined);
}

// ---- a fetch that failed is asked again, not kept for ever ----
//
// A transport failure has to be VISIBLE — a network error must never look like a clear PR — but the
// pump skips any row already in `revSums`, so a failure stored as though it were a reading is the
// last word on that row for the life of the page.
{
  const b = board();
  b.refuse("alpha");
  b.open("alpha");
  await b.drain();
  t.check("a fetch that failed says so on the row",
    String(b.got(1).unread_because).includes("no such repo"), true);

  b.refuse(null);            // whatever it was, it is over
  b.open();                  // and you come back to the queue
  await b.drain();
  t.check("coming back asks again rather than keeping the failure", b.got(1) && b.got(1).line, "x");
  t.check("and the row is read", b.sums(), 6);
}

// ---- a pull request still being pushed to is read like any other ----
//
// It used to be left alone for an hour ("PR should be analyzed in background once there have been
// no commits for atleast an hour"), and the owner removed that on 2026-08-24: the day's spend
// ceiling is the money guard now (`review::over_budget`), and re-anchoring a drafted comment by its
// line text (SKEIN-214/215) made a reading of a moving head worth keeping. The page held a SECOND
// copy of that hour after the server dropped it — `revSettled` — which could only ever refuse rows
// skein's own background reader was already reading.
{
  const b = board();
  b.hot([2, 5]);           // two of the six were pushed to a moment ago
  b.open("alpha");
  await b.drain();

  t.check("a branch that is still moving is read", b.got(2) && b.got(2).line, "x");
  t.check("it was asked for like the rest", b.reads().filter(u => /\/2\/summary/.test(u)).length, 1);
  t.check("and the whole lane is read, not a settled subset", b.sums(), 6);

  // A read somebody asks for carries the marker that exempts it from the day's budget — the
  // ceiling is on skein's initiative, never on the person.
  b.fetchOne(3);
  await b.drain();
  t.check("a read you ask for says so on the wire",
    b.reads().some(u => /\/3\/summary\?redraft=1$/.test(u)), true);
}

// ---- a reading survives the commits that land after it ----
//
// Asked for: "if new commits come in show that there have been new commits since analysis and I
// will trigger reanalysis manually". It used to be deleted, which is what made the pump read the
// same pull request again on its own.
{
  const b = board();
  b.open("alpha");
  await b.drain();
  const before = b.reads().length;
  t.check("read once", b.got(1) && b.got(1).line, "x");

  b.moved([1]);            // somebody pushes to #1
  b.open();                // and the queue is read again
  await b.drain();

  t.check("the reading is still there", b.got(1) && b.got(1).line, "x");
  t.check("marked as being of an earlier commit", b.got(1) && b.got(1).stale, true);
  t.check("and it was not read again on its own", b.reads().length, before);

  // The manual re-read is the only thing that replaces it.
  b.fetchOne(1);
  await b.drain();
  t.check("asking again replaces it", b.got(1) && b.got(1).stale, undefined);
}

// ---- a queue served stale keeps asking until it is current ----
//
// Reported live: "sometimes the list of PRs just shows 34 while the PR button shows 44 and it
// suddenly shows up later."
//
// Two sources of different ages. The badge polls `/api/review/counts` on its own; the pane's list
// comes back REMEMBERED (`fresh: false`) while the server fetches the real one. The pane then asked
// again exactly once, four seconds later, and gave up — so any GitHub read slower than that left the
// older list on screen under a newer number, until something else happened to reload it. That
// "something else" is the "later".
{
  const b = board();
  b.stale(true);
  b.open("alpha");
  await b.drain();
  t.check("a remembered queue still paints", b.rows(), 6);

  // It wants to be woken, and with a growing gap each time: the reason it was stale is that GitHub
  // is slow, so asking again at the same interval only asks more often for the same reason.
  const gaps = [];
  for (let i = 0; i < 4; i++) {
    const due = b.waits().filter(w => w.ms >= 4000);
    if (!due.length) break;
    gaps.push(due[due.length - 1].ms);
    b.fire();
    await b.drain();
  }
  t.check("it asks again, and backs off each time", gaps, [4000, 8000, 16000, 32000]);

  // And it gives up eventually rather than asking for ever.
  for (let i = 0; i < 6; i++) { b.fire(); await b.drain(); }
  t.check("it gives up rather than asking for ever", b.tries(), 5);

  // The server catches up. The pane converges on its own — which is the whole complaint.
  b.stale(false);
  b.open();
  await b.drain();
  t.check("and once the server is current, so is the pane", b.rows(), 6);
  t.check("the chase resets for the next time", b.tries(), 0);
}

// ---- what skein already knows shows up at once, whatever the limits are ----
//
// Reported live: "it is on latest build but still I can only see 2 PRs with summaries while before
// there were a bunch of them" … "shouldn't they just load if they are just reading from disk".
//
// Exactly right, and the cause was that the pane learned what skein knew only by asking for one
// pull request at a time — down the same call that COMPUTES a reading. So every limit meant to bound
// money also bounded memory: a draft's reading was hidden, an unsettled branch's was hidden, and
// everything past the sixth row was hidden because the loop stops when the allowance is gone.
{
  const b = board();
  // Six pull requests, and skein has already read all of them. Two are drafts, two were pushed to a
  // minute ago — every one of those was invisible before, and none of them costs anything now.
  b.hot([3, 4]);
  b.holds({
    "1": { number: 1, head_sha: "alpha1", depth: "line", line: "read one" },
    "2": { number: 2, head_sha: "alpha2", depth: "line", line: "read two" },
    "3": { number: 3, head_sha: "alpha3", depth: "line", line: "read three" },
    "4": { number: 4, head_sha: "alpha4", depth: "line", line: "read four" },
    "5": { number: 5, head_sha: "alpha5", depth: "line", line: "read five" },
    // A reading of an earlier commit, which the server marks rather than hides.
    "6": { number: 6, head_sha: "old", depth: "line", line: "read six", stale: true },
  });
  b.open("alpha");
  await b.drain();

  t.check("every reading skein holds is on screen", b.sums(), 6);
  t.check("including one of an earlier commit, marked", b.got(6).stale, true);
  t.check("and nothing was asked for one at a time", b.reads(), []);
}

// ---- and what is genuinely unread still obeys every limit ----
{
  const b = board();
  b.hot([5, 6]);
  b.holds({ "1": { number: 1, head_sha: "alpha1", depth: "line", line: "read one" } });
  b.open("alpha");
  await b.drain();

  // #1 came free, off disk. Everything else is genuinely unread, so it is asked for — including
  // the two being pushed to, since the settle hour is gone from both halves now. What bounds this
  // is the server's day ledger, spent where the model call is.
  const asked = b.reads().map(u => Number(u.match(/review\/(\d+)\/summary/)[1])).sort();
  t.check("only what is missing is asked for", asked, [2, 3, 4, 5, 6]);
  t.check("and everything known or read is on screen", b.sums(), 6);
}

// ---- a lane says whose move it is, not whether you have already acted ----
//
// The done-when fixture from SKEIN-139, at the pane: a red PR, a draft, a conflicted one, one of
// yours and one genuinely awaiting you — the last is the only row in "your move", the not-ready
// three are a COUNT that states its own composition, and expanding it is one click. Nothing is
// hidden: the rows are all there behind the fold.
{
  const b = board();
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  const pr = (n, extra) => ({
    number: n, head_sha: "alpha" + n, committed_at: at, settled: true, draft: false,
    reasons: ["reviewer"], checks: "none", ...extra,
  });
  b.lanes([
    // Red, and STILL your move: on this fleet CI runs only after review, so failing checks are
    // the ordinary state of a PR awaiting you — demoting them hid live PRs (reported as "some
    // PRs are cut out from the view, including 577").
    pr(1, { lane: "needs-you", checks: "failing" }),
    pr(2, { lane: "not-ready", draft: true }),
    pr(3, { lane: "not-ready", mergeable: false }),
    pr(4, { lane: "waiting", reasons: ["author"] }),
    pr(5, { lane: "needs-you" }),
  ]);
  b.open("alpha");
  await b.drain();

  t.check("one your-move list at the top, and two folded groups under it",
    b.heads(), [["your move", 2], ["waiting on others", 1], ["not ready", 2]]);
  t.check("a red PR awaiting review is drawn with your move, not folded away", b.rows(), 2);
  t.check("the not-ready fold states its own composition",
    b.pane().includes("1 draft, 1 conflicted"), true);
  t.check("and so does waiting-on-others", b.pane().includes("1 you opened"), true);

  b.toggleNR();
  t.check("one click and every not-ready row is there — folded is not hidden", b.rows(), 4);
  b.toggleTheirs();
  t.check("and the same for the one you opened", b.rows(), 5);
}

// ---- the actual review: what the pane posts is exactly what was kept ----
//
// The vetting happens HERE, in the pane — the server posts whatever this sends. So the property
// that "a dropped comment is not posted" lives in `revCritiquePost`, and is proven against the
// real function with a fetch that records what it was given.
function critWorld() {
  const sent = [];
  const toasts = [];
  const painted = [];
  const asked = [];               // every question put to a native dialog, if any still is
  const due = [];                 // timers, fired by the test rather than waited for
  const body = `
    let view = { repo: "*", box: null };
    let revQueue = { prs: [] };
    ${grab("esc")}
    ${grab("rk")}
    ${grab("REV_UNDO_MS")}
    ${grab("revCrits")}
    ${grab("revCritKeep")}
    ${grab("revCritiquePost")}
    ${grab("revCritiqueUndo")}
    ${grab("revCritiqueTick")}
    ${grab("revCritiqueFire")}
    ${grab("revCritActsPaint")}
    ${grab("revCritActsHtml")}
    ${grab("revCritiqueHtml")}
    // The strip offers a second control now — approve with this review (SKEIN-273) — and stands
    // down for a verdict already held on the pull request, so it reaches the verdict hold and the
    // receipt that hold draws. Whether either is right is undo.mjs's, which owns the window; here
    // they only have to exist so the panel can be drawn at all.
    let revSums = new Map(), revNotes = new Map();
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    ${grab("revPending")}
    ${grab("revReceiptHtml")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revDraftedReview")}
    ${grab("revReviewToPost")}
    ${grab("revApproveWithReviewHtml")}
    // Which render each press asked for. The press-time one must be the FORCING one — a press that
    // paints only once the reader lets go of a selection is a press that looks like it did nothing.
    const renderReview = () => painted.push("deferred");
    const renderReviewNow = () => painted.push("now");
    // The strip, modelled as an element the surgical repaint can actually replace. Without it the
    // paint falls through to a render and the whole point of being surgical — the answer showing up
    // while the reader still holds a caret — is untestable outside a browser.
    const strip = { outerHTML: "" };
    const revpane = {
      querySelector: q => (q.startsWith("[data-critacts=") ? strip : null),
      querySelectorAll: () => [],
    };
    const toast = m => toasts.push(m);
    // A native dialog must no longer be able to swallow this press, so the world records every
    // question put to one and answers no — the shape of a browser told to suppress them.
    const confirm = q => { asked.push(q); return false; };
    const fetch = (url, opts) => {
      sent.push({ url, body: JSON.parse(opts.body) });
      return Promise.resolve({ json: () => Promise.resolve({ ok: true, text: "posted" }) });
    };
    return {
      seed: (repo, n, critique) => {
        revQueue.prs.push({ repo_id: repo, number: n, head_sha: (critique || {}).head_sha });
        revCrits.set(repo + "#" + n, { open: true, busy: false, posting: false, critique,
                                       drop: new Set(), posted: "", hold: null, said: "" });
      },
      drop: (repo, n, i) => revCritKeep(repo + "#" + n, i, false),
      post: (repo, n, sha) => revCritiquePost(repo, n, sha),
      undo: (repo, n) => revCritiqueUndo(repo + "#" + n),
      // What the strip says right now — the receipt IS the acknowledgement, so this is where a
      // press either landed or did not.
      acts: (repo, n) => revCritActsHtml({ repo_id: repo, number: n,
        head_sha: (revCrits.get(repo + "#" + n).critique || {}).head_sha }),
      // A post already in flight. The only state left in which there is nothing to press.
      inflight: (repo, n) => { revCrits.get(repo + "#" + n).posting = true; },
      html: pr => revCritiqueHtml(pr),
      // What the surgical repaint last wrote into the strip.
      painted: () => strip.outerHTML,
    };
  `;
  const world = new Function(
    "sent", "toasts", "painted", "asked", "setTimeout", "clearTimeout", body,
  )(sent, toasts, painted, asked, fn => { due.push(fn); return due.length; },
    id => { if (id) due[id - 1] = null; });
  // The eight seconds, driven rather than waited for: what the window IS belongs to undo.mjs, which
  // owns it; what matters here is what happens on each side of it.
  return {
    world, sent, toasts, painted, asked,
    lapse: () => due.splice(0).forEach(f => f && f()),
    // How many timers are armed. A second press must not arm a second window — with `sent` alone
    // that is invisible, because the first hold is simply overwritten and its timer then finds
    // nothing to do.
    armed: () => due.filter(Boolean).length,
  };
}

// ---- the press is a receipt, not a modal (SKEIN-264) ----
//
// Posting used to be gated on a native `confirm`, and a browser where somebody once ticked "prevent
// this page from creating additional dialogs" answers every later call with false — so the press
// returned silently and no request was ever made. Reported by the owner as "it just doesn't work.
// idk why". SKEIN-162's hold-and-undo cannot be suppressed by a browser setting, it appears where
// the press was, and it is what every other verdict on this surface already does.
{
  const { world, sent, painted, asked, lapse } = critWorld();
  world.seed("alpha", 20, { head_sha: "h1", overall: "note", truncated: false,
    comments: [{ path: "a.rs", line: 2, anchored: true, text: "first" },
               { path: "b.rs", line: 5, anchored: true, text: "second — to be dropped" }] });
  world.drop("alpha", 20, 1);
  painted.length = 0;
  world.post("alpha", 20, "h1");

  t.check("no native dialog stands between the press and the post", asked, []);
  t.check("the press paints at once rather than waiting for the reader's hands", painted[0], "now");
  const held = world.acts("alpha", 20);
  t.check("and the button becomes the receipt, in place",
    held.includes("✓ posting 1 comment as one review"), true);
  t.check("with the way back", held.includes(`onclick="revCritiqueUndo('alpha#20')"`), true);
  t.check("and how long there is to take it", /data-crit-left="alpha#20">8s</.test(held), true);
  t.check("nothing has gone to GitHub while the window is open", sent.length, 0);

  lapse();
  t.check("when the window lapses it posts, once", sent.length, 1);
  t.check("carrying what was kept and not what was dropped",
    sent[0].body.comments.map(c => c.text), ["first"]);
  t.check("the head the draft read rides along", sent[0].body.head_sha, "h1");
  t.check("it posts to the row's own repo", sent[0].url.includes("/repos/alpha/"), true);
  t.check("the strip says the request is out, in place", world.painted().includes("✓ posting…"), true);

  // The ANSWER, which is the half a render cannot deliver: by the time it lands the reader may be
  // mid-word in another drafted comment, so it is written into the strip alone — the same surgery
  // `revPendingPaint` does for a row's receipt. A `renderReview()` here would be deferred and the
  // panel would sit on "posting…" for as long as the reader kept typing.
  const before = painted.length;
  await new Promise(r => setTimeout(r, 0));
  t.check("and when GitHub answers, the strip says so", world.painted().includes("✓ posted"), true);
  t.check("without a render that could be deferred out from under it",
    painted.slice(before), []);
}

{
  // Taking it back inside the window: nothing was sent, and the button is a button again.
  const { world, sent, lapse } = critWorld();
  world.seed("alpha", 21, { head_sha: "h1", overall: "note",
    comments: [{ path: "a.rs", line: 2, anchored: true, text: "only" }] });
  world.post("alpha", 21, "h1");
  world.undo("alpha", 21);
  t.check("undo puts the press back", world.acts("alpha", 21).includes("post 1 comment as one review"), true);
  lapse();
  await new Promise(r => setTimeout(r, 0));
  t.check("and the window it cancelled never fires", sent.length, 0);
}

{
  // Pressing twice inside the window is one review, not two. The second press has no button to land
  // on — the receipt replaced it — but the guard holds even if something calls it directly, and
  // what it protects is the WINDOW: without it the second press silently restarts the eight seconds
  // and orphans the first hold's countdown.
  const { world, sent, lapse, armed } = critWorld();
  world.seed("alpha", 22, { head_sha: "h1", overall: "note", comments: [] });
  world.post("alpha", 22, "h1");
  const after = armed();
  world.post("alpha", 22, "h1");
  t.check("a second press inside the window arms nothing new", armed(), after);
  lapse();
  await new Promise(r => setTimeout(r, 0));
  t.check("and two presses inside one window post one review", sent.length, 1);
}

{
  // Everything dropped and no note: refused in the pane, before any request exists to regret.
  const { world, sent, toasts } = critWorld();
  world.seed("alpha", 8, { head_sha: "h1", overall: "", comments: [{ path: "a.rs", line: 2, anchored: true, text: "only" }] });
  world.drop("alpha", 8, 0);
  world.post("alpha", 8, "h1");
  await new Promise(r => setTimeout(r, 0));
  t.check("nothing kept posts nothing", sent.length, 0);
  t.check("and says so", toasts.length >= 1, true);
}

{
  // Keeping or dropping a comment is a press too, and its only acknowledgement is the render: the
  // row greys, and the count in the post button changes. Measured in chromium: ticking a checkbox
  // leaves `document.activeElement` on the checkbox — an INPUT — so §6 rule 2 held every one of
  // these renders until the reader clicked somewhere else.
  const { world, painted } = critWorld();
  world.seed("alpha", 23, { head_sha: "h1", overall: "note",
    comments: [{ path: "a.rs", line: 2, anchored: true, text: "one" },
               { path: "b.rs", line: 3, anchored: true, text: "two" }] });
  painted.length = 0;
  world.drop("alpha", 23, 0);
  t.check("dropping a comment paints inside the frame", painted[0], "now");
  t.check("and the count on the press follows it",
    world.acts("alpha", 23).includes("post 1 comment as one review"), true);
}

{
  // A draft of an earlier commit. The pane names it — "you read an old commit" stays true and worth
  // knowing — but it does NOT take posting away: `review::post_critique` re-anchors each kept
  // comment by its line text (SKEIN-214/215), so refusing here would restore the treadmill by
  // itself. The `disabled` attribute is read off the post control in particular, because the row
  // textareas carry one too as soon as a comment is dropped.
  const { world } = critWorld();
  world.seed("alpha", 9, { head_sha: "old", overall: "x", comments: [] });
  const html = world.html({ repo_id: "alpha", number: 9, head_sha: "new" });
  t.check("a stale draft is named", html.includes("Drafted before the latest commits"), true);
  t.check("and it says the matching comments still land, at their new place",
    html.includes("still match will post at their new place"), true);
  t.check("and that the displaced ones travel in the note, naming the drafted commit",
    /the rest go into the review note, naming that commit/.test(html), true);
  const strip = world.acts("alpha", 9);
  t.check("posting a review drafted at a moved head is offered, not refused",
    strip.includes("disabled"), false);
  // The sha the server re-anchors FROM, and the reason it has to be entity-escaped: a bare
  // `JSON.stringify` puts the attribute's own delimiter inside it, the handler ends at the sha's
  // opening quote, and the click throws instead of posting (SKEIN-261).
  t.check("and the sha it posts under is the one the draft read, whole, inside the handler",
    strip.includes(`onclick="revCritiquePost('alpha', 9, &quot;old&quot;)"`), true);

  // A post already in flight has no button at all now, which is the SKEIN-162 rule: a control that
  // has been pressed is replaced by what became of the press, never left there to press again.
  world.inflight("alpha", 9);
  const busy = world.acts("alpha", 9);
  t.check("a post already in flight offers nothing to press again",
    busy.includes("revCritiquePost"), false);
  t.check("and says what is happening instead", busy.includes("✓ posting…"), true);
}

// ---- a stack of dependent pull requests is one row, opened in review order ----
//
// The finding behind SKEIN-147: 15 of 29 PRs on the live queue were one linear chain
// (ladder/tenants-*), scattered across the queue in near-reverse order, with the author's own
// numbering wrong at one step (#624 "slice 5" sits after "slice 6") — and nothing on any row said
// the chain existed. The queue below is that chain, in the item's own numbers.
{
  const b = board();
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  const chainPr = (n, head, base, title) => ({
    number: n, head_ref: head, base_ref: base, title,
    head_sha: "alpha" + n, committed_at: at, updated_at: at, settled: true, draft: false,
    reasons: ["reviewer"], checks: "failing", lane: "needs-you", author: "dev-rhea",
  });
  const chain = [
    chainPr(613, "ladder/chassis-tenants",        "develop",                       "chore(ladder): the tenants chassis, empty"),
    chainPr(614, "ladder/tenants-01-compose",     "ladder/chassis-tenants",        "tenants slice 1: compose brings the module up"),
    chainPr(615, "ladder/tenants-02-tables",      "ladder/tenants-01-compose",     "tenants slice 2: the thing tables"),
    chainPr(616, "ladder/tenants-03-identity",    "ladder/tenants-02-tables",      "tenants slice 3: identity rows carry a tenant"),
    chainPr(617, "ladder/tenants-04-membership",  "ladder/tenants-03-identity",    "tenants slice 4: membership"),
    chainPr(618, "ladder/tenants-06-context-seam","ladder/tenants-04-membership",  "tenants slice 6: the context seam"),
    chainPr(624, "ladder/tenants-05-member-reads","ladder/tenants-06-context-seam","tenants slice 5: member reads go through the seam"),
    chainPr(626, "ladder/tenants-invites",        "ladder/tenants-05-member-reads","tenants: invitations, expiry and replay"),
    chainPr(628, "ladder/tenants-billing",        "ladder/tenants-invites",        "tenants: billing rows"),
    chainPr(631, "ladder/tenants-audit",          "ladder/tenants-billing",        "tenants: the audit trail"),
    chainPr(627, "ladder/tenants-exports",        "ladder/tenants-audit",          "tenants: exports"),
    chainPr(632, "ladder/tenants-webhooks",       "ladder/tenants-exports",        "tenants: webhooks"),
    chainPr(642, "ladder/tenants-quotas",         "ladder/tenants-webhooks",       "tenants: quotas"),
    chainPr(645, "ladder/tenants-migration",      "ladder/tenants-quotas",         "tenants: the migration"),
    chainPr(646, "ladder/tenants-cutover",        "ladder/tenants-migration",      "tenants slice 11: cut the old path over"),
  ];
  // Scattered, the way the live queue served them — newest tip first — plus one loose PR.
  const loose = { ...chainPr(700, "fix/null-deref", "develop", "fix a null deref"), checks: "none" };
  b.lanes([...chain].reverse().concat([loose]));
  b.open("alpha");
  await b.drain();

  t.check("the chain is one row, not fifteen",
    b.pane().includes("15 pull requests, one change"), true);
  t.check("no chain member appears as a loose row", b.rows(), 1);
  t.check("named after the branches it shares", b.pane().includes("ladder"), true);
  t.check("the heading counts starts and pull requests, both",
    b.pane().includes("from 16 pull requests"), true);

  b.stack("stack:alpha#613");
  const steps = b.pane().split('class="steps"')[1] || "";
  const order = [...steps.matchAll(/#(\d+)</g)].map(m => Number(m[1]));
  t.check("expanding lists every step in the order it must be reviewed",
    order, [613, 614, 615, 616, 617, 618, 624, 626, 628, 631, 627, 632, 642, 645, 646]);
  t.check("and contradicts the titles where they lie",
    steps.includes("named 05, sits after 06"), true);
  t.check("exactly once — the other steps' names are not second-guessed",
    (b.pane().match(/named \d\d, sits after/g) || []).length, 1);

  // Exclusive expansion: a stack is most of a viewport, so opening anything else closes it.
  b.row("alpha#700");
  t.check("opening a row closes the stack", b.pane().includes('class="steps"'), false);
  t.check("and the row is the one thing open", b.openRow(), ["alpha#700"]);
}

// ---- a trunk pull request is not a stack seam ----
//
// The live queue that lost Rhea's stack: #625 (master → develop) makes develop "a head", and the
// old chain walk linked through it — so no develop-based PR could be a root, one arbitrary stack
// survived through the single child slot, and the other dissolved into loose rows. A branch with
// several open pull requests based on it is a trunk; chains only link through branches with exactly
// one successor.
const trunkSeam = async trunk => {
  const b = board();
  if (trunk) b.trunk(trunk);
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  const pr = (n, author, head, base) => ({
    number: n, head_ref: head, base_ref: base, title: "pr " + n,
    head_sha: "alpha" + n, committed_at: at, updated_at: at, settled: true, draft: false,
    reasons: ["reviewer"], checks: "none", lane: "needs-you", author,
  });
  b.lanes([
    // the trunk PR that used to dissolve everything rooted on develop
    pr(625, "prateekreddy", "develop", "master"),
    // Rhea's stack, rooted on the trunk
    pr(613, "dev-rhea", "ladder/chassis-tenants", "develop"),
    pr(614, "dev-rhea", "ladder/tenants-01-compose", "ladder/chassis-tenants"),
    pr(615, "dev-rhea", "ladder/tenants-02-tables", "ladder/tenants-01-compose"),
    // a second stack, rooted on the same trunk
    pr(670, "prateekreddy", "fix/readiness-abstention-kinds", "develop"),
    pr(671, "prateekreddy", "fix/readiness-named-findings", "fix/readiness-abstention-kinds"),
    pr(672, "prateekreddy", "fix/readiness-live-banner", "fix/readiness-named-findings"),
    // ordinary develop-based rows, which must stay ordinary
    pr(667, "dev-vale", "worktree-narration-write-guard", "develop"),
    pr(651, "dev-sixth", "fix/example-topic-7-grounds-heading", "develop"),
  ]);
  b.open("alpha");
  await b.drain();

  t.check("both stacks are recognised, not just whichever won a map slot",
    (b.pane().match(/pull requests, one change/g) || []).length, 2);
  t.check("Rhea's stack is one of them",
    b.pane().includes("3 pull requests, one change") && b.pane().includes("ladder"), true);
  t.check("the trunk pull request is an ordinary row, not a stack member",
    b.pane().includes("625") && !b.pane().includes("#625"), true);
  // 9 PRs: six fold into the two stack rows; 625, 667 and 651 stay loose.
  t.check("ordinary develop-based rows stay ordinary", b.rows(), 3);
};
// Named, which is the rule (SKEIN-288): the queue reports the trunk and `revChains` severs at it.
await trunkSeam("develop");
// And unnamed, which is the fallback. A queue that could not report its trunk falls back to the
// old proxy — a base with several open children is assumed to be one — because without the name
// skein genuinely cannot tell a trunk from a fork, and dissolving every stack is the worse of the
// two failures. It shatters forks, which is the bug above; it keeps the stacks standing.
await trunkSeam("");

// ---- SKEIN-288: a fork is not a break, and a step number means depth ----
//
// Reported by the owner: "PR ordering in stack is broken. For example, PR 586 is 4th on the list
// while it shows up as 1st", then "stacking seems incorrect altogether now".
//
// Driven by their ACTUAL queue rather than a hand-made pair, because the bug needed two things at
// once that no hand-made case had together: a trunk pull request (#625, `develop → master`) and a
// real fork (`fix/readiness-abstention-kinds` carries both #586 and #671). The old rule used "this
// base has several children" as a proxy for "this base is the trunk", which cannot tell those two
// apart — so it cut the 21-step stack in half at the fork and renumbered the far half from 1.
{
  const b = board();
  b.trunk(THING.trunk);
  b.lanes(THING.prs.map(p => ({
    ...p, head_sha: "sha" + p.number, committed_at: "", updated_at: "", settled: true,
    draft: false, reasons: ["reviewer"], checks: "none",
  })));
  b.open("alpha");
  await b.drain();

  const chains = b.chains();
  const of = n => chains.find(st => st.steps.some(p => p.number === n));
  const big = of(586);
  t.check("the fork no longer cuts the stack: 650, 670, 671 and 586 are one stack",
    [!!big, big && big.steps.some(p => p.number === 650), big && big.steps.some(p => p.number === 670),
     big && big.steps.some(p => p.number === 671)],
    [true, true, true, true]);
  // The rows the same rule stranded.
  t.check("and the rows it stranded are in it too",
    [671, 672, 711].every(n => big && big.steps.some(p => p.number === n)), true);

  // The number the owner caught. 650 is the deepest step skein can see, 670 sits on it, and the
  // stack branches there — so 586 is the third step down its own path, and never the first.
  t.check("586 is not step 1", big && big.depth.get("alpha#586"), 3);
  t.check("and 650 is the bottom of what is visible", big && big.depth.get("alpha#650"), 1);
  t.check("671 and 586 are siblings, at the same depth",
    big && big.depth.get("alpha#671"), big && big.depth.get("alpha#586"));

  // 650's own base — `example-topic-17-file-hash` — is in nobody's queue, so the real stack is deeper
  // than this. A "1" would be the same kind of lie the fork was.
  t.check("the stack does not claim to start at the trunk", big && big.rooted, false);

  // Opened, so the steps are drawn: the numbers and the branch mark live on them.
  b.stack("stack:alpha#650");
  t.check("its numbers say at least", b.pane().includes("step 1+"), true);
  t.check("and the pane says why", b.pane().includes("not in your queue"), true);
  // A fork says so where it happens, rather than silently becoming a straight line.
  t.check("the branch point is marked", b.pane().includes("branches from step"), true);

  // The 18-step stack IS rooted on develop — 686's base is the trunk — so its numbers are exact.
  const rooted = of(686);
  t.check("a stack that does start at the trunk is numbered exactly",
    [!!rooted, rooted && rooted.rooted], [true, true]);

  // And the bug the old rule was written for stays fixed.
  t.check("the trunk pull request is not swallowed into a stack",
    chains.every(st => st.steps.every(p => p.number !== 625)), true);
  t.check("every develop-rooted stack survives the trunk pull request", chains.length >= 4, true);
}

// ---- SKEIN-302: ONE "your move" list, mixing both roles, ordered by stack then age ----
//
// The owner's ask, answered at the top level of the pane: "I want to know what needs me very
// clearly." Under the lane split the top of the pane was `Lane::NeedsYou` alone — the REVIEWER's
// question — so a pull request the owner had opened was `waiting` by definition however stuck it
// was, and the one screen built to say what needs you could not say it about half their work.
//
// Driven by the owner's own 54-pull-request branch graph, because membership and ORDER are the two
// halves of the claim and a hand-made pair proves neither: this queue is four stacks (26, 18, 3 and
// 2 steps) and five loose rows, so 49 of the 54 would interleave under a plain per-pull-request age
// sort. The ages below are chosen to make that interleaving visible — the newest touch in the whole
// queue is a step buried inside a stack, and the oldest is a stack's own next step.
{
  const b = board();
  const ago = h => new Date(Date.now() - h * 3600 * 1000).toISOString();
  // Each stack's NEXT ACTIONABLE step carries the age its row is sorted at; everything else is
  // recent enough that a naive sort would float it to the top.
  const AGES = { 650: 100, 652: 60, 218: 40, 667: 20, 685: 5, 700: 0.01, 711: 0.02 };
  const thread = (id, who, h) => ({ id, resolved: false, outdated: false, author: who,
                                    started_at: ago(h), url: `https://github.com/x/y#${id}` });
  b.trunk(THING.trunk);
  b.lanes(THING.prs.map(p => ({
    ...p, head_sha: "sha" + p.number, committed_at: "", settled: true, draft: false,
    checks: "failing",           // the whole fleet is red; none of it decides anything here
    // Every pull request NOT awaiting your review is one you opened — which is what the owner's own
    // repository queue looks like, and what makes the CI assertion at the bottom mean something:
    // twenty-six red pull requests of theirs, and the rule must leave every one of them alone.
    reasons: [p.lane === "needs-you" ? "reviewer" : "author"],
    my_review: "none", review_is_current: false,
    updated_at: ago(AGES[p.number] != null ? AGES[p.number] : 1),
    // #218 is the owner's OWN pull request, with two threads open on it. It is `waiting` at the
    // server and belongs at the top of the mixed list — this is the row the lane split could not
    // show, and it sorts by age BETWEEN the stacks rather than after them.
    ...(p.number === 218
      ? { reasons: ["author"], review_threads: [thread("t1", "dana", 40), thread("t2", "sam", 39)],
          review_threads_total: 2 }
      : {}),
  })));
  b.open("alpha");
  await b.drain();

  // MEMBERSHIP and ORDER in one assertion, because they are one claim. Four stack rows and the
  // owner's own pull request, oldest first — and #218 in the middle of them, which is the whole
  // point of mixing the roles.
  t.check("the your-move list is four stacks and your own pull request, oldest first",
    b.nav(),
    ["stack:alpha#650", "stack:alpha#661", "alpha#218", "stack:alpha#659", "stack:alpha#686"]);
  // A plain age sort over the same rows would open with #700 and #711 — two steps in the middle of
  // two different stacks, neither of which can be reviewed yet.
  t.check("the newest touch in the queue is a stack step, and it does not lead the list",
    b.nav().slice(0, 1), ["stack:alpha#650"]);
  t.check("and no step of a stack is loose in the list",
    b.nav().some(k => ["alpha#700", "alpha#711", "alpha#652", "alpha#667", "alpha#685"].includes(k)),
    false);

  t.check("one list at the top, one folded group under it",
    b.heads(), [["your move", 5], ["waiting on others", 4]]);
  // The heading reconciles the two counts §5.3 says must both be true: five things you can start,
  // out of fifty of them.
  t.check("and it says how many pull requests those five starts are",
    /from 50 pull requests/.test(b.pane()), true);
  t.check("the group below states what it is made of",
    b.pane().includes("4 you signed off") || b.pane().includes("you opened")
      ? b.pane().includes("— click to expand") : false, true);

  // The row says WHY it needs you, in words, on the collapsed line. `revRow` is stubbed in this
  // world, so the sentence itself is asserted where the real row is drawn (rowWorld, below); what
  // this proves is that the rule put it there rather than in the reviewer half.
  t.check("your own pull request is in your move because of its threads, never its red checks",
    b.chains().every(st => st.steps.every(p => p.number !== 218)), true);

  // The CI rule, at the pane. Every row in this queue is `checks: "failing"` — on the owner's fleet
  // CI runs after review, so red is the ordinary state — and not one of the twenty-six pull
  // requests they authored is in the list because of it.
  b.toggleTheirs();
  t.check("twenty-six red pull requests of theirs stay out of the your-move list",
    [b.nav().slice(0, 5), b.nav().length],
    [["stack:alpha#650", "stack:alpha#661", "alpha#218", "stack:alpha#659", "stack:alpha#686"], 9]);
  b.toggleTheirs();
}

// ---- SKEIN-304/306: the conversation, and who still owes an approval ----
//
// Two kinds of conversation, drawn deliberately differently, and the asymmetry is the whole design:
// a PR-level comment renders its TEXT here, and an inline review thread renders who, when and the
// way to it and never a word of what was said. The owner: "not keyed on lines… if they are inline
// comments then link out. If they are normal comments then just show it here and also link out."
//
// The obvious instinct is to treat both alike, which is why this is asserted in both directions —
// the body is present for one and absent for the other, in the same render.
function convWorld() {
  const body = `
    let revQueue = { ai: true, blind_spots: [] };
    ${pure("move")}
    ${grab("rk")}
    ${grab("revAgo")}
    ${grab("revTeamsBlind")}
    ${grab("revApprovals")}
    // A thread's line is its own element with its own key (SKEIN-305) — the receipt and the resolve
    // live on it, and the hold machinery it rides is proven in tests/ui/undo.mjs.
    ${grab("revPending")}
    ${grab("revReceiptHtml")}
    ${grab("revThreadKey")}
    ${grab("revThreadHtml")}
    ${grab("revConversation")}
    return {
      approvals: pr => revApprovals(pr),
      conv: pr => revConversation(pr),
      blind: bs => { revQueue.blind_spots = bs; },
    };
  `;
  return new Function("esc", body)(grabbedEsc);
}
// The page's real `esc`, so a comment body carrying markup is asserted against the escaping that
// actually ships rather than against a stub that would pass either way.
const grabbedEsc = new Function(`${grab("esc")}; return esc;`)();
{
  const w = convWorld();
  const ago = h => new Date(Date.now() - h * 3600 * 1000).toISOString();
  const base = over => ({ number: 41, repo_id: "alpha", title: "t", lane: "waiting",
    reasons: ["author"], base_ref: "main", url: "https://github.com/a/b/pull/41",
    my_review: "none", review_is_current: false, review_decision: "", checks: "failing",
    review_threads: [], review_threads_total: null, comments: [], comments_total: null,
    review_requests: [], ...over });

  const both = w.conv(base({
    review_threads: [
      { id: "T1", resolved: false, outdated: false, author: "dana", started_at: ago(30),
        url: "https://github.com/a/b/pull/41#discussion_r1" },
      { id: "T2", resolved: true, outdated: false, author: "sam", started_at: ago(20), url: "u2" },
    ],
    review_threads_total: 2,
    comments: [{ author: "dana", body: "Can we ship this before Friday?", created_at: ago(2),
                 url: "https://github.com/a/b/pull/41#issuecomment-9" }],
    comments_total: 1,
  }));
  t.check("a PR-level comment renders its text, here, without leaving skein",
    both.includes("Can we ship this before Friday?"), true);
  t.check("an inline thread renders who opened it, when, and the way to it",
    [both.includes("dana"), both.includes("discussion_r1")], [true, true]);
  t.check("a resolved thread is counted, not listed", both.includes("1 resolved"), true);
  // The half that is not an omission but a decision: the bodies are never FETCHED (prq::ReviewThread
  // — SKEIN-287 cut this payload from 155KB to 12KB), so there is nothing here to render even if
  // somebody wanted to. If a `body` key ever appears on a thread, this is where it gets drawn by
  // accident.
  const withBody = w.conv(base({
    review_threads: [{ id: "T1", resolved: false, author: "dana", started_at: ago(3),
                       url: "u", body: "what dana actually wrote" }],
    review_threads_total: 1,
  }));
  t.check("a thread's text is not drawn even when a payload turns up carrying one",
    withBody.includes("what dana actually wrote"), false);

  const truncated = w.conv(base({
    review_threads: [{ id: "T1", resolved: true, author: "dana", started_at: ago(30), url: "u" }],
    review_threads_total: 5,
    comments: [{ author: "sam", body: "the last word", created_at: ago(1), url: "u" }],
    comments_total: 40,
  }));
  t.check("a capped thread list says how many it could not see",
    truncated.includes("4 more threads skein did not fetch"), true);
  t.check("and a capped conversation says which part of it this is",
    truncated.includes("The last 1 of 40"), true);

  // A comment body is somebody else's text off the internet. It is escaped and never markdown —
  // `marked.parse` is pointed at skein's own prose and nothing else on this page.
  const nasty = w.conv(base({
    comments: [{ author: "x", body: "<img src=x onerror=alert(1)>", created_at: ago(1), url: "u" }],
    comments_total: 1,
  }));
  t.check("a comment body is escaped, not rendered",
    [nasty.includes("<img"), nasty.includes("&lt;img")], [false, true]);

  t.check("a pull request with no conversation at all draws nothing", w.conv(base({})), "");

  // ---- who still owes an approval ----
  t.check("an authored row names the people and teams GitHub is waiting on",
    w.approvals(base({ review_requests: [{ name: "dana", team: false },
                                         { name: "acme/core", team: true }] }))
      .includes("waiting on @dana and the acme/core team"), true);
  t.check("and says plainly when the repository is satisfied",
    w.approvals(base({ review_decision: "APPROVED" })).includes("every approval this repository asks for is in"),
    true);
  t.check("a pull request somebody else opened is not asked this question",
    w.approvals(base({ reasons: ["reviewer"], review_requests: [{ name: "dana", team: false }] })), "");

  // SKEIN-262's blind spot, at the one place a short list does harm: a roster read as whole is how
  // somebody concludes an approval has landed that never will.
  w.blind(["alpha: team review requests are missing — `gh` cannot list your teams. Fix: gh auth refresh -s read:org"]);
  t.check("a roster that could not see teams says so rather than reading as complete",
    w.approvals(base({ review_requests: [{ name: "dana", team: false }] })).includes("incomplete"), true);
  w.blind(["alpha: 12 pull requests are missing from this queue"]);
  t.check("and an unrelated blind spot does not make it cry wolf",
    w.approvals(base({ review_requests: [{ name: "dana", team: false }] })).includes("incomplete"), false);
}

// ---- the row is five cells at one height, and it never says nothing ----
//
// Three findings, one row (SKEIN-156/157/158): flex let six rows grow to 62px among 23 at 36px, so
// a late summary shifted 28 rows under a click; the check dot was red on 25 of 29 rows — a texture,
// not a signal; and `revGist` returned "" for every row past the read budget, so "read this, it is
// routine" and "never looked" rendered alike. The real revRow runs here: one grid line, a move
// mark, and a gist that always states something.
function rowWorld() {
  const body = `
    let revOpen = new Set(), revSums = new Map(), revRepoFilter = "";
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    let revCommonChips = new Set();
    // The pump reads what skein is allowed to read on its own, so the row's read control asks
    // whether it is running before offering to do by hand what is already coming (SKEIN-228).
    // "Allowed" is two questions now (SKEIN-242/277): read-ahead on for the repo, and the pull
    // request inside worth_reading's scope — hence read_prs below, and reasons on the rows.
    let revQueue = { ai: true };
    let revSel = null, revFlash = "";        // SKEIN-159: revRow paints sel/flash/held from these
    const revPending = new Map();
    const revFlows = new Map([["alpha", { read_prs: true }]]);
    // The move mark and the WHY on the collapsed line both read cockpit/src/move.mjs (SKEIN-302).
    ${pure("move")}
    ${grab("rk")}
    ${grab("revDecided")}
    ${grab("revMoved")}
    ${grab("revFlowChip")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revRail")}
    // The age cell renders the LANE's own sort key rather than a second computation of it
    // (SKEIN-251), so the row world needs the order it is audited against.
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revGist")}
    ${grab("revCrits")}
    ${grab("revDraftedReview")}
    ${grab("revDraftAtHead")}
    ${grab("revReadyChip")}
    // The absence beside it (SKEIN-275): a reading of THIS head with no review drafted says which
    // of the three reasons it is — on the line, and in the section the review would have filled.
    ${grab("revNoDraftWhy")}
    ${grab("revNoDraftChip")}
    // The read-only section now offers a verdict of its own — approve with this review (SKEIN-273)
    // — and stands down for one already held on the pull request, so it reaches the verdict hold,
    // the receipt that hold draws, and the reading view's hand-written notes it deliberately does
    // not carry.
    let revNotes = new Map();
    ${grab("revReceiptHtml")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revReviewToPost")}
    ${grab("revApproveWithReviewHtml")}
    ${grab("revDraftSection")}
    // The row's own read control (SKEIN-228), and the two questions it asks about the pump's scope.
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    ${grab("revReadAgain")}
    // The expanded half of an unread row: which of the reasons it is, and — SKEIN-282 — the switch
    // it names, offered rather than only mentioned.
    const marked = { parse: s => s };
    ${grab("revDetail")}
    ${grab("revUpdatedChip")}
    ${grab("revRow")}
    const revBody = () => "";
    const toggleRevRow = () => {};
    return { row: pr => revRow(pr), gist: s => revGist(s), move: pr => revMove(pr), sums: revSums,
             detail: pr => revDetail(pr),
             readAhead: on => { revFlows.set("alpha", { read_prs: on }); },
             commons: kinds => { revCommonChips = new Set(kinds); },
             section: pr => revDraftSection(pr),
             // The keep/drop panel open on this row, which is the one state the read-only section
             // must keep quiet in.
             vetting: key => revCrits.set(key, { open: true, busy: false, critique: null, drop: new Set(), posted: "" }) };
  `;
  return new Function("esc", body)(String); // the same esc stub board() uses
}
{
  const w = rowWorld();
  const pr = (over) => ({ number: 41, repo_id: "alpha", title: "fix the thing", author: "dev-rhea",
    lane: "needs-you", checks: "failing", updated_at: new Date(Date.now() - 5 * 3600 * 1000).toISOString(),
    my_review: "none", review_is_current: false, draft: false, reasons: ["reviewer"], ...over });

  // 158/150 — the gist column is never empty, and each state is distinguishable.
  const unfetched = w.row(pr());
  t.check("a row skein never fetched says so where you scan", unfetched.includes(">not read<"), true);
  t.check("and says it as a stated absence, not a value", unfetched.includes('class="gist unknown"'), true);
  w.sums.set("alpha#41", { depth: "unread", unread_because: "its diff is too large" });
  t.check("a failed reading carries its reason", w.row(pr()).includes("not read — its diff is too large"), true);
  w.sums.set("alpha#41", "…");
  t.check("a reading in flight says so at the same height", w.row(pr()).includes('class="gist reading"'), true);
  w.sums.set("alpha#41", { depth: "line", line: "moves the audit write behind the lock", flags: [] });
  const read = w.row(pr());
  t.check("a summarised row shows the line, not the absence mark", 
    read.includes("moves the audit write") && !read.includes("gist unknown"), true);

  // 156 — one .revline, and the gist is a cell inside it, not a second line after it.
  t.check("the row is one line", (read.match(/class="revline"/g) || []).length, 1);
  t.check("the gist lives inside the line, so its arrival cannot move a row",
    read.indexOf('class="gist') > read.indexOf('class="revline"')
      && read.indexOf('class="gist') < read.indexOf("</div>"), true);

  // 157 — the left mark is whose-move, the check dot is gone, and mass-truth chips are gone.
  t.check("the left mark says your move", read.includes('class="mv yours"'), true);
  t.check("the check dot is gone from the row", read.includes("revdot"), false);
  t.check("the reviewer chip is gone — the filter strip already selects on it",
    read.includes(">reviewer<"), false);
  t.check("their move draws hollow", w.move(pr({ lane: "waiting" })), "theirs");
  t.check("a decision that holds draws done", w.move(pr({ my_review: "approved", review_is_current: true })), "done");
  t.check("archived draws done", w.move(pr({ lane: "archived" })), "done");
  t.check("a decision the branch moved from under is your move again",
    w.move(pr({ my_review: "approved", review_is_current: false })), "yours");

  // SKEIN-228 — re-analysis without opening the fold, and only where it means something. The
  // control used to live behind the fold as `re-read`, ninth of nine chips, which reads from the
  // outside as no re-analysis at all.
  w.sums.set("alpha#41", { depth: "line", line: "a current reading", flags: [], head_sha: "h1" });
  t.check("a row with a current reading offers no read control",
    w.row(pr({ head_sha: "h1" })).includes("revread"), false);

  w.sums.set("alpha#41", { depth: "line", line: "read before", flags: [], head_sha: "old", stale: true });
  const moved = w.row(pr({ head_sha: "h1" }));
  t.check("a reading of an earlier commit offers the re-read on the line",
    moved.includes(">re-read</button>"), true);
  // The ONE control since SKEIN-293 — it reads again and drafts a new review from that reading,
  // warning first only where the reader has vetted the draft it would replace.
  t.check("which asks the way a person asks", moved.includes(`revReadAgainPress('alpha', 41)`), true);
  t.check("and pressing it does not open the row underneath",
    moved.includes("event.stopPropagation()"), true);
  t.check("saying what it costs, which is nothing",
    moved.includes("never counted against the day's budget"), true);

  w.sums.set("alpha#41", { depth: "unread", budget_stopped: true, unread_because: "the budget is spent" });
  t.check("a row the day's budget stopped offers the read it invites",
    w.row(pr()).includes(">read it</button>"), true);

  w.sums.set("alpha#41", "…");
  t.check("a reading already in flight offers nothing to press",
    w.row(pr()).includes("revread"), false);

  // Scarcity, which is the row's whole design: the pump reads the your-move lane on its own, so an
  // unread row there would wear a control that is gone a second later — a flicker, not an
  // affordance. A draft is never read unasked, so it keeps one.
  w.sums.delete("alpha#41");
  t.check("a row the pump is about to read wears nothing", w.row(pr()).includes("revread"), false);
  t.check("but a draft, which nothing will read for you, does",
    w.row(pr({ draft: true })).includes(">read it</button>"), true);
  // And so does a row in a repo skein reads nothing in on its own (SKEIN-242): "gamma" has no
  // read-ahead consent, so no pump is coming and the control is the only way this one gets read.
  t.check("and so does a row in a repo with read-ahead switched off",
    w.row(pr({ repo_id: "gamma" })).includes(">read it</button>"), true);
}

// ---- a drafted review says so on the row, and shows itself when the row opens (SKEIN-216) ----
//
// The background pass drafts a review beside the summary, in the same model call. Before this, the
// only way to find out was to open the row AND press "review the code…", so the thing skein had
// already paid for was invisible on a queue of thirty. Both the chip and the section read the
// critique the bulk payload already carried — `review::known` puts it there — so neither costs a
// request.
{
  const w = rowWorld();
  const pr = (over) => ({ number: 41, repo_id: "alpha", title: "fix the thing", author: "dana",
    lane: "needs-you", head_sha: "head1", updated_at: new Date().toISOString(),
    my_review: "none", review_is_current: true, draft: false, reasons: ["reviewer"], ...over });
  // `Known` as the server serialises it: the summary flattened, `has_critique` and `drafted` — the
  // row's vocabulary, which survives `Known::thin` — and the critique itself, which does not.
  const drafted = (over) => ({ depth: "line", line: "moves the audit write behind the lock", flags: [],
    head_sha: "head1", has_critique: true, drafted: { head_sha: "head1", comments: 2 },
    critique: { number: 41, head_sha: "head1", overall: "the lock is taken twice on the error path",
      truncated: false, comments: [
        { path: "src/audit.rs", line: 40, anchored: true, text: "this returns before the unlock" },
        { path: "src/lib.rs", line: 0, anchored: false, text: "and the caller cannot tell" },
      ] },
    ...over });

  w.sums.set("alpha#41", { depth: "line", line: "x", flags: [], head_sha: "head1" });
  t.check("a row with no drafted review wears no chip", w.row(pr()).includes("review ready"), false);

  w.sums.set("alpha#41", drafted());
  const ready = w.row(pr());
  t.check("a drafted review announces itself on the collapsed row", ready.includes("review ready · 2"), true);
  t.check("and it is drawn as a chip, in the row's chip column", ready.includes('class="revtag ready"'), true);

  // The one way this chip could mislead: a review of a commit that is no longer there, announced as
  // a review of this pull request. The draft carries its own head, so the row can tell.
  t.check("the chip goes when the head moves past the draft",
    w.row(pr({ head_sha: "head2" })).includes("review ready"), false);
  // "nothing to flag" is a review somebody paid for — and the one that saves the most reading.
  w.sums.set("alpha#41", drafted({ drafted: { head_sha: "head1", comments: 0 },
    critique: { number: 41, head_sha: "head1", overall: "nothing to flag", comments: [] } }));
  const quiet = w.row(pr());
  t.check("a review that found nothing still says it is there", quiet.includes("review ready"), true);
  t.check("with no count, because there is nothing to count", quiet.includes("review ready ·"), false);

  // The section: the same draft, beside the summary, when the row is open.
  w.sums.set("alpha#41", drafted());
  const section = w.section(pr());
  t.check("the expanded row carries the review as its own section",
    section.includes('class="revdraft"'), true);
  t.check("with the overall note", section.includes("the lock is taken twice on the error path"), true);
  t.check("and every drafted comment, against the file it is about",
    section.includes("src/audit.rs:40") && section.includes("this returns before the unlock")
      && section.includes("and the caller cannot tell"), true);
  t.check("a comment the diff could not anchor carries no line number",
    section.includes("src/lib.rs:0"), false);
  t.check("and the way through to keeping and posting is on it",
    section.includes("go through 2 comments and post…"), true);
  t.check("a draft of an earlier commit is not shown as this commit's review",
    w.section(pr({ head_sha: "head2" })), "");

  // §4's other half is a property of the QUEUE, not of the chip: skein drafts a review for every
  // row whose review is yours to give, so on a lane it has worked through "review ready" is true of
  // everything and stops saying which row to open. Demoted exactly like `moved`, and never hidden.
  w.commons(["ready"]);
  t.check("a chip most of the queue would wear leaves the line", w.row(pr()).includes("review ready"), false);
  t.check("and the review is still there when the row opens",
    w.section(pr()).includes('class="revdraft"'), true);
  w.commons([]);

  // Two copies of one review on one row is worse than either, and the editable one must win.
  w.vetting("alpha#41");
  t.check("the section keeps quiet while the keep/drop panel has the same draft open",
    w.section(pr()), "");
}

// ---- a review drafted by the pane's own reader wears its chip on the first read ----
//
// The pump's readings and the drafted reviews are ONE model call (9cfefce). This block used to open
// the pane twice to see the chip — a second `loadReview` the live page never issues, and the whole
// of SKEIN-243: six reviews drafted, paid for, and not one row saying so for the rest of the
// session. So the shape of the test is the claim. Nothing here calls `b.open` more than once.
{
  const b = board();
  b.drafts([1]);            // one of the six, so the chip is signal rather than the texture below
  b.open("alpha");
  await b.drain();
  t.check("the pump's own readings are on screen", b.sums(), 6);
  t.check("and a reading carries the review drafted in the same call", !!b.got(1).has_critique, true);
  t.check("so the row wears its chip from that one read, with no reload",
    b.chip(1).includes("review ready"), true);
  t.check("a row whose reading drafted nothing wears none", b.chip(2), "");
  t.check("one drafted review in six is not texture", b.common().includes("ready"), false);
}

// ---- and the bulk answer does not overwrite a reading the page holds ----
//
// The other half of the same handler, and the reason it is not simply "last answer wins": the disk
// copy can be OLDER than what the page has just been told. It used to be replaced in part — the
// reading kept, the draft grafted on from disk — which was the workaround for the gap above; with
// the gap closed, the held reading must survive whole, draft included.
{
  const b = board();
  b.drafts([1]);
  b.open("alpha");
  await b.drain();
  // Only now: the disk copy has to arrive at a page that is already holding a reading, which is
  // the second visit, not the first. (On the first, nothing is held and the disk copy is all there
  // is — that is the branch below it, and it is right to take it.)
  b.holds({ "1": { number: 1, head_sha: "alpha1", depth: "line", line: "off disk", has_critique: true,
                   critique: { number: 1, head_sha: "alpha1", overall: "an older draft", comments: [] } } });
  b.open("alpha");
  await b.drain();
  t.check("the newer reading survives the bulk answer", b.got(1).line, "x");
  // `|| {}` so a draft that never landed reads as a named failure rather than as a TypeError from
  // the assertion itself — a suite that crashes says less about what broke than one that reports.
  t.check("and keeps its own draft rather than the disk's", (b.got(1).critique || {}).overall, "one thing");
}

// ---- a lane skein has worked through wears the chip on nothing ----
//
// The queue-level half of §4, counted at render time over what is on screen: a chip true of most of
// the queue is texture, and skein drafts a review for every row whose review is yours to give.
{
  const b = board();
  const drafted = n => ({ number: n, head_sha: "alpha" + n, depth: "line", line: "read " + n,
    has_critique: true,
    critique: { number: n, head_sha: "alpha" + n, overall: "a note", comments: [] } });
  b.holds(Object.fromEntries([1, 2, 3, 4, 5, 6].map(n => [String(n), drafted(n)])));
  b.open("alpha");
  await b.drain();
  t.check("a review drafted on every row is texture, and the chip stands down",
    b.common().includes("ready"), true);
}

// ---- red is a queue-level sentence, not row wallpaper ----
{
  const b = board();
  const at = new Date(Date.now() - 3600 * 1000).toISOString();
  const pr = (n, checks) => ({ number: n, head_ref: "b" + n, base_ref: "develop", title: "pr " + n,
    head_sha: "s" + n, committed_at: at, updated_at: at, settled: true, draft: false,
    reasons: ["reviewer"], checks, lane: "needs-you", author: "x" });
  b.lanes([pr(1, "failing"), pr(2, "failing"), pr(3, "failing"), pr(4, "passing")]);
  b.open("alpha");
  await b.drain();
  t.check("red said once at the top when it is most of the queue",
    b.pane().includes("3 of 4 are red"), true);

  const b2 = board();
  b2.lanes([pr(1, "failing"), pr(2, "passing"), pr(3, "passing"), pr(4, "passing")]);
  b2.open("alpha");
  await b2.drain();
  t.check("a lone failure is not a queue-level story", b2.pane().includes("are red"), false);
}

// ---- an empty queue and a broken one both offer the next move (SKEIN-154) ----
//
// Both states were correct in prose and inert as affordances: "nothing here." in the corner of a
// 1400px page while another repo held ten, and an orange box that replaced twenty-nine rows which
// were on screen a second ago and are still on disk.
{
  // alpha is the repo you are looking at and the merged answer carries only beta — the shape of a
  // fleet where the queue you opened is clear and another one is not.
  const b = board();
  b.serves(["beta"]);
  b.open("alpha");
  await b.drain();
  const pane = b.pane();
  t.check("a cleared queue says so as an answer, not as an absence",
    pane.includes("alpha is clear."), true);
  t.check("and names what the rest of the fleet holds", /revclear-n">6<\/span>\s*<span>beta/.test(pane), true);
  t.check("with the way to it", pane.includes(`onclick="openReview('beta')"`), true);

  // A repo skein could not READ is listed here too: "empty" and "not looked at" must never be the
  // same screen.
  const b2 = board();
  b2.serves(["beta"]);
  b2.broken(["gamma"]);
  b2.open("alpha");
  await b2.drain();
  t.check("a repo that could not be read is named on the cleared screen",
    b2.pane().includes("gamma — skein could not read this queue: boom"), true);

  // Nothing anywhere is a different sentence from nothing here, and it is the good one.
  const b3 = board();
  b3.lanes([]);
  b3.open("");
  await b3.drain();
  t.check("with no repo chosen, the answer is about the fleet",
    b3.pane().includes("Nothing is waiting on you."), true);
  t.check("and nothing is claimed about repos it has no rows for",
    b3.pane().includes("revclear-next"), false);
}

// ---- "clear" is a claim, and skein only makes it about queues it read (SKEIN-245) ----
//
// `prq::merged` reports a repo whose review queue is switched off in `skipped` rather than leaving
// it out, on the rule that "never looked" and "nothing waiting" must not be the same silence. The
// pane dropped the field, so the one screen built to keep that rule broke it: `beta is clear.` —
// about pull requests skein never asked GitHub for.
{
  const b = board();
  b.unasked([{ repo_id: "beta", needs_you: 0, error: "",
               skipped: "review queue is switched off for this repo" }]);
  b.open("beta");
  await b.drain();
  const pane = b.pane();
  t.check("a repo skein never asked about is never called clear", pane.includes("beta is clear."), false);
  t.check("the headline says so in the verb that is true", pane.includes("skein did not ask about beta."), true);
  t.check("and names what was switched off",
    pane.includes("review queue is switched off for this repo"), true);
  t.check("with the setting that would turn it back on",
    pane.includes(`onclick="openSettings('repos')"`), true);
  // The picker was the first place the two became the same thing: `beta · 0`, byte for byte a repo
  // with a clean queue.
  t.check("and the picker stops counting it as a zero", pane.includes("beta · —"), true);
}

// A fleet where NOTHING was read. The calm screen's headline is a claim about every repo skein
// watches, and with no queue in the answer there is nothing behind it — reachable on a fresh
// process with a bad token, where `prq::merged` has no remembered copies to fall back on.
{
  const b = board();
  b.serves([]);
  b.broken(["alpha", "beta"]);
  b.open("");
  await b.drain();
  const pane = b.pane();
  t.check("a fleet where every queue failed is not a calm fleet",
    pane.includes("Nothing is waiting on you."), false);
  t.check("it says no queue was read", pane.includes("skein has not read any queue."), true);
  t.check("counting what could not be", pane.includes("2 could not be read"), true);
  t.check("and the move is on the screen making the claim",
    pane.includes(`class="revclear-acts"`) && pane.includes("try again"), true);
}

// The scoped form of the same lie, which is where it read worst: an orange box saying alpha's queue
// could not be built, and directly under it the headline "alpha is clear."
{
  const b = board();
  b.serves([]);
  b.broken(["alpha"]);
  b.open("alpha");
  await b.drain();
  const pane = b.pane();
  t.check("the repo whose queue failed is not called clear either",
    pane.includes("alpha is clear."), false);
  t.check("its queue is unknown, and the headline says which",
    pane.includes("skein could not read alpha."), true);
}

// The other side of the same rule, and the reason it is not simply "any failure refuses": one queue
// that WAS read earns the fleet's answer, and what skein did not ask about is a row on it.
{
  const b = board();
  b.lanes([]);
  b.unasked([{ repo_id: "beta", needs_you: 0, error: "",
               skipped: "no GitHub remote, so there are no pull requests to list" }]);
  b.open("");
  await b.drain();
  const pane = b.pane();
  t.check("a queue that was read still earns the fleet's answer",
    pane.includes("Nothing is waiting on you."), true);
  t.check("and the repo skein did not ask about is named beside it",
    pane.includes("beta — skein did not ask: no GitHub remote"), true);
  t.check("with a dash where a count would be",
    /revclear-n">—<\/span>\s*<span>beta/.test(pane), true);
}

// ---- the queue could not be built: the remembered rows stay, and there is a way out ----
{
  const b = board();
  b.open("alpha");
  await b.drain();
  t.check("six rows, read and on screen", b.rows(), 6);

  b.outage("401 Bad credentials");
  b.open("alpha");
  await b.drain();
  const pane = b.pane();
  t.check("the failure does not replace the queue that was on screen", b.rows(), 6);
  t.check("and the rows say they are not live", pane.includes(`class="revwrap notlive"`), true);
  t.check("the box says what GitHub said", pane.includes("401 Bad credentials"), true);
  t.check("that these are the ones skein last read", pane.includes("skein last read"), true);
  t.check("and offers both moves", pane.includes("try again") && pane.includes("GitHub &amp; keys"), true);
  t.check("the settings it offers is the one holding that credential",
    pane.includes(`onclick="openSettings('github')"`), true);

  // A failure is not an answer: it must never be remembered as the queue.
  b.outage(null);
  b.open("alpha");
  await b.drain();
  t.check("and the next good answer clears it", b.pane().includes("401 Bad credentials"), false);
}

// ---- a standing blind spot and a failed queue are two different things (SKEIN-164) ----
//
// Both used to be the same orange box, which is how the largest, loudest object on the pane came to
// report a condition that is true on every load until somebody runs a command. An alarm spent on a
// constant is an alarm the eye learns to skip.
{
  const b = board();
  b.blinds(["team review requests are missing — `gh` cannot list your teams. Fix: gh auth refresh -s read:org"]);
  b.open("alpha");
  await b.drain();
  const pane = b.pane();
  t.check("the standing gap is stated", pane.includes("cannot list your teams"), true);
  t.check("with its cure in the same sentence", pane.includes("gh auth refresh -s read:org"), true);
  t.check("in the standing treatment", pane.includes('class="revblind"'), true);
  t.check("and not in the failure's", pane.includes('class="revfail"'), false);

  // A repo whose queue could not be built at all: that IS skein failing, and it keeps the box.
  const b2 = board();
  b2.serves(["alpha"]);
  b2.broken(["beta"]);
  b2.open("");
  await b2.drain();
  const p2 = b2.pane();
  t.check("a queue that could not be built keeps the alarm", p2.includes('class="revfail"'), true);
  t.check("naming the repo and what GitHub said",
    p2.includes("beta — boom"), true);
  t.check("and it is no longer filed as something the queue could not see",
    p2.includes('class="revblind"'), false);

  // Filtering to a healthy repo is not a reason to stop reporting the broken one's failure… but it
  // is not that repo's failure either, so it belongs to the repo it came from.
  b2.open("alpha");
  t.check("a repo's own view carries its own failures only",
    b2.pane().includes('class="revfail"'), false);
}

// ---- many repositories are one dropdown, and SKEIN-163's numbers survive inside it ----
// The strip wrapped nine repos into six ragged rows and pushed the queue below the fold
// (SKEIN-211), so the FORM changed to a select — but every count and failure mark stays in the
// option text, readable without opening anything, and choosing still routes through `openReview`.
{
  const b = board();
  b.serves(["alpha"]);          // beta managed but serving nothing this round
  b.broken(["beta"]);
  b.open("");
  await b.drain();
  const pane = b.pane();
  t.check("the picker is one select, not a pile of buttons",
    pane.includes(`<select class="revrepo"`) && !pane.includes("reprep"), true);
  t.check("each repo shows its count in its option text",
    pane.includes(">alpha · 6</option>") && pane.includes(">all repos · 6</option>"), true);
  t.check("a repo whose queue failed wears ! instead of a number it does not have",
    /<option value="beta"[^>]*>\s*beta · !<\/option>/.test(pane), true);
  t.check("and that option's title is the error itself",
    /<option value="beta"[^>]*title="boom"/.test(pane), true);
  t.check("choosing an option is the strip's click, verbatim",
    pane.includes(`onchange="openReview(this.value)"`), true);

  const b2 = board();
  b2.serves(["alpha", "beta"]);
  b2.open("");
  await b2.drain();
  t.check("two repos, both counted",
    b2.pane().includes(">beta · 6</option>") && b2.pane().includes(">all repos · 12</option>"), true);
  b2.open("beta");
  t.check("selecting a repo filters the merged queue to it, without clearing what was read",
    b2.rows(), 6);
  t.check("the closed control shows the current selection with its count",
    /<option value="beta" selected[^>]*>\s*beta · 6<\/option>/.test(b2.pane()), true);

  const b3 = board();
  b3.serves(["alpha"]);         // beta managed, serving nothing, and NOT broken
  b3.open("");
  await b3.drain();
  t.check("a repo with an empty queue dims but stays choosable",
    /<option value="beta"[^>]* class="none"[^>]*>\s*beta · 0<\/option>/.test(b3.pane()), true);
}

// ---- what skein read moves the row, within its lane and never out of it ----
{
  const b = board();
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  b.holds({
    1: { number: 1, head_sha: "alpha1", depth: "line", line: "changes a default", flags: ["default"] },
    3: { number: 3, head_sha: "alpha3", depth: "line", line: "routine bump", flags: [] },
  });
  // The pump would read the rest and leave nothing unread to sort around — refusing its fetches
  // keeps 2/4/5/6 genuinely unread (the transient "could not reach" IS the unread state).
  b.refuse("alpha");
  b.open("alpha");
  await b.drain();
  b.search("");   // one explicit re-render with everything landed
  const order = [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1]));

  const pos = n => order.indexOf(n);
  t.check("a flagged reading lifts its row above the unread",
    order.length === 6 && pos(1) === 0 && pos(3) === 5, true);
  t.check("and nothing the model said removed a row from its lane",
    order.includes(1) && order.includes(3), true);
}

// ---- chips stay scarce: wallpaper kinds are demoted, and the rest are capped ----
{
  const b = board();
  b.holds({
    1: { number: 1, head_sha: "alpha1", depth: "line", line: "a", flags: ["behaviour", "default"] },
    2: { number: 2, head_sha: "alpha2", depth: "line", line: "b", flags: ["behaviour"] },
    3: { number: 3, head_sha: "alpha3", depth: "line", line: "c", flags: ["behaviour"] },
  });
  b.open("alpha");
  await b.drain();
  t.check("a flag most of the queue wears is texture, not signal",
    b.common().includes("behaviour") && !b.common().includes("default"), true);

  const w = rowWorld();
  const pr = { number: 41, repo_id: "alpha", title: "t", author: "a", lane: "needs-you", checks: "none",
    updated_at: new Date().toISOString(), my_review: "none", review_is_current: false, draft: false, reasons: [] };
  w.sums.set("alpha#41", { depth: "line", line: "x", flags: ["schema", "default", "interface", "ux"] });
  const row = w.row(pr);
  t.check("at most two flag chips ride the row, the rest fold into +n",
    (row.match(/revtag flag/g) || []).length === 3 && row.includes(">+2<"), true);
  w.commons(["schema", "default", "interface", "ux"]);
  t.check("a row wearing only wallpaper wears nothing",
    (w.row(pr).match(/revtag flag/g) || []).length, 0);
}

// ---- twenty-six red pull requests are one decision ----
//
// SKEIN-144: the same judgement — "not until CI is green" — was per-row or nowhere. One press now
// snoozes every red row still in your lane at the head it shows, and each returns on its own when
// its author pushes (the sha stops matching — no timer, no memory, no undo to remember).
{
  const b = board();
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  const pr = (n, checks, over) => ({ number: n, head_ref: "b" + n, base_ref: "develop",
    title: "pr " + n, head_sha: "s" + n, committed_at: at, updated_at: at, settled: true,
    draft: false, reasons: ["reviewer"], checks, lane: "needs-you", author: "x", ...over });
  b.lanes([
    pr(1, "failing"), pr(2, "failing"), pr(3, "passing"),
    pr(4, "failing", { lane: "waiting" }),   // red but not yours — not swept
  ]);
  b.open("alpha");
  await b.drain();
  t.check("the red line offers the one decision", b.pane().includes("set the red ones aside"), true);
  b.snoozeRed();
  await b.drain();
  const posts = b.posts();
  t.check("one snooze per red row in your lane, and only those",
    posts.filter(u => /\/(1|2)\/snooze$/.test(u)).length === 2 && posts.length === 2, true);
}

// ---- your move is ordered by how long it has waited on you, oldest first ----
//
// SKEIN-140. updated_at DESC was upside down for a review queue: the PR waiting longest sank to
// the bottom, and any push — a bot's included — lifted a row to the top. Your move now reads
// oldest-waiting first (a decided PR whose head moved counts from the commit that invalidated the
// decision), and the other lanes keep recency, because for your own PRs "what moved most
// recently" is the right question.
{
  const b = board();
  const ago = h => new Date(Date.now() - h * 3600 * 1000).toISOString();
  const pr = (n, over) => ({ number: n, head_ref: "b" + n, base_ref: "develop", title: "pr " + n,
    head_sha: "s" + n, committed_at: ago(2), updated_at: ago(2), settled: true, draft: false,
    reasons: ["reviewer"], checks: "none", lane: "needs-you", author: "x",
    my_review: "none", review_is_current: false, ...over });
  b.lanes([
    pr(300, { updated_at: ago(0.02) }),                       // pushed a minute ago
    pr(100, { updated_at: ago(168) }),                        // asked of you a week ago
    // Decided, then the head moved: waiting since THAT commit, not since the latest touch.
    pr(200, { my_review: "approved", review_is_current: false,
              committed_at: ago(72), updated_at: ago(0.01) }),
    // Their move keeps recency: newest first.
    pr(400, { lane: "waiting", author: "me", updated_at: ago(50) }),
    pr(500, { lane: "waiting", author: "me", updated_at: ago(1) }),
  ]);
  b.open("alpha");
  await b.drain();
  const order = [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1]));
  t.check("a week-old request outranks a minute-old push",
    order.slice(0, 3), [100, 200, 300]);
  // Waiting-on-others is a group you go looking at, so it is folded and draws no rows until asked
  // (SKEIN-302). Its ORDER is still recency, newest first — for your own pull requests "what moved
  // most recently" is the right question — which is what one click shows.
  t.check("waiting on others draws nothing until you ask for it", order.slice(3), []);
  b.toggleTheirs();
  t.check("their move keeps recency, newest first",
    [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1])).slice(3), [500, 400]);
  b.toggleTheirs();

  // SKEIN-251. This fixture already had the row that proves it — #200, committed 72h ago and
  // touched 30 seconds ago — and asserted only the ORDER, so the divergence was baked in as
  // correct: the row sorted second of three while its age cell read `1m`, above rows reading `4d`.
  // The one column docs/review-ux.md §4 puts on the row so the order can be AUDITED was the one
  // that made it unauditable, and the amber three-day mark was applied to the wrong number too.
  //
  // Asserted through the real `revRail` — the board world stubs `revRow` past it, and the claim is
  // about the number in the cell, so the cell is what is read. The same rows, from the same
  // builder, so the order above and the numbers below cannot be about different pull requests.
  const w = rowWorld();
  const cell = over => {
    const m = /<span class="revage([^"]*)"[^>]*>([^<]*)</.exec(w.row({ repo_id: "alpha", ...pr(0, over) }));
    return m ? { label: m[2], old: /\bold\b/.test(m[1]) } : null;
  };
  // The LABEL only: 72h is exactly on the amber threshold (`d > 3`), so asserting the colour here
  // would be a coin toss on the millisecond the fixture was built. The threshold itself is covered
  // by the week-old row below — and covering it there is the point, because against `updated_at`
  // that row read `1m` and could never have gone amber at all.
  t.check("the age cell on a decided row that moved counts from the commit, not the comment",
    cell({ my_review: "approved", review_is_current: false,
           committed_at: ago(72), updated_at: ago(0.01) }).label, "3d");
  t.check("a week-old request reads as a week, and is amber",
    cell({ updated_at: ago(168) }), { label: "7d", old: true });
  t.check("and a minute-old push still reads as a minute",
    cell({ updated_at: ago(0.02) }), { label: "1m", old: false });
  // Their move measures something else, and the cell must render THAT — its lane sorts on
  // updated_at, so rendering waited-since there would be the same fault mirrored.
  t.check("their move's cell is recency, which is what their move is sorted by",
    cell({ lane: "waiting", author: "me", my_review: "approved", review_is_current: false,
           committed_at: ago(72), updated_at: ago(50) }), { label: "2.1d", old: false });
}

// ---- the search bar: a PR is findable by what you remember about it ----
{
  const b = board();
  const at = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  const pr = (n, title, author, over) => ({ number: n, head_ref: "b" + n, base_ref: "develop",
    title, head_sha: "s" + n, committed_at: at, updated_at: at, settled: true, draft: false,
    reasons: ["reviewer"], checks: "none", lane: "needs-you", author, ...over });
  b.lanes([
    pr(577, "bedrock transport for the audit path", "dev-sixth"),
    pr(650, "worktree analysis status", "dev-vale"),
    pr(613, "chore(ladder): the tenants chassis", "dev-rhea", { head_ref: "ladder/chassis-tenants" }),
    pr(614, "tenants slice 1: compose", "dev-rhea", { head_ref: "ladder/tenants-01", base_ref: "ladder/chassis-tenants" }),
    pr(615, "tenants slice 2: tables", "dev-rhea", { head_ref: "ladder/tenants-02", base_ref: "ladder/tenants-01" }),
  ]);
  b.open("alpha");
  await b.drain();
  t.check("the stack folds before anyone searches", b.pane().includes("pull requests, one change"), true);

  b.search("577");
  t.check("a number finds its pull request and nothing else",
    [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1])), [577]);
  t.check("and the lane heading counts what is shown", b.pane().includes('class="revn">1<'), true);

  b.search("dev-vale");
  t.check("an author finds their rows",
    [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1])), [650]);

  // A hit inside a stack must be visible directly: the aggregation hides exactly what the
  // searcher is after, so a live search dissolves stacks into their matching rows.
  b.search("615");
  t.check("a stack step is reachable by search, loose",
    [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1])), [615]);
  t.check("and no stack row hides it", b.pane().includes("pull requests, one change"), false);

  b.search("");
  t.check("clearing restores the full queue",
    [...b.pane().matchAll(/<row n=(\d+)>/g)].map(m => Number(m[1])).length >= 2, true);
  t.check("and the stack folds back", b.pane().includes("pull requests, one change"), true);
}

// ---- the change is readable in the pane, and a verdict lives only where the evidence is ----
//
// SKEIN-148/161. The pane used to contain no code: expanding an unread row offered approve first,
// next to nothing. Now the row's acts are read/set-aside, the reading view shows the diff skein
// already fetched, and the verdict buttons exist only there.
{
  const b = board();
  b.open("alpha");
  await b.drain();

  // Asserted against the row body's SOURCE: the board world stubs revRow, so the rendered pane
  // cannot see what an expanded row would offer — and a vacuous pass here is exactly how approve
  // would creep back in beside nothing.
  t.check("the queue's row body offers no verdict",
    /'approve'|'request-changes'/.test(grab("revBody")), false);

  b.read("alpha", 11);
  await b.drain();
  const pane = b.pane();
  t.check("the reading view shows the change itself", pane.includes("fn added() {}"), true);
  t.check("both changed files are listed", pane.includes("src/lib.rs") && pane.includes("docs/note.md"), true);
  t.check("the verdict is reachable from the evidence",
    /revAct\('alpha', 11, 'approve'\)/.test(pane), true);
  t.check("opening the change was a revealed request for a reading",
    b.reads().some(u => u.includes("/11/summary")), true);

  b.back();
  await b.drain();
  t.check("esc returns to the queue", b.reading(), null);
  t.check("and the queue is drawn again, not rebuilt empty", b.rows() > 0, true);
}

// ---- read-ahead is reachable from the queue you are looking at ----
//
// SKEIN-282. `read_prs` is the whole scope of what skein reads on its own — after SKEIN-242 it
// governs the pane's own pump as well as the ten-minute background pass — and the only control for
// it returned "" unless the view was already narrowed to one repo. On the owner's fleet every repo
// had it off, so the merged queue showed rows that would never be read, offered nothing that would
// change that, and the way to find the switch was to already know it was there.
{
  const b = board();
  b.serves(["alpha", "beta"]);
  b.open("");                       // the merged view: no repo chosen, which is where this failed
  await b.drain();
  b.readAhead({ alpha: false, beta: false });

  t.check("the merged queue says how much of itself skein reads on its own",
    /read ahead · 0 of 2/.test(b.pane()), true);
  t.check("and says what switching it on would buy, rather than only that it is off",
    /skein reads nothing on its own in 2 of these 2 repos/.test(b.pane()), true);

  b.readAll(true);
  await b.drain();
  t.check("one press is one consent per repo, for every repo on screen",
    b.readingPosts().sort(), ["alpha", "beta"]);

  // Consent stays per-repo underneath, and the count is what says where it got to.
  b.readAhead({ alpha: true, beta: false });
  t.check("a partly-on fleet counts itself honestly", /read ahead · 1 of 2/.test(b.pane()), true);
  b.readAhead({ alpha: true, beta: true });
  t.check("and an all-on fleet says so", /read ahead · 2 of 2/.test(b.pane()), true);
  const was = b.readingPosts().length;
  b.readAll(true);
  t.check("pressing on when everything is already on asks for nothing",
    b.readingPosts().length, was);
}
// One repo on screen keeps the sentence that can name it.
{
  const b = board();
  b.open("alpha");
  await b.drain();
  b.readAhead({ alpha: false });
  t.check("a narrowed view still gets the plain switch, not a count of one",
    /read ahead<\/button>/.test(b.readChip()) && !/of 1/.test(b.readChip()), true);
  b.readAhead({ alpha: true });
  t.check("and says so when it is on", /read ahead · on/.test(b.readChip()), true);
}
// And the sentence on an unread row reaches the switch it names, rather than only naming it.
{
  const w = rowWorld();
  const pr = { number: 41, repo_id: "alpha", title: "fix the thing", author: "dev-rhea",
               lane: "needs-you", checks: "passing", head_sha: "h1", updated_at: "2026-08-20T00:00:00Z",
               my_review: "none", review_is_current: false, draft: false, reasons: ["reviewer"] };
  w.readAhead(false);
  const body = w.detail(pr);
  t.check("an unread row says which repo skein is not reading", body.includes("skein does not read alpha"), true);
  t.check("and offers the switch it names, from the row",
    /revSetReadingFor\("alpha", true\)/.test(body), true);
}

// ---- the code on screen is a COMMIT, and a moved head is a press away ----
//
// SKEIN-254. `revDiffs` was keyed by the pull request and fetched behind `if (!revDiffs.has(key))`
// with no invalidation anywhere in the file — not on a queue refresh, not on a head moving, not on
// an act. So once a tab had opened #11's code it showed that commit's code until the tab was
// reloaded, and `re-read` in the header forces the SUMMARY, never the diff. The honesty was already
// right — the reader was TOLD the branch moved, and a note posts against the sha it was written on
// — and honesty with no move in it is the failure law 1 names: the reader was told the code was old
// on the one surface built so a thirty-a-day reviewer would not have to go to github.com, and the
// only way to see the new code was to go to github.com.
{
  const b = board();
  b.lanes([{ number: 11, lane: "needs-you", draft: false, head_sha: "sha11",
             committed_at: new Date(Date.now() - 2 * 3600 * 1000).toISOString(),
             settled: true, reasons: ["reviewer"] }]);
  b.open("alpha");
  await b.drain();
  b.read("alpha", 11);
  // drain() only pumps the answers this world queues; the diff route answers immediately, so the
  // microtask its promise chain needs comes from settle().
  await b.drain();
  await b.settle();
  t.check("the change on screen is the one that was opened",
    b.pane().includes("fn added() {}"), true);
  const asked = b.diffs().length;

  // Somebody pushes. The queue learns the new head; the reader is part-way down the old one.
  b.push(11, "sha11-two");
  t.check("a push under an open reading does not swap the code out from under the reader",
    b.pane().includes("fn added() {}") && !b.pane().includes("fn added_again"), true);
  t.check("and it does not silently go fetching either", b.diffs().length, asked);
  t.check("the reader is told the branch moved",
    b.pane().includes("the branch moved since you read"), true);
  t.check("and the way to the new code is on the notice, not on github.com",
    b.pane().includes("show the new code"), true);

  b.reload("sha11-two");
  t.check("pressing it asks for the change again", b.diffs().length, asked + 1);
  // Nothing to judge until the evidence is back — review-ux §6, and the same rule the bar already
  // applied to a diff that had not arrived yet.
  t.check("and offers no verdict over a change that is not on screen yet",
    /revAct\('alpha', 11, 'approve'\)/.test(b.pane()), false);
  await b.settle();
  t.check("the new code arrives", b.pane().includes("fn added_again() {}"), true);
  t.check("the notice goes with it", b.pane().includes("the branch moved since you read"), false);
  t.check("and the verdict is offered again, over what is now on screen",
    /revAct\('alpha', 11, 'approve'\)/.test(b.pane()), true);
}

// ---- and a second visit reads the commit that is there, not the one the tab first saw ----
{
  const b = board();
  b.lanes([{ number: 11, lane: "needs-you", draft: false, head_sha: "sha11",
             committed_at: new Date(Date.now() - 2 * 3600 * 1000).toISOString(),
             settled: true, reasons: ["reviewer"] }]);
  b.open("alpha");
  await b.drain();
  b.read("alpha", 11);
  // drain() only pumps the answers this world queues; the diff route answers immediately, so the
  // microtask its promise chain needs comes from settle().
  await b.drain();
  await b.settle();
  b.back();
  b.push(11, "sha11-two");
  b.read("alpha", 11);
  // drain() only pumps the answers this world queues; the diff route answers immediately, so the
  // microtask its promise chain needs comes from settle().
  await b.drain();
  await b.settle();
  t.check("re-opening after a push fetches the change again", b.diffs().length, 2);
  t.check("and shows the commit that is there now, not the one the tab first saw",
    b.pane().includes("fn added_again() {}"), true);
}

// ---- an unreadable change refuses a verdict rather than offering one next to nothing ----
{
  const b = board();
  b.open("alpha");
  await b.drain();
  b.read("alpha", 11);
  // No drain: the diff has not answered yet.
  const pane = b.pane();
  t.check("no diff yet, no verdict yet", /revAct\('alpha', 11, 'approve'\)/.test(pane), false);
  t.check("and the pane says it is fetching", pane.includes("fetching the change"), true);
}

// ---- line comments post with the verdict, and are cleared by it ----
{
  const store = { data: {}, getItem(k) { return this.data[k] ?? null; }, setItem(k, v) { this.data[k] = v; }, removeItem(k) { delete this.data[k]; } };
  const { world, posts } = composeWorld(store);
  world.noteFor("alpha#21", "src/lib.rs", 2, "this write never fsyncs");
  world.act("alpha", 21, "approve");
  await new Promise(r => setTimeout(r, 0));
  t.check("the verdict carried the line comment",
    posts.length === 1 && posts[0].comments.length === 1 && posts[0].comments[0].path === "src/lib.rs", true);
  t.check("a posted comment does not linger for the next verdict", world.noteCount("alpha#21"), 0);

  world.noteFor("alpha#22", "a.rs", 1, "x");
  world.act("alpha", 22, "merge");
  await new Promise(r => setTimeout(r, 0));
  t.check("a non-verdict act does not smuggle comments", (posts[1].comments || []).length, 0);
  t.check("and leaves them waiting for the verdict they belong to", world.noteCount("alpha#22"), 1);
}

// ---- what you typed into the composer survives a reload ----
//
// Reported live on PR 577: notes were written, "draft with skein" answered, the page was reloaded
// while another call ran — and both were gone, because the composer was pure page state. It is
// saved per (repo, PR, kind) now, restored on reopen, and cleared only by an actual post.
function composeWorld(store) {
  const posts = [];
  const body = `
    ${grab("esc")}
    ${grab("rk")}
    ${grab("revComposing")}
    ${grab("revComposeStore")}
    ${grab("revComposeSave")}
    ${grab("revCompose")}
    ${grab("revAct")}
    ${grab("REV_UNDO_MS")}
    ${grab("revPending")}
    ${grab("revDecided")}
    ${grab("revHold")}
    ${grab("revTick")}
    ${grab("revFire")}
    ${grab("revMarkDone")}
    ${grab("revRepaintRow")}
    // The pending paint routes a THREAD key to its own paint (SKEIN-305), so it needs the marker
    // that tells the two kinds of key apart. (No backticks: this whole world is a template literal.)
    ${grab("REV_THREAD_MARK")}
    ${grab("revThreadAt")}
    ${grab("revThreadPaint")}
    ${grab("revPendingPaint")}
    let revReading = null;
    let revQueue = { prs: [] };
    const revpane = null;
    const revRow = () => "";
    const document = { getElementById: () => null };
    const revNotes = new Map();
    // Keyed by the COMMIT a diff is of (SKEIN-254); revDiffRead is how an act finds the one this
    // pull request was actually read from, whichever commit that was.
    const revDiffs = new Map();
    ${grab("revDiffRead")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    const closeReading = () => {};
    const renderReview = () => {};
    const renderReviewNow = () => {};
    const toast = () => {};
    // The merge verdict is the pane's other confirmation, so it goes through the same wrapper.
    ${grab("CONFIRM_FLOOR_MS")}
    ${grab("confirmed")}
    const confirm = () => true;
    const loadReview = () => {};
    const revPost = (repo, number, kind, text, comments) => { posts.push({ repo, number, kind, text, comments: comments || [] }); return Promise.resolve({ ok: true, text: "sent" }); };
    return {
      compose: (repo, n, kind) => revCompose(repo, n, kind),
      type: text => { revComposing.text = text; revComposeSave(); },
      text: () => revComposing.text,
      act: (repo, n, kind) => revAct(repo, n, kind),
      noteFor: (key, path, line, body) => { revNotesFor(key).push({ path, line, body }); revNotesSave(key); },
      noteCount: key => revNotesFor(key).length,
    };
  `;
  // Immediate timers: SKEIN-162's undo window collapses to zero here, because these contracts are
  // about the payload and the draft, not the window — undo.mjs owns the window itself.
  return { world: new Function("localStorage", "posts", "setTimeout", "clearTimeout", body)(store, posts, fn => { fn(); return 0; }, () => {}), posts };
}
{
  const store = { data: {}, getItem(k) { return this.data[k] ?? null; }, setItem(k, v) { this.data[k] = v; }, removeItem(k) { delete this.data[k]; } };
  const { world: first } = composeWorld(store);
  first.compose("alpha", 577, "comment");
  first.type("the audit-write path never fsyncs");

  // The reload: a fresh page over the same browser storage.
  const { world: second } = composeWorld(store);
  second.compose("alpha", 577, "comment");
  t.check("what you typed survives a reload", second.text(), "the audit-write path never fsyncs");

  // Posting is the one thing that clears it — the draft has done its job.
  second.act("alpha", 577, "comment");
  await new Promise(r => setTimeout(r, 0));
  const { world: third } = composeWorld(store);
  third.compose("alpha", 577, "comment");
  t.check("a posted draft does not resurface", third.text(), "");
}

// ---- one row that throws is one row, not a blank page (SKEIN-268) ----
//
// Reported by the owner: "any small error anywhere in the review page just blanks the entire page
// and gives the error." The pane is built as ONE string and assigned in one shot, so a throw
// anywhere inside it means `revpane.innerHTML = …` is never reached — and everything that touches
// model output, a diff, a draft or a workflow is called from inside that string, per row.
{
  const b = board();
  b.breakRow([3]);
  b.open("alpha");
  await b.drain();
  t.check("the pane is not blank", b.pane().length > 0, true);
  t.check("the five rows that can be drawn are drawn", b.rows(), 5);
  t.check("and the one that cannot says so, in its place", b.brokenRows(), 1);
  t.check("naming which pull request it was", b.pane().includes("alpha#3 — skein could not draw this row"), true);
  t.check("and why, in the page rather than in devtools",
    b.pane().includes("cannot read properties of undefined"), true);
  t.check("with a way to go and look at it anyway",
    b.pane().includes("https://github.com/alpha/pull/3"), true);
  // The row still EXISTS. Dropping it from the keyboard's list would shorten j/k for as long as
  // the fault lasted — a second failure hiding behind the first.
  t.check("the broken row keeps its place in the queue", b.pane().includes(`data-rk="alpha#3"`), true);
}

{
  // Every row throwing is still not a blank page: six rows that say so beats nothing at all.
  const b = board();
  b.breakRow([1, 2, 3, 4, 5, 6]);
  b.open("alpha");
  await b.drain();
  t.check("a queue where every row throws still draws a queue", b.brokenRows(), 6);
  t.check("and the pane's own controls survive it", b.pane().includes("revsearch"), true);
}

t.done();
