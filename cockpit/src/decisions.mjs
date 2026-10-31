// The decision inbox: every question waiting on the person, as one strip above the board.
//
// **No store of its own.** The rows are `/api/queue`'s, ranked by the server's one comparator
// (`queue::who_needs_you`), and this only chooses which of them are decisions and draws them. A
// second ordering here would be the disagreement `queue.rs` exists to prevent.
//
// **Every card has a way to act on it, and the way is one the page can honour.** A question asked
// with `skein_ask_person`, a package request and a write request each have a panel that answers
// them. The agent's own question on its screen has no such panel — only its terminal can take the
// answer — so its card says that and opens the terminal, rather than offering a click that would do
// nothing. A row this file has no action for is not drawn as a card at all: a card with nothing to
// press reads as a decision the cockpit lost.
//
// **Every word a box wrote is escaped, as plain text**, under "its own words, unverified" — the
// wording the owner signed off for a box's question (SKEIN-1060). The question, the packages, the
// repository, the reason and the box's name are all the box's.

const DECISION_ACTIONS = {
  "question": {
    what: "question",
    before: "asked by",
    after: "",
    open: "askq",
    press: "Answer in Questions",
  },
  "package": {
    what: "package",
    before: "",
    after: "asks for it to be installed for the fleet",
    open: "subq",
    press: "Decide in Package requests",
  },
  "git-write": {
    what: "write access",
    before: "",
    after: "asks to write to a repo that is not its own",
    open: "gitq",
    press: "Decide in Repo write access",
  },
};

// The agent's own question, on its screen. Its terminal is the only place it can be answered.
const DECISION_IN_TERMINAL = {
  what: "question",
  before: "",
  after: "is asking in its terminal",
  open: "terminal",
  press: "Open its terminal to answer",
};

const decisionEsc = s =>
  String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[c]));

// What a row's card does, or null when the row is not a decision this page can act on.
export function decisionAction(row) {
  if (!row || !row.name) return null;
  if (row.source === "box") return row.blocked === "question" ? DECISION_IN_TERMINAL : null;
  return DECISION_ACTIONS[row.source] || null;
}

// The rows that are decisions, in the server's order.
export const decisionRows = rows => (Array.isArray(rows) ? rows : []).filter(r => decisionAction(r));

// The strip, as HTML, or "" when nothing is waiting on a decision — the board then hides it.
//
// `seen` is when the rows arrived and `ageText(secs)` the board's own formatter, so the age on a card
// is written the way every other age on the page is, and the board's one-second tick keeps it moving.
export function decisionsHtml(rows, seen, ageText) {
  const cards = decisionRows(rows).map(r => {
    const a = decisionAction(r);
    const secs = r.waiting_secs === null || r.waiting_secs === undefined ? "" : String(r.waiting_secs | 0);
    const age = ageText ? decisionEsc(ageText(secs === "" ? null : Number(secs))) : "";
    const verb = [a.before, `<b>${decisionEsc(r.name)}</b>`, a.after].filter(Boolean).join(" ");
    return `<div class="dcard" data-source="${decisionEsc(r.source)}">`
      + `<div class="sq-meta">${decisionEsc(a.what)} · ${verb} — its own words, unverified · `
      + `<span class="age" data-secs="${secs}" data-seen="${Number(seen) || 0}">${age}</span></div>`
      + `<div class="aq-body">${decisionEsc(r.headline || "")}</div>`
      + (r.why ? `<div class="dc-why">${decisionEsc(r.why)}</div>` : "")
      + `<div class="sq-row"><button type="button" class="kbtn primary" data-open="${decisionEsc(a.open)}" data-box="${decisionEsc(r.name)}">${decisionEsc(a.press)}</button></div>`
      + `</div>`;
  });
  if (!cards.length) return "";
  return `<div class="dhead"><b>Decisions</b><span class="n">${cards.length}</span>`
    + `<span class="sub">waiting on you, longest first</span></div>` + cards.join("");
}
