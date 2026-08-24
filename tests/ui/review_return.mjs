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
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed down to what these functions touch.
function board() {
  const asked = [];
  const body = `
    let view = { box: null, mode: "term", kind: "agent" };
    const repos = [{ id: "alpha" }, { id: "beta" }];
    let revQueue = null, revOpen = new Set(), revSums = new Map(), revMods = null;
    let revReading = null, revReturnScroll = 0, revSearch = "";
    const revDiffs = new Map(), revNotes = new Map();
    let revFilter = "all", revLoading = false, revStaleTimer = null;
    let revRepoFilter = "";
    let revCrits = new Map();
    let revModsOpen = false, revCounts = [];
    let revSumBusy = 0;
    const REV_SUM_PARALLEL = 3;
    ${grab("revSeen")}
    ${grab("rk")}
    ${grab("revMergeQueues")}
    ${grab("revScopeRepo")}
    ${grab("revChains")}
    ${grab("revStackName")}
    ${grab("revMisnamed")}
    ${grab("revStackNext")}
    ${grab("revStackLane")}
    ${grab("revStackOpenKey")}
    ${grab("revStackStep")}
    ${grab("toggleRevStack")}
    ${grab("toggleStackStep")}
    ${grab("revRail")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revStackRow")}
    ${grab("revStackSteps")}
    ${grab("toggleRevRow")}
    ${grab("revStaleTries")}
    ${grab("REV_STALE_TRIES")}
    ${grab("REV_SUM_AUTO")}
    ${grab("revSumAuto")}
    ${grab("revSumRepo")}
    ${grab("revHeld")}
    ${grab("REV_LANES")}
    ${grab("revNotReadyOpen")}
    ${grab("toggleNotReady")}
    ${grab("revNotReadyWhy")}
    ${grab("REV_SETTLE_MS")}
    ${grab("revSettled")}
    ${grab("revSettlesIn")}
    ${grab("revAllowanceFor")}
    ${grab("loadKnownSummaries")}
    ${grab("revPumpSummaries")}
    ${grab("revFetchSummary")}
    ${grab("revMatchesFilter")}
    ${grab("openReview")}
    ${grab("loadReview")}
    ${grab("revSnooze")}
    ${grab("revSnoozeRed")}
    ${grab("renderReview")}
    ${grab("renderReading")}
    ${grab("revWaitedSince")}
    ${grab("revReadBand")}
    ${grab("revMoved")}
    ${grab("revMatchesSearch")}
    ${grab("revSearchSet")}
    ${grab("readingFiles")}
    ${grab("openReading")}
    ${grab("closeReading")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    const revRenderNotes = () => {};
    const revComposeHtml = () => "";
    const renderDiff = txt => '<div class="diff">' + txt.split(String.fromCharCode(10)).map(l => '<span class="ln">' + l + '</span>').join('') + '</div>';
    // Stubbed: this suite asks WHAT is on screen, not how a row is drawn.
    const revRow = pr => "<row n=" + pr.number + ">";
    const revBody = () => "";
    const revModsCount = () => "notes";
    const revModsHtml = () => "";
    const revAgo = () => "just now";
    const renderRevBadge = () => {};
    const applyView = () => {};
    const persistView = () => {};
    const toast = () => {};
    const loadWorkflows = () => {};
    let revEdit = null;
    const revEditHtml = () => "";
    const revReadChip = () => "";
    // The page's own scheduler, so the test can see WHEN it would ask again rather than waiting.
    
    return {
      open: id => openReview(id),
      // Clicking a box: the dock's own view change, verbatim from \`showBox\`.
      box: name => { view = { box: name, mode: "term", kind: "agent" }; },
      rows: () => (revpane.innerHTML.match(/<row /g) || []).length,
      sums: () => [...revSums.values()].filter(s => s !== "…").length,
      got: (n, repo) => revSums.get((repo || "alpha") + "#" + n),
      open_rows: () => revOpen.size,
      openKeys: () => [...revOpen],
      spent: () => revSumAuto,
      expand: (n, repo) => { revOpen.add((repo || "alpha") + "#" + n); },
      tries: () => revStaleTries,
      fetchOne: (n, repo) => revFetchSummary(repo || "alpha", n, true),
      toggleNR: () => toggleNotReady(),
      stack: key => toggleRevStack(key),
      row: key => toggleRevRow(key),
      openRow: () => [...revOpen],
      search: q => revSearchSet(q),
      common: () => [...revCommonChips],
      bands: () => ((revQueue && revQueue.prs) || []).map(p => [p.number, revReadBand(p)]),
      snoozeRed: () => revSnoozeRed(),
      read: (repo, n) => openReading(repo, n),
      back: () => closeReading(),
      reading: () => revReading,
      note: (key, path, line, body) => { revNotesFor(key).push({ path, line, body }); revNotesSave(key); },
      notes: key => revNotesFor(key).slice(),
    };
  `;
  // Settled by default — two hours since the head commit. A pull request skein has no commit date
  // for, or one pushed to minutes ago, is not read on its own, so a fixture without this reads as
  // an empty queue and every assertion about reading would be vacuous.
  const SETTLED = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  let hot = [];            // numbers whose head commit landed just now
  let moved = [];          // numbers whose head has moved since it was read
  let fresh = true;          // whether the server has the current list yet
  let laneRows = null;       // when set, the queue serves exactly these rows
  const queue = id => ({
    repo_id: id,
    ai: true,
    fresh,
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
    blind_spots: [],
  });
  const lanes = rows => { laneRows = rows; };
  let served = ["alpha"];    // which repos the merged answer carries
  // Answers on demand rather than immediately, so a test can look at the pane between a request and
  // its answer — which is where both defects lived.
  let pending = [];
  let refuse = null;              // a repo whose summaries the server will not serve
  let brokenRepos = [];           // repos whose queue the merged answer reports as failed
  let known = {};                 // readings already on disk, as the bulk route answers them
  const fetch = (url) => {
    asked.push(url);
    // The merged queue: every repo in one answer, the shape the pane opens on (SKEIN-146).
    if (/^\/api\/review($|\?)/.test(url)) {
      return new Promise(resolve => pending.push(() => resolve({
        ok: true,
        text: () => Promise.resolve(JSON.stringify({
          ai: true, queues: served.map(queue),
          failed: brokenRepos.map(id => ({ repo_id: id, needs_you: 0, error: "boom", skipped: "" })),
          skipped: [],
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
      return Promise.resolve({ ok: true, json: () => Promise.resolve({
        head_sha: "sha" + rd[1], diff, cut: false,
      }) });
    }
    const id = decodeURIComponent(url.match(/repos\/([^/]+)\//)[1]);
    // The bulk read: what skein already holds. This fixture holds nothing — every scenario here is
    // about what the pane ASKS for, so an empty answer keeps the pump as the only source and the
    // assertions about it meaningful.
    if (/review\/summaries$/.test(url)) {
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
        return resolve({ ok: true, text: () => Promise.resolve(JSON.stringify({
          number: Number(sum[1]), head_sha: id + sum[1], depth: "line", line: "x", computed: true,
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
    pane: () => revpane.innerHTML,
    // Everything, until it stops asking.
    drain: async () => { for (let i = 0; i < 20 && pending.length; i++) await settle(); },
    refuse: id => { refuse = id; },
    serves: ids => { served = ids; },
    // The server has not caught up yet: it hands over the copy it remembers.
    stale: on => { fresh = !on; },
    broken: ids => { brokenRepos = ids; },
    // What the page asked to be woken for, and how long it wanted to wait.
    waits: () => waits.slice(),
    fire: () => { const due = waits.map(w => w.fn); waits.length = 0; due.forEach(f => f()); },
    // A pull request somebody has just pushed to.
    hot: ns => { hot = ns; },
    // Readings skein already holds, which the pane must show without asking for any of them.
    holds: map => { known = map; },
    // The branch moving under a reading that has already been made.
    moved: ns => { moved = ns; },
    // Summary requests only — the queue's own fetches are not what these counts are about.
    reads: () => asked.filter(u => /review\/\d+\/summary/.test(u)),
    posts: () => asked.filter(u => u.includes("/snooze")),
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
  const spent = b.spent();
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
  t.check("so the allowance is not spent twice", b.spent(), spent);

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

// ---- a pull request still being pushed to is left alone ----
//
// Asked for: "PR should be analyzed in background once there have been no commits for atleast an
// hour". Reading a branch somebody is mid-push on spends a model call describing a commit that is
// about to stop being the head — and the next poll spends another.
{
  const b = board();
  b.hot([2, 5]);           // two of the six were pushed to a moment ago
  b.open("alpha");
  await b.drain();

  t.check("a branch that is still moving is not read", b.got(2), undefined);
  t.check("nor asked for", b.reads().filter(u => /\/2\/summary/.test(u)), []);
  t.check("and the settled ones are read", b.sums(), 4);

  // Asking by hand reads it at any age — the rule governs what skein does on its OWN, exactly as
  // it does for drafts. Without this the rule is a wall rather than a default.
  b.fetchOne(2);
  await b.drain();
  t.check("asking for it by hand reads it anyway", b.got(2) && b.got(2).line, "x");
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
  t.check("so no allowance was spent on what was already paid for", b.spent(), 0);
}

// ---- and what is genuinely unread still obeys every limit ----
{
  const b = board();
  b.hot([5, 6]);
  b.holds({ "1": { number: 1, head_sha: "alpha1", depth: "line", line: "read one" } });
  b.open("alpha");
  await b.drain();

  // #1 came free. #2, #3, #4 are unread and settled, so they are asked for. #5 and #6 are still
  // being pushed to, so they are not — the limits are for new analysis, which is the whole point.
  const asked = b.reads().map(u => Number(u.match(/review\/(\d+)\/summary/)[1])).sort();
  t.check("only what is missing is asked for", asked, [2, 3, 4]);
  t.check("and everything known or read is on screen", b.sums(), 4);
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

  t.check("whose-move lanes are on screen",
    ["your move", "their move", "not ready"].every(l => b.pane().includes(l)), true);
  t.check("a red PR awaiting review is drawn with your move, not folded away", b.rows(), 3);
  t.check("the fold states its own composition",
    b.pane().includes("1 draft, 1 conflicted"), true);

  b.toggleNR();
  t.check("one click and every not-ready row is there — folded is not hidden", b.rows(), 5);
}

// ---- the actual review: what the pane posts is exactly what was kept ----
//
// The vetting happens HERE, in the pane — the server posts whatever this sends. So the property
// that "a dropped comment is not posted" lives in `revCritiquePost`, and is proven against the
// real function with a fetch that records what it was given.
function critWorld() {
  const sent = [];
  const toasts = [];
  const body = `
    let view = { repo: "*", box: null };
    ${grab("esc")}
    ${grab("rk")}
    ${grab("revCrits")}
    ${grab("revCritKeep")}
    ${grab("revCritiquePost")}
    ${grab("revCritiqueHtml")}
    const renderReview = () => {};
    const toast = m => toasts.push(m);
    const confirm = () => true;
    const fetch = (url, opts) => {
      sent.push({ url, body: JSON.parse(opts.body) });
      return Promise.resolve({ json: () => Promise.resolve({ ok: true, text: "posted" }) });
    };
    return {
      seed: (repo, n, critique) => revCrits.set(repo + "#" + n, { open: true, busy: false, posting: false, critique, drop: new Set(), posted: "" }),
      drop: (repo, n, i) => revCritKeep(repo + "#" + n, i, false),
      post: (repo, n, sha) => revCritiquePost(repo, n, sha),
      html: pr => revCritiqueHtml(pr),
    };
  `;
  return { world: new Function("sent", "toasts", body)(sent, toasts), sent, toasts };
}

{
  const { world, sent } = critWorld();
  world.seed("alpha", 7, {
    head_sha: "h1", overall: "note", truncated: false,
    comments: [
      { path: "a.rs", line: 2, anchored: true, text: "first" },
      { path: "b.rs", line: 5, anchored: true, text: "second — to be dropped" },
      { path: "c.rs", line: 0, anchored: false, text: "third" },
    ],
  });
  world.drop("alpha", 7, 1);
  world.post("alpha", 7, "h1");
  await new Promise(r => setTimeout(r, 0));

  t.check("one review request went out", sent.length, 1);
  const posted = sent[0].body;
  t.check("the dropped comment is not in it", posted.comments.map(c => c.text), ["first", "third"]);
  t.check("what was kept is sent verbatim, vetted here and nowhere else",
    posted.comments.every(c => c.text !== "second — to be dropped"), true);
  t.check("the head the draft read rides along", posted.head_sha, "h1");
  t.check("it posts to the row's own repo", sent[0].url.includes("/repos/alpha/"), true);
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
  // A draft of an earlier commit: the pane says so and the post button is off — the server would
  // refuse too, but the person deserves the sentence before the press, not after.
  const { world } = critWorld();
  world.seed("alpha", 9, { head_sha: "old", overall: "x", comments: [] });
  const html = world.html({ repo_id: "alpha", number: 9, head_sha: "new" });
  t.check("a stale draft is named", html.includes("Drafted before the latest commits"), true);
  t.check("and posting is off until it is drafted again", html.includes("disabled"), true);
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
{
  const b = board();
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
    let revCommonChips = new Set();
    const revFlows = new Map();
    ${grab("rk")}
    ${grab("revMoved")}
    ${grab("revFlowChip")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revRail")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revGist")}
    ${grab("revRow")}
    const revBody = () => "";
    const toggleRevRow = () => {};
    return { row: pr => revRow(pr), gist: s => revGist(s), move: pr => revMove(pr), sums: revSums,
             commons: kinds => { revCommonChips = new Set(kinds); } };
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

// ---- nine repositories are a strip with counts, not a dropdown whose numbers are a tooltip ----
{
  const b = board();
  b.serves(["alpha"]);          // beta managed but serving nothing this round
  b.broken(["beta"]);
  b.open("");
  await b.drain();
  const pane = b.pane();
  t.check("each repo shows its count where it can be read",
    pane.includes("alpha <b>6</b>") && pane.includes("all <b>6</b>"), true);
  t.check("a repo whose queue failed wears ! instead of a number it does not have",
    /beta <b class="bad"[^>]*>!<\/b>/.test(pane), true);

  const b2 = board();
  b2.serves(["alpha", "beta"]);
  b2.open("");
  await b2.drain();
  t.check("two repos, both counted", b2.pane().includes("beta <b>6</b>") && b2.pane().includes("all <b>12</b>"), true);
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
  t.check("their move keeps recency, newest first", order.slice(3), [500, 400]);
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
    ${grab("revComposing")}
    ${grab("revComposeStore")}
    ${grab("revComposeSave")}
    ${grab("revCompose")}
    ${grab("revAct")}
    let revReading = null;
    const revNotes = new Map();
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    const closeReading = () => {};
    const renderReview = () => {};
    const toast = () => {};
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
  return { world: new Function("localStorage", "posts", "setTimeout", body)(store, posts, () => {}), posts };
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

t.done();
