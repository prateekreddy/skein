// Can the board tell a calm fleet from a dead server?
//
// It could not, and the failure was guaranteed rather than intermittent. The staleness banner ages
// `lastTickAt`, which only advanced when box data arrived — and the stream deliberately sends none
// while nothing moves, with a re-sync floor of ten minutes. So every quiet afternoon put up "board
// is Ns stale — reconnecting…" on a fleet where everything was fine, and the reconnects underneath
// it were real: ten minutes of a connection carrying zero bytes is a connection anything in the path
// may drop, and each reconnect costs a fresh fleet snapshot.
//
// The producer now sends an `alive` event every ten seconds (`stream::ALIVE_EVERY`). Rust proves it
// is SENT; this proves the page LISTENS, which is the half that made the banner wrong. Both halves
// are needed and neither implies the other — a heartbeat nobody reads changes nothing on screen.
//
// Runs the page's real `connect()` against a fake EventSource rather than asserting on its source
// text, because "the string `alive` appears in index.html" would pass on a listener that renders the
// wrong thing or on one that was commented out.
//
//   node tests/ui/stream.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed down to what `connect` actually touches.
function board() {
  const listeners = {};
  let renders = 0;
  class FakeEventSource {
    static OPEN = 1;
    constructor(url) {
      this.url = url;
      this.readyState = FakeEventSource.OPEN;
    }
    addEventListener(name, fn) {
      listeners[name] = fn;
    }
    close() {}
  }
  const body = `
    let es = null;
    let lastTickAt = 0;
    let boxes = [];
    const receivedAt = new Map();
    const render = () => { bump(); };
    ${grab("connect")}
    connect();
    return {
      fire: (name, data) => listeners[name]({ data }),
      has: name => typeof listeners[name] === "function",
      at: () => lastTickAt,
      url: () => es.url,
    };
  `;
  const made = new Function(
    "EventSource",
    "listeners",
    "bump",
    "fetch",
    "console",
    body,
  )(
    FakeEventSource,
    listeners,
    () => { renders++; },
    async () => ({ json: async () => [] }),
    console,
  );
  return { ...made, renders: () => renders };
}

const b = board();

t.check("the board opens the event stream", b.url(), "/api/events");
t.check("it listens for the heartbeat", b.has("alive"), true);

// The heartbeat's whole job: mark the board as current. Without this the banner measures time since
// the FLEET changed, which on a quiet fleet is unbounded.
const before = b.at();
b.fire("alive", "");
t.check("a heartbeat marks the board current", b.at() > before, true);

// And it says nothing about the fleet, so it must not redraw one. A heartbeat that rendered would
// reset every row's age on a fleet where nothing had happened — the board would look busy while
// being told the opposite.
t.check("a heartbeat does not redraw the board", b.renders(), 0);

// The data events still do both, or the heartbeat has replaced something rather than added to it.
const fresh = board();
fresh.fire("snapshot", JSON.stringify({ event: "snapshot", boxes: [] }));
t.check("a snapshot still redraws", fresh.renders(), 1);
t.check("a snapshot still marks the board current", fresh.at() > 0, true);

t.done();
