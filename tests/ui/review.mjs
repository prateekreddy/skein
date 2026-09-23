// Browser test for the review pane: a repo's PR queue, in a real browser, clicked the way you
// would click it.
//
// It has its own fixture rather than riding on smoke.mjs because the two need incompatible repos:
// smoke's is adopted from a local path (deliberately — that is the shape that broke `slug_from_url`)
// and a local path has no GitHub identity, so it has no PR queue by design. Bending that fixture to
// serve both would weaken the case it was built to prove.
//
// The rule inherited from smoke.mjs applies here too, and is the reason this file exists at all:
// **assert what is VISIBLE**. `#gitq` once shipped with complete markup, a poller, decision handlers
// and sixteen passing tests, and no CSS — a whole feature that could not be reached. Unit tests
// cannot see that. A browser can.
//
//   node tests/ui/review.mjs
//
// **The suite is this file and the parts under `tests/ui/review/`** (SKEIN-1117). It ran to more
// than three thousand lines as one file, so it was cut along the sections it already had, and not
// one check changed on the way. `./review/setup.mjs` builds the fixture, the server and the page,
// and each part below is a section of the queue's questions, run in the order the one file ran
// them.
//
// **In order, on one page, and that is why the parts are imported one at a time with `await
// import`.** A part inherits the queue as the one before it left it — a group folded, a filter
// pressed, a row open — so running them side by side would be a different suite. Static imports
// would be exactly that: sibling modules with top-level `await` are evaluated interleaved, not one
// after the other. The quiet check and the report stay here because they are about the whole run.

import fs from "node:fs";
import path from "node:path";
import { browser, check, fx, log, noise, page, report, results, sayBlips, settle } from "./review/setup.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

await import("./review/queue.mjs");
await import("./review/keyboard.mjs");
await import("./review/summaries.mjs");
await import("./review/row.mjs");
await import("./review/authoring.mjs");
await import("./review/edges.mjs");
await import("./review/merge.mjs");
await import("./review/hostile.mjs");

console.log("\nquiet");
await check("no page errors and no 5xx along the way", () => {
  sayBlips();
  if (noise.length) throw new Error(noise.join(" | "));
});

// ---------- report ----------
// SKEIN_SHOT=<path> captures the pane whether or not anything failed. Assertions prove the pane
// works; only a picture shows whether it reads well, and the two are not the same review.
if (process.env.SKEIN_SHOT) {
  await page.click("#revpane .revchip:has-text('all')").catch(() => {});
  await settle();
  await page.screenshot({ path: process.env.SKEIN_SHOT });
  console.log(`\nscreenshot: ${process.env.SKEIN_SHOT}`);
}
const shot = path.join(fx.root, "failure.png");
if (results.some(([ok]) => !ok)) await page.screenshot({ path: shot, fullPage: false });
const failed = report({ log });
if (failed.length) console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
