// What the fleet costs, rendered — SKEIN-835's surface half.
//
// **The whole panel is a pure function of the payload.** `usageHtml` takes the reading, the clock
// and an escaper, and returns markup; the page's job is to fetch, to call this, and to bind one
// button. That is §13's law rather than a preference: the rendering is where every judgement in
// this panel lives — which caveat shows, whether a reading is called stale, what an empty list
// says — and none of it can be tested if it is spelled out inside an `innerHTML` in a 11,700-line
// page.
//
// `esc` is an argument because these modules are concatenated into one script and may not import
// (cockpit/build.mjs refuses one that does). The page's own `esc` (src/web/index.html:1889) is the
// one it is called with, so there is one escaper in the product and no second copy to drift.
//
// **The numbers this was built against**, so the shapes below are not designed on lorem: a fleet
// reading on 2026-09-12 across 16 boxes and 1,313 transcripts came to $30,477.56 over 44.6B tokens,
// of which 98% were cache reads.

// An hour, which is the ceiling the owner set on re-reading: never on page load, on demand, and
// otherwise at most hourly. A reading older than this is still shown — it is the only reading
// there is — but it is shown as old rather than as current.
export const USAGE_STALE_SECS = 3600;

// Money. Hand-grouped rather than `toLocaleString`, because the default locale of whoever opens the
// cockpit is not a thing this panel should render differently: `30.477,56` and `30,477.56` are the
// same reading and only one of them can be asserted in a test.
export function usd(n) {
  if (n === null || n === undefined || Number.isNaN(Number(n))) return "—";
  const v = Number(n);
  const sign = v < 0 ? "-" : "";
  const [whole, cents] = Math.abs(v).toFixed(2).split(".");
  return `${sign}$${whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}.${cents}`;
}

// Exact counts, grouped. Used where the count IS the point — an unpriced model's tokens, the
// number of transcripts read — and a reader who cannot tell 12,345 from 12.3K has lost the fact.
export function fmtExact(n) {
  if (n === null || n === undefined || Number.isNaN(Number(n))) return "—";
  return Math.round(Number(n)).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

// Compact counts, for the ones nobody reads digit by digit. Two formatters on purpose, the same
// argument sizes.mjs makes: one that takes "a number" is one that eventually gets handed the wrong
// one, and 44,600,000,000 in a table cell is a number nobody reads at all.
export function fmtTok(n) {
  if (n === null || n === undefined || Number.isNaN(Number(n))) return "—";
  const v = Number(n);
  if (Math.abs(v) >= 1e9) return `${(v / 1e9).toFixed(1)}B`;
  if (Math.abs(v) >= 1e6) return `${(v / 1e6).toFixed(1)}M`;
  if (Math.abs(v) >= 1e3) return `${(v / 1e3).toFixed(1)}K`;
  return fmtExact(v);
}

// A share, at the precision the share deserves. Fresh input in this fleet is 0.0017% of all tokens,
// and a fixed two decimals prints that as `0.00%` — which reads as "none" when the true statement
// is "seventeen ten-thousandths of a percent", a different and much more interesting fact.
export function usagePct(part, whole) {
  const w = Number(whole);
  if (!w || Number.isNaN(w) || part === null || part === undefined) return "—";
  const p = (Number(part) / w) * 100;
  if (!Number.isFinite(p)) return "—";
  return `${p >= 0.1 ? p.toFixed(2) : Number(p.toPrecision(2))}%`;
}

// How long ago, in words rather than in the board's glyphs.
//
// Deliberately NOT `ago()` from ago.mjs, and the difference is the point rather than an oversight.
// That one labels a row in a dense list and must be four characters wide — `47m ago`. This one ends
// a sentence a person reads once, where "read 47m ago" reads like a machine and "read 47 minutes
// ago" reads like an answer. They format different things for different places; neither is derived
// from the other, so there is no number here that can disagree with one there.
//
// `null` for "we do not know", which is a different statement from "just now" and must never render
// as one — a panel that says a reading is current when it has no idea is the failure this whole
// staleness line exists to prevent.
export function usageAge(secs) {
  if (secs === null || secs === undefined || Number.isNaN(Number(secs))) return null;
  const s = Math.max(0, Math.floor(Number(secs)));
  if (s < 45) return "just now";
  if (s < 90) return "a minute ago";
  if (s < 3600) return `${Math.round(s / 60)} minutes ago`;
  if (s < 5400) return "an hour ago";
  if (s < 86400) return `${Math.round(s / 3600)} hours ago`;
  if (s < 172800) return "a day ago";
  return `${Math.round(s / 86400)} days ago`;
}

// What the panel says about its own freshness, from the two fields that carry it.
//
// **Both, not either.** `read_at` is when the reading was taken and `fresh` is the reader's own
// verdict on it, and they can disagree — a reader that failed halfway can produce a reading that is
// two minutes old and known bad. Taking only the clock would call that current; taking only the
// flag would leave a person with no idea how old "not fresh" is. So the age is always said, and the
// flag adds the sentence the age cannot carry.
export function usageRead(readAt, fresh, nowMs) {
  const t = Date.parse(readAt || "");
  if (Number.isNaN(t)) {
    return { secs: null, stale: true, text: "skein does not know when this reading was taken" };
  }
  const secs = Math.max(0, Math.floor((Number(nowMs) - t) / 1000));
  const old = secs >= USAGE_STALE_SECS;
  const base = `read ${usageAge(secs)}`;
  if (fresh === false) return { secs, stale: true, text: `${base} — and skein has marked it out of date` };
  return { secs, stale: old, text: old ? `${base} — over an hour old` : base };
}

// ── skein's own reading (SKEIN-1074, SKEIN-1076) ───────────────────────────────────────────────────

// What each of skein's own call sites is called on the page, keyed by the code the payload carries
// (`crate::ai::Site::code`). The words are the owner's, approved under SKEIN-1075, and they are
// the only place these codes become text.
export const OWN_SITES = {
  S3: "reviewing a pull request",
  S1: "summarising a pull request",
  S2: "reading a pull request in detail",
  S4: "checking the review read every changed file",
  S5: "answering a check the repository asks for",
  S6: "answering your question about a pull request",
  S7: "drafting a review comment",
  S8: "writing a module note",
  S9: "summarising what a box just did",
  S10: "deciding whether a box's question can be continued",
  S11: "checking the model answers (doctor)",
};

const plural = (n, one, many) => `${fmtExact(n)} ${Number(n) === 1 ? one : many}`;

// The section, above "By box" where it cannot collide with a box that happens to be called skein.
//
// **The per-thing figures cover the same span as the headline total** (the owner's decision on
// SKEIN-1075): everything this reading counted.
export function ownHtml(own, esc) {
  const e = esc || (s => String(s));
  const o = own || {};
  const lines = [];
  const r = o.readings || {};
  if (Number(r.finished) > 0) {
    lines.push(`${usd(r.per_finished)} per completed pull-request reading · ${fmtTok(r.tokens_per_finished)} tokens`
      + `   (${plural(r.finished, "reading", "readings")}${
        Number(r.unfinished) > 0 ? `; ${fmtExact(r.unfinished)} that did not finish are counted in` : ""})`);
  } else if (Number(r.unfinished) > 0) {
    lines.push(`no reading finished in this span, so there is nothing to divide by — ${usd(r.cost)} went on ${fmtExact(r.unfinished)} that did not`);
  }
  // The dollars per completed tracker item return here, fleet-wide only, once SKEIN-1139 gives skein
  // a record of when a box held and finished an item. Until then the owner's decision (2026-09-24)
  // is that no tracker line of any kind is drawn — not a reason, not a placeholder.
  if (o.unlabelled_in_boxes) {
    lines.push("calls made before skein labelled its own are counted under the box they ran in");
  }
  const figures = lines.map(l => `<div class="note ug-per">${e(l)}</div>`).join("");

  const sites = (Array.isArray(o.sites) ? o.sites : []).slice().sort((a, b) => b.cost - a.cost);
  if (!Number(o.calls) && !sites.length) {
    return `<div class="note ug-empty">skein made no model calls of its own in this reading</div>` + figures;
  }
  const top = Math.max(...sites.map(x => x.cost), 0);
  const fig = x => `<b>${usd(x.cost)}</b> · ${fmtTok(x.tokens)} tokens · ${plural(x.calls, "call", "calls")}`;
  const row = `<div class="ug-row ug-own">
      <span class="ug-nm">skein's own reading</span>${bar(o.cost, o.cost)}
      <span class="ug-fig">${fig(o)}</span>
    </div>`;
  const breakdown = sites.map(x => `<div class="ug-row ug-site">
      <span class="ug-nm">${e(OWN_SITES[x.site] || x.site)}</span>${bar(x.cost, top)}
      <span class="ug-fig">${fig(x)}</span>
    </div>`).join("");
  return row + breakdown + figures;
}

// ── the panel ─────────────────────────────────────────────────────────────────────────────────────

const bar = (v, max) =>
  `<span class="ug-bar"><i style="width:${max > 0 ? Math.max(1, Math.round((Math.min(1, v / max)) * 100)) : 0}%"></i></span>`;

const section = (title, rows, empty) =>
  `<div class="ug-h">${title}</div>` + (rows || `<div class="note ug-empty">${empty}</div>`);

/** The whole panel, as markup.
 *
 * `u` is the reading (`null` before there has ever been one). `nowMs` is the clock, passed in so
 * the staleness line is a function of its arguments and can be asserted at a chosen instant.
 * `note` is an optional line the page puts under the head — it is how a failed request is said,
 * and it is a parameter rather than a field on `u` because the payload's shape belongs to the
 * reader lane and this panel does not get to add to it.
 */
export function usageHtml(u, nowMs, esc, note) {
  const e = esc || (s => String(s));
  const refresh = `<button type="button" class="kbtn ug-refresh" id="ug-refresh">Refresh</button>`;
  const hint = note ? `<div class="ug-note">${e(note)}</div>` : "";

  if (!u || !u.totals) {
    // Never a zero. A fleet that has not been read and a fleet that cost nothing look identical the
    // moment this renders `$0.00`, and of the two the wrong one is the one a person would believe.
    return `<div class="ug-head"><span class="ug-cost">not read yet</span>
      <span class="ug-when">Refresh asks each box for its own transcripts — nothing is read on page load</span>
      ${refresh}</div>${hint}`;
  }

  const t = u.totals;
  const read = usageRead(u.read_at, u.fresh, nowMs);

  const head = `<div class="ug-head">
    <span class="ug-cost">${usd(t.cost)}</span>
    <span class="ug-when${read.stale ? " stale" : ""}">${e(read.text)}</span>
    ${refresh}
    <span class="ug-sub">${fmtTok(t.tokens)} tokens · ${fmtExact(u.boxes_read)} boxes · ${fmtExact(u.transcripts_read)} transcripts${
      u.prices_as_of ? ` · prices as of ${e(u.prices_as_of)}` : ""}</span>
  </div>`;

  // The caveat, always drawn — the empty case included.
  //
  // A model the price table did not know contributes tokens and no dollars, so its cost silently
  // reads as zero and the fleet total reads as a cheaper month than it was. That is a wrong answer
  // wearing the clothes of a right one, and the only defence is to say so on screen. The empty list
  // is drawn too, because "no unpriced models" and "this panel does not check" are indistinguishable
  // when the row is simply absent, and a reader cannot tell which silence they are looking at.
  const un = Array.isArray(u.unpriced) ? u.unpriced : [];
  const unpriced = un.length
    ? `<div class="ug-caveat bad"><span class="ug-mark">!</span><span class="ug-ctext">
         <b>${fmtExact(un.length)} model${un.length === 1 ? "" : "s"} had no price</b> — the tokens are
         counted, the dollars are not, so the total above is a floor rather than the bill:
         ${un.map(m => `${e(m.model)} (${fmtExact(m.tokens)} tokens)`).join(", ")}.
       </span></div>`
    : `<div class="ug-caveat ok"><span class="ug-mark">✓</span><span class="ug-ctext">
         every model in this reading had a price, so no tokens are counted at zero.
       </span></div>`;

  // The insight, on screen rather than in a `title`.
  //
  // Put in the open deliberately, and SKEIN-486's incident is the reason: every detail of the health
  // report used to live in a `title` attribute — uncopyable, invisible on a phone, gone on scroll —
  // and moving it into the pane is what made it readable. A panel that shows only dollars invites
  // exactly one conclusion, "write shorter prompts", and this line is the evidence that the
  // conclusion is wrong: almost the whole bill is context replayed on every turn.
  const insight = `<div class="ug-insight">
    <b>${usagePct(t.cache_read, t.tokens)}</b> of these tokens are cache reads — the conversation so
    far, re-sent on every turn — against ${usagePct(t.output, t.tokens)} written by the model and
    ${usagePct(t.input, t.tokens)} typed as fresh input. The bill follows how much context a turn
    carries and how many turns there are, not how much anyone types.
  </div>`;

  const boxes = (Array.isArray(u.boxes) ? u.boxes : []).slice().sort((a, b) => b.cost - a.cost);
  const topBox = Math.max(...boxes.map(b => b.cost), 0);
  const boxRows = boxes.map(b => {
    const mix = Object.entries(b.models || {}).sort((x, y) => y[1] - x[1])
      .map(([m, c]) => `${e(m)} ${usd(c)}`).join(" · ");
    return `<div class="ug-row">
      <span class="ug-nm">${e(b.box)}</span>${bar(b.cost, topBox)}
      <span class="ug-fig"><b>${usd(b.cost)}</b> · ${fmtTok(b.tokens)}${
        b.days ? ` · ${fmtExact(b.days)}d` : ""}${
        b.first && b.last ? ` · ${e(b.first)}→${e(b.last)}` : ""}</span>
      ${mix ? `<span class="ug-mix">${mix}</span>` : ""}
    </div>`;
  }).join("");

  const months = (Array.isArray(u.months) ? u.months : []).slice().sort((a, b) => String(a.month).localeCompare(String(b.month)));
  const topMonth = Math.max(...months.map(m => m.cost), 0);
  const monthRows = months.map(m => `<div class="ug-row">
      <span class="ug-nm">${e(m.month)}</span>${bar(m.cost, topMonth)}
      <span class="ug-fig"><b>${usd(m.cost)}</b> · ${fmtTok(m.tokens)}${
        m.cache_read !== undefined && m.tokens ? ` · ${usagePct(m.cache_read, m.tokens)} cache` : ""}</span>
    </div>`).join("");

  const models = (Array.isArray(u.models) ? u.models : []).slice().sort((a, b) => b.cost - a.cost);
  const topModel = Math.max(...models.map(m => m.cost), 0);
  const modelRows = models.map(m => `<div class="ug-row">
      <span class="ug-nm">${e(m.model)}</span>${bar(m.cost, topModel)}
      <span class="ug-fig"><b>${usd(m.cost)}</b> · ${fmtTok(m.tokens)}</span>
    </div>`).join("");

  // The daily series is the only thing here that shows a direction rather than a position, which is
  // why it gets a shape instead of a table. The hover carries the exact day; the busiest one is
  // named in words underneath, because a fact that exists only in a `title` is a fact on a phone
  // nobody has.
  const daily = (Array.isArray(u.daily) ? u.daily : []).slice().sort((a, b) => String(a.day).localeCompare(String(b.day)));
  const topDay = Math.max(...daily.map(d => d.cost), 0);
  const busiest = daily.slice().sort((a, b) => b.cost - a.cost)[0];
  const busiestBox = busiest && Object.entries(busiest.by_box || {}).sort((x, y) => y[1] - x[1])[0];
  const dayStrip = daily.length
    ? `<div class="ug-days">${daily.map(d =>
        `<span class="ug-day" title="${e(d.day)} · ${usd(d.cost)}"><i style="height:${
          topDay > 0 ? Math.max(2, Math.round((d.cost / topDay) * 100)) : 0}%"></i></span>`).join("")}</div>`
      + `<div class="ug-dfoot">${fmtExact(daily.length)} day${daily.length === 1 ? "" : "s"} of spend`
      + (busiest ? `, the heaviest ${e(busiest.day)} at ${usd(busiest.cost)}` : "")
      + (busiestBox ? ` — most of it ${e(busiestBox[0])}, ${usd(busiestBox[1])}` : "")
      + `.</div>`
    : "";

  return head + hint + unpriced + insight
    + `<div class="ug-h">skein's own reading</div>` + ownHtml(u.own, e)
    + section("By box", boxRows, "no box reported a reading")
    + section("By month", monthRows, "no month in this reading")
    + section("By model", modelRows, "no model in this reading")
    + section("By day", dayStrip, "no daily series in this reading");
}
