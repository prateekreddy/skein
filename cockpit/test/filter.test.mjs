import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  boardRows, matchesFilter, wantsForeign, withoutForeignTerm, wantsManaged, withoutManagedTerm,
} from "../src/filter.mjs";

const box = (name, extra = {}) => ({ name, foreign: false, ...extra });

test("every word must match, so adding one narrows", () => {
  const b = box("web-main", { branch: "feat/auth", repo: "skein", headline: "waiting on you" });
  assert.equal(matchesFilter(b, ""), true, "an empty filter matches everything");
  assert.equal(matchesFilter(b, "web"), true);
  assert.equal(matchesFilter(b, "web auth"), true, "two words that both appear");
  assert.equal(matchesFilter(b, "web nope"), false, "one word that does not");
  // Case and the fields it searches — a filter that only looked at the name would be a filter
  // nobody could use to find the box they remember by its branch.
  assert.equal(matchesFilter(b, "FEAT/AUTH"), false, "the caller lowercases; this does not");
  assert.equal(matchesFilter(b, "skein"), true, "the repo is searched");
  assert.equal(matchesFilter(b, "waiting"), true, "the headline is searched");
  assert.equal(matchesFilter(box("x"), "x"), true, "the optional fields being absent is not a failure");
});

test("`foreign:` changes which boxes are eligible, not how many are shown", () => {
  const mine = [box("a"), box("b")];
  const theirs = [box("other", { foreign: true })];
  assert.deepEqual(boardRows(mine, "", theirs).map(b => b.name), ["a", "b"]);
  assert.deepEqual(boardRows(mine, "foreign:", theirs).map(b => b.name), ["other"]);
  // The rows are FETCHED, so before they arrive the answer is empty rather than the board's own.
  assert.deepEqual(boardRows(mine, "foreign:", null), []);
  assert.deepEqual(boardRows(mine, "foreign:", []), []);
});

test("eligibility first, then the words", () => {
  const mine = [box("web-main"), box("api-main")];
  const theirs = [box("other-web", { foreign: true }), box("other-api", { foreign: true })];
  assert.deepEqual(boardRows(mine, "web", theirs).map(b => b.name), ["web-main"]);
  assert.deepEqual(boardRows(mine, "foreign: web", theirs).map(b => b.name), ["other-web"]);
  assert.equal(withoutForeignTerm("foreign: web"), "web");
  assert.equal(wantsForeign("web"), false);
  assert.equal(wantsForeign(undefined), false, "no filter is not a foreign filter");
});

test("`managed:` narrows to skein's own boxes and never hides them", () => {
  // The distinction the whole design rests on. `foreign:` HIDES by default and reveals on demand,
  // because nothing on a foreign row works. A managed box is skein's own, in skein's own sandbox,
  // spending model calls — so it is on the board unasked and the term only narrows. This fails the
  // moment somebody makes `managed:` work like `foreign:` and filters these out of the default view.
  const mine = [box("web-main"), box("pr-review-7", { managed: true })];
  assert.deepEqual(
    boardRows(mine, "", []).map(b => b.name),
    ["web-main", "pr-review-7"],
    "a box skein started for itself is on the board without being asked for",
  );
  assert.deepEqual(boardRows(mine, "managed:", []).map(b => b.name), ["pr-review-7"]);
  assert.equal(withoutManagedTerm("managed: web"), "web");
  assert.equal(wantsManaged("web"), false);
  assert.equal(wantsManaged(undefined), false, "no filter is not a managed filter");
  // The term composes with the words, in the same order everything else does: which boxes, then
  // which of those.
  assert.deepEqual(boardRows([box("a", { managed: true }), box("ab", { managed: true })], "managed: ab", []).map(b => b.name), ["ab"]);
  // A row missing the field entirely — an older server, or a sandbox adapted into a row — is not
  // managed. `undefined` narrowing to "shown" would put every stranger in skein's own section.
  assert.deepEqual(boardRows([{ name: "old", foreign: false }], "managed:", []).map(b => b.name), []);
  // The two terms are about different questions and do not interfere: a foreign row is never
  // skein's, so asking for both is asking for nothing rather than for everything.
  assert.deepEqual(boardRows(mine, "foreign: managed:", [box("other", { foreign: true })]).map(b => b.name), []);
});

test("nothing to show is empty rather than everything", () => {
  // The shape a bug takes when a filter is applied to the wrong list: an unmatched query returning
  // the whole board reads as "the filter is broken", and returning the board reads as "there is
  // nothing here" — only one of those is what happened.
  assert.deepEqual(boardRows([box("a")], "zzz", []), []);
  assert.deepEqual(boardRows(null, "", []), []);
});
