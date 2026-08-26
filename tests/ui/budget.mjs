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
import { draftRules, grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed to what these functions touch.
// `readAhead` is the owner's per-repo consent (`repos::Repo::read_prs`), which the page reads off
// the workflows payload. `shape` overrides the lane and the reasons of every row, because the
// pump's scope is a question about those two and nothing else.
function board({ computed = () => true, prs = 29, answer, readAhead = true, shape } = {}) {
  const asked = [];
  let inflight = 0, peak = 0;
  const rows = Array.from({ length: prs }, (_, i) => ({
    number: i + 1, repo_id: "acme", lane: "needs-you", draft: false, head_sha: "h" + (i + 1),
    // Somebody asked you to review it: the ordinary row this whole suite was written about.
    reasons: ["reviewer"],
    ...(shape ? shape(i + 1) : {}),
  }));
  const body = `
    ${grab("REV_SUM_PARALLEL")}
    ${grab("revHeld")}
    let revSumBusy = 0;
    let revSums = new Map();
    // A reading in flight is state of its own (SKEIN-333); the row's gist consults it, so a world
    // that lifts the gist has to have one even when nothing here ever puts a reading in it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    ${grab("rk")}
    ${grab("revCrits")}
    const marked = { parse: text => text };
    // What loadWorkflows leaves behind, and the only thing the pump reads it for.
    const revFlows = new Map([["acme", { read_prs: ${readAhead} }]]);
    const revQueue = { ai: true, prs: ${JSON.stringify(rows)} };
    ${grab("revDraftedReview")}
    ${grab("revDraftSection")}
    ${grab("revGist")}
    ${grab("revDetail")}
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    // A reading is not a review (SKEIN-371): a step skein read and could not review must not count
    // as read, and must offer its own retry.
    ${grab("revNoReviewCameBack")}
    ${grab("revReadAgain")}
    ${grab("revPumpSummaries")}
    // A reading that COST a model call records how long it took, so a stack read can estimate
    // (SKEIN-337, and SKEIN-352's correction: the server's own computed flag, not who asked).
    ${grab("revNoteReadMs")}
    let revReadMs = [];
    // What the "updated" mark means — a reading REPLACED, not a reading arrived.
    ${grab("revHasReading")}
    ${grab("revFetchSummary")}
    // A read is STARTED by a request and ANSWERED on the page's live stream (SKEIN-366), so a world
    // that drives revFetchSummary has to carry both halves: the map of what is being waited for,
    // and the function an EventSource message runs. (No backticks here - template literal.)
    ${grab("revReadWaits")}
    // Where a landed reading came from (SKEIN-390). Lifted wherever revReadSettle or the bulk merge
    // is, because both write to it: a reading replaced loses the note about which queue built it.
    ${grab("revReadFrom")}
    ${grab("revReadSettle")}
    ${grab("revReadArrived")}
    return {
      // The stream, as this world plays it: hand a reading back the way /api/events does.
      arrive: d => revReadArrived(d),
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
      // The collapsed line's read control, which appears only on a row nothing is going to read.
      readControl: n => revReadAgain(revQueue.prs[n - 1]),
      line: n => revGist(revSums.get("acme#" + n)),
      // A page reload forgets what it has read; the server's ledger does not.
      forget: () => { revSums = new Map(); },
    };
  `;
  // **The request no longer carries the answer** (SKEIN-366). It starts the reading and returns at
  // once; the reading itself comes back on the page's `EventSource`. So the stub is two things: a
  // POST that answers immediately, and the stream that delivers the summary a tick later.
  //
  // `inflight` therefore counts readings from the press to the ANSWER, which is what the throttle is
  // about — counting the POST alone would make every assertion about "three at a time" vacuous,
  // because a POST that returns immediately is never concurrent with anything.
  let arrive = null;
  const fetch = (url) => {
    const number = Number(url.match(/review\/(\d+)\/read/)[1]);
    asked.push(url);
    inflight++; peak = Math.max(peak, inflight);
    const s = answer
      ? answer(number)
      : { number, head_sha: "h" + number, depth: "line", line: "x", computed: computed(number) };
    // A microtask later, so the throttle is observable: everything resolving synchronously would
    // make "three at a time" unmeasurable and the assertion vacuous.
    setTimeout(() => {
      inflight--;
      arrive({ repo_id: "acme", number, summary: s });
    }, 0);
    return Promise.resolve({ ok: true, text: () => Promise.resolve("{\"ok\":true}") });
  };
  const made = new Function(
    "fetch", "encodeURIComponent", "esc", "console", "renderReview", "renderReviewNow", body,
  )(fetch, encodeURIComponent, String, console, () => {}, () => {});
  arrive = made.arrive;
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

// ---- what skein reads on ITS OWN: the scope, and where it stops ----
//
// One sentence decides every check in this block: **if you pressed it, it is free and
// unconditional; if skein decided to read it, that happens only in a repo you switched read-ahead
// on for, and it is counted against the day.** The pane's pump is the second half — it is skein's
// initiative however present you are — so it obeys exactly the scope the ten-minute background
// pass obeys (`review::unasked_scope`, `review::worth_reading`).
//
// The server enforces all of it at the model call, which is why a client edit cannot widen it
// (SKEIN-242). What this block proves is the page's half: that it does not ASK for what would be
// refused, because a pane that asks anyway paints a refusal on every row and spends a round trip
// per row to do it.
{
  const off = board({ readAhead: false });
  await drain(off);
  t.check("a repo with read-ahead off is not read by the pane's pump either", off.read(), 0);
  t.check("and no request was made at all, not one refused", off.asked.length, 0);
  // The other half of the same sentence: what you press is yours, and the switch has no say in it.
  off.byHand(1, "force");
  await settle();
  t.check("pressing read it on one of its rows still reads it", off.read(), 1);
  t.check("and it says who asked, so the day is not charged for it",
    off.asked.every(u => /redraft=1$/.test(u)), true);
  // The collapsed line, on a row the pump is never going to reach: it must offer the press. It
  // used to keep its own copy of the pump's rule and answer "a reading is coming" for ever.
  t.check("a row nothing will read on its own offers the press",
    board({ readAhead: false }).readControl(1).includes(">read it<"), true);
}

// A mention is somebody talking ABOUT you, and reaches the your-move lane like anything else. It
// used to be read and charged by the pump alone, in a lane the background reader agreed with and a
// scope it did not (SKEIN-242).
{
  const m = board({ shape: () => ({ reasons: ["mentioned"] }) });
  await drain(m);
  t.check("a mention is not a reason to read on skein's own initiative", m.read(), 0);
  m.byHand(3, "force");
  await settle();
  t.check("and it is still readable the moment you ask", m.read(), 1);
}
{
  const team = board({ shape: () => ({ reasons: [{ team: "platform" }] }) });
  await drain(team);
  t.check("a team's review request IS a review request", team.read(), 29);
}

// SKEIN-277. The server has read and drafted the pull requests you opened since f69c611 — one
// merged model call, in the waiting lane, because that is the only lane they are ever in
// (`prq.rs:1190`). The pane's pump still said `lane === "needs-you"`, so an authored row filled in
// only when the ten-minute background tick reached it, never from having the pane open.
{
  const mine = board({ shape: () => ({ lane: "waiting", reasons: ["author"] }) });
  await drain(mine);
  t.check("a pull request you opened is read from the pane, not only from the background",
    mine.read(), 29);
  t.check("and before it lands the row offers no button, because one is already coming",
    board({ shape: () => ({ lane: "waiting", reasons: ["author"] }) }).readControl(1), "");
}
// Not the whole lane, though: a pull request you already decided on sits in it too, and it has had
// your attention already.
{
  const theirs = board({ shape: () => ({ lane: "waiting", reasons: ["reviewer"] }) });
  await drain(theirs);
  t.check("a pull request you already decided on is left alone", theirs.read(), 0);
}
// A draft is the author saying it is not finished — including your own, which is the case
// authorship could have swallowed.
{
  const draft = board({ shape: () => ({ lane: "waiting", reasons: ["author"], draft: true }) });
  await drain(draft);
  t.check("a draft you opened yourself is still not read unasked", draft.read(), 0);
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
    b.asked.some(u => /\/1\/read\?asked=1$/.test(u)), true);
  b.byHand(2, "force");
  await settle();
  // ONE spelling, and it is the one that says what must come BACK. `redraft=1` implies the forced
  // read and implies asked (`let force = redraft || …; let asked = force || …`), so a press that
  // also said force=1 or asked=1 would be two more names for facts this one already carries. Since
  // SKEIN-293 there is one control and it always produces both halves, so this IS the press.
  t.check("a read a person pressed says redraft and nothing more",
    b.asked.some(u => /\/2\/read\?redraft=1$/.test(u)), true);
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

// ---- SKEIN-313: what the chip's demotion tally is allowed to ask -------------------------------
//
// `revCommonChips` counts "ready" so a chip true of most of the queue stops being drawn — it has
// stopped saying which row to open. It counted with `revDraftedReview`, which needs `s.critique`,
// and `Known::thin` took `critique` out of the queue payload (SKEIN-287). So on the payload
// `/review/summaries` actually sends, "ready" was counted zero times however many rows wore it, and
// the chip could never be demoted.
//
// Checked as the disagreement itself rather than by rendering a queue: the two functions answer the
// same question off different vocabularies, and the bug is entirely that the tally asked the one
// the wire no longer carries. A fixture that includes `critique` cannot fail this at all, so this
// one deliberately does not have the key.
{
  const thinned = new Function(`
    let revSums = new Map();
    // A reading in flight is state of its own (SKEIN-333); the row's gist consults it, so a world
    // that lifts the gist has to have one even when nothing here ever puts a reading in it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    ${grab("rk")}
    let revQueue = null;
    ${grab("revDraftedReview")}
    ${draftRules()}
    // Exactly what a thinned row carries: has_critique + drafted, and no critique key at all.
    const pr = { repo_id: "acme", number: 7, head_sha: "h7" };
    revSums.set("acme#7", { has_critique: true, drafted: { head_sha: "h7", comments: 3 } });
    return {
      onTheWire: revDraftAtHead(pr),
      needsTheDroppedField: revDraftedReview(pr),
      // And the head is still checked, so a draft that moved with a kept reading of an EARLIER
      // commit is not counted as a draft of this pull request.
      movedHead: revDraftAtHead({ ...pr, head_sha: "h8" }),
    };
  `)();

  t.check("a drafted review is visible in the row vocabulary the queue payload still carries",
    thinned.onTheWire, true);
  t.check("and invisible to the one it dropped — which is why the tally must not ask that one",
    thinned.needsTheDroppedField, null);
  t.check("a draft against an earlier commit is still not a draft of this one",
    thinned.movedHead, false);

  // The tally line itself, read out of the page: the check above says which function is right, and
  // this says the demotion actually asks it. Text rather than behaviour because `revCommonChips` is
  // computed inside `revRenderPane`, which needs a browser — and the failure being guarded is one
  // identifier, on one line.
  const tally = grab("revRenderPane");
  // `revDraftHeld` since SKEIN-355: the chip is earned by a draft of ANY vintage, labelled by the
  // commit it read, so the tally that decides whether to demote it has to count the same rows the
  // chip draws on. Counting `revDraftAtHead` would leave every older draft out of the demotion and
  // the chip standing on a queue where it says nothing.
  t.check("the ready tally asks the question the wire can answer",
    /revDraftHeld\(p\)\) kinds\.add\(/.test(tally), true);
  // …and counts POSTED as its own kind (SKEIN-364), because the two are two chips. One shared
  // tally on a lane skein has drafted for would take the mark off exactly the reviews already sent
  // — the minority, and the ones whose chip changes what you do.
  t.check("and counts a posted review apart from one still waiting to be posted",
    /revDraftPosted\(p\) \? "posted" : "ready"/.test(tally), true);
  t.check("and not the one it cannot", /revDraftedReview\(p\)\) kinds\.add/.test(tally), false);
}

t.done();
