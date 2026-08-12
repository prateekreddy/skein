// Approving a package installs it under every box in the fleet, so the decision the cockpit sends
// has to be exactly the one its owner made — and it has to say so only once.
//
// Two things are worth testing here and neither needs a browser:
//
//   1. **What the button sends.** "Remember" is on by default, and unticking it must mean "install
//      it now, but a rebuilt sandbox comes back without it". A denial must never record anything,
//      whatever the checkbox happens to be showing at the time.
//   2. **How often it announces.** A request sits pending until a person answers it, which is
//      precisely the shape that produced the endless re-announcing this cockpit was fixed for once
//      already. It must be announced once and then be quiet, however long it waits.
//
//   node tests/ui/substrate.mjs
import { grab, harness } from "./lift.mjs";

const source = [
  "subqAnnounced", "subqPrimed", "decideSubq", "pollSubq", "paintSubqBadge", "subqCard",
].map(grab).join("\n");

const scope = new Function(`
  const sent = [], notes = [];
  let alertsOn = true;
  let checkbox = { checked: true };
  let panelOpen = false;
  let queue = [];
  const esc = s => String(s);
  const toast = m => notes.push("toast:" + m);
  const pushNote = (body, tag) => notes.push({ body, tag });
  const badge = { n: null, title: "" };
  const document = {
    getElementById: id => {
      if (id === "subq") return { classList: { contains: () => panelOpen } };
      if (id.startsWith("sq-rem-")) return checkbox;
      // Modelled with a real child, not a stub that always answers null: clearing the badge works
      // by finding the old one and removing it, so a querySelector that never finds anything would
      // make "the badge went away" untestable — and it did, on the first run of this file.
      if (id === "subqbtn") return {
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
    return Promise.resolve({ ok: true, json: () => Promise.resolve(queue) });
  };
  const loadSubq = () => { notes.push("reloaded"); };
  ${source}
  return {
    decideSubq, pollSubq, subqCard,
    sent: () => sent,
    notes: () => notes,
    badge: () => badge,
    setQueue: q => { queue = q; },
    setAlerts: v => { alertsOn = v; },
    tick: () => { checkbox.checked = false; },
    untick: () => { checkbox.checked = false; },
    retick: () => { checkbox.checked = true; },
    reset: () => {
      sent.length = 0; notes.length = 0; queue = [];
      subqAnnounced.clear(); subqPrimed = false;
      checkbox = { checked: true }; alertsOn = true;
    },
  };
`);

const { check, done } = harness();
const T = scope();
const ask = (id, over = {}) => ({
  id, box: "web-main", kind: "apt", packages: ["libnss3"], asked: "2026-08-12T09:00:00Z",
  state: "pending", remember: true, log: "", ...over,
});

// --- what the decision carries ---------------------------------------------------------------
T.reset();
T.decideSubq("r1", true);
check("approving sends an approval", T.sent()[0].body.approve, true);
check("and records it by default", T.sent()[0].body.remember, true);

T.reset();
T.untick();
T.decideSubq("r1", true);
check("unticking approves without recording", T.sent()[0].body, { approve: true, remember: false });

// A denial must not record, and the checkbox is irrelevant to that — it is still ticked here.
T.reset();
T.retick();
T.decideSubq("r1", false);
check("denying never records, whatever the box shows", T.sent()[0].body, { approve: false, remember: false });

// --- announced once, however long it waits -----------------------------------------------------
T.reset();
T.setQueue([ask("a")]);
await T.pollSubq();
check("the first poll seeds rather than announcing", T.notes().filter(n => n.body).length, 0);

await T.pollSubq();
await T.pollSubq();
check("a request already seen is never re-announced", T.notes().filter(n => n.body).length, 0);

// A genuinely new ask does get through.
T.setQueue([ask("a"), ask("b", { box: "api" })]);
await T.pollSubq();
const said = T.notes().filter(n => n.body);
check("but a new request is announced once", said.length, 1);
check("naming the box that asked", said[0].tag, "api");

await T.pollSubq();
check("and then goes quiet too", T.notes().filter(n => n.body).length, 1);

// --- alerts off means silent, not merely quieter -----------------------------------------------
T.reset();
T.setQueue([ask("x")]);
await T.pollSubq();          // seed
T.setAlerts(false);
T.setQueue([ask("x"), ask("y")]);
await T.pollSubq();
check("alerts off says nothing at all", T.notes().filter(n => n.body).length, 0);

// --- the badge counts what needs a person ------------------------------------------------------
T.reset();
T.setQueue([ask("a"), ask("b"), ask("c", { state: "installed" }), ask("d", { state: "denied" })]);
await T.pollSubq();
check("the badge counts only what is still pending", T.badge().n, "2");

T.reset();
T.setQueue([ask("c", { state: "installed" })]);
await T.pollSubq();
check("nothing pending clears the badge", T.badge().n, null);

// --- the card offers a decision only while there is one to make --------------------------------
check("a pending request offers approve and deny", /Approve/.test(T.subqCard(ask("a"))), true);
check("a decided one does not", /Approve/.test(T.subqCard(ask("a", { state: "installed" }))), false);
check(
  "a failure shows what apt actually said",
  /No space left/.test(T.subqCard(ask("a", { state: "failed", log: "E: No space left on device" }))),
  true,
);

done();
