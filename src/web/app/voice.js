// ---------- the ear: hold a key, say a short thing ----------
//
// The pair to the mouth, and it only earns its place if it works *without* coming back to the board:
// you are in another window, skein says a box wants permission, you answer. So this listens through
// a held key that reaches you wherever you are — including inside a focused terminal, which the
// fleet keymap deliberately yields every key to.
//
// Deliberately a closed set of short commands rather than open dictation. The vocabulary that
// matters here is not English, it is *this fleet* — ten box names the page already knows — and
// resolving against a list beats transcribing a name every time. `tell it …` is the one open path,
// and it sends its words verbatim rather than trying to understand them.
//
// Nothing here is hard to undo. No destroy, no merge, no ship: a misheard word must cost a glance,
// never a branch. The verbs are continue, next, show, open, and read-me-the-inbox.
let lastSpoken = null;   // the box the mouth last named — what "it" binds to
let hearing = null;      // the live SpeechRecognition, when one is running
let heardFinal = "";

const vstrip = () => {
  let el = document.getElementById("vstrip");
  if (!el) {
    el = document.createElement("div");
    el.id = "vstrip";
    el.innerHTML = `<span class="vdot"></span><span class="vtext"></span>`;
    document.body.append(el);
  }
  return el;
};
let vstripT;
function showHeard(text, state) {
  const el = vstrip();
  el.querySelector(".vtext").textContent = text;
  el.classList.toggle("hot", state === "listening");
  el.classList.toggle("heard", state !== "listening");
  el.classList.add("show");
  clearTimeout(vstripT);
  if (state !== "listening") vstripT = setTimeout(() => el.classList.remove("show"), 2600);
}

/// Box names as they might be *said*, matched against a transcript that has none of the punctuation.
/// Two passes because a name is spelled for reading: `PROJ-S6` is said "proj s six" or "proj s6", and
/// the space is the part no transcriber agrees on — so the second pass removes them from both sides.
function boxFromWords(text) {
  const flat = s => s.toLowerCase().replace(/[^a-z0-9]+/g, "");
  const said = text.toLowerCase();
  const loose = flat(text);
  let best = null;
  for (const b of boxes) {
    for (const label of [b.name, b.branch || ""]) {
      if (!label) continue;
      const spoken = label.toLowerCase().replace(/[-_/]+/g, " ");
      const hit = said.includes(spoken) || (flat(label).length > 3 && loose.includes(flat(label)));
      // Longest match wins, so "gadget demo preperation" is not claimed by "gadget demo".
      if (hit && (!best || label.length > best.label.length)) best = { box: b, label };
    }
  }
  return best?.box || null;
}

/// Which box a command is about, in the order a person means it.
///
/// The point of the ordering is that you rarely have to say a name at all: the mouth just told you
/// which box is asking, so "it" is that one. Saying `PROJ-S6` out loud is the thing this design is
/// most trying to avoid.
function voiceTarget(text) {
  const named = boxFromWords(text);
  if (named) return named;
  const live = name => boxes.find(b => b.name === name);
  return (
    live(lastSpoken) ||
    live(sel) ||
    // If exactly one box is owed an answer there is no ambiguity to resolve.
    (b => (b.length === 1 ? b[0] : null))(
      boxes.filter(b => ["attn", "error"].includes(groupOf(b.state)))
    )
  );
}

// Longest phrases first, so "continue all" is never eaten by "continue". `needs` marks the verbs
// that act on one box and therefore have to resolve a target before they can run.
const VOICE_VERBS = [
  { say: ["continue all", "continue everything", "resume everything", "all of them"],
    run: () => { batchResume(); return "continuing everything that only needs a nudge"; } },
  { say: ["what needs me", "whats waiting", "what is waiting", "read the inbox", "status"],
    run: () => { sayInbox(); return null; } },
  { say: ["next", "skip", "next one"],
    run: () => { nextNeedsYou(); return "next"; } },
  { say: ["yes", "yeah", "yep", "continue", "proceed", "go ahead", "approve", "do it", "carry on"],
    needs: true, run: b => { resumeBox(b.name); return `continuing ${b.name}`; } },
  { say: ["show me", "show", "the diff", "diff"],
    needs: true, run: b => { openDiff(b.name); return `showing ${b.name}`; } },
  { say: ["open", "attach", "take me there", "go there"],
    needs: true, run: b => { openTerminal(b.name); return `opening ${b.name}`; } },
];

/// `tell it <words>` / `tell <box> <words>` — the one open path, sent verbatim.
///
/// Verbatim on purpose: this is the case where the words *are* the content, so anything clever done
/// to them here is damage. It goes to the same `/resume` the board's continue button uses, which
/// takes a prompt.
function voiceTell(text) {
  const m = text.match(/^tell\s+(.+)$/s);
  if (!m) return null;
  let rest = m[1];
  const named = boxFromWords(rest);
  if (named) {
    const spoken = named.name.toLowerCase().replace(/[-_]+/g, " ");
    const at = rest.toLowerCase().indexOf(spoken);
    if (at >= 0) rest = rest.slice(0, at) + rest.slice(at + spoken.length);
  } else {
    rest = rest.replace(/^(it|that|this|them|him|her|the box)\b/i, "");
  }
  // "tell it TO use the real database" — the connective belongs to the sentence, not the prompt.
  const prompt = rest.replace(/^\s*(to|that)\b/i, "").trim();
  const box = named || voiceTarget("");
  if (!box) return { spoken: "I don't know which box you mean" };
  if (!prompt) return { spoken: "nothing to send" };
  toast(`sending to ${box.name}…`);
  fetch(`/api/boxes/${encodeURIComponent(box.name)}/resume`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ prompt }),
  })
    .then(r => r.json())
    .then(d => toast(d.ok ? `sent to ${box.name}` : "send failed: " + (d.error || "").split("\n")[0]))
    .catch(() => toast("send failed"));
  return { spoken: `told ${box.name}` };
}

function heard(raw) {
  // Punctuation is the transcriber's guess at prosody and means nothing to a verb table.
  const text = raw.toLowerCase().replace(/[.,!?;]+$/g, "").trim();
  if (!text) return;
  const told = voiceTell(text);
  if (told) return showHeard(`“${raw}” → ${told.spoken}`, "done");
  for (const verb of VOICE_VERBS) {
    const hit = verb.say.find(phrase => text === phrase || text.startsWith(phrase + " "));
    if (!hit) continue;
    if (!verb.needs) {
      const said = verb.run();
      return showHeard(said ? `“${raw}” → ${said}` : `“${raw}”`, "done");
    }
    const box = voiceTarget(text);
    if (!box) return showHeard(`“${raw}” → which box?`, "done");
    return showHeard(`“${raw}” → ${verb.run(box)}`, "done");
  }
  // Said plainly rather than guessed at. A voice surface that acts on a maybe is worse than one that
  // asks again, because the cost of a wrong guess here is a box being sent something.
  showHeard(`“${raw}” — not a command I know`, "done");
}

// The one language this ear listens in, named once because two places have to agree on it: the
// recogniser is set to it, and the on-device question below is asked ABOUT it. Asking whether a
// model exists for one language and then listening in another is a probe that answers about
// nothing.
const VOICE_LANG = "en-US";

/// The browser's recogniser, or null where there is none.
///
/// One spelling of the lookup, because everything that reaches for it has to reach for the SAME
/// object: `tests/ui/smoke.mjs` stands a fake one in `window` before it holds the key, and a second
/// lookup written differently is one the fake does not cover.
function recogniser() {
  return window.SpeechRecognition || window.webkitSpeechRecognition || null;
}

/// Why an attempt to listen ended, in words rather than in its code.
///
/// `could not listen (service-not-allowed)` is what this used to say, and the owner asked what it
/// meant instead of acting on it — which is the measure of an error message being wrong. Every
/// entry here names what happened and what is left to do, the way the microphone arm already did.
/// The last clause matters more than it looks: the ear is a shortcut, not the only way in, and
/// somebody who has just been told it will not work needs to know they can still type.
const VOICE_TROUBLE = {
  "not-allowed": "the microphone is blocked for this page",
  "service-not-allowed": "this browser will not let a page use its speech service — you can still type to a box",
  "language-not-supported": `this browser has no speech model for ${VOICE_LANG} — you can still type to a box`,
  "audio-capture": "no microphone answered — check which input this browser is using",
  network: "the speech service could not be reached — check the network, or type to a box",
  "phrases-not-supported": "this browser would not take the fleet's box names — try again, it listens without them",
};

// Whether this machine can recognise speech WITHOUT sending it anywhere: `null` until asked, then
// `true` or `false`. It is a cached answer rather than a question asked at the moment the mic
// opens, and both halves of that are load-bearing.
//
// **Asking is asynchronous and opening the mic must not be.** The availability query returns a
// promise, and an `await` between the key going down and the mic opening is a window in which the
// key can come back UP: the release runs `stopListening()`, finds `hearing` still null because the
// start has not finished, stops nothing — and then the start completes and opens the microphone
// after the key is gone, with nothing left that will ever close it. That is precisely the failure
// the `blur` handler below exists to prevent, so the ear is not given a second road into it. The
// question is asked when a hold BEGINS, inside the 260 ms the hold already costs, and `listen()`
// only ever reads what has already come back.
//
// **So the first hold of a session may use the browser's default, and that is the right trade.**
// Where the answer has not arrived, nothing is forced and the browser chooses for itself — which is
// exactly what it would have done had this page never asked.
let localReady = null, localAsked = false;

/// Ask — once, and never awaited — whether recognition can happen on this device.
///
/// Only `"available"` counts. `"downloadable"` and `"downloading"` are a model that is not here
/// yet, and this page does not download one or ask to: it is a two-second command, and a first use
/// that stalls on a download is worse than one that quietly uses the recogniser the browser already
/// has.
function askLocal(Recognition) {
  if (localAsked || !Recognition) return;
  localAsked = true;
  try {
    // Both spellings, because the API was renamed after it shipped. `available({langs,
    // processLocally})` is what is there now — the statics on `SpeechRecognition` in Chromium 151
    // are exactly `available` and `install` — while `availableOnDevice(lang)` is the explainer's
    // original name. A probe that knows only one of them returns `undefined` in the browser that
    // has the other, and `undefined` is silently "no, for ever".
    const asked = Recognition.available
      ? Recognition.available({ langs: [VOICE_LANG], processLocally: true })
      : Recognition.availableOnDevice?.(VOICE_LANG);
    Promise.resolve(asked).then(state => { localReady = state === "available"; }, () => { localReady = false; });
  } catch { localReady = false; }
}

/// Start listening, and say so when we cannot.
///
/// Every exit from here puts a sentence in the strip. Holding a key and getting nothing is
/// indistinguishable from not having held it long enough (SKEIN-997), and by design the person
/// holding it is not looking at the screen — so whatever is on the screen when they do look has to
/// be the answer.
function listen() {
  const Recognition = recogniser();
  if (!Recognition) return showHeard("this browser cannot listen — you can still type to a box", "done");
  // Asked here as well as on the key press, because the command palette's "Listen for a command"
  // reaches `listen()` with no hold in front of it. Costs nothing on the second call.
  askLocal(Recognition);
  if (hearing) return;
  let r;
  try { r = new Recognition(); }
  catch { return showHeard("this browser would not open a recogniser — you can still type to a box", "done"); }
  r.lang = VOICE_LANG;
  r.continuous = false;
  r.interimResults = true;
  r.maxAlternatives = 1;
  // On-device only where the model is ALREADY installed, and never as a preference to fall back
  // from, because `processLocally` is a REQUIREMENT and not a hint: set it with no local model and
  // the whole recognition fails rather than going to the browser's own recogniser. Measured in
  // Chromium 151 on a machine with no model — `available({langs:["en-US"]})` answers `"available"`
  // while the same question with `processLocally: true` answers `"unavailable"`, and a start under
  // the flag ends in an error event with nothing heard. Left unset, the browser chooses, which is
  // the behaviour that works everywhere.
  //
  // **This line used to be `try { r.processLocally = true; } catch {}` and the `try` was guarding
  // the wrong failure.** It catches a throw on ASSIGNMENT, which cannot happen: the property is on
  // the prototype where it is supported, and assigning it where it is not just adds a property
  // nobody reads. So the comment's "where it is unsupported the browser uses its own default" was
  // true only of the browsers that ignore the flag entirely, and false — silently, permanently —
  // of every browser that honours it without a model to honour it with.
  if (localReady) r.processLocally = true;
  // The fleet's own vocabulary. Box names and branches are exactly the words a general recogniser
  // gets wrong, and exactly the ones skein knows. Proposed rather than shipped, so it is offered and
  // ignored where it is not understood — resolution against the live board is what actually carries
  // this, and it works either way.
  try {
    if (window.SpeechRecognitionPhrase && r.phrases) {
      for (const b of boxes) r.phrases.push(new SpeechRecognitionPhrase(b.name.replace(/[-_]+/g, " "), 3.0));
    }
  } catch {}
  heardFinal = "";
  r.onresult = e => {
    let interim = "";
    for (let i = e.resultIndex; i < e.results.length; i++) {
      const t = e.results[i][0].transcript;
      if (e.results[i].isFinal) heardFinal += t;
      else interim += t;
    }
    showHeard(heardFinal + interim || "listening…", "listening");
  };
  r.onerror = e => {
    hearing = null;
    // `aborted` and `no-speech` are not failures — they are a key released with nothing said — so
    // they take the strip away rather than explaining themselves. Everything else is something the
    // person is owed a sentence about, and a code this page has never seen still gets one, with the
    // code inside it: naming what we cannot explain beats "could not listen" on its own.
    if (e.error === "aborted" || e.error === "no-speech") vstrip().classList.remove("show");
    else showHeard(
      VOICE_TROUBLE[e.error] || `this browser refused to listen (${e.error}) — you can still type to a box`,
      "done",
    );
  };
  r.onend = () => {
    hearing = null;
    if (heardFinal.trim()) heard(heardFinal.trim());
    else vstrip().classList.remove("show");
  };
  hearing = r;
  try { r.start(); showHeard("listening…", "listening"); }
  catch { hearing = null; showHeard("this browser would not start listening — you can still type to a box", "done"); }
}
function stopListening() {
  // `stop` rather than `abort`: it finalises what was already said instead of discarding it, which
  // is the difference between releasing the key and changing your mind.
  try { hearing?.stop(); } catch {}
}

// Push to talk on a held Right-⌥, and it lives in its own handler for the reason the tab chords
// below do: the fleet keymap yields every key to a focused terminal, and answering a box *while
// heads-down in another one* is the entire use.
//
// Held, not tapped. The delay is what keeps this out of the way of ⌥-chords — ⌥[ and friends press
// Alt too — since a chord's second key arrives long before the window elapses and cancels it. It
// also happens to be how push-to-talk should feel.
let pttTimer = null;
const PTT_HOLD = 260;
// Capture, not bubble, and that is load-bearing rather than stylistic. The tab chords below (⌥[,
// ⌥], ⌥1-9) are bound on the capture phase and call `stopPropagation`, so a bubble listener here
// never saw the second key of exactly the chords this is meant to stand aside for — the mic opened
// 260ms into every ⌥[ tab switch, whenever any box tab was open. The comment above said a chord
// cancels the timer; it did not, and a browser test caught it after months of failing unnoticed.
function holdPtt(e) {
  if (e.code !== "AltRight" || e.repeat) {
    // Any other key while Alt is down means a chord, not a held mic.
    if (pttTimer && e.code !== "AltRight") { clearTimeout(pttTimer); pttTimer = null; }
    return;
  }
  if (pttTimer || hearing) return;
  // The hold is also the only moment this page asks whether the machine can hear on its own. It
  // happens here rather than at load for two measured reasons: `available({processLocally:true})`
  // CRASHES the renderer in Playwright's headless shell, which is the browser every suite in
  // `tests/ui/` runs, and those suites stand their own recogniser in `window` before they press
  // this key — so asking through `recogniser()` at press time asks the fake, exactly as the rest of
  // the ear does. A page that never listens never asks at all, which is also the right answer for a
  // query the platform treats as fingerprinting surface.
  askLocal(recogniser());
  pttTimer = setTimeout(() => { pttTimer = null; listen(); }, PTT_HOLD);
}
/// The end of a hold, however it ends: cancel one that has not opened the mic yet, and stop one
/// that has.
///
/// **Both halves, always.** `keyup` used to be the only path that cleared the timer and `blur` the
/// only one that stopped the recogniser, and the gap between them was reachable: lose the window
/// 50 ms into a hold and `blur` stopped a `hearing` that was still null, cancelled nothing, and the
/// timer fired 210 ms later into a page that no longer had focus — while the `keyup` went to
/// whatever window did. Nothing was left that could close it, since a second `blur` cannot fire on
/// a window that is already blurred, so the microphone stayed open for the life of the tab
/// (SKEIN-1002) — which is the exact failure the comment on the `blur` line claimed to prevent.
function endPtt() {
  if (pttTimer) { clearTimeout(pttTimer); pttTimer = null; }
  stopListening();
}
document.addEventListener("keydown", holdPtt, true);
document.addEventListener("keyup", e => { if (e.code === "AltRight") endPtt(); });
// Losing the window with the key down would otherwise leave the mic open for as long as the tab
// lives — the one failure of a push-to-talk that nobody notices until they hear themselves back.
window.addEventListener("blur", endPtt);

// ---------- the mouth: skein says which box needs you ----------
//
// The point is the one moment skein exists for: "which of my agents needs me right now" — answered
// while you are heads-down in an editor on the other monitor, without you looking at anything. The
// board already knows *when* (the transition below) and *what* (`headline`, the box's actual ask),
// so speaking is only a matter of saying it well.
//
// `speechSynthesis` rather than anything bought: it is already in the browser, it uses the Mac's own
// voices, it costs nothing per minute, it needs no key, and it keeps working with the network down.
//
// Silence is the design. It speaks on a real transition into needing you, only while the cockpit is
// unfocused, at most one sentence per tick, and a burst becomes a count instead of a monologue. A
// narrator that reads every state change gets switched off within the hour and never switched on.
let voiceOn = false;
try { voiceOn = localStorage.getItem("skein.voice") === "1"; } catch {}

// Prefer a voice that runs on the machine: no network, no latency, and it still works on a plane.
// Named preferences first because the default en-US voice is not always the nicest one installed.
const VOICE_PICKS = ["Samantha", "Alex", "Ava", "Allison", "Daniel", "Karen", "Moira"];
let pickedVoice = null;
function pickVoice() {
  if (pickedVoice) return pickedVoice;
  let all = [];
  try { all = speechSynthesis.getVoices() || []; } catch {}
  // Empty on the first call in most browsers — the list arrives asynchronously, and `voiceschanged`
  // below re-runs this. Returning null meanwhile is fine: the utterance just uses the default.
  if (!all.length) return null;
  const en = all.filter(v => /^en\b/i.test(v.lang || ""));
  const pool = en.length ? en : all;
  for (const want of VOICE_PICKS) {
    const hit = pool.find(v => (v.name || "").includes(want));
    if (hit) return (pickedVoice = hit);
  }
  return (pickedVoice = pool.find(v => v.localService) || pool[0] || null);
}
try { speechSynthesis.addEventListener("voiceschanged", () => { pickedVoice = null; pickVoice(); }); } catch {}

// `sayName` and `forSpeech` are NOT declared here. They are the shared bundle's, beside the sentence
// they serve (`cockpit/src/announce.mjs`), so the grammar has one home and node can test it. They
// were here, and the standing-debt channel stopped using them without anybody deciding to — see
// SKEIN-112. `tests/page_scripts.rs` is what stops a second copy appearing beside the bundle's.

function say(text) {
  if (!voiceOn || !text) return;
  try {
    // A queue one deep. Ten boxes flipping while you were at lunch must not become ten sentences
    // read to an empty room — and cancelling instead would truncate whatever is mid-word, which
    // sounds broken rather than busy.
    if (speechSynthesis.pending) return;
    const u = new SpeechSynthesisUtterance(text);
    // Slightly quick: this is a status line, not a story, and the default cadence feels sedated.
    u.rate = 1.08;
    u.volume = 0.85;
    const v = pickVoice();
    if (v) u.voice = v;
    speechSynthesis.speak(u);
  } catch {}
}

// `VERB` and `utteranceFor` are the bundle's too, for the same reason.

// setFavicon does a synchronous canvas draw + toDataURL PNG encode — real main-thread cost (tens
// of ms), unlike the near-free DOM writes elsewhere in notify(). notify() runs on every 2s fleet
// tick regardless of whether anything changed, so without this guard that encode cost was paid every
// single tick — including every tick where `need` was identical to last time — a periodic
// main-thread stall that shows up as "typing lags even when the board is calm". Only redraw the
// favicon when the count actually moves.
// (document.title stays unconditional: the focus handler below resets it independently of `need`,
// so it must keep resyncing every tick or it'd stay stuck at "skein" after a tab refocus.)
let lastNeedCount = -1;
function notify(snapshot) {
  const owed = owedIn(snapshot);
  const need = owed.length;
  document.title = need ? `(${need}) skein · ${need} need you` : "skein";
  if (need !== lastNeedCount) { lastNeedCount = need; setFavicon(need); }
  // `!hasFocus()`, not `document.hidden`: the all-day case is skein visible on one monitor while the
  // dev works in an editor on another — the tab is visible but unfocused, and still needs the ping.
  const away = !document.hasFocus();
  // Collected rather than spoken inline: several boxes can turn in one 2s tick, and reading four
  // sentences to an empty room is how a useful voice becomes an intolerable one. One turn is said
  // in full; more than one becomes a count, which is all you can act on anyway.
  const finished = [];
  for (const b of snapshot) {
    const g = groupOf(b.state), was = prevState[b.name];
    // `was &&`: a tab (re)opened mid-fleet starts with an empty prevState — without it, restoring
    // the board in the background fired a burst of notifications for every already-paused box.
    //
    // Only `done` is announced off a raw transition. Every "needs you" state used to be too, and a
    // transition is the wrong signal for those: they flicker — `waiting` especially — and a channel
    // driven by flicker repeats itself, which is the whole of "it keeps reminding me". They are
    // announced by announceStandingDebt below, once each, on a settled state. `done` is set by a
    // person and stays set, so its edge is real and saying it the moment it lands costs nothing.
    if (was && g !== was && g === "done") finished.push(b);
    prevState[b.name] = g;
  }
  // Voice and alerts are gated on their own switches rather than on each other: notifications need a
  // permission the browser may have refused, and speaking needs none — tying them together would
  // silence the half that still works. Both only while away, because a board you are looking at has
  // already told you.
  if (away && finished.length) {
    if (alertsOn) for (const b of finished) pushNote(`${b.name} finished`, b.name);
    if (voiceOn) {
      if (finished.length === 1) {
        // What "it" means from here on. The whole point of announcing a box by name is that you
        // should not then have to say that name back — "yes" and "show me" are what a person says.
        lastSpoken = finished[0].name;
        say(utteranceFor(finished[0], "done"));
      } else say(`${finished.length} boxes finished.`);
    }
  }
  // The whole fleet, not `owed`: the debt tracker has to see a box *stop* being owed to know it may
  // be forgotten. Handed only the owed subset it cannot tell "answered" from "never existed".
  announceStandingDebt(snapshot, away, away && voiceOn && finished.length > 0);
}

// There is no `owedSentence` here any more: `sentenceFor` in the bundle is the one sentence-maker,
// so what you hear on demand and what the board says on its own cannot become two accounts of the
// same fleet. That is what they had become — one read the box's ask aloud and the other did not.

// Say what is *still* owed, not only the moment it became owed.
//
// The mouth used to speak on state transitions alone, and only while you were away — so a box that
// turned while you were looking at the board was marked seen and never spoken, and it could then sit
// waiting for an hour in silence. "Things are waiting for me and it said nothing" was the ordinary
// outcome, not an edge case: you are usually looking at the board at the moment work stops.
//
// So the debt is announced once it has stood for a while. Three rules, answering three different
// objections — and the third was missing, which made this a nag.
//
// GRACE is about you: glancing at a terminal for ten seconds must not be narrated.
//
// SETTLE is about the box. `waiting` is mostly TRANSIENT — a box passes through it between turns —
// so a box has to have been owed for a while before it is worth saying out loud, which is also the
// honest test of whether it is really waiting on you rather than on itself.
//
// ONCE PER BOX is about the fact that you heard it. A debt is news exactly once; after that it is
// something you already know and have decided about. Parking a box because there is no more work for
// it is a normal thing to do, and a voice that will not let you is worse than no voice.
const OWED_SETTLE_MS = 12000;
let awaySince = 0, spokenOwed = new Set(), stateSince = {};

// The boxes that have been owed long enough to be worth a sentence.
//
// Tracked here rather than read off `prevState`, which notify() consumes for transitions and
// overwrites in the same pass — this needs to know *when* a group was entered, not just that it
// changed.
function settledOwed(snapshot) {
  const now = Date.now();
  const live = new Set();
  for (const b of snapshot) {
    live.add(b.name);
    const g = groupOf(b.state);
    if (stateSince[b.name]?.g !== g) stateSince[b.name] = { g, at: now };
  }
  // A destroyed box must not keep its timestamp: one recreated under the same name would look like
  // it had been waiting since before it existed.
  for (const name in stateSince) if (!live.has(name)) delete stateSince[name];
  return owedIn(snapshot).filter(b => now - stateSince[b.name].at >= OWED_SETTLE_MS);
}

// A box stays remembered as announced until it has been NOT owed for as long as it had to be owed
// to earn a sentence in the first place.
//
// The missing half of the settle window, and the whole of the "it keeps reminding me" bug. A box
// flickers out of `waiting` for a tick constantly — that is what SETTLE exists to absorb on the way
// in — but on the way out it was forgotten instantly. So one box, parked deliberately because there
// was no more work, blinked, re-settled twelve seconds later, and was announced again. And again.
// Symmetry is the fix: the same hysteresis in both directions.
function forgetSettledDebts(snapshot) {
  const now = Date.now();
  const group = new Map(snapshot.map(b => [b.name, groupOf(b.state)]));
  spokenOwed = new Set([...spokenOwed].filter(name => {
    if (!group.has(name)) return false;                    // gone from the fleet entirely
    if (NEEDS_YOU.includes(group.get(name))) return true;  // still owed — obviously still said
    // Owed until recently: hold the memory, so a blink cannot re-earn a sentence. Once it has
    // genuinely been working this long, needing you again is real news and is announced.
    return now - (stateSince[name]?.at ?? 0) < OWED_SETTLE_MS;
  }));
}

// The banner's half-sentence. Deliberately not `utteranceFor`, which is written to be *heard* and
// carries the box's last words — length a notification has no room for and truncates anyway.
// `noteFor` is the bundle's, beside `announcementsFor`, which is the only thing that calls it. The
// copy here was dead: when the decision moved, the notification body became "needs you" and this
// function stayed behind, defined and unreferenced, with nothing to say it had stopped mattering.

// The single announcer for "this box needs you", across both channels. One rule, two outputs — so
// they cannot drift into disagreeing about what is worth interrupting you for, and so a fix to the
// nag is a fix to all of it rather than to whichever half was noticed.
function announceStandingDebt(snapshot, away, speakingAlready) {
  const owed = settledOwed(snapshot);
  if (!away) { awaySince = 0; return; }
  if (!awaySince) awaySince = Date.now();
  forgetSettledDebts(snapshot);
  // Once per box, not once per change to the set. Keyed on the owed *set*, this re-read the whole
  // list every time any box entered or left it — so the boxes that had been waiting all along were
  // named again in every sentence, which is the other half of saying the same thing over and over.
  const fresh = owed.filter(b => !spokenOwed.has(b.name));
  // **The decision is `announcementsFor`, tested in node.** What is left here is the doing: this
  // function speaks and notifies, so it cannot be tested without a mouth — and the rule it carries
  // (voice and alerts are independent switches; they were once one condition, and turning alerts
  // off silenced the voice) is a rule worth asserting by calling rather than by reading.
  const plan = announcementsFor(fresh, {
    voiceOn, alertsOn, awayForMs: Date.now() - awaySince, speakingAlready,
    // Passed in rather than imported: the bundle's modules are concatenated and may not depend on
    // each other, so `sentenceFor` takes the group mapping as an argument. Without it every owed box
    // sounds the same, which is the whole of what went wrong here.
    groupOf,
  });
  // Nothing was announced, so nothing is credited: the backlog stays intact and turning a channel
  // on tells you what you missed instead of starting from silence.
  if (!plan.notes.length && !plan.say) return;
  for (const b of fresh) spokenOwed.add(b.name);
  // What "it" binds to from here on. One box named out loud is one you can answer without saying
  // its name back; a count names nothing, so it leaves the binding alone.
  if (plan.say && fresh.length === 1) lastSpoken = fresh[0].name;
  for (const n of plan.notes) pushNote(n.body, n.tag);
  if (plan.beep) beep();
  if (plan.say) say(plan.say);
}

// Read the inbox on demand — the same sentence the mouth would have spoken, for when you *are*
// looking at the board but do not want to read it, and as the honest way to hear what it sounds
// like before trusting it to speak on its own.
function sayInbox() {
  const need = owedIn(boxes);
  // The one case `sentenceFor` cannot answer: it returns "" for an empty set, which is right for a
  // channel that speaks on its own and wrong for one you asked. Silence in reply to a question reads
  // as broken.
  if (!need.length) return say("Nothing needs you.");
  if (need.length === 1) lastSpoken = need[0].name;
  say(sentenceFor(need, groupOf));
}
function pushNote(body, tag) {
  if (Notification.permission !== "granted") return;
  try {
    const n = new Notification("skein", { body, tag, renotify:true });
    // a notification that leads nowhere is half a notification — click focuses the box it names
    n.onclick = () => { try { window.focus(); } catch {} if (boxes.some(b => b.name === tag)) openTerminal(tag); };
  } catch {}
}
function paintAlerts() {
  const el = document.getElementById("alerts");
  if (!el) return;
  el.style.color = alertsOn ? "var(--done)" : "";
  el.title = alertsOn
    ? "Desktop alerts on — you'll be pinged when a box needs you. Click to silence"
    : !("Notification" in window)
      ? "This browser won't deliver notifications from this page — open the cockpit on localhost"
      : Notification.permission === "denied"
        ? "Your browser is blocking notifications for this site — allow them in site settings"
        : "Desktop alerts when a box needs you";
}
document.getElementById("alerts").addEventListener("click", async () => {
  // Say why instead of doing nothing. The Notification API is absent outside a secure context, so
  // reaching the cockpit on a LAN address rather than localhost made this button dead and silent —
  // indistinguishable from alerts that are on and simply never firing.
  if (!("Notification" in window)) {
    toast("Notifications need a secure context — reach the cockpit on http://127.0.0.1 rather than a LAN address.");
    paintAlerts();
    return;
  }
  // A switch that only switches on is not a switch. The palette offered "Disable desktop alerts"
  // and re-enabled them instead, because a second click re-read the still-granted permission.
  if (alertsOn) {
    alertsOn = false;
    try { localStorage.setItem("skein.alerts", "0"); } catch {}
    paintAlerts();
    return;
  }
  const p = Notification.permission === "granted" ? "granted" : await Notification.requestPermission();
  alertsOn = p === "granted";
  if (!alertsOn) toast("Your browser blocked notifications for this site — allow them in site settings to be pinged.");
  try { localStorage.setItem("skein.alerts", alertsOn ? "1" : "0"); } catch {}
  // Create + resume the AudioContext inside this click: browsers only allow audio started from a
  // user gesture, and beep() fires from a timer when the tab is unfocused — a context created there
  // is born suspended and plays silence. Priming it here makes the "needs you" beep actually audible.
  if (alertsOn) {
    try {
      audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
      if (audioCtx.state === "suspended") audioCtx.resume();
    } catch {}
  }
  paintAlerts();
});
function paintVoice() {
  const el = document.getElementById("voice");
  if (!el) return;
  el.style.color = voiceOn ? "var(--done)" : "";
  el.title = voiceOn
    ? "Speaking when a box needs you — click to silence"
    : "Say out loud when a box needs you";
}
document.getElementById("voice").addEventListener("click", () => {
  voiceOn = !voiceOn;
  try { localStorage.setItem("skein.voice", voiceOn ? "1" : "0"); } catch {}
  paintVoice();
  if (voiceOn) {
    // Spoken from inside the click on purpose, and it is not just feedback: Safari will not start
    // speech that did not originate in a user gesture, and every later utterance fires from a timer
    // while the tab is unfocused. Saying something here is what makes those audible at all — the
    // same trap the AudioContext above is primed against.
    say("Voice on.");
  } else {
    // Stop mid-word rather than finishing the sentence: the click means "be quiet now".
    try { speechSynthesis.cancel(); } catch {}
  }
});
paintVoice();
paintAlerts();
// Refocusing acknowledges the alert styling but must not lie: keep the live count in the title.
window.addEventListener("focus", () => { notify(boxes); });

