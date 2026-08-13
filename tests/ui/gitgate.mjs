// Granting write access hands a box a real credential for someone else's repository, so what the
// button sends has to be exactly what its owner chose — and it has to say so only once.
//
// Three things are worth testing and none needs a browser:
//
//   1. **What the grant carries.** The default expires; "keep indefinitely" is the deliberate act.
//      The two controls must not be able to disagree — a ticked keep with `12` still in the hours
//      box has to mean one thing, not two. A denial must never carry a duration that could be read
//      as an approval by a later change to the endpoint.
//   2. **How often it announces.** A request sits pending until a person answers it, which is the
//      exact shape that produced endless re-announcing once already.
//   3. **What an unconfigured fleet says.** With no GitHub App, nothing is scoped and no box has
//      lost anything — an empty panel must say which of those it is showing.
//
//   node tests/ui/gitgate.mjs
import { grab, harness } from "./lift.mjs";

const source = [
  "gitqAnnounced", "gitqPrimed", "decideGitq", "pollGitq", "paintGitqBadge", "gitqCard",
  "gitqGrantRow", "revokeGitq",
].map(grab).join("\n");

const scope = new Function(`
  const sent = [], notes = [];
  let alertsOn = true;
  let keep = { checked: false };
  let hours = { value: "24" };
  let panelOpen = false;
  let payload = { requests: [], grants: [], app_ready: true, app_problem: "" };
  const esc = s => String(s);
  const toast = m => notes.push("toast:" + m);
  const pushNote = (body, tag) => notes.push({ body, tag });
  const badge = { n: null, title: "" };
  const document = {
    getElementById: id => {
      if (id === "gitq") return { classList: { contains: () => panelOpen } };
      if (id.startsWith("gq-keep-")) return keep;
      if (id.startsWith("gq-h-")) return hours;
      if (id === "gitqbtn") return {
        querySelector: () => (badge.n === null ? null : { remove: () => { badge.n = null; } }),
        appendChild: el => { badge.n = el.textContent; },
        set title(v) { badge.title = v; },
        get title() { return badge.title; },
      };
      return null;
    },
    createElement: () => ({ className: "", textContent: "" }),
  };
  const fetch = (url, opts) => {
    if (opts && opts.method === "POST") {
      sent.push({ id: decodeURIComponent(url.split("/").pop()), body: JSON.parse(opts.body) });
      return Promise.resolve({ ok: true });
    }
    if (opts && opts.method === "DELETE") {
      sent.push({ deleted: url });
      return Promise.resolve({ ok: true });
    }
    return Promise.resolve({ ok: true, json: () => Promise.resolve(payload) });
  };
  const loadGitq = () => { notes.push("reloaded"); };
  ${source}
  return {
    decideGitq, pollGitq, gitqCard, gitqGrantRow, revokeGitq,
    sent: () => sent,
    notes: () => notes,
    badge: () => badge,
    setPayload: p => { payload = p; },
    setKeep: v => { keep.checked = v; },
    setHours: v => { hours.value = v; },
    reset: () => {
      sent.length = 0; notes.length = 0;
      payload = { requests: [], grants: [], app_ready: true, app_problem: "" };
      gitqAnnounced.clear(); gitqPrimed = false;
      keep = { checked: false }; hours = { value: "24" }; alertsOn = true;
    },
  };
`);

const { check, done } = harness();
const T = scope();
const ask = (id, over = {}) => ({
  id, box: "web-main", repo: "acme/thing", reason: "fix the shared type",
  asked: "2026-08-13T09:00:00Z", state: "pending", decided: "", ...over,
});

// --- what the grant carries --------------------------------------------------------------------
T.reset();
T.decideGitq("r1", true);
check("granting sends an approval", T.sent()[0].body.approve, true);
check("and expires by default", T.sent()[0].body.hours, 24);

T.reset();
T.setHours("2");
T.decideGitq("r1", true);
check("a shorter window is carried through", T.sent()[0].body.hours, 2);

// Ticked keep wins over whatever the number says, so the two controls cannot mean two things.
T.reset();
T.setHours("12");
T.setKeep(true);
T.decideGitq("r1", true);
check("keeping indefinitely sends 0, not the leftover number", T.sent()[0].body.hours, 0);

// A number nobody can honour must not become a grant that never expires — 0 means forever here.
T.reset();
T.setHours("nonsense");
T.decideGitq("r1", true);
check("an unusable duration falls back to the default", T.sent()[0].body.hours, 24);
T.reset();
T.setHours("0");
T.decideGitq("r1", true);
check("and zero typed into the box is not silently forever", T.sent()[0].body.hours, 24);

T.reset();
T.setKeep(true);
T.decideGitq("r1", false);
check("denying is a denial", T.sent()[0].body.approve, false);

// --- revoking ----------------------------------------------------------------------------------
T.reset();
T.revokeGitq("web-main", "acme/thing");
check(
  "a revoke encodes the repo's slash so it stays one path segment",
  T.sent()[0].deleted.endsWith("/web-main/acme%2Fthing"),
  true,
);

// --- announced once, however long it waits -----------------------------------------------------
T.reset();
T.setPayload({ requests: [ask("a")], grants: [], app_ready: true });
await T.pollGitq();
check("the first poll seeds rather than announcing", T.notes().filter(n => n.body).length, 0);
check("but the badge is painted straight away", T.badge().n, "1");

await T.pollGitq();
await T.pollGitq();
check("a request already seen is never re-announced", T.notes().filter(n => n.body).length, 0);

T.setPayload({ requests: [ask("a"), ask("b", { box: "api" })], grants: [], app_ready: true });
await T.pollGitq();
const said = T.notes().filter(n => n.body);
check("a new request is announced once", said.length, 1);
check("naming the box that asked", said[0].tag, "api");
check("and saying which repo it wants", said[0].body.includes("acme/thing"), true);

await T.pollGitq();
check("and then goes quiet", T.notes().filter(n => n.body).length, 1);

// --- the badge clears when the last request is answered -----------------------------------------
T.setPayload({ requests: [ask("a", { state: "granted" }), ask("b", { state: "denied" })], grants: [] });
await T.pollGitq();
check("an answered queue leaves no badge", T.badge().n, null);

// --- what the card actually shows ---------------------------------------------------------------
const card = T.gitqCard(ask("a"));
check("the card names the repository", card.includes("acme/thing"), true);
check("and the box that asked", card.includes("web-main"), true);
check("and the reason, which is the whole basis for deciding", card.includes("fix the shared type"), true);

// A decided request offers no buttons — approving twice is not a thing that should be possible.
const decided = T.gitqCard(ask("a", { state: "granted" }));
check("a granted request has no Grant button left", decided.includes("Grant write"), false);

// --- grants list --------------------------------------------------------------------------------
const live = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "2026-08-14T00:00:00Z", live: true });
check("a live grant can be revoked from the list", live.includes("Revoke"), true);
const dead = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "2026-08-12T00:00:00Z", live: false });
check("an expired one is shown", dead.includes("expired"), true);
check("but cannot be revoked twice", dead.includes("Revoke"), false);
const forever = T.gitqGrantRow({ box: "web-main", repo: "o/r", expires: "", live: true });
check("a permanent grant says so rather than showing a blank date", forever.includes("no expiry"), true);

done();
