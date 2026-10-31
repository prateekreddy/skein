// The decision inbox: every question waiting on the person, as one strip above the board.
//
// **No store of its own.** The rows are `/api/queue`'s, ranked by the server's one comparator
// (`queue::who_needs_you`), and this only chooses which of them are decisions and draws them. A
// second ordering here would be the disagreement `queue.rs` exists to prevent.
//
// **Every card has a way to act on it, and the way is one the page can honour.** A question asked
// with `skein_ask_person`, a package request and a write request each have a panel that answers
// them. A disk ask has none: its card carries Approve and Deny itself, each sending back the ask the
// card was drawn from (`row.disk`), because the box can rewrite its ask between the render and the
// press (SKEIN-1425). The agent's own question on its screen has no such panel — only its terminal can take the
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
  "disk": {
    what: "disk allowance",
    before: "",
    after: "",
    open: "",
    press: "",
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

// The panels a link can open by its hash — the `url` the server puts on each decision row
// (`/#askq`, `/#subq`, `/#gitq`). `/v2` has no panel of its own for these, so its click opens that
// link, and this is what makes it land on the panel that answers the row rather than on the board.
const DECISION_PANELS = ["askq", "subq", "gitq"];
export function panelForHash(hash) {
  const h = String(hash || "").replace(/^#/, "");
  return DECISION_PANELS.includes(h) ? h : null;
}

const decisionEsc = s =>
  String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[c]));

// What a row's card does, or null when the row is not a decision this page can act on.
export function decisionAction(row) {
  if (!row || !row.name) return null;
  if (row.source === "box") return row.blocked === "question" ? DECISION_IN_TERMINAL : null;
  // A disk card's buttons send back the ask it was drawn from, so one without it has nothing to press.
  if (row.source === "disk" && !(row.disk && row.disk.id && row.disk.mb)) return null;
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
    if (r.source === "disk") {
      // `<box> asks for 20 GB disk (now 10 GB, using 12.6 GB): <reason>` — the box's name and reason
      // are its own words; the figures are skein's.
      const ref = ` data-box="${decisionEsc(r.name)}" data-id="${decisionEsc(r.disk.id)}" data-mb="${Number(r.disk.mb) || 0}"`;
      return `<div class="dcard" data-source="disk">`
        + `<div class="sq-meta">${decisionEsc(a.what)} · its reason in its own words, unverified · `
        + `<span class="age" data-secs="${secs}" data-seen="${Number(seen) || 0}">${age}</span></div>`
        + `<div class="aq-body"><b>${decisionEsc(r.name)}</b> ${decisionEsc(r.headline || "")}: ${decisionEsc(r.why || "")}</div>`
        + `<div class="sq-row"><button type="button" class="kbtn primary" data-disk="approve"${ref}>Approve</button>`
        + `<button type="button" class="kbtn ghost" data-disk="deny"${ref}>Deny</button></div>`
        + `</div>`;
    }
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
