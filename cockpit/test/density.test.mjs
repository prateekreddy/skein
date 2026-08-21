import { test } from "node:test";
import assert from "node:assert/strict";
import { densityFor, COMFORTABLE, DENSE, MANY } from "../src/density.mjs";

test("density follows the size of the fleet, not the size of the window", () => {
  assert.equal(densityFor(0), COMFORTABLE);
  assert.equal(densityFor(MANY - 1), COMFORTABLE);
  assert.equal(densityFor(MANY), DENSE);
  assert.equal(densityFor(400), DENSE);
});

test("no rows is comfortable, not dense", () => {
  // The empty board is the one somebody reads a sentence on, so it must never be the scanning shape.
  assert.equal(densityFor(undefined), COMFORTABLE);
  assert.equal(densityFor(null), COMFORTABLE);
});
