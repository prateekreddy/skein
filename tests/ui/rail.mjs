// The review rail's age and size cells, and the fleet clock that used to eat one of them.
//
// Reported live, twice:
//
//   * the age column showed the true age for one second and then "?" forever — `tickAges` rewrites
//     EVERY `.age` in the document from `data-secs` once a second, and the review rail reused the
//     fleet's `age` class with no `data-secs` to rewrite from. The rail's cell is `revage` now, and
//     this suite runs the REAL `tickAges` over a rendered row to hold that line.
//   * "What is the 7f, 4f and so on column, that doesn't help me since I don't know what it is" —
//     the size cell said `7f 782±`. It says `7 files ±782` now, with the full words in a tooltip.
//
// The real `revRail`/`revSize`/`revAge`/`tickAges` run here, lifted out of index.html; `ageNow` is
// the real one from vendor/cockpit.js, because tickAges' "?" for an unknown age is the exact
// behavior the rail must survive.
//
//   node tests/ui/rail.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { grab, harness, pure } from "./lift.mjs";

const t = harness();
const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

// The fleet clock's own pieces, from the file the page loads them from — not a copy.
const cockpit = readFileSync(join(root, "src", "web", "vendor", "cockpit.js"), "utf8");
const { ageNow } = new Function("window", "document", `${cockpit}\n;return { ageNow };`)({}, {});

// A document of plain span records, deep enough for querySelectorAll(".age") and dataset/text.
function world() {
  const els = [];
  const document = {
    els,
    querySelectorAll: sel => {
      const cls = sel.slice(1);
      return els.filter(e => (e.className || "").split(/\s+/).includes(cls));
    },
  };
  const src = `
    ${grab("esc")}
    // Whose move it is decides which clock the age cell is on, and that rule lives in
    // cockpit/src/move.mjs so the list, the badge and this column cannot pick different answers.
    ${pure("move")}
    ${grab("revAge")}
    ${grab("revSize")}
    // The age cell renders the LANE's own sort key (SKEIN-251) — a column that computed its own
    // number audited nothing — so the rail needs the order, and the word that says what it means.
    ${grab("revMoved")}
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${grab("revRail")}
    ${grab("tickAges")}
    return { rail: pr => revRail(pr), tick: () => tickAges(), size: pr => revSize(pr) };
  `;
  const made = new Function("document", "ageNow", src)(document, ageNow);
  return {
    ...made,
    els,
    // Rendered markup → span records tickAges can walk, exactly as querySelectorAll would find them.
    mount(html) {
      for (const m of html.matchAll(/<span class="([^"]*)"([^>]*)>([^<]*)<\/span>/g)) {
        els.push({ className: m[1], attrs: m[2], textContent: m[3], dataset: {} });
      }
      return els;
    },
    cell: cls => els.find(e => (e.className || "").split(/\s+/).includes(cls)),
  };
}

const DAY = 86400000;
// `lane` because the age cell renders the lane's own sort key now (SKEIN-251): their move is
// recency, and that is what every row here is about.
const pr = over => ({
  number: 7, author: "prateek", lane: "waiting",
  updated_at: new Date(Date.now() - 2 * DAY).toISOString(),
  changed_files: 7, additions: 340, deletions: 38, ...over,
});

// ---- the fleet clock must not eat the review age ----
{
  const w = world();
  w.mount(w.rail(pr()));
  // A fleet row shares the document, exactly as it does on the page — with the data the clock reads.
  w.els.push({ className: "age", dataset: { secs: "70", seen: String(Date.now()) }, textContent: "not yet ticked" });
  // The rail's FIRST cell is the age, whatever its class is called — found by position on purpose,
  // so renaming the class back to the fleet's `age` fails here as the symptom ("?") and not as a
  // lookup error.
  const ageCell = w.els[0];
  t.check("the rail renders the true age", ageCell.textContent, "2d");
  w.tick();
  t.check("one tick later the review age still stands", ageCell.textContent, "2d");
  t.check("and it never becomes the clock's unknown glyph", ageCell.textContent === "?", false);
  t.check("while the fleet's own age cell is still the clock's to rewrite", w.cell("age").textContent, "1m ago");
  // The clock's "?" for a fleet row with no data is intentional and must survive the fix.
  w.els.push({ className: "age", dataset: {}, textContent: "3s ago" });
  w.tick();
  t.check("a fleet age with no data still reads as unknown", w.els.at(-1).textContent, "?");
}

// ---- the age cell is the key its lane is sorted by ----
//
// SKEIN-251. The cell rendered `updated_at` while the your-move lane sorted on `revWaitedSince`, so
// a pull request you approved, force-pushed eight days ago and commented on thirty seconds ago sat
// near the top of oldest-first reading `1m`, above rows reading `4d`. The one column the design put
// on the row so the order could be AUDITED was the one that made it unauditable, and the amber
// three-day mark was being applied to the wrong number too.
{
  const w = world();
  // `my_review_requested` is what makes this row yours again (SKEIN-354). It used to be enough
  // that the head had moved past your review; `decided` now asks GitHub's two answers instead — is
  // your verdict standing, and has GitHub asked you again — so "a decision that came back to you"
  // means a re-request, and a push on its own does not take your approval away. The head still
  // moved, which is what the clock below is about.
  const moved = { lane: "needs-you", my_review: "approved", my_review_requested: true,
                  review_is_current: false,
                  committed_at: new Date(Date.now() - 8 * DAY).toISOString(),
                  updated_at: new Date(Date.now() - 30 * 1000).toISOString() };
  w.mount(w.rail(pr(moved)));
  t.check("your move counts from the commit that came after your review", w.els[0].textContent, "8d");
  t.check("and it is amber, which against the latest touch it could never have been",
    w.els[0].className.split(/\s+/).includes("old"), true);
  t.check("the cell says which of the two clocks it is on",
    w.els[0].attrs.includes("waiting on you since the commit that came after your review"), true);

  // Their move measures something else, and the cell must render THAT — its lane sorts on
  // updated_at, so rendering waited-since there would be the same fault mirrored.
  const theirs = world();
  theirs.mount(theirs.rail(pr({ ...moved, lane: "waiting" })));
  // `1m` is the floor: the cell is 34px and the question is "can I do this now", so revAge rounds
  // anything under a minute up to one rather than growing a seconds unit.
  t.check("their move is recency, which is what their move is sorted by", theirs.els[0].textContent, "1m");
  t.check("and says so", theirs.els[0].attrs.includes("last moved this long ago"), true);
}

// ---- the size cell speaks the reader's language ----
{
  const w = world();
  w.mount(w.rail(pr()));
  t.check("size reads as words and a signed total", w.cell("size").textContent, "7 files ±378");
  t.check("with the full story in the tooltip", w.cell("size").attrs.includes('title="+340 −38 across 7 files"'), true);
  t.check("one file is singular", w.size(pr({ changed_files: 1, additions: 3, deletions: 0 })).label, "1 file ±3");
}

// ---- unknown size: an empty cell that still holds its column ----
{
  const w = world();
  w.mount(w.rail(pr({ changed_files: null })));
  t.check("the cell is present", !!w.cell("size"), true);
  t.check("and empty", w.cell("size").textContent, "");
  t.check("with no tooltip promising numbers it does not have", w.cell("size").attrs.includes("title"), false);
}

t.done();
