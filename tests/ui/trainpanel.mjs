// The merge-train panel inside the workflows pane — the visibility the owner asked for verbatim:
// "I need to know exactly what is it working on, which step is it on, status of previous steps",
// and "I should be able to pause it or resume it at any point" (SKEIN-213).
//
// Five contracts, each against the real functions lifted out of index.html:
//   * the panel names the front PR and spells its step out from standing;
//   * paused is words plus a resume button, not a missing pause button;
//   * the pause button POSTs the flipped `pr_workflows` to /api/settings and then paints what
//     GET /api/workflows answers — never what the click hoped;
//   * a PR's journal renders oldest-first with its kinds, and "cleared" reads as a person's act;
//   * an older server sends no `trains` and no `journal`, and the panel degrades to what standing
//     alone gives — no crash, no invented front.
//
//   node tests/ui/trainpanel.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed down to what the panel touches. `serverEnabled` is what
// GET /api/workflows will ANSWER after a settings POST — deliberately independent of what was
// posted, so a test can prove the panel paints the server's answer rather than the click.
function world(flowsByRepo, opts = {}) {
  const posts = [];
  const counts = { painted: 0, reloads: 0, toasts: [] };
  const fetch = (url, init) => {
    if (init && init.method === "POST") {
      posts.push({ url, body: JSON.parse(init.body) });
      return Promise.resolve({ ok: true, json: () => Promise.resolve({}), text: () => Promise.resolve("") });
    }
    if (url === "/api/workflows") {
      return Promise.resolve({ ok: true, json: () => Promise.resolve({ enabled: opts.serverEnabled }) });
    }
    return Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
  };
  const src = `
    let revFlows = new Map(Object.entries(${JSON.stringify(flowsByRepo)}));
    let revQueue = ${JSON.stringify(opts.queue || null)};
    let revOpen = new Set();
    let revFlowsHeld = null;
    const opened = [];
    const openReview = id => opened.push(id);
    const toggleRevRow = key => { revOpen = new Set([key]); };
    const revClearStop = () => {};
    const toast = said => counts.toasts.push(said);
    const renderReview = () => { counts.painted++; };
    // A press paints now rather than waiting for the reader's hands (SKEIN-264); this panel counts
    // paints, and both kinds are one.
    const renderReviewNow = () => { counts.painted++; };
    const loadWorkflows = () => { counts.reloads++; };
    ${grab("esc")}
    ${grab("revAge")}
    ${grab("revTrainOpen")}
    ${grab("revFlowsOn")}
    ${grab("revTrainPaused")}
    ${grab("revTrainToggle")}
    ${grab("revTrainGo")}
    ${grab("revTrainLog")}
    ${grab("revTrainNumBtn")}
    ${grab("revTrainTitle")}
    ${grab("revTrainWhen")}
    ${grab("revTrainJournal")}
    ${grab("revTrainsFromStanding")}
    ${grab("revTrainLine")}
    ${grab("revTrainRepo")}
    ${grab("revTrainHtml")}
    // The other half of SKEIN-326: the same standing, on the pull request's own row. The panel and
    // the row read one payload, so they belong in one world — a held pull request that the panel
    // leaves out of the line and the row still describes as "waiting for something to change" is
    // the two surfaces disagreeing about the same fact.
    const revSetWorkflow = () => {};
    ${grab("revFlowBox")}
    ${grab("revFlowChip")}
    return {
      html: () => revTrainHtml(),
      flowBox: (repo, number) => revFlowBox({ repo_id: repo, number }),
      flowChip: (repo, number) => revFlowChip({ repo_id: repo, number }),
      toggle: on => revTrainToggle(on),
      go: (id, n) => revTrainGo(id, n),
      log: key => revTrainLog(key),
      openKeys: () => [...revOpen],
      opened,
    };
  `;
  const made = new Function("fetch", "counts", src)(fetch, counts);
  return { ...made, posts, counts };
}

// One repo, one serial train: #99 stopped and passed over, #101 at the front mid-flow, #104 behind.
const running = () => ({
  acme: {
    enabled: true,
    prs: {
      99: { workflow: "merge-train", how: "matched", next: "", step: 0, stopped: "CI failed on its head",
            journal: [
              { at_ms: Date.now() - 7200e3, flow: "merge-train", step: 2, kind: "did", what: "rebased onto trunk" },
              { at_ms: Date.now() - 3600e3, flow: "merge-train", step: 3, kind: "stopped", what: "CI failed on its head" },
              { at_ms: Date.now() - 60e3, flow: "", step: 0, kind: "cleared", what: "the stop was cleared — the workflow may act again" },
            ] },
      101: { workflow: "merge-train", how: "matched", next: "add-label:ci-queue", step: 5, stopped: "" },
      104: { workflow: "merge-train", how: "matched", next: "merge:squash+delete", step: 7, stopped: "" },
    },
    trains: [{ flow: "merge-train", front: 101, line: [99, 101, 104],
               stopped: [{ number: 99, why: "CI failed on its head" }] }],
  },
});
const queue = { prs: [
  { repo_id: "acme", number: 99, title: "teach the parser commas" },
  { repo_id: "acme", number: 101, title: "fix the flux capacitor" },
  { repo_id: "acme", number: 104, title: "rename the widget" },
] };

// ---- what it is working on NOW: the front, its title, its step spelled out ----
{
  const w = world(running(), { queue });
  const html = w.html();
  t.check("the panel says the train is running", html.includes("merge train — running"), true);
  t.check("with a pause button, not a resume one", html.includes(">pause<") && !html.includes(">resume<"), true);
  t.check("the front PR is named", html.includes("now:") && html.includes("#101"), true);
  t.check("with its title from the loaded queue", html.includes("fix the flux capacitor"), true);
  t.check("and its step spelled out from standing", html.includes("step 5") && html.includes("add-label:ci-queue"), true);
  t.check("the front car is marked in the line", html.includes(">front<"), true);
  // The line, in train order — the stopped #99 rides ahead of the front it was passed over by.
  // Asked of the cars, not the whole panel: the now-line names the front first, honestly.
  const cars = html.slice(html.indexOf("revtrain-car"));
  const at = n => cars.indexOf(`>#${n}<`);
  t.check("every PR in the line appears in train order",
    at(99) >= 0 && at(99) < at(101) && at(101) < at(104), true);
  t.check("a stopped PR carries its reason", html.includes("stopped — CI failed on its head"), true);
  t.check("and the existing clear affordance", html.includes("revClearStop(&quot;acme&quot;, 99)"), true);
  t.check("PRs behind the front show their own next step, abbreviated",
    html.includes(">merge<"), true);
}

// ---- a front with nothing applicable says so in words ----
{
  const flows = running();
  flows.acme.prs[101].next = "";
  flows.acme.prs[101].step = 0;
  const w = world(flows, { queue });
  t.check("an idle front reads as waiting, not as a blank",
    w.html().includes("waiting — nothing applies right now"), true);
}

// ---- paused is unmistakable: the words, the dimming class, the resume button ----
{
  const flows = running();
  flows.acme.enabled = false;
  const w = world(flows, { queue });
  const html = w.html();
  t.check("the state line says paused", html.includes("merge train — paused"), true);
  t.check("and says what that means in words", html.includes("paused — nothing will act"), true);
  t.check("the panel body wears the dimming class", html.includes('class="revtrain off"'), true);
  t.check("the one button now resumes", html.includes(">resume<") && !html.includes(">pause<"), true);
}

// ---- the pause button: POST the flipped switch, then paint the server's answer ----
{
  // The server will answer `enabled: true` no matter what is posted — a panel that trusts its own
  // click would show paused here, and that is exactly the lie this check exists to catch.
  const w = world(running(), { queue, serverEnabled: true });
  w.toggle(false);
  await new Promise(r => setTimeout(r, 0));
  t.check("pausing POSTs pr_workflows:false to /api/settings",
    w.posts[0], { url: "/api/settings", body: { pr_workflows: false } });
  t.check("the panel paints what GET /api/workflows answered, not the click",
    w.html().includes("merge train — running"), true);
  t.check("and the per-repo payloads are re-read to keep them agreeing", w.counts.reloads >= 1, true);
}
{
  const flows = running();
  flows.acme.enabled = false;
  const w = world(flows, { queue, serverEnabled: false });
  w.toggle(true);
  await new Promise(r => setTimeout(r, 0));
  t.check("resuming POSTs pr_workflows:true", w.posts[0], { url: "/api/settings", body: { pr_workflows: true } });
  t.check("a server that stayed paused keeps the panel paused",
    w.html().includes("merge train — paused"), true);
}

// ---- a number in the panel is one click into that PR's row in the queue ----
{
  const w = world(running(), { queue });
  t.check("the number is wired to the click", w.html().includes("revTrainGo(&quot;acme&quot;, 101)"), true);
  w.go("acme", 101);
  t.check("clicking it opens the repo's queue", w.opened, ["acme"]);
  t.check("with that PR's row expanded", w.openKeys(), ["acme#101"]);
}

// ---- previous steps: the journal, oldest first, each kind named ----
{
  const w = world(running(), { queue });
  w.log("acme#99");
  const html = w.html();
  // Ordering is asked of the log itself, not the whole panel — the stop's reason also rides on
  // the car line above it, and matching that copy would pass on the wrong evidence.
  const log = html.slice(html.indexOf("revtrain-log"));
  const at = s => log.indexOf(s);
  t.check("the journal renders oldest first",
    at("rebased onto trunk") >= 0
      && at("rebased onto trunk") < at("CI failed on its head")
      && at("CI failed on its head") < at("the stop was cleared"), true);
  t.check("did and stopped keep their kinds", at(">did<") >= 0 && at(">stopped<") >= 0, true);
  t.check("a cleared entry reads as a person's act", html.includes("cleared by hand"), true);
  t.check("each entry carries a relative time", html.includes('class="when"'), true);
  w.log("acme#99");
  t.check("the timeline folds back up", w.html().includes("rebased onto trunk"), false);
}

// ---- a PR the server never journaled says so, rather than showing nothing ----
{
  const w = world(running(), { queue });
  w.log("acme#101");
  t.check("no journal reads as its own absence", w.html().includes("no history recorded yet"), true);
}

// ---- an older server: no `trains`, no `journal` — standing alone, no crash ----
{
  const w = world({
    acme: {
      enabled: true,
      prs: {
        7: { workflow: "merge-train", how: "matched", next: "merge:squash+delete", step: 2, stopped: "" },
        9: { workflow: "merge-train", how: "matched", next: "", step: 0, stopped: "a conflict with trunk" },
      },
    },
  });
  const html = w.html();
  t.check("the panel still stands, from standing alone", html.includes("merge-train"), true);
  t.check("carrying PRs are listed lowest number first",
    html.indexOf(">#7<") >= 0 && html.indexOf(">#7<") < html.indexOf(">#9<"), true);
  t.check("a front the server never named is not invented", html.includes("now:"), false);
  t.check("stops still read red with their reason", html.includes("stopped — a conflict with trunk"), true);
  w.log("acme#7");
  t.check("expanding without a journal says so, and does not crash",
    w.html().includes("no history recorded yet"), true);
}

// ---- a HELD pull request is not a car (SKEIN-279/326) ----
//
// Somebody put a workflow on it by hand and the workflow's own `matches` are not true yet. It keeps
// `standing.workflow` on purpose — the row still has to say which workflow they chose — and is
// deliberately excluded from ACTING, so `prwork::sweep` skips it. Drawn as a car it promises an act
// the tick will never take; drawn as the FRONT, which is what the lowest number gets, it names the
// wrong pull request as the one being worked on. The comment above the route's own carrying list
// says it: "a panel that showed it in the line would promise an act the tick will never take."
{
  const w = world({
    acme: {
      enabled: true,
      prs: {
        // Lowest number, so under the old rule it would have been drawn first and read as the front.
        3: { workflow: "merge-train", how: "assigned", next: "", step: 0, stopped: "",
             holding: "merge-train is on this pull request and is not acting yet: its own condition is not true — approved. Nothing is stopped and nothing is waiting on a clock; it joins in on the next pass after that changes." },
        7: { workflow: "merge-train", how: "matched", next: "merge:squash+delete", step: 2, stopped: "" },
        // Carrying a workflow with nothing to do — the state the fallback sentence IS right for.
        9: { workflow: "merge-train", how: "matched", next: "", step: 0, stopped: "", holding: "" },
      },
      defined: [{ name: "merge-train" }],
    },
  });
  const html = w.html();
  t.check("a held pull request is left out of the line", html.includes(">#3<"), false);
  t.check("and the one that can act is still in it", html.includes(">#7<"), true);

  // The row's own half. It used to fall through to "waiting for something to change" — true, and
  // wrong in the way that matters: it reads as a clock running when none is, and it names nothing.
  // The owner is reading this pane as a DRY RUN with the train switched off, so this is the surface
  // where "put it on the train, it goes when it is approved" either says what it is waiting for or
  // does not.
  const held = w.flowBox("acme", 3);
  t.check("the row says which condition is not met yet", held.includes("approved"), true);
  t.check("and no longer implies a wait with nothing behind it",
    held.includes("waiting for something to change"), false);
  // Not a stop: nothing failed, and there is nothing to press.
  t.check("it is drawn as a held workflow, not a stopped one",
    [held.includes("revflow-holding"), held.includes("revflow-stop"), held.includes("let it run again")],
    [true, false, false]);
  // The chooser still shows the workflow somebody put on it — that is why `workflow` stays set.
  t.check("the workflow somebody chose is still the chosen one",
    /<option value="merge-train" selected>/.test(held), true);
  t.check("the collapsed row's chip says the same thing rather than 'nothing to do right now'",
    [w.flowChip("acme", 3).includes("not acting yet"),
     w.flowChip("acme", 3).includes("nothing to do right now")], [true, false]);
  // And a pull request that really has nothing to do keeps the sentence that was right for it.
  t.check("an idle workflow still reads as idle",
    w.flowBox("acme", 9).includes("waiting for something to change"), true);
}
{
  // …and a train whose every car is held is no train at all, rather than an empty frame with a
  // heading promising work.
  const w = world({
    acme: { enabled: true, prs: {
      3: { workflow: "merge-train", how: "assigned", next: "", step: 0, stopped: "", holding: "not acting yet: approved is not true" },
    } },
  });
  t.check("a train of nothing but held pull requests shows no line", w.html(), "");
}

// ---- nothing to show is no panel, not an empty frame ----
{
  const w = world({ acme: { enabled: true, prs: {} } });
  t.check("a fleet with no train shows no panel", w.html(), "");
}

t.done();
