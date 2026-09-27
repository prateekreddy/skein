import { strict as assert } from "node:assert";
import { test } from "node:test";
import { awayItems } from "../src/away.mjs";
import { groupOf } from "../src/groups.mjs";

const at = "2026-09-27T08:00:00Z";
const moment = (name, from, to) => ({ at, name, from, to });

test("several moments for one box become one line about where it ended up", () => {
  // Fails if the first moment's `to` is kept instead of the last: the box would read as "went back
  // to work" when it is in fact waiting on you.
  const items = awayItems(
    [moment("alpha", "waiting", "working"), moment("alpha", "working", "needs-input")],
    [{ name: "alpha", state: "needs-input", headline: "which schema?" }],
    groupOf,
  );
  assert.deepEqual(items.map(i => [i.name, i.kind, i.text]), [["alpha", "attn", "now needs a decision — which schema?"]]);
});

test("a box that went somewhere and came back is not news", () => {
  // Fails if the from/to comparison is dropped: a box that worked, waited and went back to work
  // would be listed although it is exactly where you left it.
  const items = awayItems(
    [moment("beta", "working", "waiting"), moment("beta", "waiting", "working")],
    [{ name: "beta", state: "working" }],
    groupOf,
  );
  assert.deepEqual(items, []);
});

test("a box the board no longer has says it left, and what needs you sorts first", () => {
  // Fails if a missing box is looked up as present (it would throw or vanish), or if the sort by
  // priority is dropped (the finished box would sort before the one waiting on you by name).
  const items = awayItems(
    [moment("aa-finished", "working", "done"), moment("zz-waiting", "working", "waiting"), moment("mm-gone", "working", "done")],
    [{ name: "aa-finished", state: "done" }, { name: "zz-waiting", state: "waiting" }],
    groupOf,
  );
  assert.deepEqual(items.map(i => [i.name, i.kind]), [["zz-waiting", "waiting"], ["aa-finished", "done"], ["mm-gone", "gone"]]);
});
