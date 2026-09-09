import { strict as assert } from "node:assert";
import { test } from "node:test";
import { savedSummary } from "../src/saved.mjs";

// The whole reason this is a function: a save that lost a box must say WHICH box, in the sentence
// somebody reads before deciding whether to destroy the sandbox.
//
// Fails on: summarising in aggregate — returning `${done} boxes copied out` for the partial case,
// which is the phrasing this replaced and the one that hides the only line that matters.
test("a partial save names the boxes that did not make it out", () => {
  const said = savedSummary([
    { name: "web-main", archive: "/s/web-main/save-1.tar", error: "" },
    { name: "api-worker", archive: "", error: "could not copy api-worker out: timed out" },
    { name: "docs-fix", archive: "", error: "could not copy docs-fix out: no space" },
  ]);
  assert.match(said, /api-worker/);
  assert.match(said, /docs-fix/);
  assert.match(said, /1 of 3/);
  // And it must not read as reassurance. A partial save is exactly when "nothing was destroyed" is
  // true and beside the point.
  assert.doesNotMatch(said, /Nothing was stopped/);
});

test("a save that copied every box says so, and says what it did not do", () => {
  const said = savedSummary([
    { name: "web-main", archive: "/s/web-main/save-1.tar", error: "" },
    { name: "api-worker", archive: "/s/api-worker/save-1.tar", error: "" },
  ]);
  assert.equal(
    said,
    "2 boxes copied out to the host. Nothing was stopped and nothing was destroyed.",
  );
  // One box is one box, not "1 boxes".
  assert.match(savedSummary([{ name: "solo", archive: "/s/solo.tar", error: "" }]), /^1 box copied/);
});

// A response that carried no boxes at all is not a success: nothing happened, and the sentence must
// not congratulate anybody on it.
//
// Fails on: returning the all-copied sentence for an empty list, which `0 - 0 === 0` makes the
// natural mistake.
test("an empty report is not reported as a clean save", () => {
  for (const empty of [[], null, undefined]) {
    assert.equal(savedSummary(empty), "no box was copied out.");
  }
});
