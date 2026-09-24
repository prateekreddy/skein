// The review pane's browser suite: a set-aside file skein could not read (SKEIN-552).
//
// One part of `tests/ui/review.mjs`, run last so the corrupt file it plants is in nobody else's
// way. It asserts what is VISIBLE: the amber line in the owner's approved words, the `move it
// aside` chip on that line and on no other, the toast after the press, and the file on disk
// renamed beside itself rather than deleted.

import fs from "node:fs";
import path from "node:path";
import { REDRAW_MS, check, fx, mustSee, page, refreshQueue, settle } from "./setup.mjs";

// Node-side, unlike setup's `until`: what is waited on here is partly the disk.
const waitFor = async (ok, why) => {
  for (const deadline = Date.now() + REDRAW_MS * 4; Date.now() < deadline; await settle(50))
    if (await ok()) return;
  throw new Error(why);
};

console.log("\nset-aside file unreadable");
const dir = path.join(fx.home, "review", "acme");
const archive = path.join(dir, "archived.json");
const kept = fs.existsSync(archive) ? fs.readFileSync(archive, "utf8") : null;
fs.mkdirSync(dir, { recursive: true });
fs.writeFileSync(archive, "[7, 8,");
await refreshQueue();

await check("an unreadable archive is an amber line with the approved sentence, not an empty list", async () => {
  const line = await mustSee("#revpane .revblind div:has(button[data-set-aside])", "the set-aside line");
  const said = (await line.textContent()).replace(/\s+/g, " ").trim();
  const want = new RegExp(
    "^incomplete — acme: skein could not read the pull requests you set aside \\("
    + archive.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    + ": [^)]+\\), so they are all back in their lanes\\. Fix the file and the next refresh picks it "
    + "up, or move it aside to start a fresh list\\. move it aside$");
  if (!want.test(said)) throw new Error(`the line says: ${said}`);
  const chips = await page.$$("#revpane .revblind button[data-set-aside]");
  if (chips.length !== 1) throw new Error(`${chips.length} move-aside chips for one unreadable file`);
});

await check("move it aside renames the file, says so, and the line goes", async () => {
  await page.click("#revpane .revblind button[data-set-aside='archived']");
  const moved = `${archive}.unreadable-`;
  const toastSaid = () => page.$eval("#toast", e => e.textContent).catch(() => "");
  await waitFor(async () => (await toastSaid()).startsWith("moved to "), "no toast after the press");
  const t = await toastSaid();
  const m = /^moved to (\S+) — your set-aside list starts empty; nothing was deleted$/.exec(t);
  if (!m) throw new Error(`the toast says: ${t}`);
  if (!m[1].startsWith(moved)) throw new Error(`moved somewhere unexpected: ${m[1]}`);
  if (fs.readFileSync(m[1], "utf8") !== "[7, 8,") throw new Error("the moved file is not the corrupt one");
  if (fs.existsSync(archive)) throw new Error("the unreadable archive is still where it was");
  await waitFor(async () => !(await page.$("#revpane .revblind button[data-set-aside]")),
    "the line is still drawn after the file was moved aside");
  fs.rmSync(m[1]);
});

if (kept !== null) fs.writeFileSync(archive, kept);
