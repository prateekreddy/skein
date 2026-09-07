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
  // Lifted with `decideSubq`, which closes over it — see the note in `gitgate.mjs`.
  "subqShown", "subqAnnounced", "subqPrimed", "decideSubq", "pollSubq", "paintSubqBadge", "subqCard",
].map(grab).join("\n");

const scope = new Function(`
  const sent = [], notes = [];
  let alertsOn = true;
  let checkbox = { checked: true };
  let panelOpen = false;
  let queue = [];
  ${grab("esc")}
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
      subqAnnounced.clear(); subqPrimed = false; subqShown.clear();
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

// --- a decision is about what was on screen -----------------------------------------------------
// `subqShown` is the same rule `gitgate.mjs` describes, and here the stakes are the plainer half:
// an approved apt or npm package runs maintainer and lifecycle scripts as root for the whole
// sandbox, and the package list is the box's own words in a file the box can rewrite. So the
// decision carries what was DISPLAYED, and a re-read is not a decision anybody made.
T.reset();
T.decideSubq("r1", true);
check("a request never rendered cannot be decided", T.sent().length, 0);

// --- what the decision carries ---------------------------------------------------------------
T.reset();
T.subqCard(ask("r1"));
T.decideSubq("r1", true);
check("approving sends an approval", T.sent()[0].body.approve, true);
check("and records it by default", T.sent()[0].body.remember, true);

T.reset();
T.subqCard(ask("r1"));
T.untick();
T.decideSubq("r1", true);
// The whole body, not just the two flags: what is sent is the decision AND what was on screen when
// it was made, so the host approves the packages a person read rather than whatever the box's file
// says by the time the request lands.
check("unticking approves without recording", T.sent()[0].body, { approve: true, remember: false, box: "web-main", kind: "apt", packages: ["libnss3"] });

// A denial must not record, and the checkbox is irrelevant to that — it is still ticked here.
T.reset();
T.subqCard(ask("r1"));
T.retick();
T.decideSubq("r1", false);
check("denying never records, whatever the box shows", T.sent()[0].body,
  { approve: false, remember: false, box: "web-main", kind: "apt", packages: ["libnss3"] });

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

// --- the card is drawn out of the box's own words, and it escapes them --------------------------
//
// Every field on this card is written by the thing being judged. The package list is "the box's own
// words in a file the box can rewrite" (above); `log` is whatever apt printed; `box` and `id` come
// off the same request. A box that wanted the cockpit to run something of its choosing would ask to
// install a package whose *name* is the payload — and the person reading the card is the fleet
// owner, in the tab that also holds a terminal.
//
// This suite stubbed `esc` as `s => String(s)` until SKEIN-531, so every assertion above described
// a page that escaped nothing. `${grab("esc")}` puts the page's own in the world now.
//
// Fails on: `esc` returning its argument, or losing any of `& < > " '`.
{
  const hostile = ask("r<1>", {
    packages: [`libnss3</div><script>alert(1)</script>`, `curl"&'`],
    box: `<img src=x onerror=alert(1)>`,
    log: `E: <b>bad</b> & "quoted" & 'quoted'`,
  });
  const card = T.subqCard(hostile);

  // Constructs, not payload strings: the payload is present either way — escaped it reads
  // `&lt;script&gt;` — so a pattern like /script/ would pass on the broken page too.
  check("a package name cannot open a tag", /<script/i.test(card), false);
  check("nor can the box name that asked for it", /<img/i.test(card), false);
  // `/<b>/` will not do here: the card writes a real `<b>` of its own around the box name, so that
  // pattern is satisfied by the template and says nothing about the payload. The payload's own tag
  // is what has to be absent.
  check("nor can apt's own output", card.includes("<b>bad</b>"), false);
  // The request id lands in an `id=` attribute and in `decideSubq`'s argument. A raw `<` there ends
  // the attribute value's element; a raw `"` ends the attribute.
  check("the id in the checkbox attribute cannot end the tag it is in",
    card.includes(`id="sq-rem-r&lt;1&gt;"`), true);

  // And the text is still shown — an absence check alone is satisfied by rendering nothing, and a
  // card that quietly drops a hostile package name is a worse bug than the one above. Written out
  // literally rather than computed with `esc`, because an expectation built by calling the function
  // under test tracks it however broken it gets. That is SKEIN-531 in one line.
  check("the packages are on screen as the characters the box typed",
    card.includes(`libnss3&lt;/div&gt;&lt;script&gt;alert(1)&lt;/script&gt; curl&quot;&amp;&#39;`), true);
  check("and so is apt's output",
    card.includes(`E: &lt;b&gt;bad&lt;/b&gt; &amp; &quot;quoted&quot; &amp; &#39;quoted&#39;`), true);
}

done();
