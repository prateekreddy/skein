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
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const page = readFileSync(join(root, "src", "web", "index.html"), "utf8");

// Lift one top-level declaration out of the page by name. Brace-matched rather than regex-to-
// end-of-line, because these span lines; a wrong slice would throw here rather than silently test
// a truncated function.
function grab(name) {
  for (const start of [`function ${name}(`, `const ${name} =`, `let ${name} =`]) {
    const at = page.indexOf(`\n${start}`);
    if (at < 0) continue;
    const from = at + 1;
    const isFn = start.startsWith("function");
    let depth = 0, opened = false;
    for (let i = from; i < page.length; i++) {
      const c = page[i];
      if (isFn) {
        // Braces only. Counting the parameter list's parens too would end the function at `)` on
        // its very first line, which is a slice that parses and tests nothing.
        if (c === "{") { depth++; opened = true; }
        else if (c === "}" && --depth === 0 && opened) return page.slice(from, i + 1);
        continue;
      }
      if (c === "{" || c === "[" || c === "(") depth++;
      else if (c === "}" || c === "]" || c === ")") depth--;
      // A declaration ends at the first line break outside any bracket — which is also the right
      // answer for `let a = 0, b = null;`, where there are no brackets to have opened at all.
      else if (c === "\n" && depth === 0) return page.slice(from, i);
    }
  }
  throw new Error(`could not lift \`${name}\` out of index.html — did it get renamed?`);
}

// The mouth's own state. `say` is the page's, stubbed to record instead of speak.
let spoken = [];
const say = t => { if (t) spoken.push(t); };

const source = [
  "GROUPS", "groupOf", "NEEDS_YOU", "owedIn", "sayName", "forSpeech", "VERB",
  // `awaySince` brings `spokenOwed` and `stateSince` with it — they share one declaration.
  "utteranceFor", "owedSentence", "OWED_GRACE_MS", "OWED_SETTLE_MS", "awaySince",
  "settledOwed", "forgetSettledDebts", "sayStandingDebt",
].map(grab).join("\n");

// The lifted code closes over two variables the page owns elsewhere. They are declared here rather
// than rewritten into accessors: `lastSpoken = x` is an assignment, and turning it into a call by
// string substitution produces `setLastSpoken(x;` — a rewrite that has to understand the code it is
// rewriting is a worse dependency than a two-line prelude.
const scope = new Function(
  "say",
  `let voiceOn = true, lastSpoken = null;
   ${source}
   return { groupOf, owedIn, utteranceFor, owedSentence, sayStandingDebt,
            NEEDS_YOU, OWED_GRACE_MS, OWED_SETTLE_MS,
            setVoice: v => { voiceOn = v; },
            reset: () => { awaySince = 0; spokenOwed = new Set(); stateSince = {}; } };`,
);

let failures = 0;
function check(what, got, want) {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) { failures++; console.error(`✗ ${what}\n   got  ${JSON.stringify(got)}\n   want ${JSON.stringify(want)}`); }
  else console.log(`✓ ${what}`);
}

const V = scope(say);
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
  V.owedSentence(V.owedIn([box("web-main", "waiting")])),
  "web main is waiting.",
);
check(
  "and an empty fleet says so",
  V.owedSentence(V.owedIn([box("a", "working")])),
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
const tick = (states, away = true, spokenNow = []) => V.sayStandingDebt(fleet(states), away, spokenNow);

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

// A transition is being announced on the same tick — that sentence stands alone.
spoken = []; V.reset();
tick({ "web-main": "waiting" });
T += V.OWED_SETTLE_MS + V.OWED_GRACE_MS + 1;
tick({ "web-main": "waiting" }, true, ["web-main"]);
tick({ "web-main": "waiting" });
check("never reads the debt out on top of a transition it just announced", spoken, []);

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
Date.now = realNow;

console.log(failures ? `\n${failures} failed` : "\nall good");
process.exit(failures ? 1 : 0);
