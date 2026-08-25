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

// ---- the review pane's table (SKEIN-151/159, docs/review-ux.md §6) ----------------------------

const REV = { pane: "review" };

test("with the review pane the surface, review keys mean review actions", () => {
  const named = new Map([
    ["Escape", "rev-back"], ["ArrowLeft", "rev-back"],
    ["j", "rev-next"], ["ArrowDown", "rev-next"],
    ["k", "rev-previous"], ["ArrowUp", "rev-previous"],
    ["n", "rev-next-undecided"], ["N", "rev-previous-undecided"],
    ["ArrowRight", "rev-into"],
    ["Enter", "rev-open"], ["o", "rev-open"],
    ["e", "rev-aside"], ["u", "rev-undo"], ["/", "rev-search"],
    ["g", "rev-chord"], ["G", "rev-last"],
    ["c", "rev-comment"], ["a", "rev-approve"], ["r", "rev-request"],
    ["]", "rev-next-file"], ["[", "rev-previous-file"],
  ]);
  for (const [k, action] of named) {
    assert.equal(shortcutFor(key(k), REV), action, k);
    assert.ok(ACTIONS.includes(action), `${action} is not in ACTIONS`);
  }
});

test("no keypress in the review pane reaches the fleet map behind it", () => {
  // SKEIN-151's done-when. With boxes present the fleet's j moved a selection BEHIND the pane and
  // ↵ navigated out of review entirely — so with pane === "review", no key may resolve to a fleet
  // action. `keys` (the ? sheet) is the one deliberate exception: one sheet, whichever pane asks.
  const fleet = ["deselect", "next", "previous", "open", "diff", "filter",
                 "next-needs-you", "load", "previous-session", "next-session"];
  for (const k of ["Escape", "j", "ArrowDown", "k", "ArrowUp", "Enter", "o", "d", "?", "/",
                   "]", "L", "l", "[", "}", "m", "q", "x"]) {
    const got = shortcutFor(key(k), REV);
    assert.ok(!fleet.includes(got), `${k} leaked to the fleet as ${got}`);
  }
  // The keys the fleet binds and review does not are shadowed to nothing, not passed through.
  assert.equal(shortcutFor(key("d"), REV), null);
  assert.equal(shortcutFor(key("l"), REV), null);
  assert.equal(shortcutFor(key("}"), REV), null);
});

test("the pass-throughs the pane promises still work", () => {
  // ⌘K / ⌘N are decided before any table; ? names the fleet's own key-sheet action.
  assert.equal(shortcutFor(key("k", { metaKey: true }), REV), "palette");
  assert.equal(shortcutFor(key("n", { ctrlKey: true }), REV), "new-box");
  assert.equal(shortcutFor(key("?"), REV), "keys");
});

test("the typing guard holds in the review pane too", () => {
  for (const k of ["j", "Enter", "e", "a", "u", "/"]) {
    assert.equal(shortcutFor(key(k), { ...REV, inField: true }), null, `${k} fired while typing`);
  }
});

test("a g chord consumes the next key whole", () => {
  const pending = { ...REV, pending: "g" };
  for (let n = 1; n <= 9; n++) {
    assert.equal(shortcutFor(key(String(n)), pending), `rev-repo-${n}`);
  }
  assert.equal(shortcutFor(key("g"), pending), "rev-first");
  assert.equal(shortcutFor(key("r"), pending), "rev-repo-menu");
  assert.equal(shortcutFor(key("h"), pending), "rev-github");
  // An unknown second key ends the chord as nothing — never as a shortcut, and never as the
  // fleet's meaning of that key.
  assert.equal(shortcutFor(key("j"), pending), null);
  assert.equal(shortcutFor(key("q"), pending), null);
  // The chord belongs to the review pane; elsewhere g is not a shortcut at all.
  assert.equal(shortcutFor(key("g"), {}), null);
});

test("merge is unbound, deliberately", () => {
  // The one act that cannot be undone from this pane; one letter must not land a commit on a
  // base branch (docs/review-ux.md §6, "three deliberate absences"). Chip only.
  assert.equal(shortcutFor(key("m"), REV), null);
  assert.equal(shortcutFor(key("m"), { ...REV, pending: "g" }), null);
  assert.ok(!ACTIONS.some(a => /merge/.test(a)), "an action named merge exists — the absence was filled");
});
