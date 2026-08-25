// Which global shortcut a keystroke is, if any. The decision only — acting on it is the caller's,
// because moving the selection and opening a terminal need a DOM and this does not.
//
// **The typing guard is the whole feature.** A shortcut that steals a keystroke in a text field or a
// live terminal is worse than no shortcut: the agent in that terminal needs Escape, and somebody
// naming a box needs every letter. So the guard comes first, before any match, and it returns
// nothing rather than something harmless — "no shortcut" is the answer, not "a shortcut that does
// nothing".
//
// One table, because a keymap written as a `switch` in the page and a guard beside it is two places
// a key can be handled and one place it can be forgotten.

// What a key means when nothing owns the focus. `null` is "not a shortcut".
const FLEET = {
  Escape: "deselect",
  j: "next",
  ArrowDown: "next",
  k: "previous",
  ArrowUp: "previous",
  Enter: "open",
  o: "open",
  d: "diff",
  "?": "keys",
  "/": "filter",
  "]": "next-needs-you",
  L: "load",
  l: "load",
  "[": "previous-session",
  "}": "next-session",
};

// What a key means while the review pane is the surface (SKEIN-151/159, docs/review-ux.md §6).
// Consulted INSTEAD of `FLEET` when `where.pane === "review"`, and a miss here does NOT fall
// through to the fleet table: with the pane open, the fleet's `j`/`k`/`↵`/`d`/`]` would move a
// selection BEHIND the pane and `↵` would navigate out of review entirely — worse than dead keys,
// and data-loss-shaped with a composer open. The one deliberate pass-through is `?`, which names
// the fleet's own `keys` action because the key sheet is one surface whichever pane asks for it
// (and `⌘K`/`⌘N` never reach either table — they are decided before the guard).
//
// One table serves both of the pane's modes (queue and reading view); the page dispatches each
// action by which mode is showing, so a key can never mean a fleet action in one review mode and
// a review action in the other. Three absences are deliberate, and stated because an absent line
// is otherwise indistinguishable from a forgotten one:
//   * `m` (merge) is UNBOUND. It is the one act that cannot be undone from this pane, and on a
//     surface used thirty times a day one letter must not land a commit on a base branch. The
//     merge chip, behind its confirm, is the only way.
//   * `a` maps to `rev-approve`, but in QUEUE mode the page refuses it out loud — you cannot
//     approve from a surface that is not showing you the change. The refusal lives in the
//     dispatcher, not here, because "this key exists and is refused" is a message, where a missing
//     key would be a fleet leak.
//   * `o` opens the thing HERE (the reading view), matching the fleet's own `o` → open. Rebinding
//     it to leave the product would train the wrong reflex; GitHub is the `g h` chord.
const REVIEW = {
  Escape: "rev-back",
  ArrowLeft: "rev-back",
  j: "rev-next",
  ArrowDown: "rev-next",
  k: "rev-previous",
  ArrowUp: "rev-previous",
  n: "rev-next-undecided",
  N: "rev-previous-undecided",
  ArrowRight: "rev-into",
  Enter: "rev-open",
  o: "rev-open",
  e: "rev-aside",
  u: "rev-undo",
  "/": "rev-search",
  g: "rev-chord",
  G: "rev-last",
  c: "rev-comment",
  a: "rev-approve",
  r: "rev-request",
  "]": "rev-next-file",
  "[": "rev-previous-file",
  "?": "keys",
};

// The second key of a `g` chord (`g` then one more). Its own table so the whole decision stays
// here, testable in node — the page remembers only THAT a chord is pending, never what keys mean.
// `g 1`…`g 9` (the nth repo) are matched in code below because nine near-identical lines is where
// a typo hides.
const REVIEW_CHORD = {
  g: "rev-first",
  r: "rev-repo-menu",
  h: "rev-github",
};

// `where` is what the caller knows about focus, which is the part that needs a DOM. Passing it in is
// what makes this testable at all.
export function shortcutFor(event, where) {
  const { inTerm = false, inField = false, dialogOpen = false, pane = "", pending = "" } = where || {};
  const key = (event && event.key) || "";
  const mod = !!(event && (event.metaKey || event.ctrlKey));

  // ⌘K and ⌘N are deliberately *before* the guard: they open the palette and the new-box dialog, and
  // somebody typing in a field is exactly who wants them. Every browser reserves the same pair, so
  // no field is expected to swallow them.
  if (mod && key.toLowerCase() === "k") return "palette";
  if (mod && key.toLowerCase() === "n") return "new-box";
  // Any other modifier chord belongs to the browser or the terminal, not to the fleet keymap.
  if (mod) return null;

  // A dialog owns its own keys; one that leaks to the fleet keymap moves the selection behind it.
  if (dialogOpen) return key === "Escape" ? "close-dialog" : null;

  // The guard. Nothing below this line may fire while somebody is typing.
  if (inTerm || inField) return null;

  if (pane === "review") {
    // A pending `g` consumes the NEXT key whole: `g 1`…`g 9` is the nth repo in the picker's
    // order, the rest are in REVIEW_CHORD — and an unknown second key ends the chord as nothing
    // rather than as a shortcut, so a fumbled chord costs a keystroke, never an act.
    if (pending === "g") return /^[1-9]$/.test(key) ? `rev-repo-${key}` : REVIEW_CHORD[key] || null;
    // No fall-through past this table (SKEIN-151): `?` keeps working because REVIEW names the
    // fleet's `keys` action itself, and every other fleet key is shadowed while the pane is the
    // surface — a key that reached the fleet map here would move a selection behind the pane.
    return REVIEW[key] || null;
  }

  return FLEET[key] || null;
}

// Every action this can return, so a caller that switches on it can be checked for completeness.
export const ACTIONS = [
  ...new Set([
    "palette", "new-box", "close-dialog",
    ...Object.values(FLEET),
    ...Object.values(REVIEW),
    ...Object.values(REVIEW_CHORD),
    ...Array.from({ length: 9 }, (_, i) => `rev-repo-${i + 1}`),
  ]),
];
