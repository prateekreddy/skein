// Hiding a warning at the top of the cockpit for a while (SKEIN-1244), as the decisions it rests on.
//
// The owner's ask: "the warning that come at the top should be dismissable, they can come back after
// some time but there should be a way to dismiss them temporarily". So a dismissal is two facts —
// WHAT was hidden and UNTIL WHEN — and the page shows a row again the moment either stops holding.
//
// **What was hidden is a list of problems, never the row's text.** Each row says something that
// changes while the problem does not: the staleness row counts seconds, the login row says when the
// refusal was heard, a health check's sentence can carry a percentage. A dismissal keyed on the text
// would come undone on the next poll, and one keyed on nothing would hide a second, different
// problem behind the first. So each row names its problems as stable ids, and a dismissal covers the
// ids that were on the row when it was pressed: a row whose every problem was dismissed stays
// hidden, and a row carrying one problem that was not shows at once. A problem that went away and
// left the others is not new, so the row stays hidden.
//
// Leaf functions — all inputs as arguments, no DOM, no storage — so the rule is tested in node
// (cockpit/test/dismiss.test.mjs) and the page only stores and draws.

const HOUR = 60 * 60 * 1000;

// The three lengths a dismissal can have, in the order the menu offers them. The first is what a
// plain press of ✕ means.
export const DISMISS_CHOICES = [
  ["hour", "for 1 hour"],
  ["hours4", "for 4 hours"],
  ["tomorrow", "until tomorrow"],
];

// When a dismissal made `now` (ms since the epoch) stops holding. "until tomorrow" is the start of
// the next day on this browser's own clock — local midnight, the moment it IS tomorrow. Anything
// this does not know is an hour, the shortest, so a mistake costs the least hiding.
export function dismissUntil(choice, now) {
  if (choice === "hours4") return now + 4 * HOUR;
  if (choice === "tomorrow") {
    const next = new Date(now);
    next.setHours(24, 0, 0, 0);
    return next.getTime();
  }
  return now + HOUR;
}

// Is the row whose problems are `ids` still covered by `record` (what storage held, or null)?
//
// Every doubt answers "no", because a wrong "no" costs a row a person already chose to hide, and a
// wrong "yes" hides a warning nobody chose to hide. So: no record, a record of the wrong shape, one
// that has run out, a row with no problems named, or a record reaching further ahead than the
// longest choice can (a clock that was wrong when it was written) all show the row.
export function stillDismissed(record, ids, now) {
  if (!record || typeof record !== "object") return false;
  if (!Array.isArray(record.ids) || typeof record.until !== "number") return false;
  if (!(now < record.until)) return false;
  if (record.until - now > 25 * HOUR) return false;
  if (!Array.isArray(ids) || !ids.length) return false;
  const had = new Set(record.ids);
  return ids.every(id => had.has(id));
}

// The environment row's problems: each failed check the report COUNTS, by its key, and each box
// named for having gone dark or being on old probes. Not the uncounted checks — they cannot raise
// the row on their own, so they are not what a person hid it from — and not a check that could not
// be answered, which is not a fault. Never the detail sentence, which can carry a changing number.
export function healthProblems(health) {
  const h = health || {};
  const counted = new Set(h.counted || []);
  const keys = (h.labels || []).map(l => l.key);
  return [
    ...keys.filter(k => counted.has(k) && h[k]?.level === "unsatisfied").map(k => `check:${k}`),
    ...(h.dark_boxes || []).map(b => `dark:${b}`),
    ...(h.stale_boxes || []).map(b => `stale:${b}`),
  ];
}

// The login row's problems: one per runtime whose login is dead. Not when it died or how skein
// found out — a second refusal of the same login is the same problem, and its new time is exactly
// the changing text a dismissal must not be keyed on.
export function loginProblems(expired) {
  return (expired || []).map(l => `login:${l.runtime}`);
}

// The merge train's problems: one per pull request it stopped on, by repo and number. Why it
// stopped is the changing part and is left out; "and workflows are paused" qualifies the stops
// rather than being one.
export function trainProblems(stops) {
  return (stops || []).flatMap(c => (c.stopped || []).map(s => `${c.repo_id}#${s.number}`));
}

// The staleness row's one problem: THIS outage, named by the moment the server last spoke. Every
// poll of a single outage names the same moment however many seconds the row counts, and the board
// going live moves it — so a dismissal lasts for one outage, or the time chosen, whichever ends
// first, and the next outage shows at once.
export function staleProblems(lastWordAt) {
  return [`quiet-since:${lastWordAt}`];
}
