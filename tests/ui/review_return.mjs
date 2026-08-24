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
    let revFilter = "all", revLoading = false, revStaleTimer = null;
    let revModsOpen = false, revCounts = [];
    let revSumBusy = 0;
    const REV_SUM_PARALLEL = 3;
    ${grab("revSeen")}
    ${grab("revStaleTries")}
    ${grab("REV_STALE_TRIES")}
    ${grab("REV_SUM_AUTO")}
    ${grab("revSumAuto")}
    ${grab("revSumRepo")}
    ${grab("revHeld")}
    ${grab("REV_LANES")}
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
    ${grab("renderReview")}
    // Stubbed: this suite asks WHAT is on screen, not how a row is drawn.
    const revRow = pr => "<row n=" + pr.number + ">";
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
      got: n => revSums.get(n),
      open_rows: () => revOpen.size,
      spent: () => revSumAuto,
      expand: n => { revOpen.add(n); },
      tries: () => revStaleTries,
      fetchOne: n => revFetchSummary(n, true),
    };
  `;
  // Settled by default — two hours since the head commit. A pull request skein has no commit date
  // for, or one pushed to minutes ago, is not read on its own, so a fixture without this reads as
  // an empty queue and every assertion about reading would be vacuous.
  const SETTLED = new Date(Date.now() - 2 * 3600 * 1000).toISOString();
  let hot = [];            // numbers whose head commit landed just now
  let moved = [];          // numbers whose head has moved since it was read
  let fresh = true;          // whether the server has the current list yet
  const queue = id => ({
    ai: true,
    fresh,
    prs: [1, 2, 3, 4, 5, 6].map(n => ({
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
  // Answers on demand rather than immediately, so a test can look at the pane between a request and
  // its answer — which is where both defects lived.
  let pending = [];
  let refuse = null;              // a repo whose summaries the server will not serve
  let known = {};                 // readings already on disk, as the bulk route answers them
  const fetch = (url) => {
    const id = decodeURIComponent(url.match(/repos\/([^/]+)\/review/)[1]);
    asked.push(url);
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
    // Everything, until it stops asking.
    drain: async () => { for (let i = 0; i < 20 && pending.length; i++) await settle(); },
    refuse: id => { refuse = id; },
    // The server has not caught up yet: it hands over the copy it remembers.
    stale: on => { fresh = !on; },
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

// ---- a real repo switch, made from a box view: nothing is carried across ----
//
// The half that keeps the fix above honest. PR numbers collide across repos, so alpha's #2 must
// never open as beta's #2 — and the switch that proves it is the one made from a box view, where
// `view.repo` is undefined and the old guard could not tell the two cases apart.
{
  const b = board();
  b.open("alpha");
  await b.drain();
  b.expand(2);

  b.box("some-box");
  b.open("beta");
  t.check("switching repos drops the previous repo's summaries", b.sums(), 0);
  t.check("and its expansions", b.open_rows(), 0);
  t.check("and the new repo gets its own allowance", b.spent(), 0);
  await b.drain();
  t.check("the new repo's queue is what is shown", b.rows(), 6);
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

// ---- the actual review: what the pane posts is exactly what was kept ----
//
// The vetting happens HERE, in the pane — the server posts whatever this sends. So the property
// that "a dropped comment is not posted" lives in `revCritiquePost`, and is proven against the
// real function with a fetch that records what it was given.
function critWorld() {
  const sent = [];
  const toasts = [];
  const body = `
    let view = { repo: "alpha", box: null };
    ${grab("esc")}
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
      seed: (n, critique) => revCrits.set(n, { open: true, busy: false, posting: false, critique, drop: new Set(), posted: "" }),
      drop: (n, i) => revCritKeep(n, i, false),
      post: (n, sha) => revCritiquePost(n, sha),
      html: pr => revCritiqueHtml(pr),
    };
  `;
  return { world: new Function("sent", "toasts", body)(sent, toasts), sent, toasts };
}

{
  const { world, sent } = critWorld();
  world.seed(7, {
    head_sha: "h1", overall: "note", truncated: false,
    comments: [
      { path: "a.rs", line: 2, anchored: true, text: "first" },
      { path: "b.rs", line: 5, anchored: true, text: "second — to be dropped" },
      { path: "c.rs", line: 0, anchored: false, text: "third" },
    ],
  });
  world.drop(7, 1);
  world.post(7, "h1");
  await new Promise(r => setTimeout(r, 0));

  t.check("one review request went out", sent.length, 1);
  const posted = sent[0].body;
  t.check("the dropped comment is not in it", posted.comments.map(c => c.text), ["first", "third"]);
  t.check("what was kept is sent verbatim, vetted here and nowhere else",
    posted.comments.every(c => c.text !== "second — to be dropped"), true);
  t.check("the head the draft read rides along", posted.head_sha, "h1");
}

{
  // Everything dropped and no note: refused in the pane, before any request exists to regret.
  const { world, sent, toasts } = critWorld();
  world.seed(8, { head_sha: "h1", overall: "", comments: [{ path: "a.rs", line: 2, anchored: true, text: "only" }] });
  world.drop(8, 0);
  world.post(8, "h1");
  await new Promise(r => setTimeout(r, 0));
  t.check("nothing kept posts nothing", sent.length, 0);
  t.check("and says so", toasts.length >= 1, true);
}

{
  // A draft of an earlier commit: the pane says so and the post button is off — the server would
  // refuse too, but the person deserves the sentence before the press, not after.
  const { world } = critWorld();
  world.seed(9, { head_sha: "old", overall: "x", comments: [] });
  const html = world.html({ number: 9, head_sha: "new" });
  t.check("a stale draft is named", html.includes("Drafted before the latest commits"), true);
  t.check("and posting is off until it is drafted again", html.includes("disabled"), true);
}

t.done();
