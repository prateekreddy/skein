import { strict as assert } from "node:assert";
import { test } from "node:test";
import { ago, ageNow, localStamp } from "../src/ago.mjs";

test("the same words the server used to send", () => {
  // Matched against `util::ago` deliberately: the point of moving it is one formatter, not a
  // second one that rounds differently and makes the board disagree with the CLI.
  assert.equal(ago(0), "0s ago");
  assert.equal(ago(59), "59s ago");
  assert.equal(ago(60), "1m ago");
  assert.equal(ago(3599), "59m ago");
  assert.equal(ago(3600), "1h ago");
  assert.equal(ago(86399), "23h ago");
  assert.equal(ago(86400), "1d ago");
  // Negative is clamped, as the Rust one clamps with `.max(0)`.
  assert.equal(ago(-5), "0s ago");
});

test("`we do not know` is not `just now`", () => {
  // `age_secs` is absent when a box's lastSeen is unparseable or missing, and rendering that as
  // "0s ago" would say a box was seen this second when nothing knows when it was seen at all.
  assert.equal(ago(null), "?");
  assert.equal(ago(undefined), "?");
  assert.equal(ago(NaN), "?");
  assert.equal(ageNow(null, 60000), "?", "an unknown age does not become known by waiting");
});

test("a row ages while the board receives nothing", () => {
  // The whole point: the stream no longer re-sends a box that only got older, so the row has to
  // age itself from the observation and the time since it arrived.
  assert.equal(ageNow(30, 0), "30s ago");
  assert.equal(ageNow(30, 45_000), "1m ago", "45s after a 30s-old observation is 75s");
  assert.equal(ageNow(0, 3_600_000), "1h ago");
  // A clock that steps backwards makes a row look freshly seen rather than negative.
  assert.equal(ageNow(10, -50_000), "10s ago");
  assert.equal(ageNow(10, undefined), "10s ago");
});

test("a server's UTC moment is shown in the reader's own zone", () => {
  // Node reads `TZ` when a date is formatted, so the zone can be chosen per assertion. Kolkata is
  // +5:30, which a formatter that merely stripped the `T` and `Z` cannot produce.
  const was = process.env.TZ;
  try {
    process.env.TZ = "Asia/Kolkata";
    assert.equal(localStamp("2026-09-24T10:12:00Z"), "2026-09-24 15:42");
    assert.equal(localStamp("2026-09-24T20:00:00Z"), "2026-09-25 01:30", "the date moves with the zone too");
    assert.equal(localStamp("2026-09-24T10:12:00Z", true), "15:42");
    process.env.TZ = "UTC";
    assert.equal(localStamp("2026-09-24T10:12:00Z"), "2026-09-24 10:12");
  } finally {
    if (was === undefined) delete process.env.TZ; else process.env.TZ = was;
  }
  assert.equal(localStamp(""), "");
  assert.equal(localStamp(undefined), "");
  assert.equal(localStamp("not a time"), "not a time", "never Invalid Date");
});
