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

// What a key means while the review pane is the surface (SKEIN-151/159, docs/parity.md §3).
// Consulted INSTEAD of `FLEET` when `where.pane === "review"`, and a miss here does NOT fall
// through to the fleet table: with the pane open, the fleet's `j`/`k`/`↵`/`d`/`]` would move a
// selection BEHIND the pane and `↵` would navigate out of review entirely — worse than dead keys,
// and data-loss-shaped with a composer open. The one deliberate pass-through is `?`, which names
// the fleet's own `keys` action because the key sheet is one surface whichever pane asks for it
// (and `⌘K`/`⌘N` never reach either table — they are decided before the guard).
//
// **One table, and now one mode.** This block used to say the table served two — a queue and a
// reading view of skein's own — with the page dispatching each action by whichever was showing.
// CKP-7 removed the reading view, so `where.pane === "review"` names a single surface and there is
// no second mode for a key to mean something else in. Four entries are deliberate and are stated
// because an odd line is otherwise indistinguishable from a forgotten one:
//   * `m` (merge) is UNBOUND. It is the one act that cannot be undone from this pane, and on a
//     surface used thirty times a day one letter must not land a commit on a base branch. The
//     merge chip, behind its confirm, is the only way.
//   * `a` maps to `rev-approve` and the page refuses it out loud, unconditionally — you cannot
//     approve from a surface that is not showing you the change, and with the reading view gone
//     there is no surface here that does. The refusal lives in the dispatcher, not here, because
//     "this key exists and is refused" is a message, where a missing key would be a fleet leak.
//   * `o` opens the thing HERE, matching the fleet's own `o` → open. What it opens is the ROW:
//     the reading, the verdicts and the composer are all in the expansion. Rebinding it to leave
//     the product would train the wrong reflex; GitHub is the `g h` chord.
//   * `c`, `r`, `]` and `[` addressed hunks and files in the reading view and outlived it. They
//     stay bound, each answering with a sentence, because being in THIS table is what shadows them
//     from `FLEET` (SKEIN-568) — unbinding them would let a keystroke move a selection behind the
//     pane, which is the leak the paragraph above exists to stop.
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
  // Shift-R, beside `r`, and a capital because it spends nothing and undoes nothing: it asks skein
  // to read the selected pull request again against the commit that is there now (SKEIN-228). The
  // day's ceiling is on skein's own initiative, never on a person, so this key is never refused.
  R: "rev-reread",
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

// The tab keys, which must work *while you are typing in the agent*, so they are decided apart from
// the tables above (which yield every key to a focused terminal). ⌥ chords only: the browser
// reserves ⌘1-9 and ⌃Tab, and the agents' own composers use ⌥←/⌥→ for word movement — brackets are
// free in all of them. Matched on `code`, since ⌥[ on macOS reports `key` as "“". Keyed by what the
// key sheet prints, so the sheet and the binding are this one table (SKEIN-1187); `⌥1`…`⌥9` are
// matched in code below for the reason `g 1`…`g 9` are.
const TABS = {
  "⌥[": "tab-previous",
  "⌥]": "tab-next",
  "⌥⇧[": "tab-move-left",
  "⌥⇧]": "tab-move-right",
};

// Which tab key an event is, if any — `tab-1`…`tab-9` (the nth tab; past the end is the last), or
// one of TABS.
export function tabShortcutFor(event) {
  if (!event || !event.altKey || event.metaKey || event.ctrlKey) return null;
  const digit = /^Digit([1-9])$/.exec(event.code || "");
  if (digit) return `tab-${digit[1]}`;
  const bracket = { BracketLeft: "[", BracketRight: "]" }[event.code];
  if (!bracket) return null;
  return TABS[`⌥${event.shiftKey ? "⇧" : ""}${bracket}`] || null;
}

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
    ...Object.values(TABS),
    ...Array.from({ length: 9 }, (_, i) => `tab-${i + 1}`),
    ...Array.from({ length: 9 }, (_, i) => `rev-repo-${i + 1}`),
  ]),
];

// ---- the key sheet: Settings → Shortcuts and `?` (SKEIN-1187) --------------------------------
//
// **Rendered from the tables above, not written beside them.** The page kept a second table for the
// sheet, which nothing bound from, and it had drifted: the fleet's `o`, `[`, `}` and the arrow keys
// were bound here and missing there, under a comment saying the two "can't drift". So the sheet is
// built from FLEET, REVIEW, REVIEW_CHORD and TABS, and all a key needs to appear on it is the words
// in SAYS; `keys.test.mjs` holds the sheet equal to what those tables bind.

// What each action does, in the words the sheet prints. Actions that share words share a row, so
// `j`/`↓` and `k`/`↑` are one line and not four.
const SAYS = {
  palette: "command palette",
  "new-box": "new box",
  "close-dialog": "close a dialog",
  keys: "this list",
  deselect: "leave the terminal, back to the board",
  next: "move the selection",
  previous: "move the selection",
  open: "open the selected box's terminal",
  diff: "open its diff",
  filter: "filter the board by name, branch, repo or headline",
  "next-needs-you": "jump to the next box that needs you",
  load: "load by box — which one is using the CPU",
  "previous-session": "previous open session",
  "next-session": "next open session",
  "rev-next": "move the selection (inside an open stack, along its steps)",
  "rev-previous": "move the selection (inside an open stack, along its steps)",
  "rev-next-undecided": "next / previous row you have not decided this session",
  "rev-previous-undecided": "next / previous row you have not decided this session",
  "rev-into": "enter the selected stack",
  "rev-back": "fold the open row, or leave the stack, back at its head",
  "rev-open": "open the selected row — o opens things here; GitHub is g h",
  "rev-aside": "set aside — the row greys in place, u takes it back within 8s",
  "rev-undo": "undo the held act",
  "rev-reread": "read this one again, against the commit that is there now",
  "rev-search": "find a pull request",
  "rev-last": "last row",
  "rev-first": "first row",
  "rev-repo-menu": "the repo picker",
  "rev-github": "open it on GitHub",
  "rev-approve": "refused here, deliberately — you cannot approve from a surface that is not showing you the change; ↵ opens the row, and approve is a chip in it",
  "rev-comment": "says where comment went — a chip on the row",
  "rev-request": "says where request changes went — a chip on the row",
  "rev-next-file": "say where the diff went — g h opens the change on GitHub",
  "rev-previous-file": "say where the diff went — g h opens the change on GitHub",
  "tab-previous": "previous tab",
  "tab-next": "next tab",
  "tab-move-left": "move the current tab left / right",
  "tab-move-right": "move the current tab left / right",
};

// How a key's name prints. Anything not here prints as itself.
const SHOWN = { Escape: "esc", Enter: "↵", ArrowDown: "↓", ArrowUp: "↑", ArrowLeft: "←", ArrowRight: "→" };

// Rows for one table: its keys grouped by the words their actions share, in the table's order.
function rowsOf(table, prefix = []) {
  const rows = new Map();
  for (const [key, action] of Object.entries(table)) {
    const says = SAYS[action];
    if (!says) continue;
    if (!rows.has(says)) rows.set(says, []);
    rows.get(says).push(...prefix, SHOWN[key] || key);
  }
  return [...rows].map(([says, keys]) => [keys, says]);
}

// **The key sheet**, as sections of `[keys, words]` rows. Four sections are the binding tables
// themselves; the last two are said here because they are not skein's to bind — the terminal's own
// keys and a drag — and a sheet that dropped them would leave people guessing at ⌘C.
//
// Written with the Mac's ⌘/⌥/⇧/⌃; `platformKeys` turns them into what another keyboard has on it.
export function keySheet() {
  return [
    { sec: "Anywhere", items: [
      [["⌘K"], SAYS.palette],
      [["⌘N"], SAYS["new-box"]],
      [["?"], SAYS.keys],
      [["esc"], SAYS["close-dialog"]],
    ] },
    { sec: "Fleet", items: rowsOf(FLEET).filter(([keys]) => keys[0] !== "?") },
    { sec: "Review — the queue", items: [
      ...rowsOf(REVIEW).filter(([keys]) => keys[0] !== "?"),
      [["g", "1", "…", "g", "9"], "the nth repo in the picker's order"],
      ...rowsOf(REVIEW_CHORD, ["g"]),
      // The one deliberate absence, LISTED rather than omitted: a sheet that silently lacks `m`
      // reads as a sheet that forgot it. `shortcutFor` returns nothing for it, and a test says so.
      [["m"], "unbound, deliberately — merging cannot be undone from this pane, so one letter must never land a commit on a base branch; the merge chip asks first"],
    ] },
    { sec: "Tabs", items: [
      [["⌥1", "…", "⌥9"], "switch to the nth open tab (⌥9 is always the last)"],
      ...rowsOf(TABS),
      [["drag"], "reorder tabs by dragging one — esc cancels, and the order is remembered"],
    ] },
    { sec: "In a terminal", items: [
      [["⌘C"], "copy the selection"],
      [["⌘V"], "paste"],
      [["⌃C"], "interrupt — goes through to the agent, never intercepted"],
      [["⌘V", "file"], "paste a file, or drag one in, to hand it to the agent"],
    ] },
  ];
}

// Every key the tables bind, by table — what `keys.test.mjs` holds the sheet to, so a key added to
// a table and not to the sheet fails there rather than going undocumented.
export function boundKeys() {
  return {
    fleet: Object.keys(FLEET),
    review: Object.keys(REVIEW),
    chord: Object.keys(REVIEW_CHORD),
    tabs: Object.keys(TABS),
  };
}

// ⌘/⌥/⇧/⌃ are Mac glyphs; away from a Mac the same bindings are Ctrl/Alt/Shift (the handlers
// accept either), so print what that keyboard actually has on it. Every place the cockpit names a
// modifier goes through this — the sheet, the footer, the buttons' hints — so a Linux user is not
// told ⌘K on a button and Ctrl+K in the sheet.
export function platformKeys(text, mac) {
  return mac ? text
    : String(text).replaceAll("⌘", "Ctrl+").replaceAll("⌃", "Ctrl+").replaceAll("⌥", "Alt+").replaceAll("⇧", "Shift+");
}

