// The half-signal caveat: does the board actually SAY when the screen half is not contributing?
//
// The badge exists because a missing screen half is invisible by construction — the row falls back
// to hook edges and looks entirely normal, which is how an answered decision sat there for twenty
// minutes. So the badge's whole job is to be present when the caveat is. Two ways it has failed at
// exactly that, and this suite is one check for each:
//
//   1. `screen_health` gained `misfiled` in Rust (07fa02b, SKEIN-220) and `SHALF` was not told, so
//      a refused observation rendered nothing at all (SKEIN-223).
//   2. It rendered nothing *because* an unknown value falls off the end of the map — and "nothing"
//      is what a healthy screen looks like. A badge that cannot say "I do not know this state" is a
//      badge that lies by omission, and it lies in the direction of confidence.
//
// The first check is derived from `src/signals.rs` rather than from a list written here, because a
// list written here is the same thing that went stale in the first place.
//
//   node tests/ui/screenhalf.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { grab, harness } from "./lift.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

// Every value `screen_health` can return, read out of the function itself. Sliced to that function's
// body so the rest of signals.rs cannot contribute strings it never returns.
function healthValuesInRust() {
  const src = readFileSync(join(root, "src", "signals.rs"), "utf8");
  const at = src.indexOf("pub fn screen_health(");
  if (at < 0) throw new Error("`pub fn screen_health` is gone from src/signals.rs — renamed?");
  const end = src.indexOf("\n}\n", at);
  const body = src.slice(at, end);
  const found = [...body.matchAll(/"([a-z]*)"/g)].map(m => m[1]);
  // `""` is the healthy answer — reading the screen fine, or the box is not running. It is the one
  // value that must NOT produce a badge, so it is dropped here rather than asserted about.
  return [...new Set(found.filter(Boolean))].sort();
}

const scope = new Function(`
  ${["esc", "SHALF", "EDGE_AHEAD", "shUnknown", "screenHalf", "screenBadge"].map(grab).join("\n")}
  return { SHALF, EDGE_AHEAD, screenHalf, screenBadge };
`);

const { check, done } = harness();
const { SHALF, EDGE_AHEAD, screenHalf, screenBadge } = scope();

// 1. Every state the server can report has an explanation on the page. This is the check that would
//    have failed the day `misfiled` landed in Rust, which is the day it should have failed.
const rust = healthValuesInRust();
check("`misfiled` is one of the states Rust reports", rust.includes("misfiled"), true);
check("every `screen_health` Rust returns has a SHALF entry", rust.filter(h => !SHALF[h]), []);

// 2. And the entry says the three things an entry is for: what it is, what it means, what to do.
//    Defaulted rather than indexed straight, so a missing entry is reported by the check above and
//    the rest of the suite still runs — a crash here would hide every check below it.
const mis = SHALF.misfiled || [];
check("misfiled's label", mis[0], "screen misfiled");
check("misfiled has means-and-do", mis.length, 3);
check(
  "misfiled says the observation is another box's",
  String(mis[1]).includes("DIFFERENT box's screen"),
  true,
);
check("misfiled's fix is a reattach, which is what exports SKEIN_BOX",
  String(mis[2]).includes("Reattach the box") && String(mis[2]).includes("SKEIN_BOX"), true);

// 3. The badge actually renders it, rather than the entry merely existing.
const badge = screenBadge({ screen_health: "misfiled" }, false);
check("a misfiled box gets a badge", badge.includes("screen misfiled"), true);
check("and it carries the reason in the title", badge.includes("SKEIN_BOX"), true);

// 4. The silence that hid all of this: a state the page has never heard of. It must not render as a
//    healthy screen — it renders as itself, named, so the next one is visible on the day it lands.
const strange = screenHalf({ screen_health: "sideways" });
check("an unknown state still produces a caveat", Boolean(strange), true);
// `|| []` for the same reason as `mis` above: the check that matters here is the one before it, and
// a page that lost the fallback should say so once rather than take the suite down at this line.
check("and names the value, so it can be grepped", (strange || [])[0], "screen: sideways");
check("the unknown badge renders", screenBadge({ screen_health: "sideways" }, false).includes("screen: sideways"), true);

// 5. An unknown state outranks the edge-ahead caveat: it is a statement about the screen half, and
//    edge-ahead is what you say when that half is fine and merely behind.
check(
  "unknown beats edge-ahead",
  screenHalf({ screen_health: "sideways", status_from: "edge-ahead" })[0],
  "screen: sideways",
);

// 6. The healthy cases, which are what stops this badge becoming the thing people learn to ignore.
check("a healthy screen says nothing", screenHalf({ screen_health: "" }), null);
check("a box with no health field says nothing", screenHalf({}), null);
check("no badge for a healthy screen", screenBadge({ screen_health: "" }, false), "");
// This one also proves `screenHalf` was lifted whole: its edge-ahead arm is on its last line, and a
// slice that stopped at the first line break would return null here and still pass everything above.
check(
  "edge-ahead is still disclosed when the screen half is healthy",
  screenHalf({ screen_health: "", status_from: "edge-ahead" }),
  EDGE_AHEAD,
);
check("a shell tab has no screen to caveat", screenBadge({ screen_health: "misfiled" }, true), "");

done();
