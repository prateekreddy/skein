import { strict as assert } from "node:assert";
import { test } from "node:test";
import { groupOf, labelOf, owedIn, NEEDS_YOU } from "../src/groups.mjs";

test("a state this page has never heard of is a box that is not talking", () => {
  assert.equal(groupOf("error"), "error");
  assert.equal(groupOf("needs-decision"), "attn");
  assert.equal(groupOf("compacting"), "working");
  // The fallback is the last group, `stale`, and it is deliberate: an unknown state must not vanish
  // from a board that groups by state.
  assert.equal(groupOf("something-new"), "stale");
  assert.equal(groupOf(""), "stale");
  assert.equal(groupOf(undefined), "stale");
});

test("`waiting` is owed to you, and one place says so", () => {
  // Written out three times once, and they disagreed: the tab title counted `waiting` and the voice
  // did not, so the title said "3 need you" while the mouth stayed shut.
  assert.ok(NEEDS_YOU.includes("waiting"));
  const owed = owedIn([
    { name: "asking", state: "needs-input" },
    { name: "ended-turn", state: "waiting" },
    { name: "busy", state: "working" },
    { name: "broken", state: "error" },
    { name: "finished", state: "done" },
  ]).map(b => b.name);
  assert.deepEqual(owed, ["asking", "ended-turn", "broken"]);
});

test("the words a person reads are not the words the wire uses", () => {
  assert.equal(labelOf("needs-decision"), "decision");
  assert.equal(labelOf("live"), "active");
  assert.equal(labelOf("working"), "working");
});
