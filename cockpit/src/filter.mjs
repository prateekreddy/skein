// The board's filter, as a function of what it is given.
//
// `foreign:` is the one term that changes WHICH boxes are eligible rather than narrowing them. A
// sandbox skein did not place is not on the board's tick at all any more — it is fetched when
// somebody asks — so the eligible pool is passed in rather than filtered out of the board's rows.

export const FOREIGN_TERM = "foreign:";
export const wantsForeign = q => (q || "").includes(FOREIGN_TERM);
export const withoutForeignTerm = q => (q || "").split(FOREIGN_TERM).join(" ").trim();

// Does this row match the words typed? Every word must appear somewhere, so adding one narrows.
export function matchesFilter(b, q) {
  if (!q) return true;
  const hay = `${b.name} ${b.branch || ""} ${b.repo || ""} ${b.headline || ""}`.toLowerCase();
  return q.split(/\s+/).filter(Boolean).every(t => hay.includes(t));
}

// Which rows the board draws, for a fleet, a filter, and the foreign rows if any have been fetched.
//
// **`foreign` is an argument.** It used to be module state the function reached out for, which is
// exactly what made this untestable — a pure function is one whose inputs are all in front of you.
//
// Eligibility first, then the text match over what is left, because `foreign:` answers "which boxes"
// and the rest answers "which of those".
export function boardRows(all, rawFilter, foreign) {
  const q = (rawFilter || "").trim().toLowerCase();
  const pool = wantsForeign(q) ? (foreign || []) : (all || []);
  const eligible = pool.filter(b => (wantsForeign(q) ? b.foreign : !b.foreign));
  const text = withoutForeignTerm(q);
  return text ? eligible.filter(b => matchesFilter(b, text)) : eligible;
}
