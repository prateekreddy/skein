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

(That the glyph alone could carry a verdict, with no freshness gate, was itself the defect —
SKEIN-321, now fixed: `title_is_spinning` takes the whole observation and requires
`title_is_fresh`, the same predicate the board already applied to the title's text.
`claude-waiting.example-box-6.frozen-spinner-title.2026-08-25.txt` is the capture that pins
it — this box, genuinely between turns, title `⠂ example-box-6`, one frame held across 40
consecutive `tmux display-message` samples, and a title whose text had not changed in 3h50m.)

## One edit is made to a capture, and only this one

`tmux capture-pane` output goes through a pipeline before it reaches a file here, and a
`str.rstrip()` anywhere in that pipeline eats the composer's trailing non-breaking space — which
this file says two paragraphs down is load-bearing. So the composer row is restored to
`❯\u{a0}` when a capture arrives without it. That is not a judgement about what the screen looked
like: every other capture from the same box carries the same two bytes (`grep -n '❯' *.txt | cat -A`),
and `box-pane.sh` writes the row raw. Nothing else in a capture is ever retyped.

## These captures are redacted, and what that costs

A pane is whatever was on somebody's screen. Half of these were captured on boxes working in
another organisation's repositories, so the prose above the status line is that organisation's:
its architecture, its pull requests, its tracker items, and the subject matter of its business.
None of it is read by any assertion, and a test fixture is a bad place for it.

**The rule, and it is a line drawn through the file rather than through a vocabulary.** A pane
splits in two at its status line. Everything BELOW — the status line itself, the chrome rows under
it, the composer, the rules, the meter and the mode footer — is what the grammar reads, and is kept
byte for byte. Everything ABOVE is prose the scan steps over without reading, and is replaced with
a same-shaped placeholder that says so: the same number of lines, the same paragraphs, the same
blank lines between them, and no line that could be mistaken for a status line, a composer, an
option row or a chrome row. Two chrome rows named a component and a page in that organisation's
product; those are substituted in place, keeping the row's lead glyph and its shape.

Two exceptions, both deliberate. The meter row is the owner's account rather than the pane, so its
figures are zeroed in place — the percentages, the tokens consumed, the time left in each window
and the spend — while the parts of it that are UI keep their values: the `5H` and `7D` window
labels, the `/1.0M` context size, and the model's name. Zeroing those would describe a row Claude
Code does not draw, and the point of a captured fixture is that it is not a description. And the
two `example-box-6` `agents-panel` captures keep their prose: it is this project's own work,
and it is the only prose here a reader of this repository can check against its own history.

**What is preserved is everything an assertion touches**: the status line, the composer line and its
exact glyph and spacing (`❯\u{a0}keep going` — that non-breaking space is load-bearing), the panel
rows, the separator chrome, the line count, and the blank-line structure between paragraphs. What is
replaced is only text the anchor scans past without reading.

**Redacting is not free and must be checked, not assumed.** A redaction that quietly turns a fixture
vacuous is the same failure as the spinner above, arrived at from the other direction. Two checks,
and the first is the cheaper one:

1. **Classify the bytes before and the bytes after, and compare.** Every capture here was run
   through the real `classify_pane` twice — once from a copy of the pre-redaction file, once from
   the committed one — with the same `title` and `title_age` its own test uses, and at a second
   `title_age` inside the freshness bound. All twelve gave the same verdict both ways, and the
   check itself was proved capable of failing by redacting one status line as well, which it named.
2. **Break the fix the fixture exists to pin and confirm the assertion still fails.** Done from the
   redacted files, one sabotage per fix: dropping `is_chrome` from the scan up the pane reddens
   seven tests; reverting the composer anchor to equality reddens `busy-queued-composer`; reading
   `is_compacting_line` from the whole tail reddens both `/compact` captures; dropping
   `title_is_fresh` reddens `frozen-spinner-title`; dropping the mode-footer arm of the composer
   test reddens the two idle captures. Every fixture below is covered by at least one of those.

## What reads what

Every capture here has a test, and a fixture nothing reads is a fixture nobody maintains.

| capture | the test it feeds |
|---|---|
| `agents-panel.…a` / `…b` | `a_turn_parked_on_background_agents_reads_busy_not_waiting`, and again as the control in `a_busy_box_whose_composer_holds_queued_text_still_reads_busy` |
| `busy-queued-composer` | `a_busy_box_whose_composer_holds_queued_text_still_reads_busy` |
| `frozen-spinner-title` | `a_frozen_spinner_glyph_in_the_title_is_not_evidence_of_work` |
| `todo-panel` | `a_running_tools_todo_panel_does_not_bury_the_status_line` |
| `working-series-a` / `-b` / `-c` | `three_consecutive_samples_of_one_live_turn_do_not_flap`; `-a` again in `tests/pane_attribution.rs` |
| `lattice.idle-after-compact` | `a_finished_compact_in_the_hook_log_is_not_live_compaction` |
| `gadget-case2.idle-after-compact` | `a_second_repos_box_reads_the_same_finished_compact_the_same_way` |
| `example-box-5.idle-recap` | `an_idle_pane_and_a_composer_holding_queued_text_both_read_waiting` |
| `refactoring.queued-composer` | the same test, as its second half |

## Naming

`<agent>-<state>.<box>.<what-is-unusual>.<date>[.<variant>].txt`

The `<what-is-unusual>` part is the point: these are kept because something about the pane was
surprising, and the filename should say what, so the next person does not have to diff two captures
to find the one character that matters.
