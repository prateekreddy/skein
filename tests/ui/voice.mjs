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
  // `awaySince` brings `owedSpokenKey` with it — they share one declaration.
  "utteranceFor", "owedSentence", "OWED_GRACE_MS", "awaySince", "sayStandingDebt",
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
            NEEDS_YOU, OWED_GRACE_MS,
            setVoice: v => { voiceOn = v; },
            reset: () => { awaySince = 0; owedSpokenKey = null; } };`,
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
// The bug this exists for: a box turns while you are LOOKING at the board, so no transition is ever
// announced, and it then sits owed in silence.
const owed = V.owedIn([box("web-main", "waiting")]);

spoken = []; V.reset();
V.sayStandingDebt(owed, false, false);
check("says nothing while you are looking at the board", spoken, []);

spoken = []; V.reset();
V.sayStandingDebt(owed, true, false);
check("says nothing the instant you glance away — the grace period", spoken, []);

// Same debt, still standing, once the grace has passed.
spoken = []; V.reset();
V.sayStandingDebt(owed, true, false);          // starts the clock
await new Promise(r => setTimeout(r, 5));
const realNow = Date.now;
Date.now = () => realNow() + V.OWED_GRACE_MS + 1;
V.sayStandingDebt(owed, true, false);
check("speaks a debt that has stood, with no transition to trigger it", spoken, ["web main is waiting."]);

// …and does not go on about it.
spoken = [];
V.sayStandingDebt(owed, true, false);
V.sayStandingDebt(owed, true, false);
check("does not repeat the same debt", spoken, []);

// A new box joining the set is a new sentence.
spoken = [];
V.sayStandingDebt(V.owedIn([box("web-main", "waiting"), box("api", "error")]), true, false);
check(
  "but a changed set earns one",
  spoken,
  ["2 boxes need you. web main, api."],
);

// A transition is being announced on the same tick — that sentence stands alone.
spoken = []; V.reset();
V.sayStandingDebt(owed, true, true);
Date.now = () => realNow() + V.OWED_GRACE_MS * 2;
V.sayStandingDebt(owed, true, false);
check("never reads the debt out on top of a transition it just announced", spoken, []);

// Silence means silence.
spoken = []; V.reset(); V.setVoice(false);
Date.now = () => realNow() + V.OWED_GRACE_MS * 3;
V.sayStandingDebt(owed, true, false);
check("stays quiet when voice is off", spoken, []);
V.setVoice(true);
Date.now = realNow;

console.log(failures ? `\n${failures} failed` : "\nall good");
process.exit(failures ? 1 : 0);
