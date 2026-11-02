// A box's question, drawn in the questions panel a person actually opens, from the real server
// reading a real queue (SKEIN-1061).
//
// Every word on the card is the box's own: it wrote the file, in a drop-box bound read-write into
// it. So the card shows the question as plain text — a `<b>` in it is two characters and a letter,
// not bold — and the offered answers become buttons whose labels are escaped. What is pressed goes
// back to the host and into the box's inbox in the approved words, and the card is redrawn as
// answered or dismissed.
//
// Four waiting questions: one offering answers (and trying markup in both its text and a label),
// one offering none (so the owner types a line), one to dismiss, and one offering answers that the
// owner answers in their own words instead (SKEIN-1466). And two the owner answered earlier, one a
// day ago and one three days ago: answered ones sit under a closed "Answered" row, and after two
// days they are not shown at all (SKEIN-1466).
//
//   node tests/ui/asks.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";
const API_TOKEN = "skein-test-" + "t".repeat(53);
const BOX = "web-main";

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-asks");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The box's questions, where `asks::requests_dir` reads them: one directory per box.
  const asked = path.join(fleet, ".skein", "asks", "requests", BOX);
  fs.mkdirSync(asked, { recursive: true });
  const put = (id, body) => fs.writeFileSync(path.join(asked, `${id}.json`), JSON.stringify({ id, box: BOX, ...body }));
  put("q-00000001", {
    question: "Should I <b>drop</b> the old fixtures table?",
    options: ["<i>Drop</i> it", "Keep it"], asked: "2026-09-24T10:12:00Z",
    // What the box says about itself, which is nobody's answer.
    state: "answered", answer: "Keep it",
  });
  put("q-00000002", { question: "Which port should the dev server use?", asked: "2026-09-24T10:13:00Z" });
  put("q-00000003", { question: "Is this still wanted?", options: ["Yes", "No"], asked: "2026-09-24T10:14:00Z" });
  put("q-00000004", { question: "Merge the two config files?", options: ["Merge them", "Leave them"], asked: "2026-09-24T10:15:00Z" });
  // Answered earlier: the box's file, and the host's record of the answer where `asks::decision_path`
  // keeps it, decided a day and three days before this run.
  const day = 24 * 60 * 60 * 1000;
  const decided = path.join(home, "asks", BOX);
  fs.mkdirSync(decided, { recursive: true });
  for (const [id, question, answer, ago] of [
    ["q-00000005", "Turn the debug logging off?", "Yes, off for good", 1 * day],
    ["q-00000006", "Rename the cache directory?", "No, keep the old name", 3 * day],
  ]) {
    put(id, { question, options: ["Yes", "No"], asked: "2026-09-20T09:00:00Z" });
    fs.writeFileSync(path.join(decided, `${id}.json`), JSON.stringify({
      id, box: BOX, question, options: ["Yes", "No"], asked: "2026-09-20T09:00:00Z",
      state: "answered", answer, decided: new Date(Date.now() - ago).toISOString(),
    }));
  }
  return { root, home, fleet };
}

const fx = makeFixture();
const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: fx.fleet,
  },
});

const { value: check, report } = ledger();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });
const until = async (f, ms = 8000) => {
  for (let t = 0; t < ms; t += 200) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};
const card = text => page.locator("#askq .msg", { hasText: text }).first();
// The text of the first match, or null when there is none — so a check reads a missing element as a
// wrong answer rather than waiting out Playwright's timeout and failing the whole suite.
const textOf = async loc => (await loc.count()) ? (await loc.first().textContent()).trim() : null;
const inbox = () => {
  const dir = path.join(fx.home, "boxes", BOX, "inbox");
  if (!fs.existsSync(dir)) return [];
  return fs.readdirSync(dir).sort().map(f => JSON.parse(fs.readFileSync(path.join(dir, f), "utf8")))
    .map(m => `${m.kind}: ${m.body}`);
};

try {
  // By the link `/v2` opens for a question row: the hash names the panel (SKEIN-1375).
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}#askq`, { waitUntil: "domcontentloaded" });
  // **What would make this fail:** the hash router not run at boot, or `#askq` naming no panel.
  check("a link to /#askq opens the questions panel, and leaves the address clean",
    { open: await until(() => page.evaluate(() => document.getElementById("askq").classList.contains("open"))),
      hash: await page.evaluate(() => location.hash) },
    { open: true, hash: "" });

  // The decision inbox above the board: one card per waiting question, the one whose own file
  // claims it is answered included, each in the box's words as plain text.
  const strip = page.locator("#decisions");
  check("the Decisions strip shows every waiting question, the box's self-answered one too",
    await until(async () => (await strip.locator(".dcard").count()) === 4), true);
  check("a decision card shows `<b>` as the characters", {
    text: await strip.locator(".dcard", { hasText: "fixtures table" }).locator(".aq-body").textContent(),
    bold: await strip.locator(".aq-body b").count(),
    buttons: await strip.locator(".dcard button").allTextContents(),
  }, {
    text: "Should I <b>drop</b> the old fixtures table?", bold: 0,
    buttons: ["Answer in Questions", "Answer in Questions", "Answer in Questions", "Answer in Questions"],
  });

  // Answered questions are kept out of the way (SKEIN-1466).
  const done = page.locator("#askq details.aq-done");
  // **What would make this fail:** the decided questions drawn in the list as before rather than
  // under a `<details>`, or that `<details>` drawn `open` for a viewer who never opened it.
  check("answered questions sit under one closed row, their answers out of sight", {
    row: await until(async () => (await done.count()) === 1) && (await textOf(done.locator("summary"))),
    open: (await done.count()) ? await done.evaluate(d => d.open) : null,
    seen: await card("debug logging").isVisible(),
  }, { row: "Answered · 1", open: false, seen: false });
  // **What would make this fail:** `ASKQ_DECIDED_SHOWN_MS` longer than three days, or the filter on
  // the decision time dropped from `askqListHtml`.
  check("a question answered three days ago is not shown at all, though its record stays",
    { cards: await page.locator("#askq .msg", { hasText: "cache directory" }).count(),
      record: fs.existsSync(path.join(fx.home, "asks", BOX, "q-00000006.json")) },
    { cards: 0, record: true });
  // **What would make this fail:** `ASKQ_DECIDED_SHOWN_MS` under a day, so the recent answer goes
  // too; or the summary not opening its `<details>`.
  if (await done.count()) await done.locator("summary").click();
  check("opening the row shows the question answered a day ago, with the answer given", {
    seen: await until(() => card("debug logging").isVisible()),
    said: (await textOf(card("debug logging").locator(".sq-row")))?.replace(/ · .*$/, ""),
  }, { seen: true, said: 'answered: "Yes, off for good"' });
  // **What would make this fail:** the `toggle` listener not saving the state, or `askqListHtml`
  // not reading it back — the redraw replaces the `<details>`, and a fresh one is closed.
  await page.evaluate(() => loadAskq());
  check("the row stays open across a redraw of the panel, once this viewer opened it",
    await page.evaluate(() => document.querySelector("#askq details.aq-done")?.open ?? null), true);
  const marked = card("fixtures table");
  check("the question is drawn, waiting, with its offered answers, a reply field and Dismiss to press",
    await until(async () => (await marked.locator("button").count()) === 4) && (await marked.locator("input.aq-reply").count()), 1);

  // **What would make this fail:** the question body drawn without `esc` — `<b>` becomes an
  // element, and the text reads "drop" rather than "<b>drop</b>".
  check("`<b>` in a question is shown as the characters, not as bold", {
    text: await marked.locator(".aq-body").textContent(),
    bold: await marked.locator(".aq-body b").count(),
  }, { text: "Should I <b>drop</b> the old fixtures table?", bold: 0 });
  // The same for a label: an offered answer is a button face and nothing more.
  check("and an offered answer's label is escaped too", {
    labels: await marked.locator("button").allTextContents(),
    italic: await marked.locator("button i").count(),
  }, { labels: ["<i>Drop</i> it", "Keep it", "Send answer", "Dismiss"], italic: 0 });
  check("the card says whose words these are", (await marked.locator(".sq-meta").first().textContent()).trim(),
    "question · asked by web-main — its own words, unverified · 2026-09-24 10:12");

  // Answering with a label: the host records it and the box's inbox gets the approved line.
  await marked.locator("button", { hasText: "Drop" }).click();
  await until(async () => (await card("fixtures table").locator("button").count()) === 0);
  check("pressing an offered answer answers it, and the card says so",
    (await card("fixtures table").locator(".sq-row").textContent()).trim().replace(/\d\d:\d\d$/, "HH:MM"),
    'answered: "<i>Drop</i> it" · HH:MM');

  // No options: a one-line field and Send answer.
  const open = card("dev server");
  check("a question with no options offers a reply field and Send answer",
    { field: await open.locator("input.aq-reply").count(), buttons: await open.locator("button").allTextContents() },
    { field: 1, buttons: ["Send answer", "Dismiss"] });
  await open.locator("input.aq-reply").fill("7979");
  await open.locator("button", { hasText: "Send answer" }).click();
  await until(async () => (await card("dev server").locator("button").count()) === 0);

  // Options offered, and the owner answers in their own words instead (SKEIN-1466).
  // **What would make this fail:** `askqCard` drawing the reply field only when there are no
  // options (it did until SKEIN-1466), or `asks::answer` refusing a label the question did not offer.
  const merge = card("config files");
  if (await merge.locator("input.aq-reply").count()) {
    await merge.locator("input.aq-reply").fill("Merge them, but keep a backup");
    await merge.locator("button", { hasText: "Send answer" }).click();
    await until(async () => (await card("config files").locator("button").count()) === 0);
  }
  const recorded = path.join(fx.home, "asks", BOX, "q-00000004.json");
  check("an answer typed on a question that offered options is recorded as typed", {
    state: fs.existsSync(recorded) && JSON.parse(fs.readFileSync(recorded, "utf8")).state,
    answer: fs.existsSync(recorded) && JSON.parse(fs.readFileSync(recorded, "utf8")).answer,
    card: (await textOf(card("config files").locator(".sq-row")))?.replace(/ · .*$/, ""),
  }, { state: "answered", answer: "Merge them, but keep a backup", card: 'answered: "Merge them, but keep a backup"' });

  // Dismiss frees the slot, and says so to the box.
  await card("still wanted").locator("button", { hasText: "Dismiss" }).click();
  await until(async () => (await card("still wanted").locator("button").count()) === 0);
  check("a dismissed question says so", (await card("still wanted").locator(".sq-row").textContent()).trim()
    .replace(/\d\d:\d\d$/, "HH:MM"), "dismissed without an answer · HH:MM");

  // **What would make this fail:** the strip not refreshed after an answer, or the queue's cache
  // serving the answered questions for its window (`queue::forget_decisions` not called).
  check("once all four are answered the Decisions strip is gone",
    await until(() => page.evaluate(() => document.getElementById("decisions").hidden), 3000), true);

  await until(async () => inbox().length === 4);
  check("each answer reached the box's inbox once, in the approved words", inbox().sort(), [
    'answer: question q-00000001, "<i>Drop</i> it"',
    'answer: question q-00000002, "7979"',
    "answer: question q-00000003 was dismissed without an answer",
    'answer: question q-00000004, "Merge them, but keep a backup"',
  ]);
  // **What would make this fail:** the decided questions sorted oldest first, or by when they were
  // asked rather than when they were decided (the day-old one was asked first of all).
  check("under the row, the newest decision comes first", {
    row: await done.locator("summary").textContent(),
    order: (await done.locator(".aq-body").allTextContents()).map(t => t.slice(0, 14)),
  }, { row: "Answered · 5", order: ["Is this still ", "Merge the two ", "Which port sho", "Should I <b>dr", "Turn the debug"] });

  sayBlips();
  check("the panel raised no page errors", errors, []);
} catch (e) {
  check("the suite could run at all", String((e && e.message) || e), "it ran");
}

await browser.close();
const failed = report();
if (failed.length) console.log(`\nserver log:\n${log()}`);
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
