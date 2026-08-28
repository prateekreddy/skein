// Reading a whole stack in one press, with progress across its steps (SKEIN-337).
//
// The owner runs an 18-step stack live — `ladder/tenants-*`, gadget-demo #684–#701, drawn in the
// pane as "ladder — 18 pull requests, one change · you are at step 2 of 18". Reading it meant
// pressing the row's control eighteen times and waiting ~35 seconds after each.
//
// Two things here are easy to get wrong and are what this suite is for:
//
//   1. **The estimate is measured, not asserted.** A hardcoded "~35s" is a claim about a model, a
//      fleet and a network that all change without telling anyone, and it would go on being printed
//      after it stopped being true. With nothing measured there is NO estimate — an empty string,
//      not a guess.
//   2. **Stopping does not undo.** Steps already read stay read; they were paid for and the
//      readings are on disk. Stopping only stops starting more.
//   3. **A press is not held to the read-ahead width** (SKEIN-353). `REV_SUM_PARALLEL` is skein's
//      OWN initiative and governs nothing a person pressed — the owner, twice: "Limit is only for
//      automatic stuff, manually I can invoke as many as I want" … "If I ask for it, then it is
//      unlimited." A pressed read has its own, far wider ceiling, and it is not a budget: it is a
//      guard against GitHub's secondary rate limit, which src/github.rs answers with a flat
//      fifteen-minute hold on every GitHub read for the whole fleet. Measured live while he waited
//      half an hour: three reads stuck at the width of three, running past their own 180-second
//      budget, and neither stack could advance a single step.
//
//   node tests/ui/stackread.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// A stack of six, which is enough to see a concurrency width of three take two rounds.
const STEPS = [1, 2, 3, 4, 5, 6].map(n => ({
  repo_id: "acme", number: 680 + n, head_sha: "h" + n, title: `tenants 0${n}`,
}));
// `forks` and `rooted` are what `revChains` puts on a stack and what `revStackRow` draws from —
// a fixture without them is a stack shape the pane never produces, and the row throws on it.
const STACK = { key: "acme:ladder", repo_id: "acme", name: "ladder", steps: STEPS,
                forks: new Set(), rooted: true };
// A SECOND stack, because one is the shape that hid SKEIN-353 for as long as it did. Five steps,
// so a run that silently inherited the other's queue would be caught by the totals alone.
const OTHER_STEPS = [1, 2, 3, 4, 5].map(n => ({
  repo_id: "acme", number: 720 + n, head_sha: "c" + n, title: `chassis 0${n}`,
}));
const OTHER = { key: "acme:chassis", repo_id: "acme", name: "chassis", steps: OTHER_STEPS,
                forks: new Set(), rooted: true };
// And one DEEPER than the manual ceiling, which is the only shape that still has a queue to stop.
// Fourteen because the owner's own stack is eighteen and the ceiling is ten: a fixture at or under
// the ceiling would make every assertion about stopping vacuous.
const DEEP_STEPS = Array.from({ length: 14 }, (_, i) => ({
  repo_id: "acme", number: 740 + i, head_sha: "d" + i, title: `deep step ${i + 1}`,
}));
const DEEP = { key: "acme:deep", repo_id: "acme", name: "deep", steps: DEEP_STEPS,
               forks: new Set(), rooted: true };

function world() {
  const body = `
    const REV_SUM_PARALLEL = 3;
    ${grab("REV_ASKED_PARALLEL")}
    let revReadMs = [];
    // A run PER STACK (SKEIN-353), which is the whole of the fix and the reason this map is not a
    // scalar. The two stacks are both in it from the start so that nothing here can pass by
    // accident on a world that only ever holds one.
    let revStackRuns = new Map();
    let revSums = new Map();
    let revInFlight = new Map();
    let revStacks = new Map([["acme:ladder", STACK], ["acme:chassis", OTHER], ["acme:deep", DEEP]]);
    const rk = pr => pr.repo_id + "#" + pr.number;
    const revStackKey = st => st.key;
    const revStepNo = (st, p) => st.steps.indexOf(p) + 1;
    ${grab("revNoteReadMs")}
    ${grab("revReadTypicalMs")}
    ${grab("revStackEstimate")}
    // A reading is not a review (SKEIN-371): a step skein read and could not review must not count
    // as read, and must offer its own retry.
    ${grab("revNoReviewCameBack")}
    ${grab("revStepNeedsReading")}
    ${grab("revStackReadAll")}
    ${grab("revStackStop")}
    ${grab("revStackPump")}
    ${grab("revStackRunHtml")}
    // What the COLLAPSED stack row says about a run (SKEIN-370). The row itself is lifted below
    // rather than the marker alone: the thing the owner cannot see is a rendered row, and a test
    // that only called the marker would pass with the row never asking for it.
    ${grab("revStackRunGist")}
    // The collapsed row, with the parts that are not its subject stubbed to nothing. It is the
    // REAL function: the marker reaching the row, and displacing what it displaces, is exactly what
    // this suite has to hold.
    const revStackOpenKey = null, revSel = null, revFlash = null;
    const revRail = () => "", revStackSteps = () => "", REV_UNROOTED_WHY = "";
    const revStackNext = st => st.steps.find(p => !p.decided) || null;
    ${grab("revStackRow")}
    ${grab("revElapsed")}
    return {
      note: ms => revNoteReadMs(ms),
      estimate: n => revStackEstimate(n),
      typical: () => revReadTypicalMs(),
      needs: p => revStepNeedsReading(p),
      read: (key, s) => revSums.set(key, s),
      flight: key => revInFlight.set(key, { started_ms: 1 }),
      // The AUTOMATIC pass's width, exposed so a case can assert that a press is not held to it
      // rather than asserting a bare number that would agree with any throttle at all.
      parallel: () => REV_SUM_PARALLEL,
      askedWidth: () => REV_ASKED_PARALLEL,
      start: (key = "acme:ladder") => revStackReadAll(key),
      stop: (key = "acme:ladder") => revStackStop(key),
      run: (key = "acme:ladder") => revStackRuns.get(key),
      html: (st = STACK) => revStackRunHtml(st),
      // The collapsed row, as a person scanning the queue reads it.
      row: (st = STACK) => revStackRow(st),
    };
  `;
  // Every read resolves when the case says so, so concurrency is observable. Resolving
  // synchronously would make "three at a time" unmeasurable and the assertion vacuous.
  const settle = [];
  const revFetchSummary = (repo, number) => new Promise(resolve => settle.push({ repo, number, resolve }));
  const w = new Function(
    "esc", "console", "toast", "renderReviewNow", "revFetchSummary", "STACK", "OTHER", "DEEP",
    "Date", body,
  )(String, console, () => {}, () => {}, revFetchSummary, STACK, OTHER, DEEP, Date);
  // Let one queued read finish, and give the pump its microtask to start the next.
  w.settleOne = async () => { (settle.shift() || { resolve: () => {} }).resolve(); await new Promise(r => setTimeout(r, 0)); };
  w.pending = () => settle.length;
  // WHICH pull requests are in flight, so a case can tell two runs apart rather than counting them.
  w.inflight = () => settle.map(x => x.number);
  return w;
}

// ── 1. the estimate is measured or it is not made ──────────────────────────────────────────────
{
  const w = world();
  t.check("a page that has read nothing offers no estimate", w.estimate(18), "");
  t.check("and its typical read is not a number it made up", w.typical(), 0);

  // One 35-second read, which is what was actually measured on the owner's fleet.
  w.note(35_000);
  t.check("one measurement is enough to estimate from", w.estimate(18), " · ~4m");
  // 18 steps, three at a time, 35s each → six rounds → 3m30s, rounded to 4m. Derived, not typed.

  t.check("a short run is said in seconds", w.estimate(3), " · ~35s");

  // The median, not the mean: one slow read must not move the estimate for the other seventeen.
  [35_000, 35_000, 35_000, 400_000].forEach(w.note);
  t.check("one outlier does not drag the estimate with it", w.estimate(3), " · ~35s");
}

// ── 2. what is worth spending on ───────────────────────────────────────────────────────────────
{
  const w = world();
  t.check("a step with no reading needs one", w.needs(STEPS[0]), true);

  w.read("acme#681", { head_sha: "h1", depth: "line", line: "read" });
  t.check("and one already read at this commit does not", w.needs(STEPS[0]), false);

  w.read("acme#681", { head_sha: "OLD", depth: "line", line: "read" });
  t.check("a reading of a commit that has moved needs a new one", w.needs(STEPS[0]), true);

  w.read("acme#681", { head_sha: "h1", depth: "unread", unread_because: "the budget" });
  t.check("so does a row that was never actually read", w.needs(STEPS[0]), true);

  w.read("acme#681", { head_sha: "h1", depth: "unread", transient: true });
  t.check("and one whose reading could not be reached", w.needs(STEPS[0]), true);

  // Never one already in flight: that is somebody else's purchase of the same thing, and starting
  // a second is paying twice for one answer.
  w.read("acme#681", {});
  w.flight("acme#681");
  t.check("but never one already being read", w.needs(STEPS[0]), false);
}

// ── 3. a press starts everything it was asked for, and the progress is true ────────────────────
//
// It used to start three and hold the rest behind the read-ahead width. The assertion below is
// written against that width by NAME rather than against the number six, so it cannot be satisfied
// by a throttle that happens to be wide enough for this fixture.
{
  const w = world();
  w.start();
  t.check("six steps to read", w.run().total, 6);
  t.check("skein's own read-ahead width is three", w.parallel(), 3);
  t.check("a press has its own, wider ceiling", w.askedWidth() > w.parallel(), true);
  t.check("and the press is not held to the read-ahead width", w.pending(), 6);
  t.check("the line says which are running",
    /6 running · steps 1, 2, 3, 4, 5, 6/.test(w.html()), true);
  t.check("with none done yet", /0 of 6 done/.test(w.html()), true);

  await w.settleOne();
  t.check("one lands and the count moves", w.run().done, 1);
  t.check("the rest are still going", w.pending(), 5);
  t.check("and the line says so", /1 of 6 done/.test(w.html()), true);

  for (let i = 0; i < 5; i++) await w.settleOne();
  // The run STAYS once it ends — the line it draws is the answer to "did those eighteen model
  // calls happen", and a run that tidied itself away would take that answer with it. Asserted as a
  // sentence before the count, so a run that vanished reads as this rather than as a TypeError.
  t.check("the finished run is still on the board", !!w.run(), true);
  t.check("the run finishes", w.run().done, 6);
  t.check("and says so rather than vanishing", /read the stack 6 of 6 done/.test(w.html()), true);
  t.check("with no stop control left to press", /stop</.test(w.html()), false);
}

// ── 4. stopping stops STARTING, and does not undo ──────────────────────────────────────────────
//
// A stack deeper than the manual ceiling still has a queue, so `stop` still has something to do:
// it stops starting more and lets what is running finish, because cancelling those would spend the
// money and throw the answer away. The fixture here is deliberately deeper than the ceiling — on a
// six-step stack read ten at a time there would be nothing queued and the case would be vacuous.
{
  const w = world();
  w.start("acme:deep");
  const width = w.askedWidth();
  t.check("a stack deeper than the ceiling starts as many as the ceiling allows",
    w.pending(), width);
  t.check("and keeps the rest queued", w.run("acme:deep").todo.length, 14 - width);

  await w.settleOne();
  const doneBefore = w.run("acme:deep").done;
  w.stop("acme:deep");
  t.check("stopping does not abandon the reads in flight", w.pending(), width);
  t.check("and does not take back what was already read", w.run("acme:deep").done, doneBefore);
  t.check("the line says it is stopping, and how many are still finishing",
    new RegExp(`stopping — ${width} still finishing`).test(w.html(DEEP)), true);

  for (let i = 0; i < 30 && w.pending(); i++) await w.settleOne();
  t.check("nothing new was started after the stop", w.pending(), 0);
  t.check("so the run ends short of its total, and says which it is",
    new RegExp(`stopped ${width + 1} of 14 done`).test(w.html(DEEP)), true);
  // And says what stopping did NOT do. A run that ends short with no word about the rest is the
  // same shape as a run that failed, and the reader's next question is whether what landed is good.
  t.check("and that stopping undid nothing",
    new RegExp(`${13 - width} left unread — nothing was undone`).test(w.html(DEEP)), true);
}

// ── 5. the control before a run, and when there is nothing to do ───────────────────────────────
{
  const w = world();
  t.check("with nothing read, the control offers the whole stack",
    /read all 6/.test(w.html()), true);
  // What it COSTS and how wide it goes — not what it does not count against. The line used to end
  // "and asking never counts against the day", which answers a budget question nobody asked and
  // reads as "this is free" on the most expensive press in the product.
  t.check("and makes the cost explicit rather than implying it",
    /6 model calls, 6 at a time/.test(w.html()), true);
  t.check("saying nothing about the day's budget, which is not what this press is about",
    /counts against the day/.test(w.html()), false);
  // No measurement yet, so no estimate — the same rule as case 1, at the place a reader sees it.
  t.check("and promises no time it cannot know", /~/.test(w.html()), false);

  w.read("acme#681", { head_sha: "h1", depth: "line" });
  w.read("acme#682", { head_sha: "h2", depth: "line" });
  t.check("with some read, it offers only the rest", /read the 4 not yet read/.test(w.html()), true);

  STEPS.forEach((p, i) => w.read(`acme#${681 + i}`, { head_sha: `h${i + 1}`, depth: "line" }));
  t.check("and with everything read it offers no press at all",
    /<button/.test(w.html()), false);
  t.check("saying why, rather than showing an empty space",
    /Every step has a reading of its current commit/.test(w.html()), true);
}

// ── 6. two stacks read at once, and neither is dropped (SKEIN-353) ─────────────────────────────
//
// Reported live: "the ladder stack was running read all and when I clicked the read all for other
// stack, I can't see the read all progress in ladder stack." The invisible progress is the smaller
// half. `revStackRun` was one global, so the second press overwrote it and `revStackPump` — which
// read the global — never advanced the first run's queue again. The reads already in flight
// finished and booked correctly (their `.finally` closes over their own run), so the reader got
// however many happened to be running and no word about the rest.
//
// The decision that caused it was written down as settled and was wrong: "one run at a time, and
// only the stack that is open; two of them racing is not a state worth supporting." The owner runs
// several stacks, and the code did not even refuse the second press.
{
  const w = world();
  const width = w.askedWidth();
  w.start("acme:deep");
  t.check("the first stack starts as many as the manual ceiling allows", w.pending(), width);
  t.check("and keeps the rest queued, which is what makes the next assertion mean anything",
    w.run("acme:deep").todo.length, 14 - width);

  w.start("acme:chassis");
  // **The press did not take the first run away.** Stated before anything is measured about it,
  // because every assertion below reaches through `w.run("acme:deep")` — and when the second press
  // overwrote the global, as it used to, those read as a TypeError rather than as this sentence.
  // A suite that crashes says less about what broke than one that reports.
  t.check("the first stack's run survives the second press", !!w.run("acme:deep"), true);
  // Independent, not sharing a pipe: a run the reader pressed does not wait on another run the
  // reader pressed. Both are things he asked for, and neither is skein deciding to spend his day.
  t.check("a second stack starts immediately rather than queueing behind the first",
    w.pending(), width + 5);
  t.check("and both runs exist, each with its own total",
    [w.run("acme:deep").total, w.run("acme:chassis").total], [14, 5]);
  // Each row draws its OWN run. This is the symptom that was reported, and on its own it is not
  // enough — see the queue assertion below.
  t.check("the first stack still says it is reading", /reading stack…/.test(w.html(DEEP)), true);
  t.check("and the second says so on its own row", /reading stack…/.test(w.html(OTHER)), true);
  t.check("neither is told the other's count",
    [/0 of 14 done/.test(w.html(DEEP)), /0 of 5 done/.test(w.html(OTHER))], [true, true]);

  // **The assertion the reported symptom does not reach.** Progress being visible and a queue still
  // draining are two different bugs, and a test that only looks at the row passes with the first
  // run's remaining steps silently abandoned. The first stack is the one with a queue, so this is
  // the half that only a per-stack run can satisfy.
  for (let i = 0; i < 40 && w.pending(); i++) await w.settleOne();
  // A run that ENDS must not take the others with it. The progress line stays on screen after the
  // last step lands (that is what the "read the stack N of N" assertions below are about), so a run
  // that tidied itself away — or tidied the board away — would blank a stack the reader is still
  // watching. Named here, before the counts, for the same reason as the survival check above.
  t.check("both runs are still on the board once they finish",
    [!!w.run("acme:deep"), !!w.run("acme:chassis")], [true, true]);
  t.check("every step of the FIRST stack is read, not only the ones that were in flight",
    w.run("acme:deep").done, 14);
  t.check("and every step of the second", w.run("acme:chassis").done, 5);
  t.check("with nothing left waiting in either queue",
    [w.run("acme:deep").todo.length, w.run("acme:chassis").todo.length], [0, 0]);
  t.check("and both rows say they are finished",
    [/read the stack 14 of 14 done/.test(w.html(DEEP)),
     /read the stack 5 of 5 done/.test(w.html(OTHER))], [true, true]);
}

// Stopping one stack stops that stack, and hands its width back to the other.
{
  const w = world();
  const width = w.askedWidth();
  w.start("acme:deep");
  w.start("acme:chassis");
  w.stop("acme:deep");
  t.check("stopping one run does not abandon its reads in flight", w.pending(), width + 5);
  t.check("the stopped row says so, and the other does not",
    [/stopping/.test(w.html(DEEP)), /stopping/.test(w.html(OTHER))], [true, false]);

  for (let i = 0; i < 40 && w.pending(); i++) await w.settleOne();
  t.check("the stopped stack ends short, as it was told to",
    w.run("acme:deep").done, width);
  t.check("and it says it was stopped, not that it finished",
    new RegExp(`stopped ${width} of 14 done`).test(w.html(DEEP)), true);
  t.check("while the run beside it finishes every one of its own",
    [w.run("acme:chassis").done, /read the stack 5 of 5 done/.test(w.html(OTHER))], [5, true]);
}

// A second press on a stack ALREADY reading is not a restart. It cannot be seen while a run is
// going — the row draws progress where the button was — so reaching here is a stale render, and
// replacing the run would be the same silent abandonment one stack in.
{
  const w = world();
  w.start("acme:deep");
  await w.settleOne();
  const before = w.run("acme:deep");
  w.start("acme:deep");
  t.check("the run in progress is the same run", w.run("acme:deep") === before, true);
  t.check("and its count was not reset", w.run("acme:deep").done, 1);
  // 14 steps, the ceiling started 10, one landed and the pump started one more: three still queued.
  // A press that rebuilt the run would put all fourteen back and lose the eleven already paid for.
  t.check("nor was its queue thrown away and rebuilt",
    w.run("acme:deep").todo.length, 14 - w.askedWidth() - 1);
}

// ── a reading is not a review, and half a reading is not a whole one ──────────────────────────
//
// SKEIN-371, found by driving the owner's 20-step stack against a build of `d48a4ce`: the row
// offered "read the 10 not yet read", the run ended "✓ read the stack 10 of 10 done", and ten steps
// were left carrying a summary with `has_critique: false` and
// `critique_because: "skein could not reach the fleet sandbox … so the model was never asked"`.
// Nothing anywhere said so. Each of those rows rendered exactly like a reviewed one.
//
// The distinction is `critique_because`: it is written only after a model call was spent and
// produced no usable review (`note_critique_tried`, src/review.rs), so it is a record of a FAILURE,
// never a decision not to review. A row nobody will ever draft a review for carries no reason at
// all and must not be offered a retry that would answer the same way.
{
  const w = world();
  // Eleven steps read AND reviewed; three read with the review missing.
  const read = n => ({ number: 740 + n, head_sha: "d" + n, depth: "line", line: "read", has_critique: true });
  const half = n => ({ number: 740 + n, head_sha: "d" + n, depth: "line", line: "read",
                       has_critique: false, critique_because: "skein could not reach the fleet sandbox" });
  DEEP_STEPS.forEach((p, i) => w.read("acme#" + (740 + i), i < 11 ? read(i) : half(i)));

  t.check("a step with a reading and a review is read", w.needs(DEEP_STEPS[0]), false);
  t.check("a step whose review did not come back is not", w.needs(DEEP_STEPS[11]), true);
  // The counter-case, and it is what stops this from meaning "any row without a review": a row
  // nobody ever bought a review for carries no reason, and re-reading it would buy the same answer.
  w.read("acme#751", { number: 751, head_sha: "d11", depth: "line", line: "read", has_critique: false });
  t.check("but a step nothing ever tried to review is left alone", w.needs(DEEP_STEPS[11]), false);
  w.read("acme#751", half(11));

  const html = w.html(DEEP);
  t.check("the read-all count includes them", /read the 3 not yet read/.test(html), true);
  t.check("and the stack says what is actually missing, not just a number",
    /3 read but with no review/.test(html), true);

  // Pressed, run to the end — and in this world the readings do not change, which is the real case:
  // a fleet that could not reach its model answers the same way twice. The line that a reader is
  // left looking at must not say the stack is done.
  w.start("acme:deep");
  for (let i = 0; i < 40 && w.pending(); i++) await w.settleOne();
  const after = w.html(DEEP);
  t.check("the completion line names what did not come back",
    /3 steps came back with no review/.test(after), true);
  t.check("and points at where the retry is", /offers its own read/.test(after), true);
}

// ── the collapsed row says its own run is going ────────────────────────────────────────────────
//
// SKEIN-370, and it is the other half of the owner's sentence: "the ladder stack was running read
// all and when I clicked the read all for other stack, I can't see the read all progress in ladder
// stack." SKEIN-353 stopped the first run being abandoned. It did not put the first run on screen,
// because the progress line is drawn only inside the stack that is OPEN and opening is exclusive.
//
// Asserted on the ROW, not on the marker: a stack whose run is going has to say so where somebody
// scanning the queue would see it, and a marker no row asks for says nothing to anybody.
{
  const w = world();
  t.check("a stack with no run reads exactly as it always did",
    /you are at step 1 of 14 · review from the bottom/.test(w.row(DEEP)), true);
  t.check("and carries no progress marker at all",
    /⟳|✓/.test(w.row(DEEP)), false);

  w.start("acme:deep");
  await w.settleOne();
  await w.settleOne();
  // Two of fourteen landed, and the reader is looking at some other stack — which is the whole
  // case. The number is `run.done`, so a marker that drew a constant would disagree with it here.
  t.check("a stack reading in the background says so on its collapsed row",
    /⟳ reading 2 of 14/.test(w.row(DEEP)), true);
  t.check("and still says where the reader is in it",
    /you are at step 1 of 14/.test(w.row(DEEP)), true);
  // The column is rationed (§4). The advice about where to start is standing, the run is news, and
  // only one of the two can lead a cell that truncates from the right.
  t.check("the standing advice steps aside for it rather than both being squeezed",
    /review from the bottom/.test(w.row(DEEP)), false);

  // The stack BESIDE it, with no run, is untouched — which is what makes two stacks reading at once
  // legible at the same time rather than one at a time.
  t.check("a stack without a run is unchanged while another one reads",
    [/⟳/.test(w.row(OTHER)), /review from the bottom/.test(w.row(OTHER))], [false, true]);

  for (let i = 0; i < 40 && w.pending(); i++) await w.settleOne();
  // Stays on screen when it ends, exactly as the expanded line does: "they happened and how many
  // landed" is the last thing a reader wants about fourteen model calls, and a marker that vanished
  // at 14 of 14 would answer it by removing it.
  t.check("and when it finishes the row still says what it read",
    /✓ read 14 of 14/.test(w.row(DEEP)), true);
}

// A run the reader stopped says stopped on the collapsed row too, rather than claiming it finished.
{
  const w = world();
  const width = w.askedWidth();
  w.start("acme:deep");
  w.stop("acme:deep");
  for (let i = 0; i < 40 && w.pending(); i++) await w.settleOne();
  t.check("a stopped run is not a finished one on the collapsed row",
    new RegExp(`✓ stopped at ${width} of 14`).test(w.row(DEEP)), true);
}

t.done();
