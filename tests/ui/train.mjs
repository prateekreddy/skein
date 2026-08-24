// The merge-train banner: a ROW above the app that names, per repo, the PRs the train stopped on
// and why — drawn from the same `/api/review/counts` answer as the badge, on the same render call.
//
// Three shapes of that answer matter:
//   * a repo with stops → the banner exists, names that repo and its PR numbers, and does NOT
//     name a repo whose train ran clean;
//   * no stops anywhere → no banner element at all (a row of nothing still pushes the app down);
//   * entries with no `stopped` field at all — an older server — → no crash and no banner.
//
// The real `renderTrainBanner` and `renderRevBadge` run here, lifted out of index.html, against a
// document stub just deep enough to hold a prepended row.
//
//   node tests/ui/train.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed down to what these two touch. `revbtn` is absent on purpose:
// the badge's early return is exactly the path that must still draw the banner first.
function board(counts) {
  const kids = [];
  const document = {
    body: { prepend: e => kids.unshift(e) },
    getElementById: id => kids.find(e => e.id === id) || null,
    createElement: tag => ({
      tag, id: "", innerHTML: "",
      remove() { const at = kids.indexOf(this); if (at >= 0) kids.splice(at, 1); },
    }),
  };
  const src = `
    let revCounts = ${JSON.stringify(counts)};
    ${grab("esc")}
    ${grab("renderTrainBanner")}
    ${grab("renderRevBadge")}
    return {
      poll: cs => { if (cs !== undefined) revCounts = cs; renderRevBadge(); },
    };
  `;
  const made = new Function("document", src)(document);
  return {
    ...made,
    banner: () => document.getElementById("trainban"),
    text: () => document.getElementById("trainban")?.innerHTML ?? null,
  };
}

// ---- a stop is named; a clean repo is not ----
{
  const b = board([
    { repo_id: "gadget-demo", needs_you: 1, error: "", skipped: "",
      stopped: [{ number: 123, why: "CI failed" }, { number: 145, why: "conflict" }] },
    { repo_id: "clean-repo", needs_you: 2, error: "", skipped: "", stopped: [] },
  ]);
  b.poll();
  t.check("a stopped train raises the banner row", !!b.banner(), true);
  t.check("the banner names the stopped repo", b.text().includes("gadget-demo"), true);
  t.check("and its PR numbers", b.text().includes("#123") && b.text().includes("#145"), true);
  t.check("with the reason beside each", b.text().includes("(CI failed)") && b.text().includes("(conflict)"), true);
  t.check("the clean repo is not named", b.text().includes("clean-repo"), false);
  t.check("the repo's segment is one click into its queue",
    b.text().includes(`openReview('gadget-demo')`), true);
}

// ---- no stops anywhere: no banner, and a banner already up comes down ----
{
  const b = board([
    { repo_id: "alpha", needs_you: 3, error: "", skipped: "", stopped: [] },
    { repo_id: "beta", needs_you: 0, error: "", skipped: "", stopped: [] },
  ]);
  b.poll();
  t.check("no stops raise no banner", b.banner(), null);

  b.poll([{ repo_id: "alpha", needs_you: 3, error: "", skipped: "",
            stopped: [{ number: 7, why: "CI failed" }] }]);
  t.check("a stop arriving on a later poll raises it", !!b.banner(), true);
  b.poll([{ repo_id: "alpha", needs_you: 3, error: "", skipped: "", stopped: [] }]);
  t.check("and the train moving again takes it down", b.banner(), null);
}

// ---- an older server: entries with no `stopped` field at all ----
{
  const b = board([
    { repo_id: "alpha", needs_you: 1, error: "", skipped: "" },
    { repo_id: "beta", needs_you: 0, error: "gh broke", skipped: "" },
  ]);
  b.poll();
  t.check("counts without the field render without crashing, and without a banner", b.banner(), null);
}

t.done();
