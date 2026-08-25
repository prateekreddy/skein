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
    ${grab("loginTerminalUrl")}
    ${grab("loginTermState")}
    ${grab("openLoginTerminal")}
    ${grab("closeLoginTerminal")}
    return {
      loadHealth: () => loadHealth(),
      openLoginTerminal: r => openLoginTerminal(r),
      closeLoginTerminal: () => closeLoginTerminal(),
      loginTerminalUrl: r => loginTerminalUrl(r),
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

t.done();
