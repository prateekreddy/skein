// The login-expired banner and its one-click repair terminal (SKEIN-212, the UI half).
//
// The owner's ask, verbatim: "there should be a popup that this happened and ask me to login.
// There should be an easy way to do it. I just click something, a session opens I login there and
// then it closes and everything else starts working."
//
// Four contracts, each against the real functions lifted out of index.html:
//   * a health report carrying `expired_logins` puts a ROW above the app that names the runtime;
//   * a report without any takes it away — the banner is state, not history;
//   * the "log in" click opens a socket onto exactly GET /api/login/:runtime/terminal;
//   * the socket CLOSING is the end of the flow: the surface goes, /api/health is re-read once,
//     the banner repaints from that answer, and the server's last sentence becomes the toast.
//
//   node tests/ui/loginban.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// A stub element deep enough for what the banner and the overlay touch: id, innerHTML, classList,
// remove. `reg` is the document's registry, so remove() and prepend() are observable.
function makeEl(id, reg) {
  const classes = new Set();
  const e = {
    id, innerHTML: "", textContent: "", title: "", onclick: null,
    classList: {
      add: c => classes.add(c), remove: c => classes.delete(c),
      contains: c => classes.has(c), toggle: (c, on) => (on ? classes.add(c) : classes.delete(c)),
    },
  };
  e.remove = () => reg.delete(e.id);
  return e;
}

// The page's world, stubbed down to what these functions touch. `health` is what /api/health will
// answer NEXT — mutable, so a test can flip a login live between two polls, exactly as the server
// would after a successful login.
function world() {
  const reg = new Map();
  for (const id of ["loginterm", "lt-host"]) reg.set(id, makeEl(id, reg));
  const document = {
    getElementById: id => reg.get(id) || null,
    createElement: () => makeEl("", reg),
    body: { prepend: e => { if (e.id) reg.set(e.id, e); } },
  };
  const state = { health: { ok: true, expired_logins: [] }, healthCalls: 0, toasts: [] };
  const fetch = url => {
    if (url === "/api/health") { state.healthCalls++; return Promise.resolve({ json: () => Promise.resolve(state.health) }); }
    return Promise.resolve({ json: () => Promise.resolve({}) });
  };
  const sockets = [];
  class WebSocket {
    constructor(url) { this.url = url; this.readyState = 1; this.sent = []; sockets.push(this); }
    send(d) { this.sent.push(d); }
    close() { this.readyState = 3; this.onclose && this.onclose(); }
  }
  class Terminal {
    constructor() { this.cols = 80; this.rows = 24; this.written = []; }
    loadAddon() {} open() {} write(d) { this.written.push(d); } dispose() {} focus() {}
    onData() {} onResize() {}
  }
  const FitAddon = { FitAddon: class { fit() {} } };
  const location = { protocol: "http:", host: "cockpit.test" };
  const requestAnimationFrame = f => f();
  const src = `
    const DEMO = false;
    let boxes = [];
    const render = () => {};
    const renderDiagnostics = () => {};
    const openSettings = () => {};
    const toast = said => state.toasts.push(said);
    ${grab("esc")}
    ${grab("lastHealth")}
    ${grab("loadHealth")}
    ${grab("renderLoginBanner")}
    // The bar no longer carries agent-CLI updates - they moved to Settings -> Update (SKEIN-486).
    // These two are the pure halves of that pane: one decides the verdict, one draws the CLI row.
    ${grab("updateVerdict")}
    ${grab("renderRuntimeUpdates")}
    ${grab("pressUpdateAgents")}
    ${grab("loginTerminalUrl")}
    ${grab("loginTermState")}
    ${grab("openLoginTerminal")}
    ${grab("closeLoginTerminal")}
    return {
      loadHealth: () => loadHealth(),
      openLoginTerminal: r => openLoginTerminal(r),
      closeLoginTerminal: () => closeLoginTerminal(),
      loginTerminalUrl: r => loginTerminalUrl(r),
      updateVerdict: u => updateVerdict(u),
      renderRuntimeUpdates: u => renderRuntimeUpdates(u),
      pressUpdateAgents: b => pressUpdateAgents(b),
    };
  `;
  const made = new Function(
    "fetch", "document", "Terminal", "FitAddon", "WebSocket", "location", "requestAnimationFrame", "state",
    src,
  )(fetch, document, Terminal, FitAddon, WebSocket, location, requestAnimationFrame, state);
  return { ...made, reg, sockets, state };
}

// Two promise hops sit between fetch and the paint (r.json(), then the handler); a couple of
// macrotask turns flushes both.
const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

// --- an expired login is a row that names the runtime, and WHICH witness said so ------------------
//
// Two witnesses since SKEIN-290, and they can disagree. `witness: "credential"` is the credential
// file's own expiry date — knowable with nothing running. `witness: "refusal"` is a model call that
// came back refused, which is a fact about a MOMENT and the only one of the two that can be wrong
// about right now. Rendered as one sentence, a person cannot tell a credential that is dead from one
// that was dead a moment ago — and those lead to different actions.
{
  const w = world();
  w.state.health = { ok: true, expired_logins: [
    { runtime: "claude", expired_at: "2026-08-22T10:00:00Z", witness: "credential" }] };
  w.loadHealth();
  await settle();
  const ban = w.reg.get("loginban");
  t.check("an expired login raises the banner row", !!ban, true);
  t.check("the credential's own expiry names the runtime, the date and the consequence",
    !!ban && ban.innerHTML.includes("the fleet's claude login expired 2026-08-22T10:00:00Z — summaries, critiques and workflows are declining model calls"),
    true);
  t.check("the banner offers the one click", !!ban && ban.innerHTML.includes(">log in<"), true);
}
{
  const w = world();
  w.state.health = { ok: true, expired_logins: [
    { runtime: "claude", expired_at: "2026-08-22T10:00:00Z", witness: "refusal",
      said: "invalid API key · run `claude login`" }] };
  w.loadHealth();
  await settle();
  const ban = w.reg.get("loginban");
  t.check("a refusal says it was refused, and when",
    !!ban && ban.innerHTML.includes("was refused at 2026-08-22T10:00:00Z"), true);
  t.check("and carries the words the runtime used, escaped",
    !!ban && ban.innerHTML.includes("invalid API key · run `claude login`"), true);
  t.check("the two witnesses do not read the same",
    !!ban && ban.innerHTML.includes("login expired 2026-08-22"), false);
}

// --- no expired logins, no banner — including after there was one -------------------------------
{
  const w = world();
  w.state.health = { ok: true, expired_logins: [{ runtime: "codex", expired_at: "2026-08-22T10:00:00Z" }] };
  w.loadHealth();
  await settle();
  t.check("the banner is up while the login is dead", !!w.reg.get("loginban"), true);
  w.state.health = { ok: true, expired_logins: [] };
  w.loadHealth();
  await settle();
  t.check("a live login takes the banner down", w.reg.get("loginban") || null, null);
}

// --- the click targets exactly the server's login route -----------------------------------------
{
  const w = world();
  t.check("the opener builds the login-terminal path", w.loginTerminalUrl("claude"), "/api/login/claude/terminal");
  w.openLoginTerminal("claude");
  t.check("the socket the click opens goes to that path",
    w.sockets.map(s => s.url), ["ws://cockpit.test/api/login/claude/terminal"]);
  t.check("opening shows the overlay", w.reg.get("loginterm").classList.contains("open"), true);
}

// --- socket close ends the flow: surface gone, health re-read, banner repainted, outcome toasted --
{
  const w = world();
  w.state.health = { ok: true, expired_logins: [{ runtime: "claude", expired_at: "2026-08-22T10:00:00Z" }] };
  w.loadHealth();
  await settle();
  w.openLoginTerminal("claude");
  const ws = w.sockets[0];
  // The server's voice arrives as text frames: coaching first, the outcome after the child exits.
  ws.onmessage({ data: "skein: type /login once it starts, then /exit — `setup-token` returns a token\r\n" });
  ws.onmessage({ data: "skein: the login reached 3 place(s) that had none\r\n" });
  const asked = w.state.healthCalls;
  w.state.health = { ok: true, expired_logins: [] };   // the login IS live now
  ws.onclose();
  await settle();
  t.check("closing the socket closes the surface", w.reg.get("loginterm").classList.contains("open"), false);
  t.check("health is re-read exactly once on close", w.state.healthCalls, asked + 1);
  t.check("the banner repaints from that answer — gone, the login is live", w.reg.get("loginban") || null, null);
  t.check("the server's last sentence is the toast", w.state.toasts.at(-1), "the login reached 3 place(s) that had none");
}

// --- a newer agent CLI is an OFFER in Settings -> Update, and no longer a bar --------------------
//
// SKEIN-405 asked for it in the bar: "show that in the bar when there is an update. You check if
// new version is out regularly." SKEIN-486 moved it, on the owner's call, and the reason is the one
// the two banners always had between them: a dead credential has STOPPED work and an old CLI has
// not, so only one of them is worth the top of a board somebody is trying to read. The CHECK is
// untouched — still skein's own clock, still answered from a remembered reading.
//
// What survives the move is what the row has to say, so these are the same assertions against the
// surface that now says it.
{
  const w = world();
  const row = w.renderRuntimeUpdates([{ runtime: "claude", have: "1.2.3", latest: "1.2.9" }]);
  t.check("a newer CLI is named with BOTH versions, so the reader can decide whether they care",
    [/1\.2\.3/.test(row), /1\.2\.9/.test(row)], [true, true]);
  t.check("and says it is the fleet's, since every box shares them",
    /every box shares/.test(row), true);
  t.check("with something to press", /pressUpdateAgents/.test(row), true);

  // The pane is not the bar, and this is the difference: a bar with nothing to say must vanish, and
  // a pane somebody deliberately opened must ANSWER. "Nothing to install" is the answer.
  const none = w.renderRuntimeUpdates([]);
  t.check("nothing behind still says so, because a pane that was opened must answer",
    /current/.test(none), true);
  t.check("and offers nothing to press when there is nothing to install",
    /pressUpdateAgents/.test(none), false);
}

// --- the move is real: a behind CLI puts nothing across the top ----------------------------------
//
// The non-vacuity check for the whole change. Without it, "we moved it" is asserted by two tests
// that would both pass if the bar were still there beside the pane.
{
  const w = world();
  w.state.health = { ok: true, expired_logins: [],
    runtime_updates: [{ runtime: "codex", have: "0.4.1", latest: "0.5.0" }] };
  w.loadHealth();
  await settle();
  t.check("a behind agent CLI raises no bar at all",
    [w.reg.get("updateban") || null, w.reg.get("loginban") || null], [null, null]);
  // Non-vacuity, and it is the assertion that makes the one above mean something: this world DOES
  // raise a bar when there is a reason to, so "no bar" is a fact about agent CLIs and not about a
  // harness that never paints.
  w.state.health = { ok: true, expired_logins: [{ runtime: "claude", expired_at: "2026-08-22T10:00:00Z" }] };
  w.loadHealth();
  await settle();
  t.check("while a dead credential still does, which is what makes that a real absence",
    !!w.reg.get("loginban"), true);
}

// --- and NOT KNOWING is not BEING BEHIND ---------------------------------------------------------
//
// The one way this pane could lie, and the cheapest to get wrong: the remote is empty whenever
// GitHub has not been asked yet, could not be reached, or refused. A verdict that read those as an
// update would light the button on a fleet that is current, and somebody who pressed it would
// rebuild for nothing and learn to ignore the light.
{
  const w = world();
  const said = u => w.updateVerdict(u)[1];
  t.check("an unanswered check says so rather than claiming an update",
    /could not ask GitHub/.test(said({ running: "abc", source: "abc", remote: "", why: "timed out" })), true);
  t.check("a fleet with no checkout says THAT, rather than comparing against nothing",
    /no checkout/.test(said({ running: "abc", source: "", remote: "def" })), true);
  t.check("current is stated plainly",
    said({ running: "abc1234", source: "abc1234", remote: "abc1234" }), "this is the newest skein");
  t.check("and a real difference is an update",
    /newer skein is on GitHub/.test(said({ running: "abc", source: "abc", remote: "def", behind: true })), true);
  // The rarer one, and it must not be phrased as the common one: the binary is not the checkout.
  t.check("a binary that is not its checkout is a DIFFERENT sentence from being behind",
    /not the checkout/.test(said({ running: "abc", source: "def", remote: "def", unbuilt: true })), true);
}

t.done();
