// A stack step says what it is (SKEIN-352).
//
// Reported live, against the stack read shipped in SKEIN-337: "when I click on reread the entire
// stack button it shows progress which is great. But when it says 4 ready of 16, there is no
// marker on any of the PRs which one is done."
//
// `revStackSteps` drew `step N | node | #num | title | <misnamed>` and nothing else. So on the
// eighteen pull requests that are most of the owner's queue, the pane's central invariant did not
// apply at all — "the gist cell is NEVER empty… 'skein read this and it is routine' and 'skein
// never looked' must not render alike" — and a count of how many steps were done could be checked
// against nothing on screen.
//
// A step now carries the loose row's own cells: the move mark, the gist and the rail. Two things
// here are what the suite is really for, and both are easy to make vacuous:
//
//   1. **The gist is the ROW's gist**, all four of its states, including the in-flight one with its
//      counter. A fixture whose reading is a short word would let an assertion pass on markup that
//      merely mentioned the number.
//   2. **`.mv.blocked` is decided from the BASE, by branch name.** A stack is a tree (SKEIN-288),
//      so "the step above me in the list" is the other branch's tip on a fork — the fork case below
//      is the one that separates the two rules, and without it a positional rule passes everything.
//
//   node tests/ui/stacksteps.mjs
import { grab, harness, pure } from "./lift.mjs";

const t = harness();

// Markers no collapsed or summarised form can produce: each reading's line ends in a token that
// only appears if the whole line was rendered into the step's own cell.
const READ_LINE = "Sets out the conflict inventory the rest works through — GIST-DREW-THE-WHOLE-LINE";

// The owner's own stack, four steps of it, at the shape it was reported in: the root reviewed, the
// next one waiting, and two above it that cannot be reviewed until it is.
const LADDER = [
  { repo_id: "acme", number: 686, head_ref: "ladder/tenants-00", base_ref: "develop",
    title: "tenants 00: the plan and the conflict inventory", author: "dev-rhea",
    lane: "needs-you", my_review: "approved", review_is_current: true,
    updated_at: "2026-08-25T14:00:00Z", changed_files: 4, additions: 100, deletions: 20,
    head_sha: "h686", reasons: ["reviewer"] },
  { repo_id: "acme", number: 684, head_ref: "ladder/tenants-01", base_ref: "ladder/tenants-00",
    title: "tenants 01: compose brings the module up", author: "dev-rhea",
    lane: "needs-you", my_review: "none", updated_at: "2026-08-25T14:00:00Z",
    changed_files: 6, additions: 300, deletions: 40, head_sha: "h684", reasons: ["reviewer"] },
  { repo_id: "acme", number: 685, head_ref: "ladder/tenants-02", base_ref: "ladder/tenants-01",
    title: "tenants 02: the two tables and their migrations", author: "dev-rhea",
    lane: "needs-you", my_review: "none", updated_at: "2026-08-25T14:00:00Z",
    changed_files: 3, additions: 90, deletions: 3, head_sha: "h685", reasons: ["reviewer"] },
  { repo_id: "acme", number: 687, head_ref: "ladder/tenants-03", base_ref: "ladder/tenants-02",
    title: "tenants 03: onboarding writes a tenant row", author: "dev-rhea",
    lane: "needs-you", my_review: "none", updated_at: "2026-08-25T14:00:00Z",
    changed_files: 8, additions: 400, deletions: 11, head_sha: "h687", reasons: ["reviewer"] },
];

// A FORK, and the only fixture that can tell the two rules apart. #702 and #701 are both based on
// #700, which is reviewed — so #702 needs you and is NOT blocked, while the step drawn immediately
// above it in the depth-first list (#701) is undecided.
const FORKED = [
  { repo_id: "acme", number: 700, head_ref: "fix/readiness", base_ref: "develop",
    title: "readiness: the abstention kinds", author: "dev-rhea",
    lane: "needs-you", my_review: "approved", review_is_current: true,
    updated_at: "2026-08-25T14:00:00Z", head_sha: "h700", reasons: ["reviewer"] },
  { repo_id: "acme", number: 701, head_ref: "fix/readiness-a", base_ref: "fix/readiness",
    title: "readiness: the first branch off it", author: "dev-rhea",
    lane: "needs-you", my_review: "none", updated_at: "2026-08-25T14:00:00Z",
    head_sha: "h701", reasons: ["reviewer"] },
  { repo_id: "acme", number: 702, head_ref: "fix/readiness-b", base_ref: "fix/readiness",
    title: "readiness: the second branch off it", author: "dev-rhea",
    lane: "needs-you", my_review: "none", updated_at: "2026-08-25T14:00:00Z",
    head_sha: "h702", reasons: ["reviewer"] },
];

function world() {
  const body = `
    // The page's own escaper, not an approximation of it: an assertion about rendered text is an
    // assertion about what escaping did to it.
    ${grab("esc")}
    let revSums = new Map();
    let revInFlight = new Map();
    let revSel = null, revStackStep = null;
    let revQueue = { queues: [{ repo_id: "acme", trunk: "develop" }], prs: [] };
    const rk = pr => pr.repo_id + "#" + pr.number;
    // Whose move a pull request is, out of cockpit/src/move.mjs — the one place that rule lives.
    // A step's mark reads it, so a stub here would be proving a second copy of the rule.
    ${pure("move")}
    ${grab("revMoved")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revElapsed")}
    ${grab("revGist")}
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${grab("revAge")}
    ${grab("revSize")}
    ${grab("revRail")}
    ${grab("revTrunkOf")}
    ${grab("revStackName")}
    ${grab("revMisnamed")}
    ${grab("revChains")}
    ${grab("REV_UNROOTED_WHY")}
    ${grab("revStepNo")}
    ${grab("revStackNext")}
    ${grab("revStepMove")}
    // The run control above the steps and the step's own expanded body are other suites' subjects
    // (stackread.mjs, conversation.mjs); this one is about the step LINE.
    const revStackRunHtml = () => "";
    const revBody = () => "";
    ${grab("revStackSteps")}
    return {
      // Built by the REAL detector, so the depth, the branch marks and the order are the ones the
      // pane would draw rather than a hand-written shape that agrees with the test.
      stackOf: prs => revChains(prs)[0],
      steps: prs => revStackSteps(revChains(prs)[0]),
      read: (key, s) => revSums.set(key, s),
      flight: (key, at) => revInFlight.set(key, { started_ms: at, asked: true }),
      mv: (prs, number) => {
        const st = revChains(prs)[0];
        return revStepMove(st, st.steps.find(p => p.number === number));
      },
    };
  `;
  return new Function("console", "Date", body)(console, Date);
}

// One step's markup, by number — every assertion below is about ONE step's cells, and a whole-block
// substring test would pass on a neighbour's.
const stepOf = (html, number) => {
  const parts = html.split('<div class="step');
  return (parts.find(p => p.includes(`#${number}<`)) || "");
};

// ── 1. the gist cell exists on a step, with all four of its states ─────────────────────────────
{
  const w = world();
  w.read("acme#686", { number: 686, head_sha: "h686", depth: "expanded", line: READ_LINE, flags: [] });
  w.read("acme#687", { number: 687, head_sha: "h687", depth: "unread",
                       unread_because: "the day's automatic budget is spent" });
  w.flight("acme#685", Date.now() - 12000);
  const html = w.steps(LADDER);

  t.check("a step skein has read says what it found, in full",
    stepOf(html, 686).includes(READ_LINE), true);
  t.check("a step nothing has read says so, rather than saying nothing",
    /class="gist unknown"[^>]*>not read/.test(stepOf(html, 684)), true);
  t.check("a step skein could not read says why",
    stepOf(html, 687).includes("not read — the day's automatic budget is spent"), true);
  t.check("and a step being read right now carries its running counter",
    /revflight-secs" data-started="\d{10,}/.test(stepOf(html, 685)), true);

  // The count the reader was given is now checkable against the list: four steps, one read, one
  // running, two not. That sentence is the whole of SKEIN-352 and this is it as an assertion.
  t.check("so a run's count can be checked against the steps",
    [(html.match(/class="gist unknown"/g) || []).length,
     (html.match(/class="gist reading"/g) || []).length], [2, 1]);
}

// ── 2. the rail, which is what makes a step auditable at all ───────────────────────────────────
{
  const w = world();
  const step = stepOf(w.steps(LADDER), 687);
  t.check("a step carries the row's rail", step.includes('class="rail"'), true);
  t.check("with the age the queue sorted on", /class="revage[^"]*"[^>]*>\d/.test(step), true);
  t.check("the size, so 'can I do this now' has an answer", step.includes("8 files"), true);
  t.check("and who wrote it", step.includes("dev-rhea"), true);
}

// ── 3. whose move, and the one state only a stack can be in ────────────────────────────────────
//
// `.mv.blocked` is written down in docs/review-ux.md §4 for "a stack step whose base is unreviewed"
// and `REV_MOVE_WORDS.blocked` has been in the page for it — with no caller. The one thing that
// makes a stack dangerous, reviewing step 7 before step 3, had no mark at all.
{
  const w = world();
  t.check("a step you have decided is marked decided", w.mv(LADDER, 686), "done");
  t.check("the step you can actually review is the lit one", w.mv(LADDER, 684), "yours");
  t.check("a step whose base is unreviewed waits on it, and says so", w.mv(LADDER, 685), "blocked");
  t.check("and so does the one above that", w.mv(LADDER, 687), "blocked");

  // Exactly one lit mark down the column, which is the design's own rule for this cell.
  const html = w.steps(LADDER);
  t.check("exactly one step in the stack is your move",
    (html.match(/class="mv yours"/g) || []).length, 1);
  t.check("and the mark carries the words for what it means",
    stepOf(html, 685).includes("waits on an unreviewed base"), true);
}

// The fork, which is the case a positional rule gets wrong. #702 sits directly below #701 in the
// depth-first list and #701 is undecided — but #702's base is #700, which is reviewed, so #702 is
// reviewable now and must be lit rather than blocked.
{
  const w = world();
  const st = w.stackOf(FORKED);
  t.check("the fixture really is a fork drawn depth-first",
    st.steps.map(p => p.number), [700, 701, 702]);
  t.check("the branch whose own base is reviewed is not blocked by the step above it",
    w.mv(FORKED, 702), "yours");
  t.check("while the reviewed root stays decided", w.mv(FORKED, 700), "done");
}

t.done();
