// Which sandboxes count as "your fleet", and how you see the ones that don't.
//
// The board's list comes from `sbx ls`, which reports every sandbox on the machine and cannot say
// which of them skein made. Left visible, a machine with a couple of unrelated `sbx` boxes read as a
// fleet full of broken rows: no branch, no signals, nothing that attaches. So a sandbox with no
// placement record is `foreign`, hidden by default, and revealed by one filter term.
//
// Three things are worth pinning and none needs a browser:
//
//   1. **Eligibility, not narrowing.** `foreign:` changes *which* boxes are on the board rather than
//      filtering the ones already there — so it shows exactly the foreign ones, and the default view
//      shows exactly the rest. Getting that backwards would either hide everything or hide nothing.
//   2. **The term composes with text.** `foreign: scratch` has to mean "foreign boxes matching
//      scratch", which means the keyword must be stripped before the text match sees it — otherwise
//      every box is tested against the literal "foreign:" and nothing ever matches.
//   3. **The counts are about your fleet.** A header that counted foreign sandboxes would report a
//      fleet you do not have, and the first-run checklist keys off the same number: with foreign
//      boxes counted, a fresh machine that happens to run `sbx` never sees the checklist at all.
//
//   node tests/ui/foreign.mjs
import { harness } from "./lift.mjs";
import { boardRows, withoutForeignTerm } from "../../cockpit/src/filter.mjs";

// **Imported, not lifted, and that is the repair rather than a tidy-up.** These functions used to be
// in `index.html` and this file pulled them out by text; they moved to `cockpit/src/filter.mjs`
// (`9116043`), after which `grab` threw and this whole suite stopped running — silently, because
// nothing runs these suites. Importing the real module is what the move was for: there is no lifted
// copy to drift, and `cockpit/test/filter.test.mjs` and this file now exercise the same code.
//
// Still not re-implemented here. The first version of this file wrote the same two lines again, and
// a mutation to the real code — dropping the default `!b.foreign` — left every check green: the test
// was agreeing with itself.

const { check, done } = harness();

// The foreign rows are a SECOND list now, fetched only when somebody asks for them
// (`askForForeign` in the page), where they used to be module state `boardRows` reached out for.
// So the harness splits the fixture the way the board holds it — passing one list and letting the
// function find the other is exactly the shape that stopped being true.
const T = {
  shownFor: (boxes, raw) =>
    boardRows(
      boxes.filter(b => !b.foreign),
      raw,
      boxes.filter(b => b.foreign),
    ).map(b => b.name),
  withoutForeignTerm,
};

const fleet = [
  { name: "thing-main", branch: "main", repo: "thing", headline: "waiting on you", foreign: false },
  { name: "thing-feat-x", branch: "feat/x", repo: "thing", headline: "", foreign: false },
  { name: "scratch-vm", branch: "", repo: "", headline: "", foreign: true },
  { name: "some-other-sbx-box", branch: "", repo: "", headline: "", foreign: true },
];

// --- eligibility ---------------------------------------------------------------------------------
check("the default board is your boxes and only yours", T.shownFor(fleet, ""), [
  "thing-main",
  "thing-feat-x",
]);
check("`foreign:` shows exactly the ones skein did not make", T.shownFor(fleet, "foreign:"), [
  "scratch-vm",
  "some-other-sbx-box",
]);
// It replaces the set rather than adding to it: "show me what is not mine" is the question being
// asked, and answering with everything would leave the foreign rows just as buried as before.
check("and not your boxes alongside them", T.shownFor(fleet, "foreign:").includes("thing-main"), false);

// --- composing with a text filter ----------------------------------------------------------------
// The keyword has to be stripped before the text match runs. Left in, every box would be tested
// against the literal "foreign:" — which no name contains — so the pane would go empty.
check("the term composes with text", T.shownFor(fleet, "foreign: scratch"), ["scratch-vm"]);
check("in either order", T.shownFor(fleet, "scratch foreign:"), ["scratch-vm"]);
check("and an ordinary filter still narrows your own boxes", T.shownFor(fleet, "feat"), [
  "thing-feat-x",
]);
check(
  "stripping the term leaves nothing behind to match on",
  T.withoutForeignTerm("foreign:"),
  "",
);

// --- a fleet that is only foreign ----------------------------------------------------------------
// The state that made this worth building: a machine running other sandboxes and no skein boxes at
// all. The default view must be empty — that is what lets the first-run checklist appear — while the
// foreign ones stay one keystroke away.
const strangersOnly = fleet.filter(b => b.foreign);
check("a machine with no skein boxes shows an empty board", T.shownFor(strangersOnly, ""), []);
check(
  "with the strangers still reachable",
  T.shownFor(strangersOnly, "foreign:").length,
  2,
);

// A box with no `foreign` field at all — an older server, or the demo fixtures — must read as yours
// rather than vanishing. `undefined` is falsy, so this holds by construction; it is asserted because
// the alternative is a board that silently empties against a server one version behind.
check(
  "a row with no flag is treated as yours, not hidden",
  T.shownFor([{ name: "demo-main", branch: "main", repo: "demo" }], ""),
  ["demo-main"],
);

done();
