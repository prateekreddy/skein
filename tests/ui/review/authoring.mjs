// The review pane's browser suite, what a reviewer writes and keeps: standing notes, setting a row
// aside, and authoring.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { REDRAW_MS, authHeader, check, fx, laneTitles, mustSee, openRow, page, port, pressRow, refreshMirror, settle, until } from "./setup.mjs";

console.log("\nstanding notes");
// Back to the queue: a posted verdict leaves its receipt in the reading view and stays there
// (SKEIN-162), and the notes chip lives in the queue's header.
await page.keyboard.press("Escape");
await settle();
await check("the pane says how much of the repo it has notes on", async () => {
  const chip = await mustSee("#revpane .revchip:has-text('notes')", "the notes chip");
  if (!/notes/.test(await chip.textContent())) throw new Error("no notes chip");
});
await check("opening it lists the repo's modules, and admits it has none written", async () => {
  await page.click("#revpane .revchip:has-text('notes')");
  await page.waitForSelector("#revpane .revmods .revmod", { timeout: 8000 });
  const mods = await page.$$eval("#revpane .revmod", els => els.map(e => ({
    path: e.querySelector("code")?.textContent.trim(),
    state: [...e.querySelector(".revmodstate").classList].filter(c => c !== "revmodstate")[0] || "absent",
    action: e.querySelector("button")?.textContent.trim(),
  })));
  if (!mods.some(m => m.path === "src")) throw new Error(`modules not found: ${JSON.stringify(mods)}`);
  if (!mods.every(m => m.state === "absent")) throw new Error(`a note appeared from nowhere: ${JSON.stringify(mods)}`);
  if (!mods.every(m => m.action === "write")) throw new Error("no way to write one");
});
// The whole design rests on this: a note carries the commit its module was at, so out-of-date is a
// fact rather than a worry.
//
// Both directions, through the API a person's clicks go through. This check used to assert only the
// stale half, and for the wrong reason: its own comment said "the fixture's clone is not a git repo,
// so freshness cannot be established". That made it pass on an accident. The fixture commits its
// tree now, so freshness is a real question here and the answer to it is worth having.
// `.modules`, because the payload distinguishes a repo with no modules from a repo skein could not
// read (SKEIN-117) — a bare array could only say the first.
const modulesNow = async () =>
  (await (await fetch(`http://127.0.0.1:${port}/api/repos/acme/modules`, { headers: authHeader() })).json()).modules;
await check("a note written against the current commit reads fresh", async () => {
  const wrote = await fetch(`http://127.0.0.1:${port}/api/repos/acme/modules/write`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ path: "src" }),
  }).then(r => r.json());
  if (!wrote.ok) throw new Error(`writing was refused: ${wrote.error}`);
  const src = (await modulesNow()).find(m => m.path === "src");
  if (src.state !== "fresh")
    throw new Error(`a note written just now against an unmoved module reads "${src.state}"`);
});
// **The same write, naming a repo that is not the one in the URL** (SKEIN-427). `src` is a module
// of nearly every repo, so membership cannot see this: repo A's `src` posted to repo B is a
// perfectly valid write of B's `src`, and what is wrong with it is only visible in the disagreement
// between where the note came from and where it is going. The cockpit's notes panel produced
// exactly that disagreement — it kept one repo's rows after the repo filter moved — and it is
// stopped in the page now, so this is here to hold the second half: the route refusing it whatever
// the page believes.
await check("a note that names another repo is refused before anything is written", async () => {
  const wrote = await fetch(`http://127.0.0.1:${port}/api/repos/acme/modules/write`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ path: "src", repo: "somewhere-else" }),
  }).then(r => r.json());
  if (wrote.ok) throw new Error("a note about another repository was written against acme");
  const why = String(wrote.error || "");
  if (!why.includes("somewhere-else") || !why.includes("acme")) {
    throw new Error(`the refusal has to name both repos — a reader told only where it went cannot `
      + `tell which of theirs the note was about: ${JSON.stringify(why)}`);
  }
});
await check("and goes stale the moment its module moves", async () => {
  // The module moves, and skein sees it move.
  //
  // This used to DELETE the mirror and let `ensure_mirror` re-clone it. That worked only while a
  // clone could come off a local checkout; it now goes to `repo.source`, which is a GitHub URL, so
  // deleting the mirror deleted the repo as far as this suite was concerned — `modulesNow()` came
  // back with nothing and the check died on `undefined.state` rather than on freshness.
  //
  // Fetching is the better stand-in anyway: it is the same `git fetch --prune` that
  // `repos::fetch_mirror` runs, which is literally what a pull does, rather than the half-made-clone
  // repair path that happened to have the same effect.
  const work = path.join(fx.root, "work");
  fs.appendFileSync(path.join(work, "src", "parser.rs"), "const RETRIES: u8 = 3;\n");
  const wgit = (...a) => spawnSync("git", ["-C", work, ...a], { stdio: "ignore" });
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "src moves on");
  refreshMirror(fx.home);

  const src = (await modulesNow()).find(m => m.path === "src");
  // Never `fresh`. A note whose freshness cannot be established is exactly a note not to trust, so
  // every uncertainty here has to fall the same way as a known move.
  if (src.state === "fresh")
    throw new Error("a note stamped with a commit that is no longer the module's read as current");
});

console.log("\nsetting aside");
// SKEIN-162 (§7.1): the press is a receipt in place with an undo window, not an immediate act —
// nothing posts inside the window, the row does not vanish under the reader, and only the next
// natural load moves it to the archived lane.
await check("set aside is a receipt in place — undo cancels, the lapse archives, the next load moves it", async () => {
  // Not the row the verdict above went to: an act's receipt stays on the strip in place of the
  // controls (SKEIN-162), so a row just commented on has no `set aside` to press.
  await pressRow("null deref", { collapsedOnly: true });
  await settle();
  const before = await laneTitles("yours");
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('set aside')");
  // The receipt replacing the control IS the press (SKEIN-162), so it is what the press waits for.
  // The ROW's own strip, not the drafted review's beside it: an expanded row grew sections with
  // their own `.revacts`, and a bare selector reads whichever the document reaches first.
  await until(() => /undo/.test(
    document.querySelector("#revpane .revrow.open .revrowacts")?.textContent || ""), null,
    async () => `the control did not become the receipt: "${
      await page.$eval("#revpane .revrow.open .revrowacts", e => e.textContent || "").catch(() => "(no strip)")}"`);
  const strip = await page.$eval("#revpane .revrow.open .revrowacts", e => e.textContent || "");
  if (!/set aside/.test(strip))
    throw new Error(`the receipt does not name the act it is for: "${strip}"`);
  const held = await laneTitles("yours");
  if (held.length !== before.length) throw new Error("the row vanished inside the undo window");
  // undo: the request never left the machine, and the strip returns.
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('undo')");
  await until(() => {
    const said = document.querySelector("#revpane .revrow.open .revrowacts")?.textContent || "";
    return /set aside/.test(said) && !/undo/.test(said);
  }, null, async () => `undo did not restore the strip: "${
    await page.$eval("#revpane .revrow.open .revrowacts", e => e.textContent || "").catch(() => "(no strip)")}"`);
  // Pressed for real: the window lapses, the archive posts, and the row greys IN PLACE.
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('set aside')");
  // The OPEN row: an earlier verdict in this file left its own row marked done, and a bare
  // `.revrow.done` matches that one instantly — the wait would pass before this act had posted.
  await page.waitForSelector("#revpane .revrow.open.done", { timeout: 15000 });
  const after = await laneTitles("yours");
  if (after.length !== before.length) throw new Error("the done row left the lane before the next load");
  // It leaves on the next natural load, by which time you are elsewhere.
  await page.click("#revpane .revhead .revchip:has-text('refresh')");
  // The load the refresh starts, and then the lane it moves the row into — a beat here is a
  // question about GitHub's round trip answered with a stopwatch.
  await until(() => {
    const el = document.querySelector("#revpane .revlane[data-lane='archived']");
    return !revLoading && !!el && el.querySelectorAll(".revtitle").length > 0;
  }, null, "nothing reached the archived lane after the next load");
});
