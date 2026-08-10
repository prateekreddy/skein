// Do your open tabs survive a reload — including a reload that lands while the fleet is slow?
//
// They did not, and the way they failed lost them for good rather than for one page load.
// `restoreSessions` ran on the FIRST fleet snapshot only, and filtered the saved tabs against the
// boxes in it. But the server serves the last good fleet list while `sbx ls` is slow or failing, so
// a reload during a stall got an empty snapshot, read it as "none of those boxes exist any more",
// dropped every tab, and never looked again — and the next `persistView` wrote `open: []` over the
// only record that those tabs had ever existed.
//
// This is exactly the class of bug `smoke.mjs` would catch and cannot: it needs chromium, which a
// box cannot install. The restore logic is a pure function of (saved state, snapshot), so it runs
// here in plain node instead, where the fix is being written.
//
//   node tests/ui/tabs.mjs
import { grab, harness } from "./lift.mjs";

const source = [
  "persistView", "restored", "pendingRestore", "pendingView", "restoreSessions", "openPending",
].map(grab).join("\n");

// The page's world, stubbed down to what the restore path actually touches.
const scope = new Function(`
  const store = {};
  const localStorage = {
    getItem: k => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = v; },
  };
  let boxes = [];
  const sessions = new Map();
  const sidOf = (box, kind) => box + "/" + kind;
  const orderedSessions = () => [...sessions.values()];
  const opened = [], shown = [];
  const createSession = (box, kind, _sock, runtime) => {
    opened.push(sidOf(box, kind));
    sessions.set(sidOf(box, kind), { box, kind, runtime });
  };
  let view = { box: null, mode: "term", kind: "agent" };
  const showBox = (box, mode, kind) => { shown.push(box + ":" + mode); view = { box, mode, kind }; };
  const applyView = () => {};
  ${source}
  return {
    persistView, restoreSessions, openPending,
    // A snapshot arriving: the fleet list the page would have rendered.
    snapshot: names => { boxes = names.map(name => ({ name })); },
    seed: v => { store["skein.view"] = JSON.stringify(v); },
    saved: () => JSON.parse(store["skein.view"] || "null"),
    openTabs: () => orderedSessions().map(s => s.box + "/" + s.kind),
    activeBox: () => view.box,
    reset: () => {
      for (const k in store) delete store[k];
      boxes = []; sessions.clear(); opened.length = 0; shown.length = 0;
      restored = false; pendingRestore = []; pendingView = null;
      view = { box: null, mode: "term", kind: "agent" };
    },
  };
`);

const { check, done } = harness();
const T = scope();
const tab = (box, kind = "agent") => ({ box, kind, runtime: null });

// --- the ordinary case ---------------------------------------------------------------------------
T.reset();
T.seed({ open: [tab("web-main"), tab("api", "shell")], box: "api", mode: "term", kind: "shell" });
T.snapshot(["web-main", "api"]);
T.restoreSessions();
check("reopens what was open, in the saved order", T.openTabs(), ["web-main/agent", "api/shell"]);
check("and returns you to the tab you were on", T.activeBox(), "api");

// --- the regression ------------------------------------------------------------------------------
// A reload while `sbx ls` is stalling: the first snapshot carries no fleet at all.
T.reset();
T.seed({ open: [tab("web-main"), tab("api")], box: "web-main", mode: "term", kind: "agent" });
T.snapshot([]);
T.restoreSessions();
check("opens nothing it cannot account for", T.openTabs(), []);
// THE bug. Something persists between snapshots — any tab render does — and this used to write an
// empty list over the saved one, so the tabs were gone for good rather than merely late.
T.persistView();
check(
  "and an empty fleet does not erase what is waiting to be reopened",
  T.saved().open.map(o => o.box),
  ["web-main", "api"],
);
check("nor the view you were on", T.saved().box, "web-main");
// The fleet turns up a moment later, as it does.
T.snapshot(["web-main", "api"]);
T.openPending();
check("and they come back when the fleet reports", T.openTabs(), ["web-main/agent", "api/agent"]);
check("landing you back where you were", T.activeBox(), "web-main");

// --- a box that is genuinely gone ------------------------------------------------------------------
// The distinction the old code could not draw: an empty snapshot is not evidence, a populated one is.
T.reset();
T.seed({ open: [tab("web-main"), tab("deleted-box")], box: "web-main", mode: "term", kind: "agent" });
T.snapshot(["web-main"]);
T.restoreSessions();
check("a box missing from a fleet that DID report is dropped", T.openTabs(), ["web-main/agent"]);
T.persistView();
check("and stops being remembered", T.saved().open.map(o => o.box), ["web-main"]);

// --- not reopened twice ----------------------------------------------------------------------------
T.reset();
T.seed({ open: [tab("web-main")], box: null });
T.snapshot(["web-main"]);
T.restoreSessions();
T.openPending();
T.openPending();
check("later snapshots do not reopen a tab that is already open", T.openTabs(), ["web-main/agent"]);

// --- the old on-disk format ------------------------------------------------------------------------
T.reset();
T.seed({ open: ["web-main", "api"], box: "api" });
T.snapshot(["web-main", "api"]);
T.restoreSessions();
check("a bare list of names still restores", T.openTabs(), ["web-main/agent", "api/agent"]);

// --- nothing saved ---------------------------------------------------------------------------------
T.reset();
T.snapshot(["web-main"]);
T.restoreSessions();
check("a first visit opens nothing and does not throw", T.openTabs(), []);

done();
