// What survives a trip to a box and back to the review queue?
//
// Reported live: "it worked first time but when I clicked a box and went back to PRs the entire
// thing disappears", followed by "and it starts reading again".
//
// Measured here rather than assumed, and it is not the rows: those come back, because the last
// queue per repo is remembered (`revSeen`). What is thrown away is everything the pane had LEARNED
// — every summary read for that repo, and every row expanded — after which it asks the server for
// all of them again. The re-read is free where the server still holds them, so the cost is not
// money; it is that the pane discards its own work in front of you and then visibly redoes it,
// every single time you look at a box.
//
// The cause is one line in `openReview`, and it is the right idea aimed at the wrong variable:
//
//     if (view.repo !== id) { revQueue = null; revOpen = new Set(); revSums = new Map(); ... }
//
// It exists because PR numbers collide across repos — #12 of one must never open as #12 of the
// other — but it asks whether the CURRENT VIEW names this repo, and a box view names no repo at
// all. Leaving review for a box therefore reads as "switching repos", every time.
//
// Both halves are tested here, because a fix that only keeps state is a fix that carries one repo's
// expansions into another's rows. The second scenario is the one that keeps the first honest.
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
    ${grab("REV_SUM_AUTO")}
    ${grab("revSumAuto")}
    ${grab("revSumRepo")}
    ${grab("revHeld")}
    ${grab("REV_LANES")}
    ${grab("revAllowanceFor")}
    ${grab("revPumpSummaries")}
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
    const revFetchSummary = n => { asked.push(n); revSums.set(n, "…"); };
    return {
      open: id => openReview(id),
      // Clicking a box: the dock's own view change, verbatim from \`showBox\`.
      box: name => { view = { box: name, mode: "term", kind: "agent" }; },
      rows: () => (revpane.innerHTML.match(/<row /g) || []).length,
      sums: () => revSums.size,
      open_rows: () => revOpen.size,
      spent: () => revSumAuto,
      asked: () => asked.slice(),
      // What a summary landing looks like, without the fetch.
      read: (n, sha) => { revSums.set(n, { number: n, head_sha: sha, line: "x" }); revSumAuto++; },
      expand: n => { revOpen.add(n); },
      repo: () => view.repo,
    };
  `;
  const queue = id => ({
    ai: true,
    fresh: true,
    prs: [1, 2, 3].map(n => ({ number: n, lane: "needs-you", draft: false, head_sha: id + n, reasons: ["reviewer"] })),
    blind_spots: [],
  });
  // Answers on the next microtask, so a test can look at the pane BEFORE the queue lands — which is
  // the moment the pane went blank.
  let pending = [];
  const fetch = (url) => {
    const id = decodeURIComponent(url.match(/repos\/([^/]+)\/review/)[1]);
    return new Promise(resolve => pending.push(() => resolve({
      ok: true, text: () => Promise.resolve(JSON.stringify(queue(id))),
    })));
  };
  const revpane = { innerHTML: "", classList: { toggle() {} } };
  const document = { body: { classList: { remove() {}, toggle() {} } } };
  const localStorage = { store: {}, getItem(k) { return this.store[k] ?? null; }, setItem(k, v) { this.store[k] = v; } };
  const made = new Function(
    "revpane", "document", "localStorage", "fetch", "asked", "esc", "encodeURIComponent",
    "decodeURIComponent", "setTimeout", "clearTimeout", "console", body,
  )(
    revpane, document, localStorage, fetch, asked, String, encodeURIComponent,
    decodeURIComponent, () => 0, () => {}, console,
  );
  return { ...made, settle: async () => { const p = pending; pending = []; p.forEach(f => f()); await new Promise(r => setTimeout(r, 0)); } };
}

// ---- a trip to a box and back: everything the pane knew is still known ----
{
  const b = board();
  b.open("alpha");
  await b.settle();
  t.check("the queue paints on the first visit", b.rows(), 3);

  // Three rows read, one expanded — the work the pane has done for you.
  b.read(1, "alpha1"); b.read(2, "alpha2"); b.read(3, "alpha3");
  b.expand(2);
  const spent = b.spent();
  // What the FIRST visit asked for is legitimate — those are the reads that filled the pane. The
  // question is whether coming back asks for any of it a second time, so the count is taken here.
  const before = b.asked().length;

  b.box("some-box");
  b.open();   // the PR button, which passes no repo id

  // BEFORE the fetch resolves. This is the moment the pane was blank: the queue had been thrown
  // away and the request that would replace it had not come back yet.
  t.check("coming back paints immediately, from what is already known", b.rows(), 3);
  t.check("the summaries that were read are still read", b.sums(), 3);
  t.check("and a row you had expanded is still expanded", b.open_rows(), 1);
  t.check("nothing is read again", b.asked().length, before);
  t.check("so the allowance is not spent twice", b.spent(), spent);

  await b.settle();
  t.check("and the refetch leaves it that way", b.rows(), 3);
  t.check("summaries survive the refetch too", b.sums(), 3);
}

// ---- a real repo switch, made from a box view: nothing is carried across ----
//
// The half that keeps the fix above honest. PR numbers collide across repos, so alpha's #2 must
// never open as beta's #2 — and the switch that proves it is the one made from a box view, where
// `view.repo` is undefined and the old guard could not tell the two cases apart.
{
  const b = board();
  b.open("alpha");
  await b.settle();
  b.read(1, "alpha1"); b.read(2, "alpha2");
  b.expand(2);

  b.box("some-box");
  b.open("beta");
  t.check("switching repos drops the previous repo's summaries", b.sums(), 0);
  t.check("and its expansions", b.open_rows(), 0);
  t.check("and the new repo gets its own allowance", b.spent(), 0);
  await b.settle();
  t.check("the new repo's queue is what is shown", b.rows(), 3);
}

t.done();
