// The board's filter, as a function of what it is given.
//
// `foreign:` is the one term that changes WHICH boxes are eligible rather than narrowing them. A
// sandbox skein did not place is not on the board's tick at all any more — it is fetched when
// somebody asks — so the eligible pool is passed in rather than filtered out of the board's rows.

export const FOREIGN_TERM = "foreign:";
export const wantsForeign = q => (q || "").includes(FOREIGN_TERM);
export const withoutForeignTerm = q => (q || "").split(FOREIGN_TERM).join(" ").trim();

// `managed:` is the other half of that pair and it is deliberately NOT the same kind of term.
//
// A managed box — one skein started itself, to review a pull request — is skein's own box in
// skein's own sandbox, and it can get stuck or ask a question like any other. So it is on the board
// by default and `managed:` NARROWS to those rows; it does not reveal them. Hiding them the way
// `foreign:` hides a stranger's sandbox would mean a box spending model calls where nobody can see
// it, which is the one failure a board exists to prevent.
export const MANAGED_TERM = "managed:";
export const wantsManaged = q => (q || "").includes(MANAGED_TERM);
export const withoutManagedTerm = q => (q || "").split(MANAGED_TERM).join(" ").trim();

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
  // `foreign:` swaps the pool; `managed:` only narrows the one already chosen. A row is never both
  // — a sandbox skein did not place has no placement record, and the purpose is written in that
  // record — so the two terms compose without either having to know about the other.
  const eligible = pool
    .filter(b => (wantsForeign(q) ? b.foreign : !b.foreign))
    .filter(b => (wantsManaged(q) ? !!b.managed : true));
  const text = withoutManagedTerm(withoutForeignTerm(q));
  return text ? eligible.filter(b => matchesFilter(b, text)) : eligible;
}
