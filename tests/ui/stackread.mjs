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
//
//   node tests/ui/stackread.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// A stack of six, which is enough to see a concurrency width of three take two rounds.
const STEPS = [1, 2, 3, 4, 5, 6].map(n => ({
  repo_id: "acme", number: 680 + n, head_sha: "h" + n, title: `tenants 0${n}`,
}));
const STACK = { repo_id: "acme", name: "ladder", steps: STEPS };

function world() {
  const body = `
    const REV_SUM_PARALLEL = 3;
    let revReadMs = [];
    let revStackRun = null;
    let revSums = new Map();
    let revInFlight = new Map();
    let revStacks = new Map([["acme:ladder", STACK]]);
    const rk = pr => pr.repo_id + "#" + pr.number;
    const revStackKey = () => "acme:ladder";
    const revStepNo = (st, p) => st.steps.indexOf(p) + 1;
    ${grab("revNoteReadMs")}
    ${grab("revReadTypicalMs")}
    ${grab("revStackEstimate")}
    ${grab("revStepNeedsReading")}
    ${grab("revStackReadAll")}
    ${grab("revStackStop")}
    ${grab("revStackPump")}
    ${grab("revStackRunHtml")}
    ${grab("revElapsed")}
    return {
      note: ms => revNoteReadMs(ms),
      estimate: n => revStackEstimate(n),
      typical: () => revReadTypicalMs(),
      needs: p => revStepNeedsReading(p),
      read: (key, s) => revSums.set(key, s),
      flight: key => revInFlight.set(key, { started_ms: 1 }),
      start: () => revStackReadAll("acme:ladder"),
      stop: () => revStackStop(),
      run: () => revStackRun,
      html: () => revStackRunHtml(STACK),
    };
  `;
  // Every read resolves when the case says so, so concurrency is observable. Resolving
  // synchronously would make "three at a time" unmeasurable and the assertion vacuous.
  const settle = [];
  const revFetchSummary = () => new Promise(resolve => settle.push(resolve));
  const w = new Function(
    "esc", "console", "toast", "renderReviewNow", "revFetchSummary", "STACK", "Date", body,
  )(String, console, () => {}, () => {}, revFetchSummary, STACK, Date);
  // Let one queued read finish, and give the pump its microtask to start the next.
  w.settleOne = async () => { (settle.shift() || (() => {}))(); await new Promise(r => setTimeout(r, 0)); };
  w.pending = () => settle.length;
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

// ── 3. several at once, and the progress is true ───────────────────────────────────────────────
{
  const w = world();
  w.start();
  t.check("six steps to read", w.run().total, 6);
  t.check("three at a time, not six", w.pending(), 3);
  t.check("and the line says which three are running", /3 running · steps 1, 2, 3/.test(w.html()), true);
  t.check("with none done yet", /0 of 6 done/.test(w.html()), true);

  await w.settleOne();
  t.check("one lands and the next starts", w.run().done, 1);
  t.check("still three in flight", w.pending(), 3);
  t.check("and the count moved", /1 of 6 done/.test(w.html()), true);

  for (let i = 0; i < 5; i++) await w.settleOne();
  t.check("the run finishes", w.run().done, 6);
  t.check("and says so rather than vanishing", /read the stack 6 of 6 done/.test(w.html()), true);
  t.check("with no stop control left to press", /stop</.test(w.html()), false);
}

// ── 4. stopping stops STARTING, and does not undo ──────────────────────────────────────────────
{
  const w = world();
  w.start();
  await w.settleOne();               // one done, three running
  const doneBefore = w.run().done;

  w.stop();
  t.check("stopping does not abandon the reads in flight", w.pending(), 3);
  t.check("and does not take back what was already read", w.run().done, doneBefore);
  t.check("the line says it is stopping, and how many are still finishing",
    /stopping — 3 still finishing/.test(w.html()), true);

  // The rest of the queue is never started.
  for (let i = 0; i < 3; i++) await w.settleOne();
  t.check("nothing new was started after the stop", w.pending(), 0);
  t.check("so the run ends short of its total, and says which it is",
    /stopped 4 of 6 done/.test(w.html()), true);
  // And says what stopping did NOT do. A run that ends two steps short with no word about them is
  // the same shape as a run that failed, and the reader's next question is whether the four that
  // landed are still good.
  t.check("and that stopping undid nothing",
    /2 left unread — nothing was undone/.test(w.html()), true);
}

// ── 5. the control before a run, and when there is nothing to do ───────────────────────────────
{
  const w = world();
  t.check("with nothing read, the control offers the whole stack",
    /read all 6/.test(w.html()), true);
  t.check("and makes the cost explicit rather than implying it",
    /6 model calls/.test(w.html()), true);
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

t.done();
