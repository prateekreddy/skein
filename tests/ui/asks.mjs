// A box's question, drawn in the questions panel a person actually opens, from the real server
// reading a real queue (SKEIN-1061).
//
// Every word on the card is the box's own: it wrote the file, in a drop-box bound read-write into
// it. So the card shows the question as plain text — a `<b>` in it is two characters and a letter,
// not bold — and the offered answers become buttons whose labels are escaped. What is pressed goes
// back to the host and into the box's inbox in the approved words, and the card is redrawn as
// answered or dismissed.
//
// Three questions: one offering answers (and trying markup in both its text and a label), one
// offering none (so the owner types a line), and one to dismiss.
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
const inbox = () => {
  const dir = path.join(fx.home, "boxes", BOX, "inbox");
  if (!fs.existsSync(dir)) return [];
  return fs.readdirSync(dir).sort().map(f => JSON.parse(fs.readFileSync(path.join(dir, f), "utf8")))
    .map(m => `${m.kind}: ${m.body}`);
};

try {
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(800);
  await page.evaluate(() => openAskq());
  const marked = card("fixtures table");
  check("the question is drawn, waiting, with its offered answers and Dismiss to press",
    await until(async () => (await marked.locator("button").count()) === 3), true);

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
  }, { labels: ["<i>Drop</i> it", "Keep it", "Dismiss"], italic: 0 });
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

  // Dismiss frees the slot, and says so to the box.
  await card("still wanted").locator("button", { hasText: "Dismiss" }).click();
  await until(async () => (await card("still wanted").locator("button").count()) === 0);
  check("a dismissed question says so", (await card("still wanted").locator(".sq-row").textContent()).trim()
    .replace(/\d\d:\d\d$/, "HH:MM"), "dismissed without an answer · HH:MM");

  await until(async () => inbox().length === 3);
  check("each answer reached the box's inbox once, in the approved words", inbox().sort(), [
    'answer: question q-00000001, "<i>Drop</i> it"',
    'answer: question q-00000002, "7979"',
    "answer: question q-00000003 was dismissed without an answer",
  ]);

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
