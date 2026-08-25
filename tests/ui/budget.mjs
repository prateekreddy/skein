// Who is allowed to spend a model call on a pull request, and who pays for it.
//
// This suite used to be about the PAGE's budget — six unasked readings per repo, held in
// `REV_SUM_AUTO`/`revSumAuto`. That budget was wrong twice over and is gone (SKEIN-227):
//
//   1. it counted REQUESTS, and a request answered from the cache on disk costs nothing — so a
//      reload spent the whole allowance on six free answers and every row past the sixth was never
//      read, on any reload, for ever;
//   2. every approve reloaded the pane, and the reload handed out a fresh allowance — one button
//      press authorised six more readings, uncapped. A budget the client holds is a budget any
//      client action can refill.
//
// The ledger lives on the server now, at the model call (`review::over_budget`), and the boundary it
// draws is WHO ASKED — the owner's rule, verbatim: "Limit is only for automatic stuff, manually I
// can invoke as many as I want." So what this suite proves is the page's half of that boundary: the
// pump asks for everything unread and marks none of it as asked, a person's read carries the marker
// that exempts it, and a row the server refused for budget says so with the button that is never
// refused.
//
//   node tests/ui/budget.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed to what these functions touch.
function board({ computed = () => true, prs = 29, answer } = {}) {
  const asked = [];
  let inflight = 0, peak = 0;
  const body = `
    ${grab("REV_SUM_PARALLEL")}
    ${grab("revHeld")}
    let revSumBusy = 0;
    let revSums = new Map();
    ${grab("rk")}
    ${grab("revCrits")}
    const marked = { parse: text => text };
    const revQueue = {
      ai: true,
      prs: Array.from({ length: ${prs} }, (_, i) => ({ number: i + 1, repo_id: "acme", lane: "needs-you",
        draft: false, head_sha: "h" + (i + 1) })),
    };
    ${grab("revDraftedReview")}
    ${grab("revDraftSection")}
    ${grab("revGist")}
    ${grab("revDetail")}
    ${grab("revPumpSummaries")}
    ${grab("revFetchSummary")}
    return {
      // The pane is open: a fetch refuses to ask for anything when it is not, which is what keeps a
      // summary landing after you left out of a pane that has moved on. (No backticks in this
      // block: the whole thing is a template literal.)
      hold: () => { revHeld = "*"; },
      pump: () => revPumpSummaries(),
      read: () => [...revSums.keys()].length,
      // What a person pressing the button in the row does.
      byHand: (n, how) => revFetchSummary("acme", n, how),
      // The expanded row, drawn from whatever answer landed for it.
      body: n => revDetail(revQueue.prs[n - 1]),
      line: n => revGist(revSums.get("acme#" + n)),
      // A page reload forgets what it has read; the server's ledger does not.
      forget: () => { revSums = new Map(); },
    };
  `;
  const fetch = (url) => {
    const number = Number(url.match(/review\/(\d+)\/summary/)[1]);
    asked.push(url);
    inflight++; peak = Math.max(peak, inflight);
    const s = answer
      ? answer(number)
      : { number, head_sha: "h" + number, depth: "line", line: "x", computed: computed(number) };
    // A microtask later, so the throttle is observable: everything resolving synchronously would
    // make "three at a time" unmeasurable and the assertion vacuous.
    return new Promise(resolve => setTimeout(() => {
      inflight--;
      resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(s)) });
    }, 0));
  };
  const made = new Function("fetch", "encodeURIComponent", "esc", "console", "renderReview", body)(
    fetch, encodeURIComponent, String, console, () => {});
  made.hold();
  return { ...made, asked, peak: () => peak };
}

const settle = () => new Promise(r => setTimeout(r, 0));
// The pump refills itself as each answer lands, so draining it means letting the queue run until it
// stops asking for more.
async function drain(b) {
  for (let i = 0; i < 200; i++) { b.pump(); await settle(); }
}

// ---- the page keeps no total of its own: everything unread is asked for ----
{
  const b = board({ computed: () => true });
  await drain(b);
  t.check("every row in the lane is read", b.read(), 29);
  // The old bug's inverse, and the reason the client-side count had to go: a queue answered
  // entirely from disk used to stop at six for ever.
  const free = board({ computed: () => false });
  await drain(free);
  t.check("and a queue answered off disk is read to the end too", free.read(), 29);
  // A reload does not change the answer either — there is nothing left to refill.
  free.forget();
  await drain(free);
  t.check("a reload reads what it forgot, and no rule stops it", free.read(), 29);
}

// ---- the throttle survives, because it is not a budget ----
{
  const b = board({ computed: () => true });
  await drain(b);
  t.check("no more than three readings are in flight at once", b.peak() <= 3, true);
  t.check("and it is the page's own constant that says three", b.peak(), 3);
}

// ---- who asked: the marker the server's ceiling turns on ----
{
  const b = board({ computed: () => true });
  await drain(b);
  t.check("nothing the pump asks for claims to have been asked for",
    b.asked.some(u => u.includes("asked=1")), false);

  b.byHand(1, "asked");
  await settle();
  t.check("a read a person revealed carries the marker",
    b.asked.some(u => /\/1\/summary\?asked=1$/.test(u)), true);
  b.byHand(2, "force");
  await settle();
  // One spelling, not two: `force` already means asked on the server (`let asked = force || …`),
  // so a re-read that also said asked=1 would be a second name for the same fact.
  t.check("a re-read says force and nothing more, because force already means asked",
    b.asked.some(u => /\/2\/summary\?force=1$/.test(u)), true);
}

// ---- the day's budget, spent: the row says so and offers the read that is never refused ----
{
  const because = "today's automatic reading budget is spent (100/100) — press read to analyse " +
    "this one now; the budget resets at midnight UTC.";
  const b = board({ prs: 3, answer: number => ({
    number, head_sha: "h" + number, depth: "unread", line: "", flags: [], yours: [], others: 0,
    computed: false, budget_stopped: number === 1, unread_because: number === 1 ? because : "its diff is too large",
  }) });
  await drain(b);

  const stopped = b.body(1);
  t.check("the refusal is surfaced rather than filed with the other absences",
    stopped.includes("skein stopped reading for today."), true);
  t.check("carrying the server's own sentence", stopped.includes("budget resets at midnight UTC"), true);
  t.check("and the button that asks for it by hand",
    stopped.includes(`revFetchSummary('acme', 1, 'asked')`), true);
  t.check("drawn as the one absence with a move in it", stopped.includes(`class="revnosum budget"`), true);

  // Another row unread for its own reason must NOT wear the invitation: a button that cannot help
  // is worse than no button, and the two states are one flag apart.
  const large = b.body(2);
  t.check("a row unread for another reason keeps the plain statement",
    large.includes("Not summarised — its diff is too large"), true);
  t.check("and offers no budget button", large.includes("read this one now"), false);

  // The collapsed line is unchanged: it says what it always says, so the queue can be scanned.
  t.check("the row still states its absence where you scan", b.line(1).includes("not read —"), true);
}

t.done();
