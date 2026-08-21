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

// `where` is what the caller knows about focus, which is the part that needs a DOM. Passing it in is
// what makes this testable at all.
export function shortcutFor(event, where) {
  const { inTerm = false, inField = false, dialogOpen = false } = where || {};
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

  return FLEET[key] || null;
}

// Every action this can return, so a caller that switches on it can be checked for completeness.
export const ACTIONS = [
  ...new Set(["palette", "new-box", "close-dialog", ...Object.values(FLEET)]),
];
