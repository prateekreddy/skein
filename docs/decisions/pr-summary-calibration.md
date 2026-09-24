# The depth of a pull request summary

**Decision.** The shape of skein's pull request summaries is the target, not a draft: a one-line
gist on every row; a fuller brief only when the change earns one; mechanical evidence from the diff
shown apart from what the model claims; and a reading that did not see everything names exactly what
it did not see, never a guess. Summaries are not to become chattier or terser.

**Date.** 2026-08-24.

**Decided by.** The project owner, on reading the summaries in use: they hit exactly the level of
detail wanted.

**Why.** The calibration was confirmed in use. The failure direction behind it is the module's own
rule: AI may only add scrutiny, never remove it, so a pull request skein has not actually read stays
at full attention and says so (`src/review/mod.rs:9-16`).

**Rules out.**

- Lengthening the one-line gist, or expanding every summary.
- Folding the diff's mechanical signals into the model's prose, where the two kinds of claim can no
  longer be told apart.
- A partial reading that says "some files were skipped" instead of naming them.

**Enforced at.**

- `src/review/summary.rs:35` — `Depth`: a line, an expanded brief, or not read.
- `src/review/summary.rs:51-54` — the one-sentence line, and the fuller brief kept for `Expanded`.
- `src/review/summary.rs:71-75` — the diff's signals are kept beside the brief, as a different kind
  of claim.
- `src/review/asking.rs:153` — a diff cut for size says how many files were not shown, and names
  them.
