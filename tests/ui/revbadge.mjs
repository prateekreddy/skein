// The number on the review button, drawn from the poll that runs when nobody has opened the pane.
//
// **The state a person stares at most is the one nothing was checking** (SKEIN-323). The badge is
// folded from two places: `loadReview`, once the pane has fetched, and `pollReviewCounts`, every
// three minutes before that. Only the first of them counted the your-move list; the second showed
// `Lane::NeedsYou` off the wire, which is the REVIEWER's lane — so a pull request the owner had
// opened with changes requested on it was missing from the number until they opened the pane, and
// then the number changed under them.
//
// So the payload here is deliberately a payload whose `needs_you` DISAGREES with its own rows. That
// is what the server sends: `prq::counts` still answers with the lane, and the rows it counted ride
// along so that `cockpit/src/move.mjs` — the one rule — can be folded over them here.
//
// The real `pollReviewCounts` and `renderRevBadge` run, lifted out of index.html, against a document
// stub deep enough to hold the button and its badge.
//
//   node tests/ui/revbadge.mjs
import { grab, harness, pure } from "./lift.mjs";

const t = harness();

// A pull request as `/api/review/counts` now serialises one, with the fields the rule reads.
const pr = over => ({
  number: 7, lane: "waiting", reasons: ["author"], base_ref: "main",
  my_review: "none", review_is_current: false, my_review_requested: false,
  review_decision: "", mergeable: null, merge_state: "", checks: "none",
  review_threads: [], review_threads_total: null, review_requests: [], ...over,
});
const asked = over => pr({ lane: "needs-you", reasons: ["reviewer"], ...over });

// The page's world, stubbed down to what the badge and the banner touch, with one answer waiting
// for the poll's `fetch`.
function board() {
  let answer = [];
  const kids = [];
  const element = (tag) => ({
    tag, id: "", className: "", innerHTML: "", textContent: "", title: "",
    attrs: {}, children: [],
    classList: { on: new Set(), toggle(name, want) { want ? this.on.add(name) : this.on.delete(name); } },
    querySelector(sel) {
      const want = sel.replace(/^\./, "");
      return this.children.find(c => c.className.split(" ").includes(want)) || null;
    },
    append(e) { this.children.push(e); },
    removeAttribute(name) { delete this.attrs[name]; },
    remove() {
      for (const parent of [{ children: kids }, ...kids]) {
        const at = (parent.children || []).indexOf(this);
        if (at >= 0) parent.children.splice(at, 1);
      }
    },
  });
  const btn = element("button");
  btn.id = "revbtn";
  kids.push(btn);
  const document = {
    body: { prepend: e => kids.unshift(e) },
    getElementById: id => kids.find(e => e.id === id) || null,
    createElement: element,
  };
  // The route's answer, as the browser's `fetch` hands it over.
  const fetch = () => Promise.resolve({ json: () => Promise.resolve(answer) });
  const src = `
    let revCounts = [];
    let revFlows = new Map();
    let revFlowsOn = null;
    ${pure("move")}
    ${grab("esc")}
    ${grab("revTrainPaused")}
    ${grab("renderTrainBanner")}
    ${grab("renderRevBadge")}
    ${grab("pollReviewCounts")}
    return { pollReviewCounts, counts: () => revCounts };
  `;
  const made = new Function("document", "fetch", src)(document, fetch);
  return {
    // One poll, awaited: `pollReviewCounts` does not hand its promise back, so the flush is the
    // test's job rather than something it can chain onto.
    async poll(payload) {
      answer = payload;
      made.pollReviewCounts();
      await new Promise(done => setTimeout(done, 0));
    },
    badge: () => btn.querySelector(".revbadge"),
    text: () => btn.querySelector(".revbadge")?.textContent ?? null,
    title: () => btn.title,
    counts: () => made.counts(),
  };
}

// ---- the bug: a payload whose lane count is not the your-move list ----
{
  const b = board();
  await b.poll([{
    repo_id: "acme", error: "", skipped: "", stopped: [],
    // What `prq::counts` says: one row in `Lane::NeedsYou`.
    needs_you: 1,
    prs: [
      // In the lane and in the list.
      asked({ number: 1 }),
      // NOT in the lane — you opened it, so the lane calls it waiting — and squarely in the list.
      // This is the row the badge could not see until the pane was opened.
      pr({ number: 2, review_decision: "CHANGES_REQUESTED" }),
      // Yours and red and nothing outstanding: their move (SKEIN-303).
      pr({ number: 3, checks: "failing", mergeable: true, merge_state: "CLEAN" }),
    ],
  }]);
  t.check("the badge counts the your-move list, not the lane the server sent", b.text(), "2");
  t.check("and the tooltip's per-repo breakdown says the same number",
    b.title().includes("acme: 2"), true);
  t.check("the sentence at the top of the tooltip is that number too",
    b.title().startsWith("2 pull requests need you"), true);
}

// ---- the other direction: rows in the lane that are nobody's move but the author's ----
//
// Not symmetry for its own sake. Since SKEIN-354 a standing approval keeps its row out of the list
// while `prq` still files it under `Lane::NeedsYou` — so a badge reading the lane claims you for
// work you have already done, and this is the case where the honest badge is NO badge.
{
  const b = board();
  await b.poll([{
    repo_id: "acme", error: "", skipped: "", stopped: [], needs_you: 2,
    prs: [asked({ number: 1, my_review: "approved" }), asked({ number: 2, my_review: "changes-requested" })],
  }]);
  t.check("a lane full of verdicts you have already given raises no badge at all", b.badge(), null);
  t.check("and the tooltip stops claiming anyone needs you",
    b.title().includes("need you"), false);
}

// ---- an older server: a payload with no rows on it at all ----
//
// The same reading `stopped`'s absence gets in `renderTrainBanner`: a tab that outlived the server
// it loaded from still draws a number, because a badge that is sometimes short is worth more than
// one that suddenly reads zero.
{
  const b = board();
  await b.poll([{ repo_id: "acme", needs_you: 4, error: "", skipped: "" }]);
  t.check("a count with no rows falls back to the lane count rather than to nothing", b.text(), "4");
}

// ---- a repo whose count could not be taken ----
//
// `prs` is empty there because nothing was counted, and that must keep reading as "broken", never as
// "clean": a badge quietly showing zero because `gh` is broken is the one thing it must never be.
{
  const b = board();
  await b.poll([{ repo_id: "acme", needs_you: 0, error: "gh broke", skipped: "", stopped: [], prs: [] }]);
  t.check("a failed count still shows as a fault and not as an empty queue", b.text(), "!");
  t.check("and names the repo and the reason", b.title().includes("acme: gh broke"), true);
}

// ---- more than one repo, and only one of them needing you ----
{
  const b = board();
  await b.poll([
    { repo_id: "alpha", needs_you: 0, error: "", skipped: "", stopped: [],
      prs: [pr({ number: 1, review_decision: "CHANGES_REQUESTED" })] },
    { repo_id: "beta", needs_you: 3, error: "", skipped: "", stopped: [],
      prs: [asked({ number: 2, my_review: "approved" })] },
    { repo_id: "gamma", needs_you: 0, error: "", skipped: "review queue is switched off for this repo" },
  ]);
  t.check("the badge is the sum of the your-move lists across repos", b.text(), "1");
  t.check("the repo with nothing yours is left out of the breakdown",
    b.title().includes("beta:"), false);
  t.check("a repo that was never asked is still named, and not as a fault",
    b.title().includes("gamma: review queue is switched off"), true);
  // The rewrite is on `revCounts` itself, so everything else drawn from the poll — the tooltip, the
  // train banner's repo segments — sees one number rather than two.
  t.check("the poll's own copy carries the rewritten number",
    b.counts().map(c => [c.repo_id, c.needs_you]), [["alpha", 1], ["beta", 0], ["gamma", 0]]);
}

t.done();
