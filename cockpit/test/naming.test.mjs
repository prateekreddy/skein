import { test } from "node:test";
import assert from "node:assert/strict";
import { slug, boxNameFor } from "../src/naming.mjs";

// The cases are `repos::tests::slug_and_box_name_handle_slashes`, copied deliberately: the point of
// this module is agreeing with that one, so the tests agree first.
test("a branch becomes a name the fleet can carry", () => {
  assert.equal(slug("feat/auth"), "feat-auth");
  assert.equal(slug("feat/auth/v2"), "feat-auth-v2");
  assert.equal(slug("user@host~weird"), "user-host-weird");
  assert.equal(slug("keep.dots_and-dashes"), "keep.dots_and-dashes");
  assert.equal(slug("/leading/and/trailing/"), "leading-and-trailing");
  assert.equal(boxNameFor("thing", "feat/auth"), "thing-feat-auth");
});

test("runs collapse and the ends are trimmed", () => {
  assert.equal(slug("a///b"), "a-b");
  assert.equal(slug("///"), "");
  assert.equal(slug(""), "");
  assert.equal(slug(null), "");
});

test("non-ascii letters are not kept, because the fleet does not keep them", () => {
  // A `\w`-based rule keeps these, and then the browser shows a name the fleet does not have.
  assert.equal(slug("caffè"), "caff");
  assert.equal(slug("日本語"), "");
});
