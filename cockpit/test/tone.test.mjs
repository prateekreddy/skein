import { test } from "node:test";
import assert from "node:assert/strict";
import { toneOf, needsAHuman, headlineOf, TONES } from "../src/tone.mjs";

test("colour says one thing: whether a human is needed", () => {
  assert.equal(toneOf("you"), "warm");
  assert.equal(toneOf("your-attention"), "warm");
  assert.equal(toneOf("machine"), "cool");
  assert.equal(toneOf("done"), "done");
  assert.equal(toneOf("quiet"), "grey");
  assert.equal(toneOf("gone"), "grey");
  // Every tone it can return is one the stylesheet knows about.
  for (const need of ["you", "your-attention", "machine", "done", "quiet", "gone", "nonsense"]) {
    assert.ok(TONES.includes(toneOf(need)), need);
  }
});

test("a need this page has never heard of is grey, not warm", () => {
  // Guessing warm would spend the one thing warm is for. A row nobody can classify is not evidence
  // that somebody is needed.
  assert.equal(toneOf("invented-later"), "grey");
  assert.equal(toneOf(undefined), "grey");
  assert.equal(needsAHuman({ need: "invented-later" }), false);
  assert.equal(needsAHuman(null), false);
});

test("the three states each get words, and calm is said out loud", () => {
  assert.match(headlineOf({ standing: "setup-incomplete", faults: 2 }).title, /2 things to set up/);
  assert.equal(headlineOf({ standing: "setup-incomplete", faults: 1 }).title, "1 thing to set up");
  assert.equal(headlineOf({ standing: "needs-you", rows: 1 }).title, "1 needs you");
  assert.equal(headlineOf({ standing: "needs-you", rows: 3 }).title, "3 need you");
  const calm = headlineOf({ standing: "calm" });
  assert.equal(calm.title, "nothing needs you");
  assert.equal(calm.tone, "grey");
});

test("not having asked yet is not the same as nothing needing you", () => {
  // The failure this prevents is the reassuring one: a board that says "nothing needs you" before
  // the first answer has come back is a board that lies for as long as the request takes.
  const unknown = headlineOf(null);
  assert.notEqual(unknown.title, "nothing needs you");
  assert.equal(unknown.sub, "asking");
});
