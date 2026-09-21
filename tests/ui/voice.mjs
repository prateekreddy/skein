// What the mouth says, tested without a browser.
//
// `smoke.mjs` needs chromium and cannot run inside a box (no working sudo to install its system
// libraries), so the voice shipped unverified and shipped wrong: `waiting` — a box that ended its
// turn and wants your next instruction — counted as "needs you" for the tab title and the needs-you
// navigation, and was left out of both voice paths. The title said "3 need you" while the mouth
// stayed shut, and "read what needs me" answered "nothing needs you" with boxes waiting on screen.
//
// The sentences are pure functions of a fleet snapshot, so they do not need a page. This lifts them
// out of index.html and runs them, which is a test that works where the fix is being written.
//
//   node tests/ui/voice.mjs
import { grab, harness } from "./lift.mjs";
// **Imported where the code lives, lifted where it still lives in the page.**
//
// The board's groups and the whole speech grammar moved into `cockpit/src` — `9116043` took
// `GROUPS`, and SKEIN-112 took `sayName`, `forSpeech`, `VERB` and `utteranceFor` — after which
// `grab` threw on the first name and this entire suite stopped running. Nothing runs these suites,
// so nobody saw it (SKEIN-113).
//
// What is left in the page is the part this file is actually about: the standing-debt STATE — the
// grace window, the settle window, and the once-per-box memory. `docs/parity.md` §1 calls that
// policy the feature, and it has no other home, because it is state and `cockpit/src` holds only
// leaf functions. So the split below is the real boundary rather than a convenience.
import { GROUPS, groupOf, NEEDS_YOU, owedIn } from "../../cockpit/src/groups.mjs";
import { announcementsFor, sentenceFor, utteranceFor, OWED_GRACE_MS }
  from "../../cockpit/src/announce.mjs";

// The page's output channels, stubbed to record instead of speak or interrupt. Both are driven by
// one announcer now, so both are recorded here and asserted with the same rules.
let spoken = [];
const say = t => { if (t) spoken.push(t); };
let notes = [];
const pushNote = (body, tag) => { notes.push(`${body} [${tag}]`); };
const beep = () => {};

const source = [
  // `awaySince` brings `spokenOwed` and `stateSince` with it — they share one declaration.
  "OWED_SETTLE_MS", "awaySince",
  "settledOwed", "forgetSettledDebts", "announceStandingDebt",
].map(grab).join("\n");

// The lifted code closes over two variables the page owns elsewhere. They are declared here rather
// than rewritten into accessors: `lastSpoken = x` is an assignment, and turning it into a call by
// string substitution produces `setLastSpoken(x;` — a rewrite that has to understand the code it is
// rewriting is a worse dependency than a two-line prelude.
// The imports the lifted code calls are passed in rather than re-declared, so what runs here is the
// same `announcementsFor` the page runs and the same one `cockpit/test/announce.test.mjs` asserts.
const scope = new Function(
  "say",
  "pushNote",
  "beep",
  "groupOf",
  "owedIn",
  "announcementsFor",
  "OWED_GRACE_MS",
  "NEEDS_YOU",
  `let voiceOn = true, lastSpoken = null, alertsOn = false;
   ${source}
   return { announceStandingDebt, OWED_SETTLE_MS,
            setVoice: v => { voiceOn = v; },
            setAlerts: v => { alertsOn = v; },
            reset: () => { awaySince = 0; spokenOwed = new Set(); stateSince = {}; } };`,
);

const { check, done } = harness();

const V = {
  ...scope(say, pushNote, beep, groupOf, owedIn, announcementsFor, OWED_GRACE_MS, NEEDS_YOU),
  groupOf, owedIn, utteranceFor, NEEDS_YOU, GROUPS, OWED_GRACE_MS,
  // What `sayInbox` reads out. `owedSentence` is gone — there is one sentence-maker now, and the
  // "nothing owed" case belongs to the caller, because "" is right for a channel that speaks on its
  // own and wrong for one you asked a question.
  onDemand: need => (need.length ? sentenceFor(need, groupOf) : "Nothing needs you."),
};
const box = (name, state, extra = {}) => ({ name, state, ...extra });

// --- what counts as owed ---------------------------------------------------------------------
check(
  "a box that ended its turn is owed to you",
  V.owedIn([box("a", "waiting")]).map(b => b.name),
  ["a"],
);
check(
  "so are blocked and errored boxes, and working ones are not",
  V.owedIn([box("a", "waiting"), box("b", "needs-input"), box("c", "error"), box("d", "working")])
    .map(b => b.name),
  ["a", "b", "c"],
);

// --- what it actually says -------------------------------------------------------------------
check(
  "a waiting box is described as waiting, not as needing a decision",
  V.utteranceFor(box("web-main", "waiting", { headline: "Added the parser" }), "waiting"),
  "web main is waiting. Added the parser",
);
check(
  "the on-demand reading never claims nothing is owed while a box waits",
  V.onDemand(V.owedIn([box("web-main", "waiting")])),
  "web main is waiting.",
);
check(
  "and an empty fleet says so",
  V.onDemand(V.owedIn([box("a", "working")])),
  "Nothing needs you.",
);

// --- when it speaks --------------------------------------------------------------------------
// Two independent clocks, so the fleet is driven through a controlled one. `waiting` is mostly
// TRANSIENT — a box passes through it between turns — so "who is owed" changes constantly, and
// without the settle window every blink would have earned a sentence.
const realNow = Date.now;
let T = 1_700_000_000_000;
Date.now = () => T;
const fleet = states => Object.entries(states).map(([n, s]) => box(n, s));
const tick = (states, away = true, speakingAlready = false) =>
  V.announceStandingDebt(fleet(states), away, speakingAlready);

spoken = []; V.reset();
tick({ "web-main": "waiting" }, false);
T += V.OWED_GRACE_MS + V.OWED_SETTLE_MS;
tick({ "web-main": "waiting" }, false);
check("says nothing while you are looking at the board", spoken, []);

spoken = []; V.reset();
tick({ "web-main": "waiting" });
check("says nothing the instant you glance away — your grace period", spoken, []);

// The whole point of the settle window: a box that blinks through `waiting` is not an event.
spoken = []; V.reset();
tick({ "web-main": "working" });
T += V.OWED_GRACE_MS;            // you have been away long enough
tick({ "web-main": "waiting" }); // …and now it blinks
T += V.OWED_SETTLE_MS / 2;
tick({ "web-main": "working" }); // …and is gone again before it settles
T += V.OWED_SETTLE_MS * 2;
tick({ "web-main": "working" });
check("a box that blinks through waiting is never announced", spoken, []);

// Settled, but you have only just looked away.
spoken = []; V.reset();
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + 1;
tick({ "web-main": "waiting" });
check("a settled box still waits on your own grace period", spoken, []);

// Both clocks run out: this is the case that used to be silent forever.
T += V.OWED_GRACE_MS;
tick({ "web-main": "waiting" });
check("speaks a debt that has stood, with no transition to trigger it", spoken, ["web main is waiting."]);

spoken = [];
tick({ "web-main": "waiting" });
tick({ "web-main": "waiting" });
check("does not repeat the same debt", spoken, []);

// A second box, settled in its turn.
spoken = [];
tick({ "web-main": "waiting", api: "error" });
check("a newcomer that has not settled yet does not change the sentence", spoken, []);
T += V.OWED_SETTLE_MS + 1;
tick({ "web-main": "waiting", api: "error" });
// Only the newcomer. Re-reading the whole list here is what made this a nag: web-main had already
// been announced, and naming it again in every subsequent sentence is saying the same thing over.
check("but once it settles, the newcomer earns a sentence of its own", spoken, ["api hit an error."]);

// A `done` sentence is being read on the same tick — that one stands alone.
spoken = []; V.reset();
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting" }, true, true);
check("never stacks the debt on top of a sentence already being read", spoken, []);
// …deferred by a tick, not credited. Crediting it — which is what the set-key version did — silenced
// these boxes permanently, so a box that settled while something else was announced was never said.
tick({ "web-main": "waiting" });
check("and says it on the next tick instead of losing it", spoken, ["web main is waiting."]);

// --- and then it shuts up -----------------------------------------------------------------------
// The nag. A box you parked on purpose — there is no more work for it — must be mentioned once and
// then left alone, however long you stay away and however much the rest of the fleet moves.
spoken = []; V.reset();
tick({ "web-main": "waiting", api: "working" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting", api: "working" });
check("says a parked box once", spoken, ["web main is waiting."]);
spoken = [];
for (let i = 0; i < 40; i++) { T += 2000; tick({ "web-main": "waiting", api: "working" }); }
check("and does not bring it up again for the next eighty seconds", spoken, []);

// THE bug: `waiting` blinks constantly, and forgetting instantly meant every blink re-earned the
// same sentence. The memory has to outlast a flicker exactly as entry has to outlast one.
spoken = [];
for (let i = 0; i < 5; i++) {
  T += 2000; tick({ "web-main": "working", api: "working" });   // blink out…
  T += 2000; tick({ "web-main": "waiting", api: "working" });   // …and back
  T += V.OWED_SETTLE_MS + 1;
  tick({ "web-main": "waiting", api: "working" });              // settled again
}
check("a box that blinks out of waiting is not re-announced when it settles back", spoken, []);

// But a box that genuinely goes back to work and later needs you again IS news.
spoken = [];
tick({ "web-main": "working", api: "working" });
T += V.OWED_SETTLE_MS * 3;
tick({ "web-main": "working", api: "working" });
T += 2000;
tick({ "web-main": "waiting", api: "working" });
T += V.OWED_SETTLE_MS + 1;
tick({ "web-main": "waiting", api: "working" });
check("but a box that worked and then needed you again is announced", spoken, ["web main is waiting."]);

// A newcomer is named on its own — not as a re-reading of everyone already owed.
spoken = [];
tick({ "web-main": "waiting", api: "waiting" });
T += V.OWED_SETTLE_MS + 1;
tick({ "web-main": "waiting", api: "waiting" });
check("a second box earns a sentence about itself, not the whole list again", spoken, ["api is waiting."]);

// Silence means silence.
spoken = []; V.reset(); V.setVoice(false);
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting" });
check("stays quiet when voice is off", spoken, []);
V.setVoice(true);

// --- the other channel ---------------------------------------------------------------------------
// Desktop notifications fired on raw transitions, with none of the above applied to them — so they
// repeated exactly as the mouth did. They are announced by the same rule now, which is the point:
// one decision about what is worth interrupting you for, not two that drift.
spoken = []; notes = []; V.reset(); V.setVoice(false); V.setAlerts(true);
tick({ "web-main": "waiting", api: "working" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting", api: "working" });
check("notifies once for a settled box", notes, ["web-main is waiting [web-main]"]);
notes = [];
for (let i = 0; i < 20; i++) {
  T += 2000; tick({ "web-main": "working", api: "working" });   // the same blink…
  T += 2000; tick({ "web-main": "waiting", api: "working" });
  T += V.OWED_SETTLE_MS + 1;
  tick({ "web-main": "waiting", api: "working" });
}
check("and does not notify again for the same standing debt", notes, []);

// A crowd becomes a count rather than a stack of banners.
notes = []; V.reset();
const crowd = { a: "waiting", b: "waiting", c: "waiting", d: "error" };
tick(crowd);
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick(crowd);
check("four boxes are one banner, not four", notes, ["4 boxes need you [skein-owed]"]);

// The switches are independent — that is why they are two switches.
notes = []; spoken = []; V.reset(); V.setAlerts(false); V.setVoice(true);
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting" });
check("voice alone still speaks with alerts off", [spoken, notes], [["web main is waiting."], []]);

// With both off nothing is announced — and nothing is marked announced either, so turning a channel
// on tells you what is waiting rather than starting from silence.
notes = []; spoken = []; V.reset(); V.setVoice(false);
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting" });
check("both off announces nothing", [spoken, notes], [[], []]);
V.setVoice(true);
tick({ "web-main": "waiting" });
check("and the backlog survives, so switching one on tells you what you missed", spoken, ["web main is waiting."]);

Date.now = realNow;

// --- the ear: what opens the microphone, what does not, and what it says when it cannot ----------
//
// The same lift, applied to the other half of this feature. The mouth's rules were already here;
// the ear's were asserted by nothing, and both bugs below shipped and stayed shipped because the
// only test of the ear was `smoke.mjs`, which stands in a recogniser that cannot fail.
//
// What the fake recogniser records is deliberately small: which objects were told to `start`, which
// were told to `stop`, what was set on them before the start, and what went into the strip. "The
// microphone is open" is then `started && !stopped`, which is a property of the real code's
// behaviour rather than of its text — the distinction `src/cockpit.rs` makes about the switch it
// stopped asserting by string match.
const earSource = [
  "VOICE_LANG", "VOICE_TROUBLE", "recogniser", "localReady", "askLocal",
  "hearing", "heardFinal", "listen", "stopListening",
  "pttTimer", "PTT_HOLD", "holdPtt", "endPtt",
].map(grab).join("\n");

// One world per scenario, because `localAsked` is a once-ever latch and a shared world would carry
// the first scenario's answer into every other one.
function ear({ available, availableOnDevice, recognition = true, constructThrows, startThrows } = {}) {
  const started = [], shown = [], asked = [];
  let timers = [], seq = 0;
  class Rec {
    constructor() {
      if (constructThrows) throw new Error("this browser will not make one");
      this.phrases = [];
      Rec.last = this;
    }
    start() {
      if (startThrows) throw new Error("this browser will not start one");
      this.started = true;
      started.push(this);
    }
    // `stop` finalises rather than discards, which is what the page relies on to hear a command at
    // all — so the fake ends the session the way a real one does.
    stop() { this.stopped = true; this.onend?.(); }
  }
  if (available) Rec.available = opts => { asked.push(opts); return available(opts); };
  if (availableOnDevice) Rec.availableOnDevice = lang => { asked.push(lang); return availableOnDevice(lang); };
  const strip = { classList: { remove() {}, add() {}, toggle() {} }, querySelector: () => ({}) };
  const world = new Function(
    "window", "setTimeout", "clearTimeout", "showHeard", "vstrip", "heard", "boxes",
    `${earSource}
     return { listen, stopListening, holdPtt, endPtt, recogniser,
              localReadyIs: () => localReady, listening: () => hearing };`,
  )(
    { SpeechRecognition: recognition ? Rec : undefined },
    (fn, ms) => { timers.push({ fn, ms, id: ++seq }); return seq; },
    id => { timers = timers.filter(t => t.id !== id); },
    text => shown.push(text),
    () => strip,
    () => {},
    [],
  );
  return {
    ...world,
    Rec,
    asked,
    shown,
    // Every recogniser this world was told to start and has not been told to stop. A push-to-talk
    // whose key is up and whose count here is not zero is the failure the page's own comment names:
    // "the mic open for as long as the tab lives".
    open: () => started.filter(r => !r.stopped).length,
    fire: () => { const due = timers; timers = []; for (const t of due) t.fn(); },
    pending: () => timers.length,
  };
}
const key = { code: "AltRight" };
const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
// What a failed attempt to listen must never be: the code on its own. A sentence is the thing a
// person can act on, and the code is what sent the owner to ask an agent what his cockpit meant.
const sentence = text => typeof text === "string" && text.trim().split(/\s+/).length >= 5;

// --- what it says when it cannot listen ---------------------------------------------------------
{
  const e = ear({ recognition: false });
  await e.listen();
  check(
    "a browser with no recogniser is told so, rather than nothing happening at all",
    [e.shown.length, sentence(e.shown[0]), e.open()],
    [1, true, 0],
  );
}
{
  const e = ear({ constructThrows: true });
  await e.listen();
  check(
    "and so is one that has the name but will not make one",
    [e.shown.length, sentence(e.shown[0]), e.open()],
    [1, true, 0],
  );
}
{
  const e = ear({ startThrows: true });
  await e.listen();
  check(
    "a start that throws says so and leaves nothing listening",
    [sentence(e.shown.at(-1)), e.listening(), e.open()],
    [true, null, 0],
  );
}
{
  // The reported bug, at the surface the owner actually met it on.
  const e = ear();
  await e.listen();
  e.Rec.last.onerror({ error: "service-not-allowed" });
  const said = e.shown.at(-1);
  check(
    "the refused service is a sentence, and the error code is not the message",
    [sentence(said), said.includes("service-not-allowed"), e.listening()],
    [true, false, null],
  );
}
{
  const e = ear();
  await e.listen();
  e.Rec.last.onerror({ error: "not-allowed" });
  check("a blocked microphone still says it is blocked", e.shown.at(-1), "the microphone is blocked for this page");
}
{
  const e = ear();
  await e.listen();
  e.Rec.last.onerror({ error: "some-error-nobody-has-seen" });
  const said = e.shown.at(-1);
  check(
    "an error this page has never seen still gets a sentence, with its code inside it",
    [sentence(said), said.includes("some-error-nobody-has-seen")],
    [true, true],
  );
}
{
  // Not every ending is a failure: a key released with nothing said must not accuse the browser of
  // anything. `listening…` is the only line these two leave behind.
  const e = ear();
  await e.listen();
  e.Rec.last.onerror({ error: "no-speech" });
  await e.listen();
  e.Rec.last.onerror({ error: "aborted" });
  check("silence and a change of mind explain nothing", e.shown, ["listening…", "listening…"]);
}

// --- on-device only where it is already there ---------------------------------------------------
//
// `processLocally` is a REQUIREMENT, not a preference: set it with no local model and the whole
// recognition fails instead of falling back. Measured in Chromium 151 with no model installed —
// `available({langs:["en-US"]})` is "available", `available({langs:["en-US"],processLocally:true})`
// is "unavailable", and a start under the flag ends in an error with nothing heard. So the only
// safe time to ask for it is when the answer is already in, and the only safe answer is "available".
for (const [state, wanted] of [["available", true], ["unavailable", undefined],
                               ["downloadable", undefined], ["downloading", undefined]]) {
  const e = ear({ available: async () => state });
  e.holdPtt(key);              // the hold is what asks
  await flush();               // …and the answer arrives inside the 260 ms it lasts
  e.fire();                    // …which is when the mic opens
  check(
    `a model that is "${state}" ${wanted ? "is used" : "is not asked for"}`,
    [e.Rec.last.processLocally, e.open()],
    [wanted, 1],
  );
}
{
  const e = ear({ available: async () => "available" });
  e.holdPtt(key);
  await flush();
  e.fire();
  check(
    "the question is asked about the language the ear actually listens in",
    [e.asked[0].langs, e.asked[0].processLocally, e.Rec.last.lang],
    [[e.Rec.last.lang], true, "en-US"],
  );
}
{
  // The name this API shipped under before it was renamed. A probe that knows only one spelling
  // answers `undefined` in the browser that has the other, and `undefined` is a permanent "no".
  const e = ear({ availableOnDevice: async () => "available" });
  e.holdPtt(key);
  await flush();
  e.fire();
  check("the older spelling of the question is asked too", [e.asked, e.Rec.last.processLocally], [["en-US"], true]);
}
{
  const e = ear();            // a browser with a recogniser and no way to ask about on-device
  e.holdPtt(key);
  await flush();
  e.fire();
  check(
    "a browser that cannot be asked still listens, on whatever it uses by default",
    [e.Rec.last.processLocally, e.open()],
    [undefined, 1],
  );
}
{
  // Never awaited: the answer arriving late must not hold up the microphone, and must not force
  // anything on a recogniser that is already running.
  let settle;
  const e = ear({ available: () => new Promise(r => { settle = r; }) });
  e.holdPtt(key);
  e.fire();
  const opened = e.open();
  settle("available");
  await flush();
  check(
    "an answer that has not come back yet costs the hold nothing",
    [opened, e.Rec.last.processLocally],
    [1, undefined],
  );
}

// --- and the microphone closes, however the hold ends -------------------------------------------
//
// The interleaving this is about: keydown starts the 260 ms timer, the key comes back up before it
// fires, and whatever the release does has to leave nothing running — including in a world where
// the start is not instantaneous. Make `listen()` asynchronous without holding the release and this
// is the failure: `stopListening()` runs while the start is still in flight, finds `hearing` null,
// stops nothing, and the mic opens after the key is gone with nothing left to close it.
{
  const e = ear({ available: async () => "available" });
  e.holdPtt(key);              // the hold begins
  e.fire();                    // …the 260 ms elapses and the start is made
  e.endPtt();                  // …and the key comes up while it is still being made
  await flush();               // …and now anything the start was waiting on comes back
  check("a release that lands mid-start leaves no microphone open", e.open(), 0);
}
{
  const e = ear();
  e.holdPtt(key);
  e.endPtt();                  // the key came up before the hold window elapsed
  e.fire();                    // …so nothing is left to fire
  check("a hold released before it opens anything opens nothing", [e.pending(), e.open()], [0, 0]);
}
{
  // SKEIN-1002: `blur` is the release that arrives with no `keyup` behind it — the keyup goes to
  // whatever window took the focus — so cancelling the pending hold has to be part of it. It was
  // not, and the mic opened 210 ms into a page that was no longer in front of anybody.
  const e = ear();
  e.holdPtt(key);
  e.endPtt();                  // what `window.blur` runs
  e.fire();
  check("losing the window mid-hold opens no microphone at all", [e.open(), e.listening()], [0, null]);
}
{
  const e = ear();
  e.holdPtt(key);
  e.fire();
  check("and a hold that runs its course does open one", [e.open(), e.listening() !== null], [1, true]);
  e.endPtt();
  check("which the release then closes", e.open(), 0);
}
{
  const e = ear();
  e.holdPtt(key);
  e.holdPtt({ code: "BracketLeft" });   // ⌥[ — a chord, not a held mic
  e.fire();
  check("a chord cancels the hold rather than opening the mic", [e.pending(), e.open()], [0, 0]);
}

done();
