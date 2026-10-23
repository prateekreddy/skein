// Which group a box's state belongs to, and what "owed to you" means.
//
// One place, because it was written out three times and they disagreed: the tab title and the
// needs-you navigation counted `waiting` and both voice paths did not — so the title said "3 need
// you" while the mouth stayed shut, and "read what needs me" answered "nothing needs you" with boxes
// visibly waiting on screen.
export const GROUPS = [
  { key: "error", label: "error", match: s => s === "error" },
  { key: "attn", label: "needs you", match: s => ["needs-input", "needs-decision", "blocked"].includes(s) },
  { key: "waiting", label: "waiting", match: s => s === "waiting" },
  { key: "done", label: "done", match: s => s === "done" },
  { key: "working", label: "working", match: s => ["working", "running", "live", "compacting"].includes(s) },
  { key: "ended", label: "ended", match: s => s === "ended" },
  { key: "idle", label: "idle", match: s => s === "idle" },
  { key: "stale", label: "stale", match: s => s === "stale" || s === "unknown" },
];

// An unrecognised state falls to the last group rather than to nothing: a box whose state this page
// has never heard of is a box that is not talking, which is what `stale` means.
export const groupOf = s => (GROUPS.find(g => g.match(s)) || GROUPS[GROUPS.length - 1]).key;

// `waiting` is a box that ended its turn and wants your next instruction, which is the most ordinary
// way a box needs you there is.
export const NEEDS_YOU = ["error", "attn", "waiting"];

export const owedIn = list => list.filter(b => NEEDS_YOU.includes(groupOf(b.state)));

// One word per thing a person can tell apart. The state that waits on a person is "needs you"
// wherever it is read — the pill, the section, a notification — and never "decision" on one and
// "needs-input" (the wire's word) on another (SKEIN-1298). `live` and `running` are both a box at work, and the
// board counts them as "working" — so the pill says so too, rather than ACTIVE on one row and
// WORKING on the next for the same state (SKEIN-1298). `compacting` keeps its own word: it is a
// working box that will not answer for a while, which is worth knowing.
export const labelOf = s =>
  ["needs-input", "needs-decision", "blocked"].includes(s) ? "needs you"
    : s === "live" || s === "running" ? "working" : s;
