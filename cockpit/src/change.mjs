// The change view's decisions (§11.1): what a module's note is worth showing, and what a signal's
// count is allowed to say.
//
// The job is **not** reading a diff. What matters is which modules changed, how the design
// decomposes now, and being able to drill to code when something warrants it — mostly it is not
// read at all. So these are the three judgements that turn `shape::of_diff`'s answer into a screen,
// and each is here rather than in the page because each is a rule somebody could get wrong quietly.

// The one-line summary above the modules: how many moved, and how much.
//
// Summed from the modules rather than taken from a field, because there is no such field — and a
// count computed in two places is the disagreement §11.7 exists to end.
export function summaryOf(modules) {
  const list = modules || [];
  return {
    modules: list.length,
    added: list.reduce((n, m) => n + (m.added || 0), 0),
    removed: list.reduce((n, m) => n + (m.removed || 0), 0),
  };
}

// A module's standing note, and what to say when there is not one worth showing.
//
// **A stale note is not shown.** Its freshness is keyed to the commit the module was at, and a
// description of code that has since changed is worse than no description because it reads exactly
// like a current one. The three states are three different offers: read it, rewrite it, write one.
export function noteOf(module) {
  const m = module || {};
  if (m.note_state === "fresh" && m.note) return { state: "fresh", text: m.note };
  if (m.note_state === "stale") return { state: "stale", text: "its note is older than the code — rewrite it before trusting it" };
  return { state: "absent", text: "no note yet" };
}

// What a contract signal's count says, or nothing at all.
//
// **Absent and zero are different answers and must read differently.** `mentions` is missing when
// the signal's detector could not name a symbol — a deleted file, a rename — and "0 mentions" would
// read as "nothing uses this", which is the opposite of "we did not look". Zero is kept when it is
// real: a symbol the change introduces is named nowhere in the base, which is worth seeing.
//
// "mentions", not "call sites": the count is lines of the base tree naming the symbol, and `git
// grep` cannot tell a call from a comment. A reviewer who trusts "12 call sites" and finds four is
// worse off than one who was told what was counted.
export function mentionsLabel(signal) {
  const n = signal && signal.mentions;
  if (n === null || n === undefined) return "";
  return `${n} mention${n === 1 ? "" : "s"}`;
}

// Where the shape of a change comes from, for the two ways one can arrive.
//
// One function, because the shape of a change does not depend on whether it came as a pull request
// or as a branch somebody is still working on — the server already serves both from one, and a page
// that decided differently would be a third opinion about the same question.
//
// **The pull-request URL is spelled the way the router spells it, not the way the feature reads.**
// A pull request's shape is one of the review routes — `/api/repos/:id/review/:number/…`, beside
// `diff` and `act` — and `:id` is the registered repo's id, which is exactly what a queue row
// carries in `repo` (`queue.rs:233`). This function shipped asking `/api/pr/:repo/:n/shape`, a route
// nothing has ever registered, so from `667e4a2` onward every click on a pull request 404'd and the
// page reported its own "the change could not be read" (SKEIN-246). The tests that let that survive
// asserted this string; what asserts it now is the router's own table — `cockpit_routes` in
// `bin/skein-server.rs` scans this bundle against it, and `the_change_view_asks_a_url_this_router_answers`
// runs this function and matches its answer.
export function shapeUrl(row) {
  const r = row || {};
  if (r.source === "box") return `/api/boxes/${encodeURIComponent(r.name)}/shape`;
  if (r.source === "pull-request") {
    const number = String(r.name || "").replace(/^#/, "");
    if (!r.repo || !number) return "";
    return `/api/repos/${encodeURIComponent(r.repo)}/review/${encodeURIComponent(number)}/shape`;
  }
  return "";
}
