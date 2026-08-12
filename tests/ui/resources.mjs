// A box's resources on hover — and specifically, what it shows before the sandbox has answered.
//
// The card has two halves with very different costs. Disk rides on `/api/boxes`, which the board
// already polls, so it is free and instant. CPU and memory come from `/api/fleet/load`, which execs
// into the sandbox and samples cgroups over half a second — far too expensive to do per hover, and
// slow enough that the card must be useful without it. So the card goes up immediately with disk
// filled in and the live figures pending, then fills them in when they arrive.
//
// That makes three things worth pinning: the card is never empty, the fetch is shared and cached
// rather than fired per hover, and the disk figure colours itself against the box's allowance.
//
//   node tests/ui/resources.mjs
import { grab, harness } from "./lift.mjs";

const source = ["loadCache", "loadRows", "resourceRows"].map(grab).join("\n");

const scope = new Function(`
  let boxes = [];
  let fetches = 0;
  let now = 1000000;
  const Date = { now: () => now };
  const esc = s => String(s);
  const fmtGb = mb => mb >= 1024 ? (mb/1024).toFixed(1) + "G" : mb + "M";
  const fmtGB = b => b >= 1073741824 ? (b/1073741824).toFixed(1) + "G" : Math.round(b/1048576) + "M";
  let served = [];
  const fetch = () => { fetches++; return Promise.resolve({ ok: true, json: () => Promise.resolve(served) }); };
  ${source}
  return {
    loadRows, resourceRows,
    setBoxes: b => { boxes = b; },
    serve: rows => { served = rows; },
    fetches: () => fetches,
    advance: ms => { now += ms; },
    reset: () => {
      boxes = []; served = []; fetches = 0; now = 1000000;
      loadCache = { at: 0, rows: [], inflight: null };
    },
  };
`);

const { check, done } = harness();
const T = scope();

// --- the card is useful before the sandbox answers ---------------------------------------------
T.reset();
T.setBoxes([{ name: "web-main", disk_mb: 2048, disk_limit_mb: 10240 }]);
let card = T.resourceRows("web-main");
check("disk is there with no round trip at all", /2\.0G \/ 10\.0G/.test(card), true);
check("and the live figures show as pending, not as zero", (card.match(/…/g) || []).length, 3);
check("the fetch has not even been made yet", T.fetches(), 0);

// --- and fills in once it has --------------------------------------------------------------------
T.serve([{ name: "web-main", cores: 2.34, mem: 3221225472, pids: 31 }]);
await T.loadRows();
card = T.resourceRows("web-main");
check("memory arrives", /3\.0G/.test(card), true);
check("cpu arrives, to one decimal", /2\.3 cores/.test(card), true);
check("and processes", /31/.test(card), true);

// --- the fetch is shared, not one per hover ------------------------------------------------------
T.reset();
T.setBoxes([{ name: "a", disk_mb: 1 }]);
T.serve([{ name: "a", cores: 1, mem: 1048576, pids: 2 }]);
await Promise.all([T.loadRows(), T.loadRows(), T.loadRows()]);
check("three hovers at once share one request", T.fetches(), 1);
await T.loadRows();
check("and a fourth within the window reuses it", T.fetches(), 1);
T.advance(16000);
await T.loadRows();
check("but it does go stale, so the numbers stay live", T.fetches(), 2);

// --- disk colours itself against this box's allowance ---------------------------------------------
T.reset();
const at = (used, cap) => {
  T.setBoxes([{ name: "b", disk_mb: used, disk_limit_mb: cap }]);
  const html = T.resourceRows("b");
  return /rp-r over/.test(html) ? "over" : /rp-r near/.test(html) ? "near" : "plain";
};
check("well under its share is not coloured", at(1024, 10240), "plain");
check("at four fifths it warns", at(8192, 10240), "near");
check("over its share it is loud", at(11000, 10240), "over");

// A box with no allowance still shows what it is using — the number is the point, the cap is not.
T.setBoxes([{ name: "b", disk_mb: 5120 }]);
card = T.resourceRows("b");
check("no allowance still shows usage", /5\.0G/.test(card), true);
check("and does not colour it", /rp-r over|rp-r near/.test(card), false);

// --- a box the fleet has never reported ------------------------------------------------------------
T.reset();
T.setBoxes([]);
card = T.resourceRows("ghost");
check("an unknown box renders rather than throwing", /ghost/.test(card), true);
check("with disk unknown rather than zero", /<span>—<\/span>/.test(card), true);

done();
