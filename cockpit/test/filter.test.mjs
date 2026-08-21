import { strict as assert } from "node:assert";
import { test } from "node:test";
import { boardRows, matchesFilter, wantsForeign, withoutForeignTerm } from "../src/filter.mjs";

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

test("nothing to show is empty rather than everything", () => {
  // The shape a bug takes when a filter is applied to the wrong list: an unmatched query returning
  // the whole board reads as "the filter is broken", and returning the board reads as "there is
  // nothing here" — only one of those is what happened.
  assert.deepEqual(boardRows([box("a")], "zzz", []), []);
  assert.deepEqual(boardRows(null, "", []), []);
});
