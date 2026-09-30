import { test } from "node:test";
import assert from "node:assert/strict";
import {
  usd, fmtExact, fmtTok, usagePct, usageAge, usageRead, usageHtml, USAGE_STALE_SECS,
  isoWeekStart, weekLabel, usageWeeks, readingDay, UG_DAY_ROWS,
} from "../src/usage.mjs";

// The agreed payload shape, copied field for field rather than invented — the reader lane owns it
// and may revise it. The numbers are a real fleet reading taken on 2026-09-12.
const READING = {
  prices_as_of: "2026-09-12",
  read_at: "2026-09-12T03:20:00Z",
  fresh: true,
  boxes_read: 16,
  transcripts_read: 1313,
  totals: { cost: 30477.56, tokens: 44600000000,
            input: 757000, output: 124000000,
            cache_read: 43800000000, cache_write: 640000000 },
  unpriced: [{ model: "some-future-model", tokens: 12345 }],
  boxes: [{ box: "deep-box", cost: 9117.60, tokens: 13430000000,
            days: 40, first: "2026-07-25", last: "2026-09-12",
            models: { "opus-5": 8223.64, "fable-5": 624.42 } }],
  months: [{ month: "2026-08", cost: 17641.79, tokens: 26000000000,
             output: 60000000, cache_read: 25500000000 }],
  daily: [{ day: "2026-09-07", cost: 1647.60, by_box: { "deep-box": 871.25 } }],
  models: [{ model: "opus-5", cost: 28161.62, tokens: 41000000000 }],
};
// Twenty minutes after the reading above, so a test can talk about "read 20 minutes ago" without
// depending on when it runs.
const NOW = Date.parse("2026-09-12T03:40:00Z");
const esc = s => String(s).replace(/[&<>"']/g, c => ({ "&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;","'":"&#39;" }[c]));
// Everything outside an HTML attribute — the test for "on screen" rather than "in the markup
// somewhere", since a fact that lives only in a `title=` is a fact a phone never shows.
//
// **What this does NOT catch, established by breaking it rather than assumed.** Wrapping the
// sentence in `<div hidden>` leaves it outside every attribute, and the first attempt at sabotaging
// the insight check did exactly that and stayed green. Text in a hidden element, or under a CSS
// rule, is invisible here — that is the browser tier's question, not node's. What this catches is
// the specific regression it is aimed at: a fact demoted into a `title`.
const visible = html => html.replace(/\w+="[^"]*"/g, "");

test("money is grouped and two-placed, the same way in every locale", () => {
  // Fails if the grouping is dropped: $30477.56, which is the number nobody can read at a glance.
  assert.equal(usd(30477.56), "$30,477.56");
  assert.equal(usd(1647.6), "$1,647.60");
  assert.equal(usd(0.03), "$0.03");
  assert.equal(usd(999), "$999.00");
  assert.equal(usd(1000), "$1,000.00");
  // Not zero. A missing cost and a cost of nothing are different claims.
  assert.equal(usd(undefined), "—");
  assert.equal(usd(null), "—");
});

test("exact counts and compact counts are different functions on purpose", () => {
  // Fails the moment one of them is made to call the other: an unpriced model's 12,345 tokens
  // becoming "12.3K" loses the exactness that is the entire reason the count is printed.
  assert.equal(fmtExact(12345), "12,345");
  assert.equal(fmtExact(1313), "1,313");
  assert.equal(fmtTok(44600000000), "44.6B");
  assert.equal(fmtTok(13430000000), "13.4B");
  assert.equal(fmtTok(124000000), "124.0M");
  assert.equal(fmtTok(757000), "757.0K");
  // Below a thousand the compact form has nothing to compact, so it is the exact one.
  assert.equal(fmtTok(999), "999");
});

test("a share too small for two decimals is printed, not rounded to nothing", () => {
  // THE ASSERTION THIS FUNCTION EXISTS FOR. Fresh input is 0.0017% of this fleet's tokens. With a
  // fixed `toFixed(2)` that prints "0.00%", which reads as "none" — and "none" is a different and
  // much less interesting claim than "seventeen ten-thousandths of a percent". Replace the
  // precision branch with toFixed(2) and this line fails.
  assert.equal(usagePct(757000, 44600000000), "0.0017%");
  assert.equal(usagePct(43800000000, 44600000000), "98.21%");
  assert.equal(usagePct(124000000, 44600000000), "0.28%");
  // No total is not a zero share.
  assert.equal(usagePct(5, 0), "—");
});

test("staleness is said in words a sentence can end with, and never guessed", () => {
  // Fails if this is made to delegate to ago.mjs's compact glyphs: "47m ago" instead of
  // "47 minutes ago". Those are for a dense row; this ends a sentence somebody reads once.
  assert.equal(usageAge(47 * 60), "47 minutes ago");
  assert.equal(usageAge(20 * 60), "20 minutes ago");
  assert.equal(usageAge(10), "just now");
  assert.equal(usageAge(3 * 3600), "3 hours ago");
  assert.equal(usageAge(2 * 86400), "2 days ago");
  // `null`, not "just now". Fails if an unknown age is defaulted to the present — which is the
  // panel claiming a reading is current when it has no idea how old it is.
  assert.equal(usageAge(undefined), null);
  assert.equal(usageAge(null), null);
});

test("the panel reads both freshness fields, because they can disagree", () => {
  // The ordinary case: recent, and the reader is happy with it.
  const ok = usageRead("2026-09-12T03:20:00Z", true, NOW);
  assert.equal(ok.text, "read 20 minutes ago");
  assert.equal(ok.stale, false);

  // Old by the clock, whatever the flag says. Fails if `stale` stops consulting the age — which is
  // the panel calling a nine-hour-old reading current.
  const old = usageRead("2026-09-11T18:00:00Z", true, NOW);
  assert.equal(old.stale, true);
  assert.match(old.text, /over an hour old/);

  // Recent by the clock and known bad by the reader. Fails if `stale` stops consulting `fresh` —
  // which is the panel calling a failed reading current because it happens to be two minutes old.
  const bad = usageRead("2026-09-12T03:38:00Z", false, NOW);
  assert.equal(bad.stale, true);
  assert.match(bad.text, /out of date/);

  // An unparseable timestamp says so rather than picking a side.
  const unknown = usageRead(undefined, true, NOW);
  assert.equal(unknown.stale, true);
  assert.equal(unknown.secs, null);
  assert.match(unknown.text, /does not know when/);

  // The hour is one number in one place, and it is the hour the owner asked for.
  assert.equal(USAGE_STALE_SECS, 3600);
});

test("a fleet nobody has read shows no figure at all, never a zero", () => {
  // THE ASSERTION THIS BRANCH EXISTS FOR. Render `usd(0)` into the head for an absent payload and
  // "we have not looked" becomes "it cost nothing" — of the two wrong readings, the believable one.
  const html = usageHtml(null, NOW, esc);
  assert.ok(!html.includes("$0.00"), html);
  assert.match(html, /not read yet/);
  // And the way out is still on screen: a panel with nothing in it and no Refresh is a dead end.
  assert.match(html, /id="ug-refresh"/);
});

test("Refresh is on the panel whether or not there is a reading", () => {
  // Fails if the button is moved inside the has-a-reading branch.
  assert.equal((usageHtml(READING, NOW, esc).match(/id="ug-refresh"/g) || []).length, 1);
  assert.equal((usageHtml(null, NOW, esc).match(/id="ug-refresh"/g) || []).length, 1);
});

test("an unpriced model is a visible caveat carrying its own token count", () => {
  const html = usageHtml(READING, NOW, esc);
  // Fails if the unpriced block is dropped, or if its tokens are compacted away. A model the price
  // table did not know contributes tokens and no dollars, so the total silently reads as a cheaper
  // month than it was — and this sentence is the only thing that says so.
  assert.match(visible(html), /some-future-model/);
  assert.match(visible(html), /12,345 tokens/);
  assert.match(visible(html), /1 model had no price/);
  assert.match(visible(html), /floor rather than the bill/);
});

test("an empty unpriced list is still drawn, because an absent row says nothing", () => {
  // Fails if the empty case renders "". "No unpriced models" and "this panel does not check for
  // unpriced models" are indistinguishable when the row is simply missing, and a reader cannot tell
  // which of the two silences they are looking at.
  const html = usageHtml({ ...READING, unpriced: [] }, NOW, esc);
  assert.match(visible(html), /every model in this reading had a price/);
  assert.ok(!visible(html).includes("had no price"), html);
});

test("the cache-read insight is on screen, not hidden in a title attribute", () => {
  // THE ASSERTION FOR THE DECISION. SKEIN-486's health report lived in a `title` — uncopyable,
  // invisible on a phone, gone on scroll — and moving it into the pane is what made it readable.
  // Strip every attribute and the sentence must survive; put it back in a `title=` and this fails.
  const seen = visible(usageHtml(READING, NOW, esc));
  assert.match(seen, /98\.21%/);
  assert.match(seen, /cache reads/);
  assert.match(seen, /not how much anyone types/);
});

test("one surface per fact: the fleet total is drawn exactly once", () => {
  // SKEIN-400. The total used to be repeated inside the unpriced caveat; two copies of a number is
  // how one of them goes stale. Re-introduce the second and this count goes to 2.
  const html = usageHtml(READING, NOW, esc);
  assert.equal((html.match(/\$30,477\.56/g) || []).length, 1);
});

test("boxes, months and models are ordered by the panel, not by the payload", () => {
  // Fails if the sorts are dropped: the reader lane is free to send these in any order, and a
  // ranking that depends on the sender's order is not a ranking.
  const shuffled = {
    ...READING,
    boxes: [
      { box: "quiet-box", cost: 12.5, tokens: 1000, days: 1, models: {} },
      { box: "big-box", cost: 4404.48, tokens: 9000000000, days: 30, models: {} },
    ],
  };
  const html = usageHtml(shuffled, NOW, esc);
  assert.ok(html.indexOf("big-box") < html.indexOf("quiet-box"), html);
});

test("names from the payload are escaped with the page's own escaper", () => {
  // Fails if any of these interpolations stops calling `e`. A box name is a git-derived string and
  // the payload is assembled from sixteen boxes' own readings, so it is not this panel's place to
  // assume it is inert.
  const html = usageHtml({ ...READING, boxes: [{ box: "<script>x</script>", cost: 1, tokens: 1, models: {} }] }, NOW, esc);
  assert.ok(!html.includes("<script>"), html);
  assert.match(html, /&lt;script&gt;/);
});

test("every section says so when it is empty rather than vanishing", () => {
  // Fails if an empty array renders nothing: a panel with no "By month" heading looks like a panel
  // that does not do months, not like a fleet with no months read.
  const html = usageHtml({ ...READING, boxes: [], months: [], models: [], daily: [] }, NOW, esc);
  for (const [heading, empty] of [
    ["By box", "no box reported a reading"],
    ["By month", "no month in this reading"],
    ["By model", "no model in this reading"],
    ["By week", "no week in this reading"],
    ["By day", "no daily series in this reading"],
  ]) {
    assert.match(visible(html), new RegExp(heading));
    assert.match(visible(html), new RegExp(empty));
  }
});

test("the daily series names its heaviest day and the box that drove it", () => {
  // Fails if the footer is dropped and the series is left as bars whose only labels are `title`
  // attributes — the same phone-invisible failure as the insight above.
  const seen = visible(usageHtml(READING, NOW, esc));
  assert.match(seen, /heaviest 2026-09-07 at \$1,647\.60/);
  assert.match(seen, /most of it deep-box, \$871\.25/);
});

test("a failed request is said in the panel rather than shown as no spend", () => {
  // Fails if the note parameter is ignored. The panel is reachable while the reader route is not
  // yet there, and "skein could not ask" must not render as a fleet that cost nothing.
  const html = usageHtml(null, NOW, esc, "skein could not read the fleet's usage: 404");
  // Escaped on the way in — the note carries a server's own words, so it goes through `e` like
  // every other string here rather than being trusted because the page composed the sentence.
  assert.match(visible(html), /could not read the fleet&#39;s usage: 404/);
  assert.ok(!html.includes("$0.00"), html);
});

// ── skein's own reading (SKEIN-1074, SKEIN-1076) ────────────────────────────────────────────────
// The words below are the owner's, approved under SKEIN-1075, and are asserted verbatim: a string
// that drifts from the signed-off text fails here rather than shipping.
const OWN = {
  cost: 41.2, tokens: 9800000, calls: 212,
  sites: [
    { site: "S1", cost: 3.1, tokens: 900000, calls: 150 },
    { site: "S3", cost: 38.1, tokens: 8900000, calls: 62 },
  ],
  readings: { finished: 38, unfinished: 4, cost: 35.72, tokens: 8071200,
              per_finished: 0.94, tokens_per_finished: 212400 },
  unlabelled_in_boxes: true,
};

test("skein's own reading is its own section, above By box", () => {
  // Fails if the section is dropped, renamed, or drawn below the boxes it must not be mistaken for.
  const html = visible(usageHtml({ ...READING, own: OWN }, NOW, esc));
  const own = html.indexOf("skein's own reading</div>");
  assert.ok(own > 0, "the section heading is not on screen");
  assert.ok(own < html.indexOf("By box"), "the section is not above By box");
  assert.match(html, /skein's own reading<\/span>[\s\S]*?\$41\.20<\/b> · 9\.8M tokens · 212 calls/);
});

test("the breakdown names each call site in the owner's words, most expensive first", () => {
  // Fails if a code leaks onto the page in place of its words, or the order follows the payload.
  const html = visible(usageHtml({ ...READING, own: OWN }, NOW, esc));
  const review = html.indexOf("reviewing a pull request");
  const summary = html.indexOf("summarising a pull request");
  assert.ok(review > 0 && summary > review, "S3 (the dearer) is not drawn before S1");
  assert.doesNotMatch(html, />S[0-9]+</, "a call-site code reached the page as text");
});

test("the per-reading figure divides by finished readings and says the unfinished are counted in", () => {
  // Fails if the figure or its parenthesis drift from the approved text.
  const html = visible(usageHtml({ ...READING, own: OWN }, NOW, esc));
  assert.match(html, /\$0\.94 per completed pull-request reading · 212\.4K tokens\s+\(38 readings; 4 that did not finish are counted in\)/);
});

test("with no finished reading there is nothing to divide by, and the page says what went on the rest", () => {
  // Fails if a zero divisor renders a figure (Infinity, NaN, $0.00) instead of the sentence.
  const own = { ...OWN, readings: { finished: 0, unfinished: 3, cost: 4.2, tokens: 10, per_finished: null } };
  const html = visible(usageHtml({ ...READING, own }, NOW, esc));
  assert.match(html, /no reading finished in this span, so there is nothing to divide by — \$4\.20 went on 3 that did not/);
  assert.doesNotMatch(html, /per completed pull-request reading/);
});

test("no tracker line of any kind is drawn until skein has a source for it (SKEIN-1139)", () => {
  // The owner's decision, 2026-09-24: hidden entirely, not explained. Fails if any tracker sentence
  // renders — the old "not connected" reason, a placeholder, or a per-item figure — in any state
  // of the payload, including one that still carries the retired `tracker_connected` field.
  for (const own of [OWN, { ...OWN, tracker_connected: true }, { ...OWN, tracker_connected: false },
                     { calls: 0, sites: [], readings: {} }]) {
    const html = visible(usageHtml({ ...READING, own }, NOW, esc));
    assert.doesNotMatch(html, /tracker|work tracking|per completed tracker item/i);
  }
});

test("old unlabelled calls are said to be under their box", () => {
  // Fails if the sentence is dropped: an unmentioned box share reads as skein costing less than it did.
  const html = visible(usageHtml({ ...READING, own: OWN }, NOW, esc));
  assert.match(html, /calls made before skein labelled its own are counted under the box they ran in/);
  const clean = visible(usageHtml({ ...READING, own: { ...OWN, unlabelled_in_boxes: false } }, NOW, esc));
  assert.doesNotMatch(clean, /calls made before skein labelled/);
});

test("with no call of its own the section says so rather than vanishing", () => {
  // Fails if an empty section disappears, which reads the same as a page that does not look.
  const html = visible(usageHtml({ ...READING, own: { calls: 0, sites: [], readings: {} } }, NOW, esc));
  assert.match(html, /skein made no model calls of its own in this reading/);
  const old = visible(usageHtml(READING, NOW, esc));
  assert.match(old, /skein made no model calls of its own in this reading/);
});

// ── weeks, the day table, and the running period (SKEIN-1237) ───────────────────────────────────
// Four days across three ISO weeks, chosen so each boundary is exercised: 30 Aug is a Sunday and
// must close the week of 24 Aug, 31 Aug is the Monday that opens the next, and 6 Sep (Sunday) and
// 7 Sep (Monday) straddle the following boundary. The reading is taken Saturday 12 Sep, inside the
// week of 7 Sep and inside September.
const WEEKS = {
  ...READING,
  totals: { ...READING.totals, cost: 100 },
  months: [
    { month: "2026-09", cost: 70, tokens: 1000 },
    { month: "2026-08", cost: 30, tokens: 1000 },
  ],
  daily: [
    { day: "2026-09-07", cost: 40, by_box: { "c-box": 40 } },
    { day: "2026-08-30", cost: 10, by_box: { "a-box": 10 } },
    { day: "2026-08-31", cost: 20, by_box: { "a-box": 5, "b-box": 15 } },
    { day: "2026-09-06", cost: 30, by_box: { "b-box": 30 } },
  ],
};
// One section's markup, from its heading to the next heading. Raw, so a test can count rows by
// class; wrap it in `visible` to ask what is on screen.
const sectionText = (html, heading) => {
  const at = html.indexOf(`<div class="ug-h">${heading}</div>`);
  assert.ok(at >= 0, `no ${heading} section`);
  const next = html.indexOf(`<div class="ug-h">`, at + 1);
  return html.slice(at, next < 0 ? undefined : next);
};

test("an ISO week starts on Monday, so a Sunday closes the week before it", () => {
  // Fails if the week is keyed on getUTCDay() without the Monday shift — a Sunday-start week files
  // 30 Aug with 31 Aug and 6 Sep with 7 Sep, which moves money across every boundary.
  assert.equal(isoWeekStart("2026-08-30"), "2026-08-24");
  assert.equal(isoWeekStart("2026-08-31"), "2026-08-31");
  assert.equal(isoWeekStart("2026-09-06"), "2026-08-31");
  // Across New Year: Thursday 1 Jan 2026 is in the week that began on Monday 29 Dec 2025.
  assert.equal(isoWeekStart("2026-01-01"), "2025-12-29");
  // Fails if an unparseable day is filed somewhere rather than nowhere.
  assert.equal(isoWeekStart("not a day"), null);
});

test("a week is labelled by its date range, with a year only where one is needed", () => {
  // Fails if the end is computed as start + 7 (an eight-day week) or the month is dropped.
  assert.equal(weekLabel("2026-09-21", 2026), "21–27 Sep");
  assert.equal(weekLabel("2026-08-31", 2026), "31 Aug–6 Sep");
  // Fails if the year is never printed: two Septembers a year apart would read the same.
  assert.equal(weekLabel("2025-09-22", 2026), "22–28 Sep 2025");
  assert.equal(weekLabel("2025-12-29", 2026), "29 Dec 2025–4 Jan 2026");
});

test("weeks are summed from the days, boxes and all, oldest first", () => {
  // Fails if a day's cost or its boxes are not added into its week, or the order follows the payload.
  const weeks = usageWeeks(WEEKS.daily);
  assert.deepEqual(weeks.map(w => [w.start, w.cost, w.days]),
    [["2026-08-24", 10, 1], ["2026-08-31", 50, 2], ["2026-09-07", 40, 1]]);
  assert.deepEqual(weeks[1].by_box, { "a-box": 5, "b-box": 45 });
  // A day that does not parse is in no week rather than in a wrong one.
  assert.deepEqual(usageWeeks([{ day: "garbage", cost: 5 }]), []);
});

test("By week is on screen: range, cost, share of the total and the boxes behind it", () => {
  // Fails if the section is dropped, if its figures live only in an attribute, or if the share is
  // taken of anything but the fleet total the head shows.
  const html = usageHtml(WEEKS, NOW, esc);
  const week = visible(sectionText(html, "By week"));
  assert.match(week, /24–30 Aug[\s\S]*?\$10\.00<\/b> · 10\.00% of the total/);
  assert.match(week, /31 Aug–6 Sep[\s\S]*?\$50\.00<\/b> · 50\.00% of the total[\s\S]*?b-box \$45\.00 · a-box \$5\.00/);
  assert.match(week, /7–13 Sep/);
  assert.ok(week.indexOf("24–30 Aug") < week.indexOf("31 Aug–6 Sep") && week.indexOf("31 Aug–6 Sep") < week.indexOf("7–13 Sep"),
    "weeks are not oldest first");
  // The zone is said, because it moves a person's Sunday evening into Monday.
  assert.match(week, /Weeks run Monday to Sunday\. Every day on this pane is a UTC date/);
  // The time sections sit together, month then week then day.
  const seen = visible(html);
  assert.ok(seen.indexOf(">By month<") < seen.indexOf(">By week<") && seen.indexOf(">By week<") < seen.indexOf(">By day<"),
    "By month, By week and By day are not in that order");
});

test("the week and the month the reading was taken in are marked as running, and no others", () => {
  // Fails if "so far" is dropped, or is put on every row, or on the latest row whatever its date.
  const html = usageHtml(WEEKS, NOW, esc);
  const week = sectionText(html, "By week");
  assert.equal((week.match(/so far/g) || []).length, 1, week);
  assert.match(week, /7–13 Sep <span class="ug-part">· so far<\/span>/);
  const month = sectionText(html, "By month");
  assert.equal((month.match(/so far/g) || []).length, 1, month);
  assert.match(month, /2026-09 <span class="ug-part">· so far<\/span>/);
  assert.doesNotMatch(month, /2026-08 <span/);
});

test("which period is running is decided by the reading, not by the clock", () => {
  // A reading taken on Saturday 12 Sep and looked at a month later: 7–13 Sep is still the week it
  // could not see the end of. Fails if `readingDay` consults the clock first.
  const later = Date.parse("2026-10-14T09:00:00Z");
  assert.equal(readingDay(WEEKS.read_at, later), "2026-09-12");
  assert.match(sectionText(usageHtml(WEEKS, later, esc), "By week"), /7–13 Sep <span class="ug-part">· so far/);
  // A week the reading has seen the end of is whole. Fails if a reading taken the Monday after is
  // still marking the week before as running.
  const monday = { ...WEEKS, read_at: "2026-09-14T08:00:00Z" };
  assert.doesNotMatch(sectionText(usageHtml(monday, later, esc), "By week"), /so far/);
  // No `read_at` to go on, and the clock stands in rather than nothing being marked.
  assert.equal(readingDay(undefined, NOW), "2026-09-12");
  assert.match(sectionText(usageHtml({ ...WEEKS, read_at: undefined }, NOW, esc), "By week"), /7–13 Sep <span class="ug-part">· so far/);
});

test("By day is readable without hovering: the latest days as rows with their cost and boxes", () => {
  // Fails if the days live only in the strip's `title` attributes — which `visible` strips, the
  // same phone-invisible failure the footer exists to prevent.
  const raw = sectionText(usageHtml(WEEKS, NOW, esc), "By day");
  const day = visible(raw);
  assert.match(day, /Mon 2026-09-07[\s\S]*?\$40\.00<\/b>[\s\S]*?c-box \$40\.00/);
  assert.match(day, /Sun 2026-08-30[\s\S]*?\$10\.00<\/b>/);
  assert.equal((raw.match(/class="ug-row ug-dayrow"/g) || []).length, 4);

  // A long series lists only the latest UG_DAY_ROWS, oldest of them first, and says so. Fails if the
  // table takes the first days instead of the last, or lists the whole series.
  const long = Array.from({ length: 20 }, (_, i) => ({
    day: `2026-08-${String(i + 1).padStart(2, "0")}`, cost: i + 1, by_box: { "x-box": i + 1 },
  }));
  const tail = sectionText(usageHtml({ ...WEEKS, daily: long }, NOW, esc), "By day");
  assert.equal((tail.match(/class="ug-row ug-dayrow"/g) || []).length, UG_DAY_ROWS);
  // Visible text only: the strip still carries every day in its hover, and that is not the table.
  assert.doesNotMatch(visible(tail), /2026-08-06/);
  assert.match(visible(tail), /Fri 2026-08-07[\s\S]*Thu 2026-08-20/);
  assert.match(tail, /The latest 14 are listed below\./);
  assert.equal(UG_DAY_ROWS, 14);
});
