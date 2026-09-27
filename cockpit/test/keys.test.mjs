import { strict as assert } from "node:assert";
import { test } from "node:test";
import { ACTIONS, boundKeys, keySheet, platformKeys, shortcutFor, tabShortcutFor } from "../src/keys.mjs";

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

// ---- the review pane's table (SKEIN-151/159, docs/parity.md §3) -------------------------------

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
    // Shift-R asks for a fresh reading and is never refused: the day's ceiling is on skein's own
    // initiative, not on a person (SKEIN-228).
    ["R", "rev-reread"],
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
  // base branch (docs/parity.md §3, "three deliberate absences"). Chip only.
  assert.equal(shortcutFor(key("m"), REV), null);
  assert.equal(shortcutFor(key("m"), { ...REV, pending: "g" }), null);
  assert.ok(!ACTIONS.some(a => /merge/.test(a)), "an action named merge exists — the absence was filled");
});

// ---- the key sheet (SKEIN-1187) ----------------------------------------------------------------
//
// Settings → Shortcuts and `?` render `keySheet()`. It used to be a second table in the page that
// nothing bound from, and it lacked the fleet's `o`, `[`, `}` and arrow keys while a comment above
// it said the two could not drift. So: every key the tables bind is on the sheet, and every key the
// sheet shows in a bound section does what its row says.
//
// Fails on: adding a key to FLEET (or REVIEW, REVIEW_CHORD, TABS) with no words in SAYS — it is
// bound and missing from the sheet; writing a key into the sheet's Anywhere rows that nothing binds
// — `⌘B` for new box.

const EVENT = { esc: "Escape", "↵": "Enter", "↓": "ArrowDown", "↑": "ArrowUp", "←": "ArrowLeft", "→": "ArrowRight" };
const sheet = () => new Map(keySheet().map(g => [g.sec, g.items]));

test("the sheet shows exactly the keys the fleet table binds", () => {
  const rows = sheet().get("Fleet");
  const shown = rows.flatMap(([keys]) => keys);
  // Every shown key does what its row says, and the row's words are the action's own.
  for (const [keys, says] of rows) {
    for (const k of keys) {
      const action = shortcutFor(key(EVENT[k] || k), {});
      assert.ok(action, `the sheet shows ${k} under Fleet, and it is not bound`);
      const same = rows.find(([, w]) => w === says)[0];
      assert.ok(same.includes(k), `${k} is shown as "${says}"`);
    }
  }
  // Every key the fleet binds is shown — `?` under Anywhere, since it is the sheet itself.
  const fleet = boundKeys().fleet.filter(k => k !== "?");
  assert.ok(fleet.includes("o") && fleet.includes("}"), "the fleet table was not read");
  for (const k of fleet) {
    const printed = Object.entries(EVENT).find(([, v]) => v === k)?.[0] || k;
    assert.ok(shown.includes(printed), `${k} is bound on the board and missing from the sheet`);
  }
  // And the Anywhere rows, which are written out rather than grouped from a table: each is bound.
  const anywhere = { "⌘K": [key("k", { metaKey: true }), {}], "⌘N": [key("n", { metaKey: true }), {}],
                     "?": [key("?"), {}], esc: [key("Escape"), { dialogOpen: true }] };
  for (const [keys, says] of sheet().get("Anywhere")) {
    for (const k of keys) {
      assert.ok(anywhere[k], `the sheet shows ${k} under Anywhere, and nothing here binds it`);
      assert.ok(shortcutFor(...anywhere[k]), `${k} is shown as "${says}" and is not bound`);
    }
  }
});

test("the sheet shows exactly the keys the review table binds, and states the one it does not", () => {
  const rows = sheet().get("Review — the queue");
  const REV = { pane: "review" };
  for (const [keys, says] of rows) {
    if (keys[0] === "m") {
      // Listed so the absence reads as a decision; still unbound.
      assert.equal(shortcutFor(key("m"), REV), null);
      assert.match(says, /unbound/);
      continue;
    }
    if (keys[0] === "g" && keys.length > 1) {
      // A chord: the second key, with `g` pending. `g 1 … g 9` is the nth repo.
      const second = keys.includes("…") ? "1" : keys[1];
      assert.ok(shortcutFor(key(second), { ...REV, pending: "g" }), `g ${second} is shown and not bound`);
      continue;
    }
    for (const k of keys) {
      assert.ok(shortcutFor(key(EVENT[k] || k), REV), `the sheet shows ${k} under review, and it is not bound`);
    }
  }
  const shown = rows.flatMap(([keys]) => keys.join(" "));
  // `g` is the chord's first key and `?` is under Anywhere; every other key is on a row of its own.
  const review = boundKeys().review.filter(k => k !== "g" && k !== "?");
  assert.ok(review.includes("R") && review.includes("ArrowLeft"), "the review table was not read");
  for (const k of review) {
    const printed = Object.entries(EVENT).find(([, v]) => v === k)?.[0] || k;
    assert.ok(rows.some(([keys]) => keys.length === 1 || keys[0] !== "g" ? keys.includes(printed) : false),
      `${k} is bound in the review pane and missing from the sheet: ${JSON.stringify(shown)}`);
  }
  for (const chord of boundKeys().chord.map(k => `g ${k}`)) {
    assert.ok(shown.includes(chord), `the chord ${chord} is bound and missing from the sheet`);
  }
});

test("the tab keys the sheet shows are the tab keys bound", () => {
  const rows = sheet().get("Tabs");
  const ev = shown => {
    const shift = shown.includes("⇧");
    const last = shown.at(-1);
    return last === "[" || last === "]"
      ? { altKey: true, shiftKey: shift, code: last === "[" ? "BracketLeft" : "BracketRight" }
      : { altKey: true, code: `Digit${last}` };
  };
  for (const [keys] of rows) {
    if (keys[0] === "drag") continue;
    for (const k of keys.filter(k => k !== "…")) {
      assert.ok(tabShortcutFor(ev(k)), `the sheet shows ${k} for tabs, and it is not bound`);
    }
  }
  const shown = rows.flatMap(([keys]) => keys);
  for (const k of boundKeys().tabs) {
    assert.ok(shown.includes(k), `${k} is bound for tabs and missing from the sheet`);
    assert.ok(ACTIONS.includes(tabShortcutFor(ev(k))), `${k} is on the tab table and does nothing`);
  }
  assert.equal(tabShortcutFor({ altKey: true, code: "Digit9" }), "tab-9");
  // Only ⌥ chords: the browser reserves ⌘1-9, and a bare digit belongs to whoever is typing.
  assert.equal(tabShortcutFor({ metaKey: true, altKey: true, code: "Digit1" }), null);
  assert.equal(tabShortcutFor({ code: "Digit1" }), null);
});

test("a modifier prints as the keyboard in front of you has it", () => {
  assert.equal(platformKeys("⌘K", true), "⌘K");
  assert.equal(platformKeys("⌘K", false), "Ctrl+K");
  assert.equal(platformKeys("⌥⇧]", false), "Alt+Shift+]");
  // Every occurrence, not the first: a footer names ⌘ more than once.
  assert.equal(platformKeys("⌘↵ posts · ⌘K palette", false), "Ctrl+↵ posts · Ctrl+K palette");
});

