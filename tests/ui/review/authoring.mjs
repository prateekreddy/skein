// The review pane's browser suite, what a reviewer writes and keeps: standing notes, setting a row
// aside, workflows, and authoring.
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

/** The workflows file, once it holds what a press was supposed to put there.
 *
 *  A save here is a round trip to the server and back out to disk, and what stood between every
 *  press and its `readFileSync` was a flat 1200ms — enough on an idle box, and on a busy one the
 *  whole verdict (SKEIN-801's shape, in a check about a file rather than about a pane). The file is
 *  the observable, so the file is what is watched: this returns the moment the save lands, and says
 *  what the file actually held when it does not.
 *
 *  It cannot turn a save that never happened green — the condition is the assertion the caller
 *  would have written, and `REDRAW_MS` is the same ceiling every wait in this file uses. */
const workflowsWhen = async (holds, why) => {
  const file = path.join(fx.home, "workflows.json");
  const read = () => { try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; } };
  for (const deadline = Date.now() + REDRAW_MS; ;) {
    const got = read();
    if (got && holds(got)) return got;
    if (Date.now() >= deadline) throw new Error(`${why}: the file holds ${JSON.stringify(read())}`);
    await settle(25);
  }
};

console.log("\nworkflows");
// Assign, see what it would do, and leave one out. The chain the owner has to be able to walk
// before trusting something that merges on its own — driven through the real page and the real
// routes, because "the endpoint returns the right JSON" is not the claim being made.
await check("a pull request can be told which workflow governs it, and what it would do", async () => {
  // A workflow that claims nothing on its own: what governs this PR must be a CHOICE somebody
  // made, or the test proves matching rather than assignment.
  fs.writeFileSync(path.join(fx.home, "workflows.json"), JSON.stringify({
    workflow: [{
      name: "ship-it",
      steps: [
        { when: ["checks:failing"], do: "flag:CI is red" },
        { when: ["no-label:ci"], do: "add-label:ci" },
      ],
    }],
  }));
  // Reopen the pane so it asks again — the workflow file is read per request, and nothing tells a
  // page already on screen that a file changed underneath it.
  await page.click("#revbtn");
  await settle(900);
  await pressRow("default timeout", { collapsedOnly: true });
  await until(() => !!document.querySelector("#revpane .revrow.open .revflow"), null,
    "an expanded row says nothing about what governs it");
  const before = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  if (!before.includes("Nothing governs")) {
    throw new Error(`a pull request nobody assigned anything to already carries something: ${before}`);
  }

  await page.selectOption("#revpane .revrow.open .revflow select", "ship-it");
  // The block saying something OTHER than it did — the assignment is a round trip, and 900ms was a
  // guess at it. What it now says is still asserted below, so a wrong answer fails as a wrong
  // answer rather than as a slow one.
  await until(was => (document.querySelector("#revpane .revrow.open .revflow")?.textContent || "") !== was,
    before, "assigning a workflow changed nothing on the row at all");
  const after = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  // The dry run: the step it WOULD take, from the same evaluator the tick uses.
  if (!after.includes("Next:") || !after.includes("add-label:ci")) {
    throw new Error(`assigning a workflow did not say what it would do: ${after}`);
  }
  // And the honest state of a fleet that has not switched this on: a plan, not a prediction.
  if (!after.includes("switched off")) {
    throw new Error(`the pane promised action on a fleet with workflows off: ${after}`);
  }
  // The collapsed line carries it too, or a queue of thirty says nothing at a glance.
  const chip = await page.$eval("#revpane .revrow.open .revtag.flow", e => e.textContent).catch(() => "");
  if (!chip.includes("ship-it")) {
    throw new Error(`the row does not show what governs it: ${chip}`);
  }
});

await check("and one pull request can be left out of it entirely", async () => {
  const before = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  await page.selectOption("#revpane .revrow.open .revflow select", "");
  await until(was => (document.querySelector("#revpane .revrow.open .revflow")?.textContent || "") !== was,
    before, "leaving a pull request out changed nothing on the row at all");
  const said = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  if (!said.includes("you left it out")) {
    throw new Error(`excluding a pull request did not stick: ${said}`);
  }
  const chip = await page.$("#revpane .revrow.open .revtag.flow");
  if (chip) throw new Error("a pull request that was left out still shows a workflow on its row");
});

console.log("\nauthoring");
await check("a workflow can be written in the cockpit, and it governs a pull request", async () => {
  // Start from nothing: this is somebody opening the editor on a fleet that has never had one.
  fs.rmSync(path.join(fx.home, "workflows.json"), { force: true });
  await page.click("#revbtn");
  await settle(900);
  await page.click("#revpane .revchip:has-text('workflows')");
  await settle();
  await page.click("#revpane .revchip:has-text('+ workflow')");
  await settle();

  // Name it, and give it a step: when checks are failing, say so and stop. Deliberately a step
  // whose action is a `flag` — the first workflow anybody writes should not be one that merges.
  await page.fill("#revpane .revflow-edit-head input", "watch-ci");
  await page.click("#revpane .revflow-edit .revchip:has-text('+ step')");
  // The step's two lines drawn, because the pickers below are read with `$$eval` — which answers
  // `[]` for a line that is not there yet, and `bothWays` reports that as "skein knows X and the
  // picker hides it", a sentence about the vocabulary written from a fact about the box.
  await until(() => document.querySelectorAll(
    "#revpane .revstep .revstep-line select").length >= 2, null,
    "pressing + step drew no step line to choose from");
  // **The picker offers exactly what the parser knows.** Taken from the server rather than written
  // into the page: a dropdown with its own idea of the vocabulary is a workflow somebody builds here
  // and cannot save, discovered by a person at the moment they were trusting the tool.
  const vocab = await (await fetch(`http://127.0.0.1:${port}/api/workflows`, { headers: authHeader() })).json();
  // Each line against its own half of the vocabulary: the `when` line offers conditions, the `do`
  // line offers actions, and neither may offer the other's words.
  const bothWays = (offered, words, what) => {
    const known = new Set(words.map(w => (w.arg ? w.kind : w.spelling)));
    for (const value of offered) {
      if (!known.has(value)) throw new Error(`the ${what} picker offers "${value}", which skein cannot parse`);
    }
    for (const want of known) {
      if (!offered.includes(want)) throw new Error(`skein knows "${want}" and the ${what} picker hides it`);
    }
  };
  const optionsIn = sel => page.$$eval(sel, els => els.map(e => e.value));
  bothWays(
    await optionsIn("#revpane .revstep .revstep-line:nth-child(1) select:first-of-type option"),
    vocab.conditions || [],
    "condition",
  );
  bothWays(
    await optionsIn("#revpane .revstep .revstep-line:nth-child(2) select:first-of-type option"),
    vocab.actions || [],
    "action",
  );

  // The step's condition picker offers the vocabulary; choose `checks`, then its value.
  //
  // By position in a locator rather than by index into a captured array (SKEIN-716): the three
  // reads used to be re-taken between the choices precisely BECAUSE the pane redraws the step line
  // under them, which is the same fact that made the handles stale. `nth` and `last` are resolved
  // when the choice is made, so the re-reads are the locator's job and the redraw is not a race.
  const steps = () => page.locator("#revpane .revstep .revstep-line select");
  await steps().nth(0).selectOption("checks");
  await settle();
  await steps().nth(1).selectOption("failing");
  await settle();
  // And the action.
  await steps().last().selectOption("flag");
  await settle();
  await page.fill("#revpane .revstep .revstep-line input", "CI is red");
  await page.click("#revpane .revchip:has-text('save')");

  // It is on disk, in the shape skein reads back — not the shape the page sent.
  const written = await workflowsWhen(w => ((w.workflow || [])[0] || {}).name === "watch-ci",
    "nothing was saved");
  const flow = (written.workflow || [])[0];
  if (JSON.stringify(flow.steps) !== JSON.stringify([{ when: ["checks:failing"], do: "flag:CI is red" }])) {
    throw new Error(`the step was not written as it was built: ${JSON.stringify(flow.steps)}`);
  }

  // And the workflow it just wrote can be put on a pull request — the two halves of this feature
  // meeting, which is the only thing that proves the editor produces something usable.
  await openRow("default timeout");
  // **The picker holding the new workflow, not merely existing.** The select is drawn from the list
  // the page already has, so it is on screen with `__rules` in it long before the save this check
  // just made has been read back — a wait for the ELEMENT passed on a quiet box and failed under
  // four lanes with `a workflow written here cannot be chosen there: __rules,`, which is this suite
  // making the very mistake it is here to remove. The option ARRIVING is what the check is about,
  // so that is what it waits for; a save that really never reaches the picker still fails with the
  // same sentence, and with the list the row did offer.
  const offered = () => page
    .$$eval("#revpane .revrow.open .revflow select option", els => els.map(e => e.value))
    .catch(() => []);
  await until(() => {
    const sel = document.querySelector("#revpane .revrow.open .revflow select");
    return !!sel && [...sel.options].some(o => o.value === "watch-ci");
  }, null, async () => `a workflow written here cannot be chosen there: ${(await offered()).join(", ")}`);
});

// SKEIN-248. `Workflow::serial` is the whole of what makes a merge train a train — one pull
// request at a time per (repo, workflow), oldest first — and it appeared ZERO times in
// `src/web/index.html`. It rode the editor's payload, so existing trains survived an edit by
// accident (the object round-trips opaquely), but one could not be built here, and one that existed
// could not be un-set. A documented feature the cockpit cannot express is one the file is the only
// interface to.
await check("a merge train can be built here: one at a time is a control, not just a file key", async () => {
  const onDisk = () => JSON.parse(fs.readFileSync(path.join(fx.home, "workflows.json"), "utf8")).workflow[0];
  if (onDisk().serial) throw new Error("the workflow written above was already serial — this proves nothing");
  const chip = "#revpane .revflow-edit-head .revchip:has-text('one at a time')";
  await mustSee(chip, "the one-at-a-time control");
  await page.click(chip);
  await page.click("#revpane .revchip:has-text('save')");
  await workflowsWhen(w => !!(w.workflow || [])[0]?.serial, "the switch did not reach the file");

  // The round trip, which is the half that would go wrong silently: the editor reads the file back
  // and has to still know this workflow is a train.
  await page.click("#revpane .revchip:has-text('workflows')");   // close
  await until(() => !document.querySelector("#revpane .revflow-edit-head"), null,
    "the workflows editor would not close, so re-opening it would prove nothing about the file");
  await page.click("#revpane .revchip:has-text('workflows')");   // and open on what is on disk
  await until(() => !!document.querySelector("#revpane .revflow-edit-head .revchip"), null,
    "the workflows editor did not come back");
  const said = await page.$eval(chip, e => e.textContent.replace(/\s+/g, " ").trim());
  if (!/one at a time · on/.test(said))
    throw new Error(`the editor reopened on a train and does not say it is one: ${said}`);

  // And it can be un-set, which the file being the only interface made impossible without an editor.
  await page.click(chip);
  await page.click("#revpane .revchip:has-text('save')");
  await workflowsWhen(w => !(w.workflow || [])[0]?.serial,
    "a train cannot be switched back to running in parallel");
});
// The control says what it DOES, because "serial" is the file's word and the person pressing it is
// deciding whether every sibling re-runs CI on every merge.
await check("and it says which of the two behaviours it is choosing", async () => {
  const title = await page.getAttribute("#revpane .revflow-edit-head .revchip:has-text('one at a time')", "title");
  if (!/same pass/.test(title || "") || !/re-runs CI/.test(title || ""))
    throw new Error(`the switch does not say what leaving it off means: ${title}`);
  await page.click("#revpane .revflow-edit-head .revchip:has-text('one at a time')");
  // The title is rewritten by the render the press causes, so the press is waited for on the title
  // itself: read after a beat, this check reports the OFF sentence as the switch's answer to being
  // switched on, which is a fact about the clock wearing the costume of a copy bug.
  const saysNow = was => {
    const el = [...document.querySelectorAll("#revpane .revflow-edit-head .revchip")]
      .find(e => /one at a time/.test(e.textContent || ""));
    return !!el && (el.getAttribute("title") || "") !== was;
  };
  await until(saysNow, title || "",
    "pressing the switch did not change what it says it would do");
  const on = await page.getAttribute("#revpane .revflow-edit-head .revchip:has-text('one at a time')", "title");
  if (!/oldest-first, one per pass/.test(on || ""))
    throw new Error(`the switch does not say what switching it on means: ${on}`);
  await page.click("#revpane .revflow-edit-head .revchip:has-text('one at a time')");
  await until(saysNow, on || "",
    "the switch would not go back, and the editor is left on for the check below");
});
// A workflow made HERE starts life able to become a train, rather than needing the file opened by
// hand — `revEditAddFlow` built `{name, matches, steps}` and nothing else, so the thing you had just
// created was the one thing you could not make serial.
await check("a workflow created in the cockpit can be made a train, without touching the file", async () => {
  const wasEditing = await page.locator("#revpane .revflow-edit-head").count();
  await page.click("#revpane .revchip:has-text('+ workflow')");
  // The second editor on screen — `count()` answers immediately, so a beat here is the difference
  // between "a new workflow has no one-at-a-time control" and "the second editor is still being
  // drawn", and only one of those is about `revEditAddFlow`.
  await until(n => document.querySelectorAll("#revpane .revflow-edit-head").length > n, wasEditing,
    "pressing + workflow drew no second editor");
  const chips = page.locator("#revpane .revflow-edit-head .revchip:has-text('one at a time')");
  const n = await chips.count();
  if (n < 2) throw new Error(`a new workflow has no one-at-a-time control: ${n}`);
  await chips.last().click();
  // A step, because a workflow with none is not one a save can be judged on.
  await page.locator("#revpane .revflow-edit .revchip:has-text('+ step')").last().click();
  await page.click("#revpane .revchip:has-text('save')");
  const written = (await workflowsWhen(w => (w.workflow || []).length > 1,
    "a workflow built from scratch here never reached the file")).workflow;
  const made = written[written.length - 1];
  if (!made.serial)
    throw new Error(`a workflow built from scratch here cannot be a train: ${JSON.stringify(made)}`);

  // Put the fixture back for the checks below, which read the file this section wrote.
  await page.locator("#revpane .revflow-edit .revchip:has-text('delete workflow')").last().click();
  await page.click("#revpane .revchip:has-text('save')");
  await workflowsWhen(w => (w.workflow || []).length === 1,
    "the workflow this check added would not go away again, and the checks below read this file");
});

await check("a workflow it could not read is refused with the step that is wrong", async () => {
  // What the server says when the file will not parse — the message that names the workflow, the
  // step and the vocabulary, rather than "invalid".
  const res = await fetch(`http://127.0.0.1:${port}/api/workflows`, {
    method: "PUT",
    headers: { "Content-Type": "application/json", ...authHeader() },
    body: JSON.stringify({ workflow: [{ name: "bad", steps: [{ when: ["checks:green"], do: "merge:squash" }] }] }),
  });
  const said = await res.json();
  if (said.ok) throw new Error("a step nobody can parse was saved");
  if (!String(said.error).includes("bad") || !String(said.error).includes("passing")) {
    throw new Error(`the refusal does not say where to look or what to say: ${said.error}`);
  }
  // And the good file is still there: a refusal that arrives after the old file is gone is a
  // refusal that cost somebody their workflows.
  const still = JSON.parse(fs.readFileSync(path.join(fx.home, "workflows.json"), "utf8"));
  if ((still.workflow || [])[0]?.name !== "watch-ci") {
    throw new Error("a refused save destroyed the file it refused to replace");
  }
});
