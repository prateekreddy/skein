import { strict as assert } from "node:assert";
import { test } from "node:test";
import { sandboxSize } from "../src/fleetsize.mjs";
import { fmtGb } from "../src/sizes.mjs";

test("the sandbox's size is what it measured, not what config.json asks the next create for", () => {
  // Fails if the line is drawn from anything but the measurement — the reading comes in MiB, so a
  // figure off by 1024 means a unit was crossed on the way.
  assert.equal(
    sandboxSize({ mem_total: 26624, cpus: 11, disk_total: 40960 }, fmtGb),
    "11 CPUs · 26.0G memory · 40.0G disk",
  );
});

test("a sandbox that has not answered says nothing rather than a size of zero", () => {
  // Fails if an unmeasured sandbox is drawn as "0 CPUs · 0M memory", which reads as a fact.
  assert.equal(sandboxSize(null, fmtGb), "");
  assert.equal(sandboxSize({ mem_total: 0, cpus: 0 }, fmtGb), "");
  assert.equal(sandboxSize({ mem_total: 2048 }, fmtGb), "2.0G memory");
});
