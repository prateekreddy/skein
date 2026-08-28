// The identity, as a function: what colour a row is allowed to be, and what the board says it is.
//
// §11.7 settles this once so it is not re-argued per component. The board is **mostly monochrome**,
// and the whole colour budget belongs to one question — *is a human needed?*
//
//   warm   a human is needed
//   cool   the machine is working
//   done   finished, muted green
//   grey   nothing is happening
//
// Four words, learned in one glance. A fifth would spend the budget and mean the first four stop
// being read.
//
// Keyed on `need` and never on `source`, for the reason `queue.rs` merges the two lists server-side:
// a pull request awaiting review and a box awaiting an answer are the same thing to the person
// looking at them, and colouring by which subsystem produced a row is how they stop being.

export const TONES = ["warm", "cool", "done", "grey"];

// Every key quoted, including the ones that need no quotes: these are the words the server sends,
// not identifiers, and `src/cockpit.rs` asserts each `Need` variant appears here by its serialised
// tag. A bare key would make that assertion pass on a coincidence.
const BY_NEED = {
  "you": "warm",
  "your-attention": "warm",
  "done": "done",
  "machine": "cool",
  "quiet": "grey",
  "gone": "grey",
};

// An unknown need is grey rather than warm: a row this page has never heard of is not evidence that
// somebody is needed, and the cost of guessing warm is the one thing warm must never become — noise.
export const toneOf = need => BY_NEED[need] || "grey";

// Is this row one of the ones the board exists to surface? `standing` counts the same two needs, on
// the server, from the same rows — so this narrows what is shown and never decides the headline.
export const needsAHuman = row => toneOf(row && row.need) === "warm";

// The headline for each of the three states (§11.3), and the argument for saying the third out loud:
// a dashboard that looks the same whether or not anything is wrong has failed at its only job, so
// "nothing needs you" is a state with words rather than an empty list nobody notices.
//
// The server decides which state it is, from the rows it built. This only says it in English —
// deriving it here as well would be a second implementation, and the two would disagree on the day
// a rule changes.
export function headlineOf(standing) {
  const s = standing || {};
  if (s.standing === "setup-incomplete") {
    const n = s.faults || 0;
    return { tone: "warm", title: `${n} thing${n === 1 ? "" : "s"} to set up`, sub: "the fleet cannot do the thing you came for until this is fixed" };
  }
  if (s.standing === "needs-you") {
    const n = s.rows || 0;
    return { tone: "warm", title: `${n} need${n === 1 ? "s" : ""} you`, sub: "work has stopped and is waiting on you" };
  }
  if (s.standing === "calm") {
    return { tone: "grey", title: "nothing needs you", sub: "everything running is running on its own" };
  }
  // Not a state — the answer has not arrived. Distinct from calm on purpose: "we have not asked yet"
  // and "we asked and nothing is waiting" are the two things this board must never conflate.
  return { tone: "grey", title: "…", sub: "asking" };
}
