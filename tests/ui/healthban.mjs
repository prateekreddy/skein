// The health banner — the one row that interrupts the whole app — against the real `loadHealth`
// lifted out of index.html (SKEIN-1009).
//
// Nothing anywhere asserted anything about this row. `grep -rn healthban src/ tests/ cockpit/`
// matched seven lines, all seven in `src/web/index.html` itself: not that it appears when
// /api/health answers `ok: false`, not that it goes away when the next report is clean, not that
// its text is the failing check's own sentence, not that clicking it reaches the diagnostics pane.
//
// WHAT THIS COST. SKEIN-1003 was a banner that did not appear AT ALL for a fleet with no route to
// GitHub, and it was found by reading three lists against each other rather than by anything going
// red. The fix put three Rust tests behind `OnBanner`, `CHECKED` and the report's own fields — but
// every one of them reads source text. The step from "`ok` is false and the page's list names the
// key" to "a row a person can act on is on screen" was asserted by nothing, and this is that step.
//
// WHY THIS IS A NODE SUITE AND NOT A BROWSER ONE. The item said the banner "is drawn from a fetch
// inside `loadHealth`, so it is a browser-tier suite or nothing", and that is wrong: `loginban.mjs`
// has lifted `loadHealth` and driven it against a stubbed `fetch` since SKEIN-212. What a browser
// would add here is a real /api/health, which is the half that CANNOT be driven — the interesting
// reports are a fleet with a dead registry and a fleet that then recovers, and a suite cannot put
// the machine into either state. Every fixture below would have to be a `page.route` interception,
// at which point the browser is supplying nothing the stub does not and costs a chromium per run.
// The fetch is stubbed either way; this tier is honest about it.
//
// AND THE BANNER BLOCK WAS ALREADY UNREACHABLE FROM THE ONE WORLD THAT LIFTS IT. `loginban.mjs`
// evaluates `loadHealth` and calls it six times, so it looked as though this code was executing and
// having its output discarded. It was not: every fixture there answers `ok: true`, which returns
// before the row is built — and even with `ok: false` it could not have got there, because that
// world defined no `renderModelChoices` and the ReferenceError was swallowed by the handler's own
// `.catch`. Both are fixed in that file; this suite is what reads the result.
//
//   node tests/ui/healthban.mjs
import { grab, harness, stubDom } from "./lift.mjs";

const t = harness();

// The page's world, stubbed to what `loadHealth` and the row it draws actually touch. `health` is
// what /api/health answers NEXT — mutable, so a test can let a fleet recover between two polls,
// which is the transition the row's `remove()` exists for and has never had a reader.
//
// `renderLoginBanner` is LIFTED rather than stubbed, so the two rows are drawn by the same pass
// over the same report that the browser makes; `renderDiagnostics`, `render` and
// `renderModelChoices` are stubs because they draw other surfaces entirely, and `openSettings`
// records instead, since where the click goes is one of the contracts.
//
// `throws` names one of the four steps `loadHealth` runs before the banner, and that step throws
// an error carrying its own name (SKEIN-1012) — for `renderLoginBanner` that means a throwing stub
// in place of the lifted one. `console` is the world's own and records, so "the throw still
// surfaces" is a thing this suite can read rather than a thing it hopes; nothing else in the lifted
// code writes to it. `fetchFails` makes the poll's own fetch reject.
const STEPS = ["render", "renderDiagnostics", "renderLoginBanner", "renderModelChoices"];
function world({ throws = null, fetchFails = false } = {}) {
  const { reg, document, where } = stubDom();
  const state = { health: { ok: true }, opened: [], logged: [] };
  const fetch = url => {
    if (fetchFails) return Promise.reject(new Error("the server went away"));
    if (url === "/api/health") return Promise.resolve({ json: () => Promise.resolve(state.health) });
    return Promise.resolve({ json: () => Promise.resolve({}) });
  };
  const console = {
    error: (...args) => state.logged.push(["error", ...args]),
    warn: (...args) => state.logged.push(["warn", ...args]),
  };
  const stub = name => throws === name
    ? `const ${name} = () => { throw new Error("${name} fell over"); };`
    : `const ${name} = () => {};`;
  const src = `
    const DEMO = false;
    let boxes = [];
    ${stub("render")}
    ${stub("renderDiagnostics")}
    ${stub("renderModelChoices")}
    const openSettings = pane => state.opened.push(pane);
    ${grab("esc")}
    ${grab("lastHealth")}
    ${grab("loadHealth")}
    ${throws === "renderLoginBanner" ? stub("renderLoginBanner") : grab("renderLoginBanner")}
    return { loadHealth: () => loadHealth() };
  `;
  const made = new Function("fetch", "document", "state", "console", src)(fetch, document, state, console);
  return { ...made, reg, where, state, ban: () => reg.get("healthban") || null };
}

// Two promise hops sit between the fetch and the paint (r.json(), then the handler); a couple of
// macrotask turns flushes both. Local rather than shared with `loginban.mjs`: it is one line, and
// the number of turns is a fact about the function under test rather than about the harness.
const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

// A check as the report carries one. The detail is deliberately TWO sentences, because "the row
// says the first one" is the contract and a one-sentence fixture cannot tell that apart from "the
// row says the whole detail".
const bad = (what, fix) => ({ level: "unsatisfied", detail: `${what}. ${fix}`, fix });

// `counted`, as `HealthReport::counted_on_the_wire` sends it today: every check but `ai`, in the
// report's order (SKEIN-1013). A fixture, not a decision — the page reads this off the report and
// keeps no list of its own, and `the_report_tells_the_page_which_checks_count` in src/health.rs is
// what holds the real report to `OnBanner`. An unhealthy report without it is not one the server
// can send, so `unhealthy` is how every `ok: false` fixture below is written.
const COUNTED = ["registry", "sbx", "git", "gh", "probes", "mailbox", "memory", "disk", "gitgate",
  "token_expiry", "proxy_injection", "warden", "cover"];
const unhealthy = checks => ({ ok: false, counted: COUNTED, ...checks });

// --- an unsatisfied counted check puts a row above the app, saying what is wrong ----------------
//
// The row used to read `environment: registry, sbx` — two nouns, no verb, no consequence, with
// every actual detail in a `title` nobody can copy, read on a phone, or keep across a scroll. So
// what is asserted is that the row carries the failing check's own FIRST sentence: enough to act on
// without opening anything, and stopping before the rest of the paragraph.
{
  const w = world();
  w.state.health = unhealthy({ registry: bad("the registry is unreachable", "check the network") });
  w.loadHealth();
  await settle();
  const ban = w.ban();
  t.check("an unsatisfied counted check raises the row", !!ban, true);
  t.check("the row names the check and says its first sentence, and stops there",
    ban?.textContent, "registry: the registry is unreachable");
  // The row is a BUTTON prepended to the body, not a pill laid over it: floating at the top covered
  // the dock's tab bar, which blocked exactly the work the warning was interrupting. Body's first
  // child pushes the app down and leaves no hole when it goes.
  t.check("and it is a button placed above the app rather than over it",
    [ban?.tag, w.where], ["button", [["prepend", "healthban"]]]);
}

// --- several failures are COUNTED, not listed, so the row stays one readable sentence ------------
{
  const w = world();
  w.state.health = unhealthy({
    registry: bad("the registry is unreachable", "check the network"),
    sbx: bad("the sandbox runtime is missing", "install it"),
    disk: bad("the disk is nearly full", "clear build output"),
  });
  w.loadHealth();
  await settle();
  t.check("three failures give one sentence and a count of the rest",
    w.ban()?.textContent, "registry: the registry is unreachable (+2 more)");
  // All three are still reachable — the row is the headline, the tooltip is the list.
  t.check("while every one of them is in the tooltip",
    ["registry", "sbx", "disk"].map(k => (w.ban()?.title || "").includes(`${k}: `)),
    [true, true, true]);
}

// --- a later clean report TAKES THE ROW AWAY — the row is state, not history --------------------
//
// `banner?.remove()` on the recovery path, which is the half that had never been exercised by
// anything at all. A row that outlives the fault it reported is worse than no row: it is a red
// interruption that cannot be cleared by fixing anything, and the next red is read past too.
{
  const w = world();
  w.state.health = unhealthy({ registry: bad("the registry is unreachable", "check the network") });
  w.loadHealth();
  await settle();
  t.check("the row is up while the fleet is unhealthy", !!w.ban(), true);
  w.state.health = { ok: true };
  w.loadHealth();
  await settle();
  t.check("a clean report takes the row away", w.ban(), null);
}

// --- a check the banner does not COUNT cannot raise it, and is still NAMED once it is up ---------
//
// The asymmetry from SKEIN-1003/1004, and the subtle one. `src/health.rs` splits every check into
// `OnBanner::Counted` and `OnBanner::NotCounted(why)`; `ai` is the only `NotCounted` one, because it
// is the enrichment toggle's own state rather than a verdict. The page's `CHECKED` array carries it
// anyway, and the comment above that array says why in prose: a key missing from the page's list
// cannot suppress a row, it can only produce `ok: false` with nothing in the row to read, which is
// the one failure this list has ever had. So the page takes the VERDICT from the report and takes
// only the WORDS from its own list. Nothing tested either half.
{
  const w = world();
  // The report a `NotCounted` fault actually produces: `first_counted_fault` skips it, so `ok`
  // stays true however loud the check itself is.
  w.state.health = { ok: true, counted: COUNTED, ai: bad("enrichment is off", "turn it on in Settings") };
  w.loadHealth();
  await settle();
  t.check("a check the banner does not count cannot raise it on its own", w.ban(), null);
  // Non-vacuity, and it is what makes the absence above a fact about `NotCounted` rather than about
  // a world that never paints: the same world, one counted fault later, DOES raise the row.
  //
  // `disk` and not `registry` since SKEIN-1013, and that is the tightening: `registry` is ahead of
  // `ai` in the report's order, so it would have been the headline under the old `failed[0]` too
  // and this could not have told the two rules apart. `disk` is BEHIND `ai`, so the row below says
  // `disk` only if the headline is chosen by what counts rather than by position.
  w.state.health = unhealthy({
    ai: bad("enrichment is off", "turn it on in Settings"),
    disk: bad("the disk is nearly full", "clear build output"),
  });
  w.loadHealth();
  await settle();
  t.check("while a counted one does, which is what makes that a real absence", !!w.ban(), true);
  // The owner's rule, in his words "a check that raised it": the one sentence a reader can read
  // and copy is always the reason the banner is up.
  t.check("the headline is the counted fault, not the uncounted one ahead of it in the list",
    w.ban()?.textContent, "disk: the disk is nearly full (+1 more)");
  t.check("and once the row is up for a counted reason, the uncounted check is named on it too",
    (w.ban()?.title || "").includes("ai: enrichment is off"), true);
}

// --- a banner up for no failing counted check still counts the uncounted one ----------------------
//
// Stale sessions raise the banner and are not a check (`health_report`'s `ok`), so the headline is
// the environment line — and an uncounted failure on the same report cannot become the headline in
// its place, but it is not dropped either: it is in the count and the tooltip, as it is when a
// counted check leads (SKEIN-1013).
{
  const w = world();
  w.state.health = unhealthy({
    ai: bad("enrichment is off", "turn it on in Settings"),
    stale_boxes: ["example-two"],
  });
  w.loadHealth();
  await settle();
  t.check("an uncounted fault does not take the headline from the reason the banner is up",
    w.ban()?.textContent, "environment: probe updates · 1 stale (+1 more)");
  t.check("and it is still in the tooltip",
    (w.ban()?.title || "").includes("ai: enrichment is off"), true);
}

// --- NOTHING DRAWN BEFORE THE BANNER CAN CANCEL IT -----------------------------------------------
//
// SKEIN-1012, the owner's call: "banner can't be cancelled". `loadHealth` runs four other drawing
// steps before it builds the row, and they used to share the handler's one `.catch(() => {})`, so
// a throw in any of them skipped the row — silently, on a report that already said `ok: false`.
// Measured before it was fixed, not argued: this suite's own world once lacked
// `renderModelChoices`, and the row was never built. Each of the four is made to throw in turn, on
// a report with a counted fault, and two things must hold: the row is there, saying what is wrong,
// and the throw reached the console under the step's own name rather than vanishing.
for (const name of STEPS) {
  const w = world({ throws: name });
  w.state.health = unhealthy({ registry: bad("the registry is unreachable", "check the network") });
  w.loadHealth();
  await settle();
  t.check(`a throw in ${name} does not cost the banner`,
    w.ban()?.textContent, "registry: the registry is unreachable");
  t.check(`and the throw from ${name} reaches the console, not a swallow`,
    w.state.logged.some(([how, said, e]) =>
      how === "error" && String(said).includes(name) && e?.message === `${name} fell over`),
    true);
}

// Non-vacuity for the console half: a world where nothing throws logs nothing, so the `some` above
// is reading this change's lines and not something the page always prints.
{
  const w = world();
  w.state.health = unhealthy({ registry: bad("the registry is unreachable", "check the network") });
  w.loadHealth();
  await settle();
  t.check("a poll where nothing throws says nothing to the console", w.state.logged, []);
}

// And the poll's own failure is said rather than swallowed: what the terminal `.catch` receives now
// is the fetch, the parse, or the banner block itself.
{
  const w = world({ fetchFails: true });
  w.loadHealth();
  await settle();
  t.check("a failed health poll is reported to the console",
    w.state.logged.some(([how, , e]) => how === "warn" && e?.message === "the server went away"), true);
}

// --- NOT KNOWING is not BEING BROKEN -------------------------------------------------------------
//
// `HealthCheck::is_fault` declines `Unknown` on the Rust side, for the reason the third state
// exists: telling somebody their fleet is broken because skein could not reach it for two seconds
// is a false alarm. The page has to make the same distinction on the same report — report it, do
// not count it — and a row that counted unknowns would inflate its own "(+N more)" with things
// nobody has established are wrong.
{
  const w = world();
  w.state.health = unhealthy({
    registry: bad("the registry is unreachable", "check the network"),
    probes: { level: "unknown", detail: "the probe timed out" },
  });
  w.loadHealth();
  await settle();
  t.check("an unanswered check is reported in the tooltip, in those words",
    (w.ban()?.title || "").includes("probes: could not be checked — the probe timed out"), true);
  t.check("and is not counted as a fault, so it does not inflate the headline",
    w.ban()?.textContent, "registry: the registry is unreachable");
}

// --- `ok: false` that the page cannot NAME still says something a reader can act on --------------
//
// The exact failure the `CHECKED` array has had once already: a fault the page's list did not name
// put a red row above the app with nothing in it to read. The list is not a selection any more, so
// that cannot happen through a missing key — but `ok` can also go false for a fleet whose boxes
// have gone dark, which no check in the array reports. The row must not be blank.
{
  const w = world();
  w.state.health = unhealthy({ dark_boxes: ["example-one"], stale_boxes: ["example-two"] });
  w.loadHealth();
  await settle();
  t.check("a fault with no named check still leaves a readable row",
    w.ban()?.textContent, "environment: probe updates · 1 dark · 1 stale");
}

// --- the row is the way to the diagnostics pane ---------------------------------------------------
//
// It is a button, and the only thing pressing it can usefully do is open the pane that lists every
// check with its detail and its fix — which is exactly what SKEIN-1003's fleet had nothing sending
// anybody to.
{
  const w = world();
  w.state.health = unhealthy({ registry: bad("the registry is unreachable", "check the network") });
  w.loadHealth();
  await settle();
  w.ban()?.onclick();
  t.check("clicking the row opens the diagnostics pane and no other", w.state.opened, ["diag"]);
}

t.done();
