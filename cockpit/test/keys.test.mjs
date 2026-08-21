import { strict as assert } from "node:assert";
import { test } from "node:test";
import { ACTIONS, shortcutFor } from "../src/keys.mjs";

const key = (k, extra = {}) => ({ key: k, ...extra });

test("a shortcut never fires while somebody is typing", () => {
  // The whole feature. A shortcut that steals a keystroke in a text field is worse than no
  // shortcut, and the agent in a live terminal needs Escape.
  for (const where of [{ inField: true }, { inTerm: true }]) {
    for (const k of ["Escape", "j", "k", "Enter", "ArrowDown", "ArrowUp"]) {
      assert.equal(shortcutFor(key(k), where), null, `${k} fired while typing`);
    }
  }
  // And with focus nowhere in particular, they do.
  assert.equal(shortcutFor(key("Escape"), {}), "deselect");
  assert.equal(shortcutFor(key("j"), {}), "next");
  assert.equal(shortcutFor(key("ArrowUp"), {}), "previous");
  assert.equal(shortcutFor(key("Enter"), {}), "open");
});

test("the two chords every browser reserves reach a field anyway", () => {
  // ⌘K and ⌘N open the palette and the new-box dialog, and somebody typing in a field is exactly
  // who wants them — no field is expected to swallow those.
  assert.equal(shortcutFor(key("k", { metaKey: true }), { inField: true }), "palette");
  assert.equal(shortcutFor(key("n", { ctrlKey: true }), { inTerm: true }), "new-box");
  assert.equal(shortcutFor(key("K", { metaKey: true }), {}), "palette", "case does not matter");
  // Without the modifier they are ordinary letters, and `k` is a movement key.
  assert.equal(shortcutFor(key("k"), {}), "previous");
  assert.equal(shortcutFor(key("n"), {}), null);
});

test("a dialog owns its own keys", () => {
  // A key that leaks to the fleet keymap from an open dialog moves the selection behind it.
  assert.equal(shortcutFor(key("j"), { dialogOpen: true }), null);
  assert.equal(shortcutFor(key("Escape"), { dialogOpen: true }), "close-dialog");
  // And the reserved chords still work, because they are checked first.
  assert.equal(shortcutFor(key("k", { metaKey: true }), { dialogOpen: true }), "palette");
});

test("an unrecognised key is no shortcut rather than a harmless one", () => {
  assert.equal(shortcutFor(key("q"), {}), null);
  assert.equal(shortcutFor(key(""), {}), null);
  assert.equal(shortcutFor(null, {}), null);
  assert.equal(shortcutFor(key("j"), null), "next", "no focus information is not a text field");
  // A modifier chord that is not one of the two reserved ones belongs to the browser or the
  // terminal — `⌘D` must not be read as the fleet's `d`.
  assert.equal(shortcutFor(key("d", { metaKey: true }), {}), null);
  assert.equal(shortcutFor(key("l", { ctrlKey: true }), {}), null, "^L is the terminal's");
});

test("every key the board dispatches is named here", () => {
  // One table, because a keymap written as a `switch` in the page and a guard beside it is two
  // places a key can be handled and one place it can be forgotten.
  const named = new Map([
    ["Escape", "deselect"], ["j", "next"], ["ArrowDown", "next"],
    ["k", "previous"], ["ArrowUp", "previous"], ["Enter", "open"], ["o", "open"],
    ["d", "diff"], ["?", "keys"], ["/", "filter"], ["]", "next-needs-you"],
    ["L", "load"], ["l", "load"], ["[", "previous-session"], ["}", "next-session"],
  ]);
  for (const [k, action] of named) {
    assert.equal(shortcutFor(key(k), {}), action, `${k}`);
    assert.equal(shortcutFor(key(k), { inField: true }), null, `${k} fired while typing`);
  }
  for (const action of named.values()) {
    assert.ok(ACTIONS.includes(action), `${action} is not in ACTIONS`);
  }
});
