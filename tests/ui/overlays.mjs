// Every overlay must actually be an overlay.
//
// This exists because one wasn't. The Repo write access panel (`#gitq`) was added with markup, a
// button, a poller, a badge, decision handlers and sixteen passing tests — and no CSS. It was
// missing from the two rules that make an overlay an overlay, so it rendered as a static div in
// normal document flow, 171px of dead scroll below the fold, and clicking its button added `.open`
// to an element nothing styled. The feature had a complete UI that could not be reached.
//
// Nothing caught it. The other UI suites lift functions out of the page and run them against a
// stubbed DOM — which is the right way to test decision logic, and structurally blind to whether
// the thing those decisions render into is visible at all. A browser would have caught it; so does
// this, for none of the cost, because the invariant is textual: an overlay is declared in the
// markup as `<div id=X aria-hidden="true">`, and the stylesheet must style both `#X` and `#X.open`.
//
// Deliberately weaker than "is in the shared rule": `#pal` carries its own copy of the same
// declarations, so requiring one particular rule would encode today's grouping rather than the
// property that matters. Styled at all, and styled when open, is the thing that was missing.
//
//   node tests/ui/overlays.mjs
import fs from "node:fs";
import { harness } from "./lift.mjs";

const html = fs.readFileSync(new URL("../../src/web/index.html", import.meta.url), "utf8");
const css = html.slice(html.indexOf("<style"), html.indexOf("</style>"));
const { check, done } = harness();

const overlays = [...html.matchAll(/<div id="([a-zA-Z0-9_-]+)" aria-hidden="true">/g)].map(m => m[1]);
check("the page still declares its overlays the same way", overlays.length > 5, true);
check("and the stylesheet was found", css.length > 1000, true);

for (const id of overlays) {
  // `(?![\w-])` so `#subq` is not satisfied by `#subqbtn`, which is the badge on the button that
  // opens it — present in the CSS, and no help whatsoever in making the panel visible.
  const styled = new RegExp(`#${id}(?![\\w-])`).test(css);
  const opens = new RegExp(`#${id}\\.open(?![\\w-])`).test(css);
  check(`#${id} is styled`, styled, true);
  check(`#${id} is styled when open`, opens, true);
}

// The other half of the same mistake: something opening an element the markup never declared.
//
// The element fetched must be the element opened, which a proximity match cannot tell. Add-repo
// fetches its *button* (`#ar-go`) two lines before opening its *modal*, so anything looser than
// this reports a failure that isn't one — it did, on the first run of this file.
const opened = new Set();
for (const m of html.matchAll(/getElementById\("([\w-]+)"\)\.classList\.add\("open"\)/g)) {
  opened.add(m[1]);
}
for (const m of html.matchAll(
  /(\w+)\s*=\s*document\.getElementById\("([\w-]+)"\);\s*\1\.classList\.add\("open"\)/g
)) {
  opened.add(m[2]);
}
check("some overlay is opened by id, or this check is watching nothing", opened.size > 0, true);
for (const id of opened) {
  check(`something opens #${id}, so the markup must declare it`, overlays.includes(id), true);
}

done();
