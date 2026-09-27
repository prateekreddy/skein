import { strict as assert } from "node:assert";
import { test } from "node:test";
import { heldNote } from "../src/held.mjs";

test("nothing held, nothing said", () => {
  // Fails if a note is drawn for a field the server did not say is held — the always-there note
  // this replaced, which said "$SKEIN_AI overrides this" whether or not it was set.
  assert.equal(heldNote("ai_enrichment", {}), "");
  assert.equal(heldNote("ai_enrichment", undefined), "");
  assert.equal(heldNote("ai_enrichment", { review_model: "SKEIN_AI_MODEL" }), "");
});

test("a held switch is held off, and a held value is held by the variable the server names", () => {
  // Fails if the variable's name is not the server's (a hard-coded `$SKEIN_REVIEW_MODEL` would hide
  // `$SKEIN_AI_MODEL`, which wins over it), or if a switch is said to be merely "held" when the
  // only way the environment can hold one is off.
  assert.equal(heldNote("ai_enrichment", { ai_enrichment: "SKEIN_AI" }), "held off by <code>$SKEIN_AI</code>");
  assert.equal(heldNote("review_model", { review_model: "SKEIN_AI_MODEL" }), "held by <code>$SKEIN_AI_MODEL</code>");
});
