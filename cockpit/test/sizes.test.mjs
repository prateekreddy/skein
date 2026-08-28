import { strict as assert } from "node:assert";
import { test } from "node:test";
import { fmtGB, fmtGb } from "../src/sizes.mjs";

test("bytes and mebibytes are different functions on purpose", () => {
  // The server sends bytes for memory and mebibytes for disk. One formatter taking "a number" is
  // one that eventually gets handed the wrong one, and a box using 2 GB would read as 2 MB.
  assert.equal(fmtGB(2 * 1073741824), "2.0G");
  assert.equal(fmtGB(512 * 1048576), "512M");
  assert.equal(fmtGB(0), "0M");
  assert.equal(fmtGb(2048), "2.0G");
  assert.equal(fmtGb(512), "512M");
  // The boundary, both ways round: exactly one unit is a G, one byte less is not.
  assert.equal(fmtGB(1073741824), "1.0G");
  assert.equal(fmtGB(1073741823), "1024M");
  assert.equal(fmtGb(1024), "1.0G");
  assert.equal(fmtGb(1023), "1023M");
});
