import { strict as assert } from "node:assert";
import { test } from "node:test";
import { announcementsFor, sentenceFor, sayName, forSpeech, utteranceFor, OWED_GRACE_MS }
  from "../src/announce.mjs";

const boxes = n => Array.from({ length: n }, (_, i) => ({ name: `b${i + 1}` }));
const away = { awayForMs: OWED_GRACE_MS + 1 };

test("voice and alerts are independent switches", () => {
  // The property, tested by calling rather than by matching the page's text. They were once one
  // condition, and turning alerts off silenced the voice — which is invisible in the source unless
  // you already know to look.
  const both = announcementsFor(boxes(1), { ...away, voiceOn: true, alertsOn: true });
  assert.equal(both.say, "b 1 needs a decision.");
  assert.equal(both.notes.length, 1);

  const voiceOnly = announcementsFor(boxes(1), { ...away, voiceOn: true, alertsOn: false });
  assert.equal(voiceOnly.say, "b 1 needs a decision.", "turning alerts off silenced the voice");
  assert.deepEqual(voiceOnly.notes, []);
  assert.equal(voiceOnly.beep, false);

  const alertsOnly = announcementsFor(boxes(1), { ...away, voiceOn: false, alertsOn: true });
  assert.equal(alertsOnly.say, "", "turning the voice off made it speak anyway");
  assert.equal(alertsOnly.notes.length, 1);
  assert.equal(alertsOnly.beep, true);
});

test("nothing to announce with is silence, not an empty backlog", () => {
  // The caller keeps its bookkeeping either way, so turning a channel on tells you what you missed
  // rather than starting from silence. This says "say nothing", not "there is nothing".
  const off = announcementsFor(boxes(3), { ...away, voiceOn: false, alertsOn: false });
  assert.deepEqual(off, { notes: [], say: "", beep: false });
});

test("being at the board is not being interrupted", () => {
  const on = { voiceOn: true, alertsOn: true };
  assert.equal(announcementsFor(boxes(1), { ...on, awayForMs: 0 }).say, "");
  assert.equal(announcementsFor(boxes(1), { ...on, awayForMs: OWED_GRACE_MS - 1 }).say, "");
  assert.equal(announcementsFor(boxes(1), { ...on, awayForMs: OWED_GRACE_MS }).say, "b 1 needs a decision.");
});

test("a sentence already being read is deferred, not swallowed", () => {
  // Deferred, not credited: the caller has not marked these as announced, so they are said on the
  // next tick. Taking the credit here would silence them permanently.
  const on = { ...away, voiceOn: true, alertsOn: true };
  assert.equal(announcementsFor(boxes(1), { ...on, speakingAlready: true }).say, "");
  assert.equal(announcementsFor(boxes(1), on).say, "b 1 needs a decision.");
});

test("past a couple, a count is all anybody can act on", () => {
  // Six banners stacked on a lock screen is the same nag wearing a different hat, and nine box
  // names read aloud is not something anybody hears.
  assert.equal(sentenceFor(boxes(1)), "b 1 needs a decision.");
  assert.equal(sentenceFor(boxes(2)), "b 1 and b 2 need you");
  assert.equal(sentenceFor(boxes(9)), "9 boxes need you");
  assert.equal(sentenceFor([]), "");

  const many = announcementsFor(boxes(6), { ...away, voiceOn: true, alertsOn: true });
  assert.deepEqual(many.notes, [{ body: "6 boxes need you", tag: "skein-owed" }]);
  const few = announcementsFor(boxes(2), { ...away, voiceOn: true, alertsOn: true });
  assert.deepEqual(few.notes.map(n => n.tag), ["b1", "b2"]);
});

test("nothing owed says nothing", () => {
  const on = { ...away, voiceOn: true, alertsOn: true };
  assert.deepEqual(announcementsFor([], on), { notes: [], say: "", beep: false });
  assert.deepEqual(announcementsFor(null, on), { notes: [], say: "", beep: false });
});

// ── the grammar ────────────────────────────────────────────────────────────────────────────────
//
// These moved here from `src/web/index.html`, where they could only be tested by loading a browser.
// They came with a bug attached: the standing-debt channel had stopped calling them, so it read box
// names literally and never said what a box was asking. Everything below is a rule learned out loud
// and each is one somebody would otherwise re-learn (SKEIN-112).

test("a box name is said, not spelled", () => {
  // `example-box-3` read literally is three words run together, and `example-box-1` comes out
  // as a word. Separators become pauses; a letter/digit boundary becomes a space.
  assert.equal(sayName("example-box-3"), "chassis statement parsing");
  assert.equal(sayName("example-box-1"), "era s 6");
  assert.equal(sayName("web_main"), "web main");
  assert.equal(sayName(null), "", "a missing name must not be the string 'null'");
});

test("what is unbearable out loud is taken out of a headline", () => {
  // The single biggest difference between a voice that helps and one that gets muted. Speaking an
  // absolute path costs eight seconds and communicates what the basename communicates for free.
  assert.equal(forSpeech("Run `rm -rf /boxes/smoke/tree/build`?"), "Run rm -rf build?");
  assert.equal(forSpeech("see https://example.com/x for **why**"), "see a link for why");
  // Not every token with a slash is a path: these are words.
  assert.equal(forSpeech("and/or 24/7"), "and/or 24/7");
  // One breath. A long ask is a reason to look, not something to recite.
  const long = forSpeech("a. " + "word ".repeat(80));
  assert.ok(long.length <= 161, `not cut to one breath: ${long.length}`);
  assert.ok(long.endsWith("…"));
});

test("the kind of ask is the part worth hearing", () => {
  // "wants permission" and "needs you to sign in" want completely different things from you, and one
  // "needs a decision" for both is what makes somebody go and look.
  const asking = { name: "example-box-1", blocked_kind: "permission", headline: "Run `rm -rf /b/build`?" };
  assert.equal(utteranceFor(asking, "attn"), "era s 6 wants permission. Run rm -rf build?");
  assert.equal(
    utteranceFor({ name: "a", blocked_kind: "question", headline: "which one?" }, "attn"),
    "a asks. which one?"
  );
  // Trust and auth are self-explaining; the box's last words add length and no information.
  assert.equal(utteranceFor({ name: "a", blocked_kind: "auth", headline: "x" }, "attn"),
    "a needs you to sign in.");
  assert.equal(utteranceFor({ name: "a", pause: "proceed" }, "attn"), "a wants to continue.");
  assert.equal(utteranceFor({ name: "a" }, "done"), "a finished.");
  // The headline is appended as the box said it — no terminal period is added, here or in
  // `waiting`. Pinned because it is the kind of thing a tidy-up would "fix" into a difference.
  assert.equal(utteranceFor({ name: "a", headline: "boom" }, "error"), "a hit an error. boom");
  // `waiting` is not `attn`: nothing is blocking it, so "needs a decision" would overstate the ask.
  assert.equal(utteranceFor({ name: "a", headline: "done for now" }, "waiting"),
    "a is waiting. done for now");
});

test("one owed box is announced with what it is asking", () => {
  // The regression this file exists to stop coming back. `sentenceFor` used to interpolate the raw
  // name and the words "needs you", which is nothing anybody can act on.
  const asking = [{ name: "example-box-1", state: "needs-input", blocked_kind: "permission",
                    headline: "Run `rm -rf /boxes/smoke/tree/build`?" }];
  const groupOf = () => "attn";
  assert.equal(sentenceFor(asking, groupOf), "era s 6 wants permission. Run rm -rf build?");

  const plan = announcementsFor(asking, {
    ...away, voiceOn: true, alertsOn: true, groupOf,
  });
  assert.equal(plan.say, "era s 6 wants permission. Run rm -rf build?");
  // And the NOTIFICATION keeps the real name — it is read and clicked, not spoken, and its tag is
  // what the browser dedupes on.
  assert.deepEqual(plan.notes, [{ body: "example-box-1 needs you", tag: "example-box-1" }]);
});

test("a caller with no group mapping still gets a usable sentence", () => {
  // Every caller that cannot say which group a box is in is asking about one that is owed, so the
  // fallback is `attn` rather than a throw or a bare name.
  assert.equal(
    sentenceFor([{ name: "a", blocked_kind: "trust" }]),
    "a needs you to trust the folder."
  );
});
