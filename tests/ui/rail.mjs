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
import { grab, harness } from "./lift.mjs";

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
    ${grab("revAge")}
    ${grab("revSize")}
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
const pr = over => ({
  number: 7, author: "prateek", updated_at: new Date(Date.now() - 2 * DAY).toISOString(),
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
