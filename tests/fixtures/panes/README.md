# Captured panes

Real `tmux capture-pane` output from real boxes, kept because every bug these fixtures pin was one
where the screen said something the board read wrong — and a hand-written pane is a description of
what somebody *thinks* the screen looks like, which is the thing that was wrong in the first place.

## Take the fixture from the probe's record, not from your own capture

`box-pane.sh` writes an observation; `tmux capture-pane` gives you a different one. They disagree,
and the disagreement is not cosmetic.

Measured 2026-08-25 while capturing `busy-queued-composer`: a hand capture caught a braille title
frame (`⠐`), and the probe's own paired record for the same box in the same minute had
`title: "_ example-box-6"`. A fixture carrying the spinner would have passed
`a_busy_box_whose_composer_holds_queued_text_still_reads_busy` through `title_is_spinning`
**whether or not the anchor was fixed** — a test that is green for a reason that has nothing to do
with what it claims to check. Use the `title` and `title_age` the probe recorded beside the tail.

(That the glyph alone can carry a verdict, with no freshness gate, is itself a defect — SKEIN-321.
`box-pane.sh`'s own comment and `TITLE_FRESH_SECS` both already say a stale title is not evidence
of work.)

## These captures are redacted, and what that costs

A pane is whatever was on somebody's screen: conversation text, spend, quota, box names. None of it
is read by any assertion, and a test fixture is a bad place for it — so prose regions, currency and
usage figures are replaced with same-shaped placeholders before the file is committed.

**What is preserved is everything an assertion touches**: the status line, the composer line and its
exact glyph and spacing (`❯\u{a0}keep going` — that non-breaking space is load-bearing), the panel
rows, the separator chrome, the line count, and the blank-line structure between paragraphs. What is
replaced is only text the anchor scans past without reading.

**Redacting is not free and must be checked, not assumed.** After redacting, break the fix the
fixture exists to pin and confirm the assertion still fails. `busy-queued-composer` was verified
that way: with the anchor reverted to equality it gives `left: Waiting, right: Busy`. A redaction
that quietly turns a fixture vacuous is the same failure as the spinner above, arrived at from the
other direction.

## Naming

`<agent>-<state>.<box>.<what-is-unusual>.<date>[.<variant>].txt`

The `<what-is-unusual>` part is the point: these are kept because something about the pane was
surprising, and the filename should say what, so the next person does not have to diff two captures
to find the one character that matters.
