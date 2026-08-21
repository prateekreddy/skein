import { strict as assert } from "node:assert";
import { test } from "node:test";
import { announcementsFor, sentenceFor, OWED_GRACE_MS } from "../src/announce.mjs";

const boxes = n => Array.from({ length: n }, (_, i) => ({ name: `b${i + 1}` }));
const away = { awayForMs: OWED_GRACE_MS + 1 };

test("voice and alerts are independent switches", () => {
  // The property, tested by calling rather than by matching the page's text. They were once one
  // condition, and turning alerts off silenced the voice — which is invisible in the source unless
  // you already know to look.
  const both = announcementsFor(boxes(1), { ...away, voiceOn: true, alertsOn: true });
  assert.equal(both.say, "b1 needs you");
  assert.equal(both.notes.length, 1);

  const voiceOnly = announcementsFor(boxes(1), { ...away, voiceOn: true, alertsOn: false });
  assert.equal(voiceOnly.say, "b1 needs you", "turning alerts off silenced the voice");
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
  assert.equal(announcementsFor(boxes(1), { ...on, awayForMs: OWED_GRACE_MS }).say, "b1 needs you");
});

test("a sentence already being read is deferred, not swallowed", () => {
  // Deferred, not credited: the caller has not marked these as announced, so they are said on the
  // next tick. Taking the credit here would silence them permanently.
  const on = { ...away, voiceOn: true, alertsOn: true };
  assert.equal(announcementsFor(boxes(1), { ...on, speakingAlready: true }).say, "");
  assert.equal(announcementsFor(boxes(1), on).say, "b1 needs you");
});

test("past a couple, a count is all anybody can act on", () => {
  // Six banners stacked on a lock screen is the same nag wearing a different hat, and nine box
  // names read aloud is not something anybody hears.
  assert.equal(sentenceFor(boxes(1)), "b1 needs you");
  assert.equal(sentenceFor(boxes(2)), "b1 and b2 need you");
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
