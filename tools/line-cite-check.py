#!/usr/bin/env python3
"""Every `file:line` cited in `docs/` still names the line it was written to name.

CLAUDE.md's first rule is "derive, do not assert": where a claim is about the code, cite the file
and line or give the command. Nothing enforced the LINE half of it.

`tools/prose-check.py` checks that a citation can be FOLLOWED — the file is on disk, the line is
not past its end — and says in as many words that it does not check more:

    * A line that exists but does not hold what the sentence says it holds CANNOT BE CHECKED
      mechanically — that needs a reader who understands the sentence. This rule does not pretend
      to. [...] So GREEN HERE MEANS FOLLOWABLE, NEVER RIGHT

That is the gap, and it is the one that actually happens. Code moves here constantly: SKEIN-756
alone shifted hundreds of lines in `src/fleet.rs`, and every citation past an edit point went
stale without a character of the document changing. `dbe2318` re-derived **44 citation lines** in
`docs/recovery-survey.md` by hand, one at a time, and **nothing would have failed had it not**.

THE INSIGHT, WHICH IS THE SAME ONE `tools/citation-check.py` HAD ABOUT SHAS. The sentence does not
have to be readable by a machine. What is missing is not comprehension, it is A RECORD OF WHAT THE
LINE SAID WHEN THE CITATION WAS WRITTEN. With that record the check is exact and needs no reader:
the line either still says it or it does not, and when it does not, the text is usually still in
the file somewhere and the repair is a line number, not an afternoon. `docs/line-cites.toml` is
that record, the way `docs/citations.toml` is the record for commit subjects.

WHY THE ANCHOR IS DERIVED FROM GIT AND NOT FROM TODAY'S TREE. Recording "whatever is at that line
now" would bless every citation that has ALREADY drifted, and a ledger seeded from a rubber stamp
is worth nothing. So the anchor for a citation is read out of the tree AS IT STOOD IN THE COMMIT
THAT WROTE THAT LINE OF THE DOCUMENT — `git blame` names it — and only a citation that is not
committed yet is read against the working tree, because its author is looking at the working tree
as they write it. Seeded that way on 2026-09-11 this gate was RED on the day it landed, with 75
findings across `docs/`, every one of them real (see `SEED_NOTE`).

WHY `--record` CANNOT BE USED TO CLEAR A FINDING, which is the property that keeps this from
becoming a rubber stamp of its own. `--record` only ADDS entries for citations the ledger has not
got, and PRUNES entries nothing cites any more. It never overwrites a recorded anchor — so running
it by reflex against a drifted citation changes nothing and says so. A drifted citation has
exactly two ways to green:

  * `--relocate --write`, which moves the LINE NUMBER in the document to wherever the recorded
    anchor now sits and rekeys the ledger. The claim is unchanged; only the address moved. This is
    the mechanical case and it is most of them.
  * a person edits the citation, because the thing it cited is gone. That is a new citation at a
    new line, so it is a new key, and `--record` records it — with the anchor visible in the
    ledger's diff, which is where the reviewer reads what is being claimed.

THE THREE DESIGNS THAT WERE MEASURED AND NOT CHOSEN. Each is right somewhere in this tree, and the
measurements are the reason none of them is the gate:

  cite the symbol, not the line.  `docs/inventory.md` does exactly this for its call-site table,
      and was right to: "Every line number this table gave had drifted — one pointed at
      `lib.rs:1489` in a file that is now 78 lines long". But a symbol cannot ADDRESS most of what
      this project's prose cites. Measured over the 420 backticked `path:line` citations in
      `docs/` on 2026-09-11: **254 of them collapse onto a symbol another citation already uses**
      — 33 distinct citations land on `src/bin/skein.rs`'s `WARN` alone, which is one `skein
      doctor` line each — and **68 name a file with no symbols to cite**, almost all of them
      `src/web/index.html`. A survey of individual messages inside one function is not expressible
      in symbols, and that is what the documents with the most citations are.
  pin the lines to a commit.      Already built: `prose-check.py`'s `CITATIONS_AT` resolves a
      dated review's citations in the tree of the commit it declares, and `docs/review-product-
      review.md` declares `7b67ae2`. Right for a DATED ACT — a review written on a day, whose
      claims are about that day — and this gate honours it by skipping those documents whole. It
      is wrong for a LIVING MAP. `docs/recovery-survey.md` declared the same pin at `7101dfd`, and
      the next commit on the branch, `dbe2318`, re-derived 44 of its citation lines against the
      working tree: the policy lost to the practice inside a day, because a citation a reader has
      to `git show` to follow is a citation nobody follows.
  line plus an anchor phrase the checker greps for.  The right shape, and the wrong anchor, AS
      THE ONLY ANCHOR. The phrase would have to be the document's own words, and a document
      paraphrases: it writes `N%` where the code writes `{pct}%`, elides at `…`, and describes
      where it does not quote. Measured over `docs/recovery-survey.md`'s 190 table rows, whose
      second column is the message itself: **29 of the quoted fragments appear within three lines
      of the citation, and 117 appear nowhere in the cited file at all.** A gate on that anchor is
      red on 161 rows the day it lands, and a gate in that state is switched off within a week.
      It is a second OPINION though, and `misanchored` below is that measurement put to the two
      uses it supports: judge on the phrases the cited file holds EXACTLY ONCE, and on the ones a
      survey row quotes IN FULL — where every line holding the message is a site of it, however
      many there are (SKEIN-889) — and be silent about every row that paraphrased.

WHAT THIS COVERS

  a citation left behind by    The common case, and the only one with a mechanical repair. The
  an edit above it             anchor is found elsewhere in the file, `--relocate` names the new
                               line, `--write` applies it to the document and the ledger.
  a citation whose target      Caught, and deliberately NOT repaired: the sentence has to be
  was changed or deleted       re-read by a person, and the finding says so and prints what the
                               line used to say, which is what they need to find its successor.
  a citation added without     Fails until `--record` is run, so a new citation's anchor lands in
  a recorded anchor            the same commit as the citation and the reviewer sees both.
  a whole file renamed away    `prose-check.py` already fails on this; this gate would see it as
                               an unresolvable path and leaves it there rather than double-report.
  a file that became a          Followed, which is the one rename with a mechanical answer. A Rust
  directory                     unit is `src/<name>.rs` OR `src/<name>/**/*.rs` (`rustcut.units`),
                                so when `src/fleet.rs` is split into `src/fleet/` the anchor is
                                searched for across every file of the directory, `moved` names the
                                new FILE as well as the line, and `--relocate --write` rewrites
                                both (`Tree.successors`, SKEIN-934). Before this, such a citation
                                fell out of the gate as unresolvable — and because a citation the
                                scan drops is an entry nothing claims, the next `--record` or
                                `--relocate --write` PRUNED its anchor: 89 of them for that split.
  a citation that inherited     `inherited`, and it is the failure the ledger's KEY makes
  an anchor another citation    possible: an entry is keyed by ADDRESS ALONE, so a row that comes
  wrote                         to cite an address another row already recorded is judged against
                                the other row's anchor — and `--relocate` then moves it to
                                wherever THAT anchor sits now. It happened: `docs/recovery-
                                survey.md`'s cross-origin row was hand-corrected onto a line the
                                ledger held for the unsupported-runtime row, one `--relocate
                                --write` moved it onto the unsupported-runtime line, and every
                                gate was green over the result (SKEIN-936). The signal is the same
                                one `--record` reads: what the cited line said in the commit that
                                wrote THIS citation's document line. Where that is not the anchor
                                the ledger holds, the anchor is not this citation's record, and
                                no relocation of it can be right.
  a citation whose anchor was   `misanchored`, and it is a DIFFERENT THING from every row above:
  never the right line          those drifted, this one was wrong when it was written. The ledger
                               cannot see it — an anchor records what the cited line SAID, not
                               whether it was the line meant — so before SKEIN-858 the gate
                               DEFENDED it: `docs/recovery-survey.md` cited
                               `src/bin/skein-server.rs:100`, a bare `}`, for a message that is at
                               `:137` and has never been anywhere else, and the gate was green
                               over it. The signal is in the document: where a row QUOTES the
                               code's own words, the line the repair would leave this citation on
                               should hold them — and where the row quotes its message IN FULL,
                               any of the lines that hold it will do, which is what let the
                               FAMILY rows be wrong in silence (SKEIN-889, `quoted_whole`).
                               Reported only where the anchor and the words
                               DISAGREE, and then not repairable by `--relocate`, because
                               following a wrong anchor to its new address is how these spread
                               (`b9db291` moved 162 at once). Where they agree the citation has
                               merely `moved` and the one-command repair is right — see `check`.

WHAT THIS DOES NOT COVER, AND WILL NOT

  * A citation at a line that holds exactly what was recorded, is the wrong line for the sentence
    around it, AND whose sentence quotes nothing the file holds — a row that paraphrases its
    message, or describes rather than quotes. Nothing mechanical reads a sentence;
    `prose-check.py` draws the same boundary, and `misanchored` is silent on 101 of the survey's
    228 site rows for exactly this reason. What the ledger adds is that the claim was recorded
    once, in a diff a person read.
  * A citation inside a fenced block, a mockup or a transcript — `citations()` in
    `prose-check.py` strips those, and this gate uses that reader rather than a second one.
  * A citation whose path names more than one file in the tree. `prose-check.py` counts those and
    declines to guess; so does this.
  * The ledger this tool writes, `docs/line-cites.toml`. See `gated`.
  * Anything outside `docs/`. `prose-check.py` reads citations from `src/`, `tests/`, `tools/`,
    `cockpit/` and `warden/` as well; this gate does not, because its ledger would then have to
    carry an anchor for every citation in a comment in the tree, and that population has not been
    measured. `--all` widens the scan and is a report, not a gate.

REFUSES TO RUN RATHER THAN PASS QUIETLY. CLAUDE.md's leak check answered `0` beside 195 matching
processes because the list it carried had gone stale, and a check that cannot fail is worse than no
check (SKEIN-647); `tools/continuation-check.py` exits 2 when it derives no Rust, for the same
reason. So this one derives its documents and its citations from the tree, exits **2** — not 0 —
when it derives no documents, no citations, resolves none of them, or finds that not one citation
in `docs/` quotes a phrase the tree holds (which is what a broken claim reader looks like from the
inside, and would otherwise read as "nothing is misanchored"), and runs `self_check()` on every
invocation over a tree it builds itself, proving on each run that it catches a moved citation,
relocates it, catches a deleted one and does NOT relocate it, honours a `historical` declaration,
refuses to let `--record` overwrite a drifted anchor, carries BOTH anchors across when one
citation relocates onto another's line — while refusing outright when two different anchors would
have to share one key — and, for the misanchor rule, that it reads a row's words at all, convicts
a citation whose words are elsewhere, outranks `moved` when the two signals DISAGREE and defers to
it when they AGREE — proved with one tree where only the row's quoted words differ — convicts one
whose row quotes its message IN FULL and sits at none of that message's several lines, and stays
silent on a paraphrase, on a FRAGMENT the file holds twice, on that same full quote cited at one
of its lines, and on a message wrapped inside the call the citation names. Read the cases, not
this sentence: a list of properties written in prose beside
the code that proves them is a list that goes stale.

AND THE REPAIR REPORTS WHAT IT WROTE, NOT WHAT IT MEANT TO WRITE. `--relocate --write` re-reads
the documents and the ledger back off the disk after writing them and counts its findings there,
because the run that bought SKEIN-821 printed "37 citation(s) rewritten; 0 left for a person"
from its own intentions in the same breath as it dropped an anchor, and the next gate run then
reported a verdict that had not existed before the repair.

MODES

    python3 tools/line-cite-check.py             # the gate: docs/ against docs/line-cites.toml
    python3 tools/line-cite-check.py --all       # every file prose-check reads. A report, not a gate
    python3 tools/line-cite-check.py --record    # add anchors for new citations, prune dead entries
    python3 tools/line-cite-check.py --relocate  # where each drifted citation's anchor sits now
    python3 tools/line-cite-check.py --relocate --write
                                                 # and rewrite the documents and the ledger to match
    python3 tools/line-cite-check.py --relocate --write --only src/fleet.rs
                                                 # ... only the citations that name that file
    python3 tools/line-cite-check.py --relocate --write --in docs/architecture.md
                                                 # ... only the ones written in that document

WHY THE REPAIR TAKES A FILTER, AND WHY ONLY THE REPAIR DOES. Parallel lanes here are given
disjoint FILES, and an insertion into one source file moves citations in every document that names
it: a 474-line insertion into `src/fleet.rs` produced 60 findings across five documents on
2026-09-20, and `--relocate --write` rewrote all five or none (SKEIN-976). Two lanes editing two
different source files therefore both rewrote `docs/recovery-survey.md` and `docs/prose-
symbols.toml`, each correct about its own file and wrong about the other's, and the merge had to be
resolved row by row by asking which file each citation named. `--only src/fleet.rs` is that
question asked once: a lane repairs the citations ITS change moved and reports the rest, so it
never rewrites a citation into a file it did not touch, and its ledger edits stay inside that
file's own block of keys — the ledger is sorted by cited path. `--in` is the same filter on the
other side, for a lane that owns a document rather than a source file.

**IT DOES NOT MAKE THE TWO LANES DISJOINT, and the measurement is what says so** rather than the
hope. Built as a fixture on 2026-09-20 — 474 lines into `src/fleet.rs` on one branch, 120 into
`src/health.rs` on the other, each repairing only its own — the merge conflicted in
`docs/recovery-survey.md` in three hunks, exactly as many as without the filter, because the rows
that conflict name BOTH files on ONE line and a line is git's unit. The filter is worth having for
what it does do; what makes the merge mechanical is the recipe below.

**Neither filter narrows the GATE**, and asking for one without `--relocate` is refused rather
than honoured: a gate that reads part of the tree and reports "0 problems" is the shape this file
exists to refuse (SKEIN-647). What is withheld is printed, every time, with the flag that withheld
it — a partial repair that cannot be told from a whole one is how a half-repaired tree gets
committed (SKEIN-821).

MERGING TWO BRANCHES THAT BOTH REPAIRED CITATIONS. Do not resolve those hunks row by row. **Take
either side of each conflicted document whole, then run `--record`, then `--relocate --write`**:
the first gives an anchor to every citation the resolution left the ledger without — read out of
the commit that wrote its document line, not out of today's tree — and the second re-derives every
line number from the anchors, which is the one thing in the conflict that did not move. It is
sound because each side was green when it was committed, so the commit that wrote any line of it
is one where that side's citations were right; and it is checked rather than trusted, because a
citation the anchors cannot resolve is reported instead of guessed. On the fixture above that
recipe left seven findings, every one an `ambiguous` anchor the insertion alone had already made
unrepairable (SKEIN-823), and not one line derived by hand.
"""

import importlib.util
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER = os.path.join(ROOT, "docs", "line-cites.toml")
LEDGER_REL = "docs/line-cites.toml"

# Files ASSEMBLED from a directory, and the directory. `cockpit/build.mjs` builds `src/web/index.html`
# from `src/web/app/` byte for byte (SKEIN-1104), so the page is on disk and every line of it is a
# line of one of the parts — but it is the one copy nobody may edit, and a citation into it sends its
# reader there. So a citation of it is followed into the parts exactly as a split file's is into its
# directory (`Tree.successors`), and is a finding until it names the part.
ASSEMBLED = {"src/web/index.html": "src/web/app"}

# The one citation reader every gate shares. `prose-check.py` owns the regex, the fence stripping,
# the path resolution and the `CITATIONS_AT` pin; a second copy of any of them would be a second
# thing to drift, which is the failure this whole tool is about. The file name has a hyphen, so it
# cannot be `import`ed by name.
_spec = importlib.util.spec_from_file_location(
    "prose_check", os.path.join(os.path.dirname(os.path.abspath(__file__)), "prose-check.py")
)
prose = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(prose)

SEED_NOTE = """\
THE SEED, AND WHY IT WAS NOT A RUBBER STAMP. Measured on 2026-09-11 at `56e149a` by deriving every
anchor from the commit that wrote its citation, before any of them was recorded: 452 citations in
`docs/`, of which 53 are in the one document that declares a `CITATIONS_AT` pin and are skipped, 2
name more than one file, and 5 named a line that was already blank in the commit that wrote them.
Of the remaining 392, **317 still hold what they were written against and 75 do not**:

  68  the cited text is still in the file, at another line — a repair `--relocate --write` makes
   4  the cited text is gone from the file entirely — a person has to re-read the sentence
   3  the cited text is in the file several times over, so the repair cannot be chosen mechanically

By document: 55 in `docs/recovery-survey.md`, 13 in `docs/prose-symbols.toml`, 5 in
`docs/review-ux.md`, 1 in `docs/inventory.md`, 1 in `docs/residue.toml`. The `.toml` ones are
generated by their own tools' `--update` and a relocation here is at worst redundant with that;
they are gated all the same, because a `sites = [...]` entry is read by `prose-check.py` as a
citation and passed by it for the same reason every other one was."""

# Line 1 of the ledger, so that a person who opens it knows what writes it and what it is for
# before they read a single entry.
LEDGER_HEADER = """\
# What each `file:line` cited in `docs/` said when it was cited. Read and written by
# `tools/line-cite-check.py`, which fails the build when a cited line no longer says it.
#
# Generated — `--record` adds an entry for a citation that has not got one, and prunes an entry
# nothing cites any more. It NEVER overwrites an anchor, so running it against a stale citation
# does not clear the finding: the repair is `--relocate --write`, or a person re-reading the
# sentence. That is the property that keeps this file from becoming a rubber stamp.
#
# An anchor is the cited line with its runs of whitespace collapsed, read out of the tree as it
# stood in the commit that wrote that line of the document.
#
# `historical = "<why>"` in place of `line` declares a citation that is MEANT to point at code
# this tree no longer has — a survey row that is the record of what was wrong, a name a document
# discusses in the past tense. It is skipped, and the reason is written by a person: a reason that
# says nothing ("historical", "old") is worse than no entry, because the next person cannot tell
# an exemption that was thought about from one that was pasted.
"""


def norm(text):
    """A line with its runs of whitespace collapsed.

    Indentation is the one thing that changes about a line without the line changing: rustfmt
    re-indents a whole block when the `if` above it grows a condition, and a citation to a line
    inside it is not thereby wrong. Everything else — a character of the code, a word of a
    message — is a real change and this gate should see it.
    """
    return " ".join(text.split())


def window(lines, n, radius=1):
    """The normalised text of lines `n-radius .. n+radius`, 1-based, clipped to the file.

    An anchor of one line is not always unique — `}` is not a citation target anybody means, but
    it is what a citation to a match arm's last line reads as. Measured over the 75 drifted
    citations in `docs/` on 2026-09-11: the single line alone chooses uniquely for 8 of them and
    the three-line window for 60, and the remaining 3 cannot be chosen by either, which is
    reported rather than guessed.
    """
    lo, hi = max(0, n - 1 - radius), min(len(lines), n + radius)
    return "\v".join(norm(lines[j]) for j in range(lo, hi))


def plain(text):
    """`text` with runs of whitespace collapsed AND the markup a document adds removed.

    Separate from `norm` on purpose, and the two are not interchangeable. `norm` is what an
    ANCHOR is stored as, so it must keep every character of the code: a backtick inside a Rust
    string is part of the line. `plain` is for comparing a DOCUMENT's words with the code's, and
    there a document's own markup is noise — `docs/recovery-survey.md` writes a message's
    `sbx` in backticks where `src/fleet.rs` writes it bare, and writes `**R**` where the code
    writes nothing at all. Comparing without stripping them produced a false negative and, one
    row further on, a false positive: `warden/src/doer.rs:301` holds `could not run \\`sbx\\`: {e}`
    and the row quotes exactly that, but with the backticks the two did not match, so the row's
    OTHER message (the one belonging to `:304`) became the only thing left to judge `:301` by.
    """
    return " ".join(re.sub(r"[`*]", "", text).split())


# A cell that is nothing but citations — the "where" column of a survey table. EVERYTHING OUTSIDE
# THE BACKTICKS HAS TO BE PUNCTUATION, and that is the whole test: it tells a site cell from a
# prose cell that happens to carry a citation without needing a list of the joining words a
# document is allowed to use, which is the kind of list that goes stale (SKEIN-647).
#
# The pattern this replaces admitted `(+` and nothing else, so it found no site cell in 21 of
# `docs/recovery-survey.md`'s 243 rows — thirteen of which name the function in the cell
# (`` `src/fleet.rs:1450` (`fleet_lifecycle_refusal`) ``), the rest a count or a second message's
# words. `claimed` then fell back to reading the site cell itself for a quotation, found none, and
# the misanchor rule said nothing at all about those rows. Five of them were citing a function's
# `fn` line while the message their row quotes sat 7 to 110 lines below, two being the very
# spot-checks SKEIN-858 named and could not catch (`room_to_copy_out`, `cockpit_port_advice`).
SITE_CELL = re.compile(r"^(?:[^`A-Za-z]*`[^`]*`)+[^`A-Za-z]*$")

# What a document writes where the code does not write it verbatim, so a run of the code's own
# words ends here: an interpolated `{name}`, an elision at `…`, a `[bracketed]` aside, and `·`,
# which is this document's separator between two messages quoted in one cell.
NOT_VERBATIM = re.compile(r"\{[^}]*\}|…|\.\.\.|\[[^\]]*\]|·")

# Prose, rather than a table, quoting the code: the quotation marks are the claim.
QUOTED = re.compile(r"\"([^\"\n]{4,})\"|“([^”\n]{4,})”")


def message_cell(row, cite_text):
    """The cell a SURVEY ROW presents as its message, for a citation in that row's `where` column.

    `None` when the row is not a table row in that shape — prose, or a citation sitting in some
    other column of a table row, such as the `reach` cell that names the cockpit's copy of a
    message. Those are read for quotations instead (`claimed`), and they are NOT read for a
    quotation in full (`quoted_whole`), because a sentence quotes a fragment by design: five real
    citations quote a button's label — `try again` in `src/web/index.html` eight times over,
    `log in` three, `request changes` four — where the citation names the block that draws it and
    not the line the label is on. `""` — not `None` — when the citation IS in a site cell and the
    row carries no message cell at all, because that is a row claiming nothing rather than a row
    of another shape.
    """
    if not row.lstrip().startswith("|"):
        return None
    cells = [c.strip() for c in row.strip().strip("|").split("|")]
    site = next((i for i, c in enumerate(cells) if cite_text in c and SITE_CELL.match(c)), None)
    if site is None:
        return None
    rest = cells[site + 1 :] + cells[:site][::-1]
    return next((c for c in rest if not prose.CITATION.search(c)), "")


def quoted_whole(row, cite_text):
    """The run a survey row quotes IN FULL — its message cell, end to end and verbatim. Or `None`.

    THIS IS THE ONE PLACE A PHRASE THE FILE HOLDS SEVERAL TIMES STILL JUDGES A CITATION
    (SKEIN-889), and the reason is that the cell is the whole claim rather than a piece of one.
    Where a row's message cell is one run with nothing else in it — no interpolation, no elision,
    no second message — the document is saying "this is the message", and then EVERY line holding
    that message is a site of it. A citation near none of them names no site, whether the message
    is at one line or at eighteen.

    That is strictly stronger than `sole_line`'s uniqueness, and it is stronger exactly where the
    silence was hiding the wrong citations. `docs/recovery-survey.md`'s `no such repo` row cited
    `src/bin/skein-server.rs:1217` — a blank line before a doc comment — while the string sits at
    thirteen `return` lines, not one of them within 35 lines of it; the `invalid box name` row
    named twelve further sites, eight of which hold no message at all. Both were green, because
    thirteen is not one. The family rows are where a survey's worst citations live and they are
    precisely the rows uniqueness cannot speak about.

    A PARAPHRASE IS STILL SILENT, and that is the boundary this draws against `sole_line`: the
    cell has to be the run, not contain it. `could not read` is six words of a message
    `src/web/index.html` writes differently and holds six times over — quoted as a FRAGMENT of a
    cell that says more, it judges nothing, which is what kept an early draft of the misanchor
    rule from reporting an 83-line error against a correct citation. `self_check` case 15 is that
    fragment and case 21 is this rule, and they are one boundary read from either side.
    """
    cell = message_cell(row, cite_text)
    if not cell:
        return None
    found = runs(cell)
    if len(found) == 1 and found[0] == plain(cell):
        return found[0]
    return None


def claimed(row, cite_text):
    """The runs of text `row` presents as the CITED FILE's own words. `[]` when it presents none.

    Two forms, both derived from the document rather than listed anywhere:

      a table row     whose citation sits in a cell that is nothing but citations — then the
                      claim is the first other cell that carries no citation of its own. That is
                      the shape of every survey table here: "where" names the site, the next
                      column is what a person sees. 228 of `docs/recovery-survey.md`'s rows are
                      in it, measured at `80d9143`.
      prose           quoting the code between quotation marks, on the line the citation is on.
                      It reaches nothing today — 2 citations in `docs/review-ux.md` sit beside a
                      quotation and neither quotes a phrase its own file holds — and it is here
                      because a table is not the only way a document quotes code, and because a
                      reader of this function should not have to guess which forms it handles.
                      `self_check` case 10 proves both forms are read.

    A run ends at anything the document did not copy verbatim (`NOT_VERBATIM`): a document
    paraphrases, and the ELIDED parts are exactly where it does. This is the distinction the
    docstring's third rejected design got wrong by taking the whole quoted fragment — 117 of
    those appear nowhere in the cited file, and a gate red on 117 rows is switched off.
    """
    cell = message_cell(row, cite_text)
    if cell is not None:
        return runs(cell)
    if row.lstrip().startswith("|"):
        cells = [c.strip() for c in row.strip().strip("|").split("|")]
        row = next((c for c in cells if cite_text in c), "")
    out = []
    for m in QUOTED.finditer(row):
        out.extend(runs(m.group(1) or m.group(2)))
    return out


def runs(claim):
    """`claim` cut into the runs it quotes verbatim — several words each, markup stripped.

    SEVERAL WORDS, and that is not a length threshold in disguise. One word can be the code's and
    still be nobody's evidence: `docs/recovery-survey.md:874` quotes two messages in one cell,
    and the bare word `exited` out of the second of them was enough to convict the first of
    naming the wrong line. A run with a space in it is a phrase a document either copied or did
    not.
    """
    out = []
    for part in NOT_VERBATIM.split(claim):
        part = plain(part).strip("—–-·✗!✓?\"' ").strip()
        if " " in part:
            out.append(part)
    return out


def sole_line(lines, run):
    """The one line of `lines` holding `run`, or `None` if none does or several do.

    UNIQUENESS IS WHAT MAKES THIS SAFE WITHOUT A LENGTH THRESHOLD, and it cuts both ways:

      * a run the document paraphrased is in the file nowhere, so nothing is concluded from it —
        which is why this check is SILENT on the 99 of 223 survey rows that quote no findable
        phrase, rather than red on them;
      * a run generic enough to be in the file twice — `skein-server:`, `reconnecting`, `could
        not read` — never judges anything. Those three are the reason the first draft of this
        rule reported 37-line and 108-line "errors" against citations that were perfectly right.

    All three are FRAGMENTS of a cell that said more. A row that quotes its message IN FULL is
    the other case and does not come here at all: see `quoted_whole`, where the count stops
    mattering because every line holding the message is a site of it (SKEIN-889).

    `lines` is already `plain`-normalised by the caller, which reads each file once however many
    citations ask about it: `src/web/index.html` alone carries about ninety.
    """
    found = None
    for n, text in enumerate(lines, 1):
        if run in text:
            if found is not None:
                return None
            found = n
    return found


def every_line(lines, run):
    """Every line of `lines` holding `run`. What `sole_line` refuses to answer, for the one claim
    shape entitled to ask it: a message quoted IN FULL, whose every occurrence is a site of it."""
    return tuple(n for n, text in enumerate(lines, 1) if run in text)


def message_region(lines, cite):
    """The lines a citation may mean by naming `cite.line` — where its message is allowed to sit.

    Three parts, each measured on this document rather than chosen:

      the cited line itself, and any range it declares.
      its immediate neighbours, radius 1 — the same radius `window()` uses, and the convention
          the survey follows: cite the `HealthCheck::unsatisfied(` or the match arm, and the
          message is the line under it. 61 of the 133 checkable citations at `80d9143` sit at
          distance 0 or 1 this way.
      the continuation of any bracket the cited line leaves open, so a message that is an
          ARGUMENT to the call the citation names is inside it however rustfmt wrapped it.
          `src/health.rs:429` is `[owner, mine, path] => HealthCheck::unsatisfied(` and its
          message's second line is at `:431`; NINE citations are in that state at `80d9143` —
          three more in `src/health.rs`, one in `src/bin/skein.rs`, one in `src/volume.rs` and
          three in `warden/` — and every one of them is a correct citation.

    Without the third part those nine read as findings; without the second, six more do. Both
    tolerances are what a citation to a CALL means, and neither reaches the 37-to-171 line
    distances that the anchors this check exists for are away from their messages.
    """
    hi = max(cite.line, cite.last or cite.line)
    region = set(range(cite.line - 1, hi + 2))
    depth, n = 0, cite.line
    while n <= len(lines) and n <= cite.line + 400:
        for ch in lines[n - 1]:
            if ch in "([":
                depth += 1
            elif ch in ")]":
                depth -= 1
        region.add(n)
        if n >= hi and depth <= 0:
            break
        n += 1
    return region


def own_words(cite, tree, words):
    """{a run of the code's own words the row quotes: the lines of the cited file holding it}.

    THE LINES, PLURAL, AND WHICH ONES DEPENDS ON HOW THE ROW QUOTED (SKEIN-889). A fragment of a
    cell that said more is believed only where the file holds it exactly once (`sole_line`), so a
    generic phrase judges nothing. A cell that IS the message, quoted end to end, names every line
    holding it (`every_line`), because each of those is a site of that message — and a citation
    near none of them is wrong at thirteen occurrences as surely as at one.

    Still empty when the row quotes nothing, or nothing it quotes is findable — and that is
    the whole reason this check does not have to understand a sentence. It reports on the rows
    where the document and the code can be compared CHARACTER FOR CHARACTER, and says nothing
    about the rest. Measured at `80d9143`: of 424 citations in `docs/`, 269 sit beside words a
    document quotes and 133 quote a phrase their cited file holds EXACTLY ONCE — 127 of the
    survey's 228 site rows, leaving 101 of them this check says nothing about. The full-quote
    half is the smaller and the sharper. At `8c372f4`, of the 364 citations sitting beside quoted
    words, 197 quote a phrase their file holds, 22 of those quote their message cell IN FULL, and
    6 of THOSE name a message the file holds at more than one line — `invalid box name` at 18
    lines, `no such repo` at 14 — every one of which uniqueness alone could say nothing about.
    Those counts are not asserted here: the report prints them for the tree in front of you, and
    the 6 is the number that goes to zero when a message cell stops being read.

    `words` caches `plain`-normalised file bodies, because the same file is asked about by up to
    ninety citations.
    """
    lines = tree.now(cite.target)
    if lines is None or cite.line > len(lines):
        return {}
    if cite.target not in words:
        words[cite.target] = [plain(t) for t in lines]
    body = words[cite.target]
    whole = quoted_whole(cite.row, cite.text)
    sites = {}
    for run in claimed(cite.row, cite.text):
        if run == whole:
            at = every_line(body, run)
        else:
            n = sole_line(body, run)
            at = () if n is None else (n,)
        if at:
            sites[run] = at
    return sites


# A relative address — how a row USED to name a second site in the same file: `(+ `:5112`)`.
# `prose-check.py`'s reader does not see these, because they carry no path, so the ledger has no
# anchor for one and `--relocate` has never moved one: the number is whatever somebody typed,
# however long ago. SKEIN-876 measured the 77 written `(+ …)` against the commit that wrote the
# survey: 31 had moved, 6 were gone and 14 had gone ambiguous, and 66 were repaired by hand. No
# gate could have said so, and the `where` column held 101 of them once the other spellings —
# `(13 sites: …)`, `/ :N` — are counted.
#
# They are gone from that document's `where` column now — a row names the sites whose words it
# quotes as ordinary citations, and gives the command for a family larger than that — and
# `stray_sites` is what keeps them out. This pattern still reads them for `row_lines`, because
# §10's prose lists and the reach cells use the same form and that is SKEIN-877's to settle.
ALSO_AT = re.compile(r"`:(\d+)(?:-\d+)?`")


def row_lines(cite):
    """Every line of `cite.target` the row names, `cite`'s own included.

    THE ROW IS THE CLAIM UNIT, not the citation. `docs/recovery-survey.md:705` cites
    `src/web/v2.html:466` and quotes two messages separated by `·`, the second of which is at
    `:526` — which the row names, in the same cell, as `(+ `:526`)`. Judging the first citation
    against the second message's line reported a correct row as a finding; that was the one false
    positive this rule produced over 424 citations, and it is the reason this function exists.

    Relative and absolute both. The survey's site cells are all absolute now (SKEIN-876); the
    relative form survives in its prose and its reach cells, where the same exclusion applies.
    """
    lines = {cite.line}
    if cite.last:
        lines.add(cite.last)
    for m in prose.CITATION.finditer(cite.row):
        # `resolve` against a one-file index answers the question this needs — does that path name
        # THIS file — and it is the same tail matching the rest of the tool resolves citations by.
        if prose.resolve(m.group(1), [cite.target]) == cite.target:
            lines.add(int(m.group(2)))
    lines.update(int(m.group(1)) for m in ALSO_AT.finditer(cite.row))
    return lines


def stray_sites(sources):
    """([(label, line, address)], site cells read) for a relative address in a `where` column.

    THE FORM THIS REFUSES NAMES NO FILE, so nothing resolves it: `prose-check.py`'s reader does
    not see it, the ledger cannot hold an anchor for it, `--relocate` cannot move it, and the
    number in the document is whatever somebody typed. Of the 77 that SKEIN-876 measured in
    `docs/recovery-survey.md` against the commit that wrote it, 31 had moved, 6 were gone and 14
    had gone ambiguous — and every gate in this repository was green over all of it. A row names
    the sites whose words it quotes as ordinary citations now, and gives the command for a family
    larger than that.

    SCOPED TO THE FIRST CELL OF A CITING TABLE ROW — the `where` column — and deliberately not to
    prose. §10's exclusion lists write the same form ACROSS A LINE BREAK: `src/repos.rs:310` ends
    one line and `:1684`, `:1688` open the next, where the nearest full citation on the line is
    `src/gitgate.rs:683`. Resolving those needs reading order, not a line, so a rule that tried
    one line at a time would charge two of them to the wrong file — a check reporting on something
    other than what it names, which is the defect this repository keeps paying for. They are
    SKEIN-877's. In a site cell the whole claim is on one line and the fix is to spell the path.

    Returns the count of site cells it read as well, because `[]` is what a broken reader returns
    too (SKEIN-647): `docs/recovery-survey.md` carried 243 of them on 2026-09-12.
    """
    found, read = [], 0
    for label, body, _ in sources:
        if label == LEDGER_REL or not gated(label):
            continue
        for n, line in enumerate(body.split("\n"), 1):
            if not line.lstrip().startswith("|"):
                continue
            cell = line.strip().strip("|").split("|")[0]
            if not prose.CITATION.search(cell):
                continue
            read += 1
            found.extend((label, n, ":" + m.group(1)) for m in ALSO_AT.finditer(cell))
    return found, read


def unaccounted(cite, tree, words, landing=None, crossing=None):
    """{phrase: lines} for the words this citation — and no other address in its row — answers for.

    THIS IS THE HALF THE LEDGER CANNOT SEE. An anchor records what the cited line SAID; it says
    nothing about whether that line was the right one, so a citation recorded from the wrong
    line is defended by this gate for ever — `docs/recovery-survey.md` cited
    `src/bin/skein-server.rs:100`, a bare `}`, for a message that is at `:137` and has never been
    anywhere else, and the gate was green over it for as long as it existed (SKEIN-858). The
    signal that tells the two apart is in the document already: a row that QUOTES the code's own
    words should name a line that holds them.

    A phrase that belongs to another line THIS ROW NAMES is that line's business, not this
    citation's. Without that, a row quoting two messages convicts its first citation of the
    second message's address (see `row_lines`).

    AND IT NAMES THEM WHERE THEY HAVE MOVED TO, not where the document still says they are
    (`landing`, built in `check`). The other addresses in a row are stale in exactly the state
    this whole gate exists for: an edit above them. Subtracting them at their old numbers
    subtracts the wrong lines, so their messages fall through onto whichever sibling is being
    judged and it is convicted of quoting them — three of the four citations in
    `docs/recovery-survey.md:835` reported that way after one insertion into `src/fleet.rs`, all
    three of them ordinary `moved` citations that a person then had to repair by hand (SKEIN-976).
    It cuts the other way too, and that is the half to keep in mind before widening it: a
    sibling's OLD line stops excusing anything, so a phrase sitting there is now this citation's
    to answer for.

    PER SITE, not per phrase, which is what the full-quote family needs (SKEIN-889): a row naming
    three of a message's fourteen lines has still said nothing about the other eleven, so the
    phrase goes on judging against those. A phrase with ONE site and that site spoken for is
    silent exactly as before — the two-message row `row_lines` exists for is a cell of two runs
    and never a quotation in full.
    """
    lines = tree.now(cite.target)
    sites = own_words(cite, tree, words)
    if not sites or lines is None:
        return {}
    landing = {} if landing is None else landing
    crossing = {} if crossing is None else crossing
    spoken = set()
    for named in row_lines(cite) - {cite.line}:
        other = landing.get(f"{cite.target}:{named}", named)
        spoken |= (
            message_region(lines, Cite(cite.doc, cite.doc_line, "", cite.target, other))
            if other <= len(lines)
            else {other}
        )
    # A SPLIT citation's row still names its other sites by the OLD path, so they are followed
    # across the split to where they landed; one that landed in THIS file answers for its words
    # here, exactly as a same-file sibling does above, and one that landed elsewhere, or nowhere
    # unique, answers for nothing in this file.
    if cite.was:
        for m in prose.CITATION.finditer(cite.row):
            if prose.resolve(m.group(1), [cite.was]) != cite.was:
                continue
            key = f"{cite.was}:{m.group(2)}"
            file, line = crossing.get(key, (None, 0))
            if file == cite.target and key != cite.was_key:
                spoken |= message_region(lines, Cite(cite.doc, cite.doc_line, "", file, line))
    left = {run: tuple(n for n in at if n not in spoken) for run, at in sites.items()}
    return {run: at for run, at in left.items() if at}


def at_line(cite, line):
    """`cite` as it would read at `line`. A range moves both ends, the way `renumber` moves them."""
    last = None if cite.last is None else cite.last + (line - cite.line)
    return Cite(cite.doc, cite.doc_line, cite.text, cite.target, line, last, cite.row)


def git(*args):
    """stdout of one git command, or `None` if it failed.

    `None` is not `""`: "this file is empty at that commit" and "there is no such commit" are
    opposite answers here, and every caller tells them apart.
    """
    try:
        run = subprocess.run(
            ["git", "-C", ROOT, *args], capture_output=True, text=True, errors="replace"
        )
    except OSError:
        return None
    return run.stdout if run.returncode == 0 else None


def blame_map(doc):
    """{line in `doc`: the sha that last wrote it}, with uncommitted lines left out.

    `git blame` reports a line that is not committed yet under an all-zero sha. Leaving it out is
    how a citation written in the working tree gets its anchor from the working tree — which is
    right, because that is the tree its author is looking at.
    """
    out = git("blame", "--line-porcelain", "--", doc)
    if out is None:
        return {}
    found = {}
    for line in out.split("\n"):
        m = re.match(r"^([0-9a-f]{40}) \d+ (\d+)", line)
        if m and m.group(1) != "0" * 40:
            found[int(m.group(2))] = m.group(1)
    return found


class Tree:
    """The contents of files, in the working tree and at arbitrary commits, read once each."""

    def __init__(self):
        self._now, self._at = {}, {}

    def now(self, rel):
        if rel not in self._now:
            try:
                body = open(os.path.join(ROOT, rel), encoding="utf-8", errors="replace").read()
            except OSError:
                body = None
            self._now[rel] = None if body is None else body.split("\n")
        return self._now[rel]

    def at(self, sha, rel):
        if (sha, rel) not in self._at:
            body = git("show", f"{sha}:{rel}")
            self._at[(sha, rel)] = None if body is None else body.split("\n")
        return self._at[(sha, rel)]

    def listing(self, rel_dir):
        """Every file under `rel_dir`, relative to the root, sorted. `[]` for no such directory."""
        found = []
        for d, _, files in os.walk(os.path.join(ROOT, rel_dir)):
            found += [os.path.relpath(os.path.join(d, f), ROOT) for f in files]
        return sorted(found)

    def successors(self, rel):
        """The files `rel` became, if it is a `.rs` file that became a directory or a file
        `ASSEMBLED` from one; `[]` otherwise.

        The rule is `rustcut.units`': a unit is `src/<name>.rs` or `src/<name>/**/*.rs`, so a cited
        `src/fleet.rs` that is gone while `src/fleet/` holds Rust files was SPLIT, not deleted, and
        its lines are somewhere in there. Only while the file itself is gone — a unit that still
        has its `.rs` is cited by its own path, and nothing needs following.

        An assembled file is followed WHILE IT IS STILL THERE, and that is the difference: it is
        not gone, it is generated, and what is wrong with citing it is who can act on the citation.
        """
        if rel in ASSEMBLED:
            return self.listing(ASSEMBLED[rel])
        if not rel.endswith(".rs") or self.now(rel) is not None:
            return []
        return [f for f in self.listing(rel[:-3]) if f.endswith(".rs")]


class Cite:
    """One `path:line` citation: where it is written, and what file and line it resolves to."""

    def __init__(self, doc, doc_line, text, target, line, last=None, row=""):
        self.doc, self.doc_line, self.text = doc, doc_line, text
        # A RANGE is anchored on its FIRST line, not the highest. `prose-check.py` checks the
        # highest, because a range that ends past the file is wrong about it; this gate asks what
        # the citation POINTS AT, and that is where it starts. `last` is carried so a relocation
        # can move both ends by the same amount rather than flattening the range to a line.
        self.target, self.line, self.last = target, line, last
        # The DOCUMENT's own line, carried because the sentence around a citation is evidence:
        # where it quotes the code's words, `misanchored` can say whether the cited line holds
        # them. Nothing else in this tool reads the document as prose.
        self.row = row
        # The files `target` was split into, when it was (`Tree.successors`). Empty for every
        # citation whose file is still on disk, which is all but a split's.
        self.heirs = []
        # Set only on the stand-in `check` builds at a split citation's NEW address: the path the
        # document still names, so the row's other sites can be followed across the same split.
        self.was, self.was_key = None, None

    @property
    def key(self):
        return f"{self.target}:{self.line}"

    def __repr__(self):
        return f"{self.doc}:{self.doc_line}  {self.text}"


def gated(label):
    """Whether `label` is a document this gate reads. See the docstring's scope section.

    `docs/` whole, prose and ledger alike. The `.toml` ledgers there carry `sites = [...]`
    citations that are citations in every sense that matters — `docs/prose-symbols.toml:41` cited
    `src/bin/skein-server.rs:5132` for the clippy lint named in the doc comment that lives at
    5296, and had been wrong for long enough that no single edit accounts for the gap — and
    `prose-check.py` reads that citation today, looks at the number, and passes it, because 5132
    is inside the file. That is the whole case for this gate in one line.

    (The lint's own name is deliberately not spelled here, and that is not fussiness. A Python
    docstring is a string EXPRESSION, not a comment, so `prose-check.py` reads this file as code
    and not as prose — and `docs/prose-symbols.toml` declares that name as one the CODE does not
    have. Spelling it here satisfies that declaration from inside the tool that is discussing it,
    and `prose-check.py` then fails with "exempts it and nothing needs it". Measured, not guessed:
    the first draft of this docstring did exactly that. Its own docstring names the trap one door
    along — "a tool must not spell what it is testing for".)

    Never the ledger this tool writes. Its keys ARE `path:line` strings and its anchors are lines
    of code, so scanning it would invent citations no document makes; `citation-check.py` skips
    `docs/citations.toml` for the same reason, and states it in the same breath.
    """
    return label.startswith("docs/") and label != LEDGER_REL


def scan(sources=None, index=None, everything=False, tree=None):
    """([Cite], skipped) over the documents in scope.

    `skipped` counts what was deliberately not read: citations in a document that declares a
    `CITATIONS_AT` pin, and citations whose path names several files or none. The first two are
    somebody else's rule and the third is `prose-check.py`'s finding, not this one's.

    A path that names no file but whose `.rs` became a directory is NOT skipped: it is kept, with
    the files it became as `heirs`, and counted as `split`. Skipping it is what let the ledger
    prune its anchor (see the docstring's table), so the count is printed beside the others.
    """
    sources = prose.citation_sources() if sources is None else sources
    index = prose.tree_files() if index is None else index
    tree = Tree() if tree is None else tree
    out, skipped = [], {"pinned": 0, "ambiguous": 0, "unresolvable": 0, "split": 0}
    for label, body, markdown in sources:
        if label == LEDGER_REL:
            continue
        if not (everything or gated(label)):
            continue
        found = prose.citations(body, markdown)
        if prose.CITATIONS_AT.search(body):
            skipped["pinned"] += len(found)
            continue
        rows = body.split("\n")
        for doc_line, path, _, text in found:
            target = prose.resolve(path, index)
            if target is None:
                skipped["ambiguous"] += 1
                continue
            # A split file is gone, so `target` is empty and the path as written is the unit; an
            # assembled one resolves like any file, and its heirs are asked of what it resolved to.
            heirs = tree.successors(path if target == "" else target)
            if target == "" and not heirs:
                skipped["unresolvable"] += 1
                continue
            if heirs:
                target = target or path
                skipped["split"] += 1
            m = re.search(r":(\d+)(?:-(\d+))?$", text)
            last = int(m.group(2)) if m.group(2) else None
            row = rows[doc_line - 1] if doc_line <= len(rows) else ""
            cite = Cite(label, doc_line, text, target, int(m.group(1)), last, row)
            cite.heirs = heirs
            out.append(cite)
    return out, skipped


def anchor_for(cite, tree, blames):
    """What `cite`'s target line said in the commit that wrote it, or `(None, why)`.

    This is the whole reason `--record` cannot bless a citation that has already drifted: it does
    not look at today's tree unless the citation itself is not committed yet.
    """
    if cite.doc not in blames:
        blames[cite.doc] = blame_map(cite.doc)
    sha = blames[cite.doc].get(cite.doc_line)
    lines = tree.now(cite.target) if sha is None else tree.at(sha, cite.target)
    where = "the working tree" if sha is None else sha[:8]
    if lines is None:
        return None, f"{cite.target} is not in {where}"
    if cite.line > len(lines):
        return None, f"{cite.target} had {len(lines)} line(s) in {where}"
    text = norm(lines[cite.line - 1])
    if not text:
        return None, f"{cite.target}:{cite.line} was blank in {where}"
    return (text, window(lines, cite.line)), ""


def history_is_readable():
    """Whether `git blame` on this checkout can name the commit that wrote a line.

    A SHALLOW CLONE ANSWERS EVERY BLAME WITH ONE SHA — its own HEAD — and answers it confidently.
    Measured on a `--depth 1` clone of this repository on 2026-09-20: all 2,498 lines of
    `docs/architecture.md` came back under one commit, so "the tree as it stood in the commit that
    wrote this line" is the working tree for every line in every document. `inherited` below would
    then convict every drifted citation in the repository, in CI, for a reason that has nothing to
    do with the change under test — the SKEIN-913 shape, a check going red for somebody else's
    reason, which teaches people to read past it. `.github/workflows/ci.yml` says in as many words
    that this gate "needs no history and is unaffected by the shallow clone", and it stays true:
    the rule is switched off here, and the summary says so rather than printing a silent zero.
    """
    out = git("rev-parse", "--is-shallow-repository")
    return out is not None and out.strip() != "true"


def inherited(cite, entry, tree, blames, history=True):
    """Why this ledger entry is NOT the record `cite` wrote — or `""` when it is, or cannot be told.

    THE LEDGER IS KEYED BY ADDRESS ALONE, so two citations that come to name one address share one
    entry, and the one that did not write it is judged against text it never claimed. `--relocate`
    then moves it to wherever that other row's anchor has got to. That is SKEIN-936, and it is not
    a hypothetical: `docs/recovery-survey.md`'s cross-origin row was hand-corrected onto
    `src/bin/skein-server.rs:5087` — correctly, by a person reading the file — while the ledger
    held that key for the unsupported-runtime row; the next `--relocate --write` moved the
    cross-origin row onto the unsupported-runtime line, printed "0 left for a person", and
    `line-cite-check` exited 0 over the result. The only thing that noticed was a human spotting
    that the ledger's entry count had fallen by one, which is a side effect and not a signal.

    THE TEST IS THE ONE `--record` ALREADY MAKES, asked of an entry that exists rather than of one
    that does not: what did the cited line say in the commit that wrote THIS citation's document
    line? For a citation left behind by an edit to the code, the document line has not been touched
    since it was recorded, so `git blame` names the same commit and the answer is the anchor
    itself — 523 of the 523 recorded citations in `docs/` answered exactly that on 2026-09-20, with
    none underivable, which is what makes this a rule and not a heuristic. For a citation a person
    has re-derived and rewritten, the document line is newer than the entry, and the answer is what
    the person was looking at — which is not the anchor, and says so.

    `""` WHERE NOTHING CAN BE PROVED, which is three states and not one: the history is not there
    (`history_is_readable`), `git blame` knows nothing about the document at all — an uncommitted
    or untracked document, and every fixture `self_check` builds — or the line cannot be read out
    of the commit that wrote it (`anchor_for` says why). Silence here is the honest answer and the
    conservative one; the repair path treats it as a reason to refuse to move rather than as
    permission, and `main` counts what it could not check rather than reporting zero.
    """
    if not history or "historical" in entry:
        return ""
    if cite.doc not in blames:
        blames[cite.doc] = blame_map(cite.doc)
    if not blames[cite.doc]:
        return ""
    anchored, _ = anchor_for(cite, tree, blames)
    if anchored is None or anchored[0] == entry["line"]:
        return ""
    sha = blames[cite.doc].get(cite.doc_line)
    where = "the working tree" if sha is None else sha[:8]
    return (
        f"the anchor recorded here is another citation's: at {where} {cite.key} said"
        f" {anchored[0][:60]!r}, while the ledger holds {entry['line'][:60]!r}"
    )


def read_ledger(path=None):
    """{key: {"line": str} or {"historical": str}} — `{}` when the ledger is not there yet."""
    import tomllib

    path = LEDGER if path is None else path
    try:
        with open(path, "rb") as fh:
            return tomllib.load(fh)
    except FileNotFoundError:
        return {}


def toml_str(text):
    """`text` as a TOML basic string."""
    out = text.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + "".join(c if c >= " " or c == "\t" else "\\u%04x" % ord(c) for c in out) + '"'


def write_ledger(entries, path=None):
    body = [LEDGER_HEADER]
    for key in sorted(entries, key=lambda k: (k.rsplit(":", 1)[0], int(k.rsplit(":", 1)[1]))):
        entry = entries[key]
        body.append(f"\n[{toml_str(key)}]")
        if "historical" in entry:
            body.append(f"historical = {toml_str(entry['historical'])}")
        else:
            body.append(f"line = {toml_str(entry['line'])}")
            if entry.get("window"):
                body.append(f"window = {toml_str(entry['window'])}")
        if entry.get("cited_by"):
            body.append("cited_by = [" + ", ".join(toml_str(c) for c in entry["cited_by"]) + "]")
    open(LEDGER if path is None else path, "w", encoding="utf-8").write("\n".join(body) + "\n")


def where_now(anchor_line, anchor_window, lines):
    """[line numbers] in `lines` where a recorded anchor sits now, best discriminator first."""
    if anchor_window:
        hits = [n for n in range(1, len(lines) + 1) if window(lines, n) == anchor_window]
        if len(hits) == 1:
            return hits
    return [n for n, text in enumerate(lines, 1) if norm(text) == anchor_line]


def where_across(anchor_line, anchor_window, files, tree):
    """[(file, line)] where a recorded anchor sits now across several files, best first.

    `where_now`, asked of a unit rather than a file, and UNIQUE ACROSS ALL OF THEM: a window that
    matches once in each of two files has not said which one the citation meant, so it falls back
    to the bare line exactly as `where_now` does within one file.
    """
    bodies = [(f, tree.now(f) or []) for f in files]
    if anchor_window:
        hits = [(f, n) for f, lines in bodies for n in range(1, len(lines) + 1)
                if window(lines, n) == anchor_window]
        if len(hits) == 1:
            return hits
    return [(f, n) for f, lines in bodies for n, text in enumerate(lines, 1) if norm(text) == anchor_line]


def check(cites, ledger, tree, blames=None, history=True):
    """[(cite, verdict, detail)] for every citation that is not in agreement with the ledger.

    Verdicts, and they are deliberately different things to a reader:
      `misanchored` — the row quotes the code's own words, and the line the repair would leave
                      this citation on is not where they are. `detail` is where they are AND what
                      the anchor did, because those two facts together are the finding. No
                      mechanical repair can be right, so none is offered.
      `inherited`   — the anchor at this citation's address was recorded for a DIFFERENT citation,
                      so following it would drag this one onto another row's line (`inherited`,
                      SKEIN-936). Also a person's, and for the same reason as `misanchored`: the
                      ledger is not describing this claim, so no move of it can be right.
      `unrecorded`  — no anchor. Nothing is being claimed about it, so nothing can be checked.
      `moved`       — the anchor is elsewhere in the file. `detail` is where.
      `gone`        — the anchor is nowhere in the file. `detail` is what it said.
      `ambiguous`   — the anchor is in the file several times. `detail` counts them.

    THE TWO SIGNALS ARE COMPARED BEFORE EITHER IS REPORTED, and that is the whole of the
    precedence question. The ledger knows where the recorded ANCHOR is now; the row knows where
    its own WORDS are. Ask where the mechanical repair would leave the citation — the anchor's new
    line when that resolves uniquely, and the cited line otherwise:

      they AGREE      the words are in that line's region, so relocating lands the citation on the
                      line its row quotes. `moved`, and `--relocate --write` is the repair. An edit
                      above a quote-bearing citation moves the anchor and the words TOGETHER, which
                      is the ordinary case and must not cost a person anything: one sibling lane
                      adding 12 lines to `src/web/index.html` produced six of these at once
                      (SKEIN-879). The agreement is also the evidence that was missing when
                      `b9db291` relocated 162 citations in one commit.
      they DISAGREE   no relocation names the words: the anchor lands where they are not, or it
                      never left the cited line while they sit elsewhere, or it is ambiguous or
                      gone. `misanchored`, ahead of every ledger verdict, because relocating here
                      is what SPREADS a wrong citation — the three `docs/recovery-survey.md` rows
                      SKEIN-858 started from had been carried along by exactly that, and the
                      `detail` says which way the two disagree so a person can read the row.
    """
    findings, words, blames = [], {}, {} if blames is None else blames
    # WHERE EVERY DRIFTED CITATION IN THIS SET WOULD LAND, worked out before any of them is
    # judged, because a row's OTHER sites are evidence about this one and they have moved too.
    # `unaccounted` subtracts the lines a row's other addresses answer for; those addresses are
    # the numbers written in the document, and during a drift those numbers are exactly the ones
    # that are stale. A 474-line insertion into `src/fleet.rs` on 2026-09-20 made
    # `docs/recovery-survey.md:835` — four sites, four messages, one cell — report THREE of its
    # four citations as `misanchored` "its words are at src/fleet.rs:8953", the last of the four
    # messages, because the other three sites were being subtracted at their pre-insertion
    # addresses and so subtracted nothing at all. Every one of those three was an ordinary
    # `moved`, and each cost a person a hand-derived line number (SKEIN-976). A site that has
    # moved answers for where it has moved TO, and this map is that answer.
    landing, crossing = {}, {}
    for cite in cites:
        entry = ledger.get(cite.key)
        lines = tree.now(cite.target)
        if entry is None or "historical" in entry or cite.key in landing:
            continue
        if cite.heirs:
            # The same map for a split file, which lands in a FILE as well as at a line.
            hits = where_across(entry["line"], entry.get("window", ""), cite.heirs, tree)
            if len(hits) == 1:
                crossing[cite.key] = hits[0]
            continue
        if lines is None or cite.line > len(lines) or norm(lines[cite.line - 1]) == entry["line"]:
            continue
        hits = where_now(entry["line"], entry.get("window", ""), lines)
        if len(hits) == 1:
            landing[cite.key] = hits[0]
    for cite in cites:
        entry = ledger.get(cite.key)
        # A `historical` declaration is a person's written reason for a citation that points at
        # code this tree no longer has, and it exempts the citation from every verdict here —
        # including `misanchored`, because "the words are not where the row says" is the expected
        # state of a row that is the record of what WAS wrong.
        if entry is not None and "historical" in entry:
            continue
        if cite.heirs:
            findings.append(split_verdict(cite, entry, tree, words, landing, crossing, blames, history))
            continue
        lines = tree.now(cite.target)
        past_end = lines is None or cite.line > len(lines)
        # THE ANCHOR'S OWN VERDICT IS WORKED OUT FIRST, so the two signals can be compared before
        # either is reported. `drifted` and `hits` are what the ledger half of this gate knows.
        drifted = entry is not None and not past_end and norm(lines[cite.line - 1]) != entry["line"]
        hits = where_now(entry["line"], entry.get("window", ""), lines) if drifted else []
        mine = {} if past_end else unaccounted(cite, tree, words, landing)
        if mine:
            # WHERE WOULD THE MECHANICAL REPAIR LEAVE THIS CITATION? At the anchor's new line when
            # that resolves uniquely, and where it is otherwise. If the row's own words are in the
            # region of THAT line, the two signals AGREE and the relocation is safe to offer —
            # which is the evidence that was missing when `b9db291` moved 162 citations at once,
            # six of them onto lines their rows did not quote (SKEIN-879). If they DISAGREE, no
            # relocation can be right and a person has to read the row (SKEIN-858).
            target = hits[0] if len(hits) == 1 else cite.line
            region = message_region(lines, at_line(cite, target))
            # A SITE, ANY SITE. Where the row quotes its message in full the words are at every
            # line that holds it, and sitting at one of them is what makes the citation right
            # (SKEIN-889); where it quotes a fragment there is exactly one line here and this
            # reads as it always did.
            at = sorted({n for lines_of in mine.values() for n in lines_of})
            if not any(n in region for n in at):
                run = max(mine, key=len)
                where = ", ".join(f"{cite.target}:{n}" for n in at)
                if len(hits) == 1:
                    how = f"while its anchor moved to {cite.target}:{hits[0]}"
                elif hits:
                    how = f"while its anchor is at {len(hits)} lines, so neither signal resolves"
                elif drifted:
                    how = "while its anchor is gone from the file"
                else:
                    how = "while its anchor is still at the cited line"
                findings.append(
                    (cite, "misanchored", f"its words are at {where} ({run[:60]!r}), {how}")
                )
                continue
        if entry is None:
            findings.append((cite, "unrecorded", ""))
            continue
        if past_end:
            # `prose-check.py` owns this finding; reporting it again would be two gates red for
            # one defect and two things to fix it in.
            continue
        if not drifted:
            continue
        if len(hits) == 1:
            # THE PROVENANCE QUESTION IS ASKED HERE AND NOWHERE ELSE, which is narrower than it
            # first looks and deliberately so. `moved` is the ONLY verdict that moves a citation,
            # so it is the only one a wrong anchor can drag — and it is also the only verdict
            # whose document line, if a past `--relocate --write` wrote it, was CORRECT at that
            # commit, because that is what the relocation did to it. Asked of the other verdicts
            # it reads the tree at a commit where the citation was already stuck: measured on a
            # two-lane merge fixture on 2026-09-20, asking it of `ambiguous` citations convicted
            # two of them whose document line a sibling citation's relocation had rewritten,
            # saying "another citation's anchor" about an entry that was their own. They are a
            # person's either way; `ambiguous` is the true thing to tell them.
            whose = inherited(cite, entry, tree, blames, history)
            if whose:
                findings.append((cite, "inherited", whose))
                continue
            findings.append((cite, "moved", f"{cite.target}:{hits[0]}"))
        elif hits:
            findings.append((cite, "ambiguous", f"{len(hits)} line(s) hold it"))
        else:
            findings.append((cite, "gone", entry["line"][:96]))
    return findings


def split_verdict(cite, entry, tree, words, landing, crossing, blames, history):
    """`check`'s verdict for a citation whose file was split into `cite.heirs`.

    The same verdicts as a citation whose file is still there, asked of the unit: `unrecorded`
    with no anchor; `moved` to `<new file>:<line>` where the anchor is in exactly one place across
    the files; `ambiguous` where it is in several; `gone` where it is in none. A split citation is
    always a finding while the document names the old path, because that path does not exist.

    THE SECOND OPINION IS KEPT, not skipped because the file changed. The row's quoted words are
    looked for in the file the anchor landed in, at the address it landed at — a stand-in citation
    carries it, with `was` naming the old path so the row's OTHER sites, which name that old path
    too, can be followed across the split to where they landed (`crossing`) and answer for their
    own words there (SKEIN-976's lesson, one file over). Where the words are in that file and the
    landing is not their line, it is `misanchored`, for the reason it is anywhere: following the
    anchor would cement a citation that was never on its message.

    Where the anchor does not land uniquely there is no single file to ask, so no second opinion:
    `ambiguous` and `gone` are both a person's already.

    ONE TIE IS BROKEN, BY PROVENANCE AND NOTHING ELSE. A line that was unique in the one file was
    recorded without a `window`, because there it needed none — and after a split it can have a
    twin in another file (a `}` of the same shape, a repeated `let body = …`). The neighbours it
    had are still on record: the old file, at the commit that wrote this citation's document line,
    which is where `anchor_for` reads the anchor itself. That window is asked across the files and
    used only when it names one line, and only when that commit's line IS the recorded anchor — the
    same agreement `inherited` demands before anything moves. Otherwise it stays `ambiguous`.
    """
    if entry is None:
        return (cite, "unrecorded", "")
    hits = where_across(entry["line"], entry.get("window", ""), cite.heirs, tree)
    if len(hits) > 1 and history:
        anchored, _ = anchor_for(cite, tree, blames)
        if anchored is not None and anchored[0] == entry["line"]:
            narrowed = where_across(entry["line"], anchored[1], cite.heirs, tree)
            hits = narrowed if len(narrowed) == 1 else hits
    if len(hits) > 1:
        return (cite, "ambiguous", f"{len(hits)} line(s) across {len(cite.heirs)} file(s) hold it")
    if not hits:
        return (cite, "gone", entry["line"][:96])
    new_file, new_line = hits[0]
    there = at_line(cite, new_line)
    there.target, there.was, there.was_key = new_file, cite.target, cite.key
    mine = unaccounted(there, tree, words, landing, crossing)
    if mine:
        region = message_region(tree.now(new_file), there)
        at = sorted({n for lines_of in mine.values() for n in lines_of})
        if not any(n in region for n in at):
            run = max(mine, key=len)
            where = ", ".join(f"{new_file}:{n}" for n in at)
            return (
                cite,
                "misanchored",
                f"its words are at {where} ({run[:60]!r}), while its anchor moved to"
                f" {new_file}:{new_line}",
            )
    whose = inherited(cite, entry, tree, blames, history)
    if whose:
        return (cite, "inherited", whose)
    return (cite, "moved", f"{new_file}:{new_line}")


def moved_path(written, old, new):
    """`written` — the path as the document spells it, `old` or a tail of it — renamed to `new`.

    Only for a split or an assembled file (`Tree.successors`), and `new` must be a file under the
    directory `old` became or is built from. The document's own spelling is kept: whatever it left
    off the front of `old` is left off `new` too — `src/fleet.rs` becomes `src/fleet/login.rs` and
    a tail `fleet.rs` becomes `fleet/login.rs`; `src/web/index.html` becomes
    `src/web/app/board.js` and a tail `index.html` becomes `app/board.js`.
    """
    home = ASSEMBLED.get(old) or (old[:-3] if old.endswith(".rs") else None)
    if home is None or not new.startswith(home + "/"):
        raise ValueError(f"{new} is not a file {old} was split into or is assembled from")
    if not (written == old or old.endswith("/" + written)):
        raise ValueError(f"{written} is not how a document spells {old}")
    return new[len(old) - len(written):]


def renumber(cite, new, target=None):
    """`cite.text` with its line number moved to `new`. A RANGE moves both ends by the same
    amount, so `foo.rs:10-14` shifted to 20 reads `foo.rs:20-24` and not `foo.rs:20`.

    With `target`, the path moves too — a split citation's new file (`moved_path`)."""
    head = cite.text[: cite.text.rindex(":")]
    if target is not None and target != cite.target:
        head = moved_path(head, cite.target, target)
    if cite.last is None:
        return f"{head}:{new}"
    return f"{head}:{new}-{cite.last + (new - cite.line)}"


def rewrite_line(text, cite, new, target=None):
    """`text` with ONE occurrence of `cite.text` renumbered to `new`.

    Anchored on the exact citation text, so a line carrying two citations has each replaced once
    and neither replacement can eat the other. Split out of `relocate` so that `self_check` can
    run the real substitution over a fixture line and read the result back with
    `prose.citations()` — the check that the document and the ledger agree on the new key has to
    exercise the code that writes the document, or it is only asking the rekey about itself.
    """
    return re.sub(
        r"(?<![A-Za-z0-9_./\\-])" + re.escape(cite.text) + r"(?![0-9])",
        renumber(cite, new, target),
        text,
        count=1,
    )


def under(path, roots):
    """Whether `path` is `roots` — one of them, or inside one of them.

    At a `/` boundary, for the same reason `prose.resolve` matches its tail at one: `src/fleet.rs`
    must not be selected by `--only src/fleet` and `docs/architecture.md` must not be selected by
    `--in docs/arch`. `roots` empty means every path, because no filter was asked for.
    """
    return not roots or any(path == r or path.startswith(r.rstrip("/") + "/") for r in roots)


def relocate(findings, write=False, root=None, select=None):
    """Rewrite each `moved` citation's line number in its document. Returns (moved, left).

    `select` is the filter `--only` and `--in` build, and it is applied HERE rather than to the
    findings, so that what is withheld can be printed beside what was written: a partial repair
    that reads like a whole one is the SKEIN-821 shape, where "0 left for a person" was printed
    over a dropped anchor.
    """
    moves = [
        (c, d) for c, v, d in findings if v == "moved" and (select is None or select(c))
    ]
    if not write:
        return moves, []
    edits = {}
    for cite, detail in moves:
        target, new = detail.rsplit(":", 1)
        edits.setdefault(cite.doc, []).append((cite, int(new), target))
    for doc, items in edits.items():
        path = os.path.join(ROOT if root is None else root, doc)
        lines = open(path, encoding="utf-8").read().split("\n")
        for cite, new, target in items:
            lines[cite.doc_line - 1] = rewrite_line(lines[cite.doc_line - 1], cite, new, target)
        open(path, "w", encoding="utf-8").write("\n".join(lines))
    return moves, []


def rekey(ledger, cites, moves):
    """The ledger as it will stand once `moves` have been applied. Returns (fresh, collisions).

    A FRESH MAPPING, NOT A RENAME IN PLACE, and that is the whole of SKEIN-821. The ledger is
    keyed by `file:line`, so while a relocation is half-applied two entries can want one key even
    though the FINAL key set is perfectly unique: `index.html:3509` and `index.html:3515` both
    moved down six lines, and the first one's new key was the second one's old key. The rename
    that was here skipped any move whose destination was still occupied (`detail not in ledger`)
    and the prune then deleted the entry it had left behind, so one anchor was gone and
    `docs/recovery-survey.md:674` cited a line nothing recorded. Building the result key by key
    from the CITATIONS, and never writing into the dict being read, is order-independent: it
    carries collisions, cycles and chains alike, because no intermediate state exists to trip
    over.

    Pruning falls out of the same loop rather than being a second pass. An entry survives only
    because a citation claims it, so an entry no citation reaches is not carried — which is what
    the old `k not in keep` sweep meant, without its dependence on having got the renames right.

    `collisions` are the ones that genuinely cannot be resolved: two DIFFERENT anchors landing on
    one key, which is two records for one line. Value-equal entries are not a collision — they
    are the same claim written twice, and either copy is the answer.
    """
    dest = {cite.key: detail for cite, detail in moves}
    fresh, source, collisions = {}, {}, []
    for cite in cites:
        entry = ledger.get(cite.key)
        if entry is None:
            continue
        new = dest.get(cite.key, cite.key)
        if new not in fresh:
            fresh[new], source[new] = entry, cite.key
        elif fresh[new] != entry:
            collisions.append((source[new], cite.key, new))
    return fresh, collisions


def record(cites, ledger, tree):
    """Add an anchor for every citation that has not got one. Returns (added, refused, pruned).

    It does not touch an entry that exists. That is the anti-rubber-stamp rule, and it is stronger
    than `citation-check.py`'s ("never overwrite an entry whose citation has stopped resolving")
    because it does not have to decide which entries are in trouble: no entry is ever rewritten.
    """
    blames, added, refused = {}, [], []
    by_key = {}
    for cite in cites:
        by_key.setdefault(cite.key, []).append(cite.doc)
    for cite in cites:
        if cite.key in ledger:
            continue
        anchored, why = anchor_for(cite, tree, blames)
        if anchored is None:
            refused.append((cite, why))
            continue
        text, around = anchored
        ledger[cite.key] = {"line": text}
        # Only worth carrying where it discriminates: an anchor that is already unique in its file
        # needs no context, and a ledger that stored three lines for every entry would be three
        # times the diff for the same claim.
        if around != text and len([1 for n in range(1, len(tree.now(cite.target) or []) + 1)
                                   if norm((tree.now(cite.target) or [""])[n - 1]) == text]) != 1:
            ledger[cite.key]["window"] = around
        added.append(cite)
    pruned = [k for k in ledger if k not in by_key]
    for key in pruned:
        del ledger[key]
    for key, entry in ledger.items():
        entry["cited_by"] = sorted(set(by_key.get(key, [])))
    return added, refused, pruned


# --------------------------------------------------------------------------------------------
# The self-check. Every claim this tool makes about itself, run on every invocation against a tree
# it builds in memory, because a gate nobody has seen fail is a gate nobody should trust.
# --------------------------------------------------------------------------------------------

SELF_TARGET = [
    "fn one() {",
    '    warn("the disk is full");',
    "}",
    "fn two() {",
    '    warn("nothing to reconnect");',
    "}",
]

# Two anchors SIX LINES APART in one file, cited by two different documents — the shape of
# SKEIN-821. `src/web/index.html` alone carries about ninety citations, so anchors this close
# together are ordinary rather than exotic, and an edit above both of them moves the first one's
# key onto the key the second one still occupies.
SELF_APART = [
    "fn a() {}",
    "fn b() {}",
    "fn c() {}",
    'fn alpha() { warn("the disk is full"); }',
    "fn d() {}",
    "fn e() {}",
    "fn f() {}",
    "fn g() {}",
    "fn h() {}",
    'fn beta() { warn("nothing to reconnect"); }',
    "fn i() {}",
]
SELF_APART_GAP = 6  # SELF_APART[9] is six lines below SELF_APART[3]

# The same line twice in one file, told apart only by the lines around it. Seventeen groups in
# `docs/line-cites.toml` were in exactly this state on 2026-09-11, which is why the unresolvable
# collision below is a case that can happen rather than one invented for the check.
SELF_TWICE = [
    "fn a() {}",
    '    warn("x");',
    "fn b() {}",
    "fn c() {}",
    "fn d() {}",
    '    warn("x");',
    "fn e() {}",
]

# A phrase a document could quote that is in the file TWICE, which is what keeps a generic
# fragment from convicting a citation. `could not read` is real: it is in `src/web/index.html`
# six times over, and the first draft of the misanchor rule used it to report a 83-line error
# against a citation that was right.
# BOTH COPIES ARE OUTSIDE the region a citation to the middle line could mean, and `self_check`
# asserts that. The first draft had them one line either side, so breaking the uniqueness rule
# outright still left the case green — the phrase's first copy was inside the radius, and the
# case was proving the radius rather than the rule. (Breaking it for real invented a finding
# against `docs/recovery-survey.md:553`, which is how the weak fixture was caught.)
SELF_GENERIC = [
    'fn a() { warn("could not read the queue"); }',
    "fn b() {}",
    "fn c() {}",
    "fn d() {}",
    "fn e() {}",
    'fn f() { warn("could not read the queue"); }',
]
SELF_GENERIC_RUN = "could not read the queue"

# AND THE CELL THE DOCUMENT WROTE AROUND IT, which is the half this fixture was missing until
# SKEIN-889. Case 15 says a phrase the file holds twice judges nothing; its justification has
# always been `could not read` — a FRAGMENT of a message `src/web/index.html` paraphrases, six
# times over. But the row said only the fragment, so the cell WAS the whole run, and the fixture
# stood for a claim far broader than the case it was written for: it asserted that a row quoting
# its message IN FULL judges nothing either, which is exactly where the survey's family rows hid
# their wrong citations (`no such repo` at thirteen sites, green). So the cell paraphrases now,
# the way the incident's did — the elision is what the document did not copy — and the run this
# check can find is a piece of it. `quoted_whole` reads nothing here, and case 15 asserts that
# outright, because a fixture that is broader than its case is how the broader claim gets
# enforced by accident.
SELF_GENERIC_SAID = SELF_GENERIC_RUN + " … and gave up waiting"

# AND THE CELL THAT IS ONE RUN AND STILL NOT A QUOTATION IN FULL, which is the other half of
# `quoted_whole`'s condition and the half nothing reached. `SELF_GENERIC_SAID` above yields TWO
# runs, so a `len(found) == 1` that has stopped meaning anything already rejects it and case 15
# goes on passing — its guard never consults the equality at all. This cell does: the
# interpolation leaves exactly ONE findable run, and the cell is that run PLUS the `{queue}` the
# document did not copy. `==` says paraphrase and stays silent; `in` — the cell merely
# CONTAINING a run — calls it a quotation in full and judges the citation against every line
# holding it.
#
# That weakening is neither hypothetical nor small, which is why it gets a case of its own rather
# than a clause in 21. Measured at `3c77a20`: 96 site-cell rows are one run that is a SUBSTRING
# of their cell rather than the whole of it — `{name} is not on PATH, and skein needs it`, `the
# boxes' disk is N% full …` — and 14 of those hold their run at more than one line, so
# containment invents a family verdict against each of them. It is the 83-line error the rule's
# first draft made, rebuilt out of the other half of the condition.
SELF_GENERIC_PARA = "{queue} " + SELF_GENERIC_RUN

# A message WRAPPED ACROSS LINES as an argument to the call a citation names — `src/health.rs`
# and `src/volume.rs` are full of these, and the citation names the call.
# The judging phrase is THREE lines below the citation, so the radius alone cannot reach it and
# only the bracket span can — and `self_check` asserts that separately, because the first draft of
# this fixture put another phrase one line down and the case passed without the span being
# consulted at all. A fixture that passes for the wrong reason is the failure this file is about.
SELF_WRAPPED = [
    "fn f() {",
    "    warn(format!(",
    "        // rustfmt put the message here, two lines under the call",
    '        "{whose} could not be asked',
    "         whether anything has taken the temp directory,",
    '         and the fleet runs as uid {mine}"',
    "    ));",
    "}",
]


def self_check():
    """Prove every property this tool claims, or refuse to run. Returns what could not be proved.

    Deliberately IN MEMORY, over trees built here — a gate that touched the filesystem on every
    invocation would be a gate that can go red for a reason having nothing to do with the tree it
    was asked about.
    """
    bad = []

    class FakeTree(Tree):
        def __init__(self, lines):
            super().__init__()
            self._now = {"src/fake.rs": list(lines)}

        def at(self, sha, rel):
            return self._now.get(rel)

    cite = Cite("docs/fake.md", 1, "src/fake.rs:2", "src/fake.rs", 2)
    anchor = {"src/fake.rs:2": {"line": 'warn("the disk is full");'}}

    # 1. A citation that still names its line is not a finding.
    if check([cite], dict(anchor), FakeTree(SELF_TARGET)):
        bad.append("a citation whose line is unchanged was reported as a finding")

    # 2. Two lines inserted above it and the same citation IS a finding, and is relocatable to the
    #    line the text actually moved to. This is the case the whole tool exists for.
    shifted = ["// added", "// added"] + SELF_TARGET
    found = check([cite], dict(anchor), FakeTree(shifted))
    if [(v, d) for _, v, d in found] != [("moved", "src/fake.rs:4")]:
        bad.append(f"a citation left behind by an insertion was not relocated to :4 — got {found}")

    # 3. The cited line deleted is a finding, and is NOT relocated: there is nowhere to move it to
    #    and a tool that guessed one would be inventing a citation.
    deleted = [ln for ln in SELF_TARGET if "disk is full" not in ln]
    found = check([cite], dict(anchor), FakeTree(deleted))
    if [v for _, v, _ in found] != ["gone"]:
        bad.append(f"a citation whose line was deleted was not reported as `gone` — got {found}")
    if relocate(found)[0]:
        bad.append("a citation whose line was deleted was offered a relocation")

    # 4. A `historical` entry is skipped — with its reason — even though its line is gone.
    hist = {"src/fake.rs:2": {"historical": "the record of what was wrong before SKEIN-702"}}
    if check([cite], hist, FakeTree(deleted)):
        bad.append("a citation declared `historical` was reported as a finding")

    # 5. `--record` does not overwrite a drifted anchor, so it cannot be used to clear a finding.
    #    This is the property that separates this ledger from a rubber stamp, and the one a future
    #    edit is most likely to break by "helpfully" refreshing entries.
    stamped = dict(anchor)
    record([cite], stamped, FakeTree(shifted))
    if stamped["src/fake.rs:2"]["line"] != anchor["src/fake.rs:2"]["line"]:
        bad.append("--record overwrote the anchor of a drifted citation")
    if [v for _, v, _ in check([cite], stamped, FakeTree(shifted))] != ["moved"]:
        bad.append("--record cleared a finding it must not be able to clear")

    # 6. A citation nothing makes any more is pruned, so the ledger cannot outlive the documents.
    leftover = {"src/fake.rs:99": {"line": "gone"}, **anchor}
    _, _, pruned = record([cite], leftover, FakeTree(SELF_TARGET))
    if pruned != ["src/fake.rs:99"]:
        bad.append(f"an entry nothing cites was not pruned — got {pruned}")

    # 7. TWO ANCHORS SIX LINES APART, and the file shifted by six, so the FIRST citation's new
    #    key is the key the SECOND one still holds (SKEIN-821). The final key set has no
    #    duplicate in it — only the INTERMEDIATE state does — so a rekey that builds a fresh
    #    mapping completes, while one that renames entries in place drops whichever entry it
    #    reaches first. That is not hypothetical: it happened to `src/web/index.html:3509`, whose
    #    anchor was gone from the ledger after a run that printed "0 left for a person", and it
    #    had to be restored by hand in `2eefbcd` before the batch could merge.
    first = Cite("docs/one.md", 3, "src/fake.rs:4", "src/fake.rs", 4)
    second = Cite("docs/two.md", 3, "src/fake.rs:10", "src/fake.rs", 10)
    apart = {
        "src/fake.rs:4": {"line": norm(SELF_APART[3]), "cited_by": ["docs/one.md"]},
        "src/fake.rs:10": {"line": norm(SELF_APART[9]), "cited_by": ["docs/two.md"]},
    }
    shifted_apart = ["// added"] * SELF_APART_GAP + SELF_APART
    both = [first, second]
    found = check(both, dict(apart), FakeTree(shifted_apart))
    moves, _ = relocate(found)
    if [d for _, d in moves] != ["src/fake.rs:10", "src/fake.rs:16"]:
        bad.append(f"two anchors {SELF_APART_GAP} lines apart did not both relocate — got {moves}")
    fresh, collisions = rekey(dict(apart), both, moves)
    if collisions:
        bad.append(f"a relocation whose final key set is unique was refused — got {collisions}")
    for key, want in (("src/fake.rs:10", apart["src/fake.rs:4"]),
                      ("src/fake.rs:16", apart["src/fake.rs:10"])):
        if fresh.get(key) != want:
            bad.append(
                f"relocating two anchors {SELF_APART_GAP} lines apart lost {key}:"
                f" the ledger holds {fresh.get(key)!r} where it should hold {want!r}"
            )
    # And the DOCUMENT has to name the key the ledger now holds. Read back through the same
    # citation reader the gate uses, over the line the real substitution produced.
    for cite, detail in moves:
        line = f"The warning lives at `{cite.text}`."
        after = prose.citations(rewrite_line(line, cite, int(detail.rsplit(":", 1)[1])), True)
        if [f"{t[1]}:{t[2]}" for t in after] != [detail]:
            bad.append(f"the document was left citing {after} where the ledger says {detail}")

    # 8. TWO ANCHORS ONTO ONE LINE, which is the collision that CANNOT be resolved: one of a
    #    duplicated line was deleted, so both entries now match the survivor and the ledger has
    #    one key for two different records. Refusing and naming them is the only honest answer —
    #    picking one would silently decide which document's citation is the real one.
    twice = {
        "src/fake.rs:2": {"line": norm(SELF_TWICE[1]), "window": window(SELF_TWICE, 2)},
        "src/fake.rs:6": {"line": norm(SELF_TWICE[5]), "window": window(SELF_TWICE, 6)},
    }
    pair = [
        Cite("docs/one.md", 3, "src/fake.rs:2", "src/fake.rs", 2),
        Cite("docs/two.md", 3, "src/fake.rs:6", "src/fake.rs", 6),
    ]
    # The first copy deleted and two lines added above, so ONE copy is left, at line 7, and
    # neither line 2 nor line 6 holds it any more — both entries are findings and both relocate
    # to the same key. (The file has to stay longer than the higher citation: a citation past the
    # end of its file is `prose-check.py`'s finding, and `check` steps over it.)
    survivor = ["// added"] * 2 + [ln for i, ln in enumerate(SELF_TWICE) if i != 1]
    found = check(pair, dict(twice), FakeTree(survivor))
    moves, _ = relocate(found)
    if sorted(d for _, d in moves) != ["src/fake.rs:7", "src/fake.rs:7"]:
        bad.append(f"the two-onto-one fixture no longer collides — got {moves}, so case 8 is moot")
    else:
        _, collisions = rekey(dict(twice), pair, moves)
        if not collisions:
            bad.append("two different anchors relocating onto one key were merged instead of refused")
        elif {k for c in collisions for k in c[:2]} != {"src/fake.rs:2", "src/fake.rs:6"}:
            bad.append(f"the collision was reported without naming both entries — got {collisions}")

    # 9. TWO ANCHORS THAT SWAP LINES. Every key in the result is also a key in the input, so an
    #    in-place rekey can move neither and leaves the ledger describing the file as it was
    #    while the documents describe it as it is — the gate then flips between the two for ever.
    swapped = {
        "src/fake.rs:1": {"line": norm(SELF_TARGET[1]), "cited_by": ["docs/one.md"]},
        "src/fake.rs:2": {"line": norm(SELF_TARGET[4]), "cited_by": ["docs/two.md"]},
    }
    cycle = [
        Cite("docs/one.md", 3, "src/fake.rs:1", "src/fake.rs", 1),
        Cite("docs/two.md", 3, "src/fake.rs:2", "src/fake.rs", 2),
    ]
    found = check(cycle, dict(swapped), FakeTree([SELF_TARGET[4], SELF_TARGET[1]]))
    moves, _ = relocate(found)
    fresh, collisions = rekey(dict(swapped), cycle, moves)
    if collisions:
        bad.append(f"two anchors that merely swapped lines were refused — got {collisions}")
    if fresh.get("src/fake.rs:2") != swapped["src/fake.rs:1"] or fresh.get(
        "src/fake.rs:1"
    ) != swapped["src/fake.rs:2"]:
        bad.append(f"two anchors that swapped lines were not both carried across — got {fresh}")

    # ------------------------------------------------------------------------------------------
    # 10-16. THE MISANCHOR RULE (SKEIN-858). A rule whose failure mode is SILENCE has to be shown
    # firing, and shown NOT firing on each thing it is deliberately blind to — "0 problems" reads
    # the same whether it examined four hundred citations or none.
    # ------------------------------------------------------------------------------------------

    def row_for(text, said):
        return f"| `{text}` | {said} | **R** — a trigger | y | yes | C |"

    # 10. The extractor finds the row's words AT ALL, in both forms the documents use. Without
    #     this, every case below could pass by deriving nothing — which is the way a checker
    #     rule dies quietly.
    table = row_for("src/fake.rs:2", "the disk is full")
    if claimed(table, "src/fake.rs:2") != ["the disk is full"]:
        bad.append(f"no words were read from a survey table row — got {claimed(table, 'src/fake.rs:2')}")
    sentence = 'It says *"the disk is full"* at `src/fake.rs:2`, which is the point.'
    if claimed(sentence, "src/fake.rs:2") != ["the disk is full"]:
        bad.append(f"no words were read from a quoting sentence — got {claimed(sentence, 'src/fake.rs:2')}")

    # 11. A row whose words ARE at the cited line is not a finding.
    at_it = Cite("docs/fake.md", 1, "src/fake.rs:2", "src/fake.rs", 2, row=table)
    if check([at_it], dict(anchor), FakeTree(SELF_TARGET)):
        bad.append("a row whose quoted words are at the line it cites was reported as a finding")

    # 12. A row whose words are somewhere ELSE is `misanchored`, names where they are, and is NOT
    #     offered a relocation — the repair is a person re-reading the sentence.
    elsewhere = Cite(
        "docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4, row=row_for("src/fake.rs:4", "the disk is full")
    )
    anchored_4 = {"src/fake.rs:4": {"line": "fn two() {"}}
    found = check([elsewhere], dict(anchored_4), FakeTree(SELF_TARGET))
    if [v for _, v, _ in found] != ["misanchored"]:
        bad.append(f"a row citing a line that does not hold its words was not `misanchored` — got {found}")
    elif "src/fake.rs:2" not in found[0][2]:
        bad.append(f"`misanchored` did not say where the words are — got {found[0][2]!r}")
    if relocate(found)[0]:
        bad.append("a misanchored citation was offered a relocation, which would cement it")

    # 13. THE PRECEDENCE, BOTH WAYS ROUND, over ONE tree and ONE anchor — the only difference
    #     between the two halves is which words the row quotes (SKEIN-879). `shifted` is
    #     `SELF_TARGET` with two lines added above it, so the recorded anchor
    #     `warn("the disk is full");` has moved from :2 to :4:
    #
    #       TOGETHER  the row quotes "the disk is full", which is AT :4 — the line the relocation
    #                 would land on. The two signals agree, so this is an ordinary drift and
    #                 `--relocate --write` is the repair. One sibling lane adding 12 lines to
    #                 `src/web/index.html` made six citations look like this at once, and calling
    #                 them `misanchored` turned a one-command fix into hand work per citation.
    #       APART     the row quotes "nothing to reconnect", which is at :7 while the anchor lands
    #                 on :4. No relocation names the words, so it stays a person's — and `moved`
    #                 here is how a wrong citation SPREADS (`b9db291`, 162 at once).
    together = Cite(
        "docs/fake.md", 1, "src/fake.rs:2", "src/fake.rs", 2,
        row=row_for("src/fake.rs:2", "the disk is full"),
    )
    apart = Cite(
        "docs/fake.md", 1, "src/fake.rs:2", "src/fake.rs", 2,
        row=row_for("src/fake.rs:2", "nothing to reconnect"),
    )
    found = check([together], dict(anchor), FakeTree(shifted))
    if [(v, d) for _, v, d in found] != [("moved", "src/fake.rs:4")]:
        bad.append(
            "a citation whose anchor AND quoted words both moved to :4 was not reported as a"
            f" plain `moved` — got {found}"
        )
    if [d for _, d in relocate(found)[0]] != ["src/fake.rs:4"]:
        bad.append("a citation whose anchor and words agree was not offered its relocation")
    found = check([apart], dict(anchor), FakeTree(shifted))
    if [v for _, v, _ in found] != ["misanchored"]:
        bad.append(f"a citation whose anchor and words disagree was reported as {found}")
    elif "src/fake.rs:7" not in found[0][2] or "src/fake.rs:4" not in found[0][2]:
        bad.append(f"the disagreement was reported without naming both lines — got {found[0][2]!r}")
    if relocate(found)[0]:
        bad.append("a citation whose anchor and words disagree was offered a relocation")
    # AND THE FIXTURE HAS TO BE BOTH CASES IT CLAIMS. The radius covered two earlier fixtures in
    # this file and they passed with the property they name deliberately broken; here the trap is
    # a tree where the words and the anchor land on the same line either way, which would make
    # the APART half agree by accident and prove nothing about precedence.
    moved_to = where_now(anchor["src/fake.rs:2"]["line"], "", shifted)
    with_words = unaccounted(together, FakeTree(shifted), {})
    without = unaccounted(apart, FakeTree(shifted), {})
    if moved_to != [4] or list(with_words.values()) != [(4,)] or list(without.values()) != [(7,)]:
        bad.append(
            "the precedence fixture is no longer the pair it claims, so 13 is moot: the anchor"
            f" relocates to {moved_to}, the agreeing row's words are at"
            f" {list(with_words.values())} and the disagreeing row's at {list(without.values())}"
        )

    # 14. A PARAPHRASE says nothing. The document writes `N%` where the code writes `{pct}%` and
    #     elides at `…`; 101 of the survey's 228 site rows quote no phrase that is findable, and
    #     a gate red on those is a gate switched off inside a week.
    para = Cite(
        "docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4,
        row=row_for("src/fake.rs:4", "the disk is quite full, at N%"),
    )
    if check([para], dict(anchored_4), FakeTree(SELF_TARGET)):
        bad.append("a row that PARAPHRASES its message was convicted of naming the wrong line")

    # 15. A FRAGMENT that is in the file TWICE judges nothing: it cannot say which line was meant.
    #     The row PARAPHRASES — `could not read the queue` is a piece of what its cell says, the
    #     way `could not read` was a piece of a message `src/web/index.html` writes differently —
    #     and 21 below is the other side of the same boundary, where the cell IS the message.
    generic = Cite(
        "docs/fake.md", 1, "src/fake.rs:3", "src/fake.rs", 3,
        row=row_for("src/fake.rs:3", SELF_GENERIC_SAID),
    )
    if check([generic], {"src/fake.rs:3": {"line": "fn c() {}"}}, FakeTree(SELF_GENERIC)):
        bad.append("a phrase the cited file holds twice was used to convict a citation anyway")
    # And the fixture has to be the case it claims, in three ways, because it can go moot in
    # three. The phrase must be READ from the row at all (case 10's trap, and 15 would pass over
    # a row nothing was derived from); the cell must be a PARAPHRASE and not a quotation in full,
    # or 15 asserts SKEIN-889's rule is off rather than that uniqueness is on; and NEITHER copy
    # may sit anywhere the citation could mean, or 15 passes whether uniqueness is enforced or not.
    copies = [n for n, t in enumerate(SELF_GENERIC, 1) if SELF_GENERIC_RUN in t]
    if SELF_GENERIC_RUN not in claimed(generic.row, generic.text):
        bad.append(
            "the twice-over fixture's phrase is not read from its row at all, so 15 is moot —"
            f" got {claimed(generic.row, generic.text)}"
        )
    if quoted_whole(generic.row, generic.text) is not None:
        bad.append(
            "the twice-over fixture quotes its message IN FULL, so 15 is asserting that"
            " SKEIN-889's rule does not fire rather than that uniqueness holds"
        )
    if len(copies) != 2 or set(copies) & message_region(SELF_GENERIC, generic):
        bad.append(
            f"the twice-over fixture no longer exercises uniqueness, so 15 is moot: copies at"
            f" {copies}, region {sorted(message_region(SELF_GENERIC, generic))}"
        )

    # 21. THE SAME PHRASE, QUOTED IN FULL, IS A DIFFERENT CLAIM (SKEIN-889) — and it sits here
    #     because 15 and 21 are one boundary read from either side, over ONE tree, with the cell
    #     the only difference. Where the row's message cell is one verbatim run end to end, every
    #     line holding it is a site of that message, and a citation at none of them is wrong
    #     however many there are. That is where the survey's worst citations were hiding: the
    #     `no such repo` row cited `src/bin/skein-server.rs:1217`, a blank line, while the string
    #     sat at thirteen `return` lines none of them within 35 lines of it, and the gate was
    #     green because thirteen is not one.
    #
    #     BOTH DIRECTIONS, because a rule that convicts every family row is as wrong as one that
    #     convicts none: the same cell cited AT one of the two copies is not a finding.
    whole_row = row_for("src/fake.rs:3", SELF_GENERIC_RUN)
    if quoted_whole(whole_row, "src/fake.rs:3") != SELF_GENERIC_RUN:
        bad.append(
            "a message cell that is one verbatim run end to end was not read as a quotation in"
            f" full, so 21 is moot — got {quoted_whole(whole_row, 'src/fake.rs:3')!r}"
        )
    family = Cite("docs/fake.md", 1, "src/fake.rs:3", "src/fake.rs", 3, row=whole_row)
    found = check([family], {"src/fake.rs:3": {"line": "fn c() {}"}}, FakeTree(SELF_GENERIC))
    if [v for _, v, _ in found] != ["misanchored"]:
        bad.append(
            "a citation whose row quotes its message IN FULL, sitting at none of that message's"
            f" lines, was not `misanchored` — got {found}"
        )
    elif not all(f"src/fake.rs:{n}" in found[0][2] for n in copies):
        bad.append(
            "the full-quote finding did not name EVERY line holding the message — the family is"
            f" the point, and it said {found[0][2]!r} of copies at {copies}"
        )
    if relocate(found)[0]:
        bad.append("a full-quote misanchored citation was offered a relocation, which cements it")
    at_a_site = Cite(
        "docs/fake.md", 1, f"src/fake.rs:{copies[-1]}", "src/fake.rs", copies[-1],
        row=row_for(f"src/fake.rs:{copies[-1]}", SELF_GENERIC_RUN),
    )
    site_anchor = {at_a_site.key: {"line": norm(SELF_GENERIC[copies[-1] - 1])}}
    if check([at_a_site], dict(site_anchor), FakeTree(SELF_GENERIC)):
        bad.append(
            "a citation sitting AT one of its message's several lines was convicted anyway, so"
            " the full-quote rule fires on every family row rather than on the wrong ones"
        )

    # 22. THE CELL HAS TO *BE* THE RUN, NOT CONTAIN IT — the equality in `quoted_whole`, which
    #     until this case nothing could fail. Weakening `found[0] == plain(cell)` to `in` left
    #     `--self-check` green and every gate green, and moved 53 rows of `docs/` into the
    #     full-quote class in silence (22 became 75, and the rows judged at several lines 6
    #     became 20). Case 15's guard calls `quoted_whole` but cannot catch it: that fixture has
    #     two runs, so `len(found) == 1` rejects it before the equality is consulted, and 15 then
    #     proves the rule is off for that row FOR THE WRONG REASON.
    #
    #     This fixture reaches the equality: ONE run, and a cell that is that run plus the
    #     `{queue}` the document did not copy. It is the commonest paraphrase shape in the
    #     survey — 96 site-cell rows at `3c77a20` are a single run that is a substring of their
    #     cell, 14 of them holding that run at more than one line.
    #
    #     BOTH the reading and the verdict, because the reading alone would not show the cost:
    #     under containment this row is convicted at lines its citation never claimed.
    para_row = row_for("src/fake.rs:3", SELF_GENERIC_PARA)
    if len(runs(SELF_GENERIC_PARA)) != 1 or SELF_GENERIC_RUN not in runs(SELF_GENERIC_PARA):
        bad.append(
            "the paraphrase-of-one-run fixture no longer yields exactly one findable run, so 22"
            f" never reaches the equality it exists for — got {runs(SELF_GENERIC_PARA)}"
        )
    if len(copies) < 2:
        bad.append("22 needs its run at more than one line to show what containment would cost")
    if quoted_whole(para_row, "src/fake.rs:3") is not None:
        bad.append(
            "a message cell that CONTAINS one verbatim run was read as a quotation in full — the"
            " cell has to be the run, or every paraphrase that elides once becomes a family"
        )
    para = Cite("docs/fake.md", 1, "src/fake.rs:3", "src/fake.rs", 3, row=para_row)
    if check([para], {"src/fake.rs:3": {"line": "fn c() {}"}}, FakeTree(SELF_GENERIC)):
        bad.append(
            "a row that PARAPHRASES around one run was convicted at every line holding that run,"
            " which is the 83-line error the misanchor rule's first draft made"
        )

    # 16. A message WRAPPED across the call the citation names is inside it. Drop the bracket
    #     half of `message_region` and this fires, along with seven real citations in
    #     `src/health.rs`, `src/volume.rs` and `warden/`.
    wrapped = Cite(
        "docs/fake.md", 1, "src/fake.rs:2", "src/fake.rs", 2,
        row=row_for("src/fake.rs:2", "{whose} could not be asked … and the fleet runs as uid {mine}"),
    )
    if check([wrapped], {"src/fake.rs:2": {"line": "warn(format!("}}, FakeTree(SELF_WRAPPED)):
        bad.append("a message wrapped inside the call its citation names was read as the wrong line")
    # AND THE FIXTURE HAS TO STILL BE THE CASE IT CLAIMS. The first draft passed with the bracket
    # span deliberately disabled, because a second phrase in the same claim sat one line below the
    # citation and the plain radius covered it: the case was asserting nothing about the span it
    # exists to prove. So say it outright — every line holding a judging phrase here is further
    # from the citation than the radius reaches.
    where = own_words(wrapped, FakeTree(SELF_WRAPPED), {})
    held = sorted({n for at in where.values() for n in at})
    if not held or min(abs(n - wrapped.line) for n in held) <= 1:
        bad.append(
            "the wrapped-message fixture no longer exercises the bracket span, so 16 is moot:"
            f" its phrases are at {held} beside a citation to :{wrapped.line}"
        )

    # 18. A ROW NAMES MORE THAN ONE SITE, and the phrase belongs to the other one. 33 of the
    #     survey's site cells are like this, and judging a citation against a message its own row
    #     assigns to a different line is the one false positive this rule produced across 424
    #     citations (`docs/recovery-survey.md:705`, `src/web/v2.html:466`). Asserted in BOTH
    #     directions, because "no finding" is also what a rule that has stopped working says: the
    #     same fixture WITHOUT the second address has to fire, or this case proves nothing.
    apart_anchor = {"src/fake.rs:4": {"line": norm(SELF_APART[3])}}
    said = "nothing to reconnect"  # SELF_APART[9], six lines below the citation
    names_both = Cite(
        "docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4,
        row=f"| `src/fake.rs:4` (+ `:10`) | {said} | **R** — a trigger | y | yes | C |",
    )
    names_one = Cite(
        "docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4, row=row_for("src/fake.rs:4", said)
    )
    if check([names_both], dict(apart_anchor), FakeTree(SELF_APART)):
        bad.append("a phrase at a line the row's OWN other citation names was charged to this one")
    if [v for _, v, _ in check([names_one], dict(apart_anchor), FakeTree(SELF_APART))] != ["misanchored"]:
        bad.append("the two-site fixture does not fire without its second address, so 18 is moot")

    # 19. A SITE CELL THAT CARRIES MORE THAN CITATIONS IS STILL FOUND. 21 of the survey's 243
    #     rows put the function's name, a count, or a second message's words in the `where`
    #     column, and the pattern that admitted only `(+ …)` found no site cell in any of them:
    #     `claimed` fell back to reading that cell for a quotation, found none, and the misanchor
    #     rule went silent on every one — five of which cited a function's `fn` line while the
    #     message their row quotes sat 7 to 110 lines below.
    #
    #     ASSERTED IN BOTH DIRECTIONS, because "no finding" is also what this rule says when it
    #     has stopped reading: the parenthetical form must be read AND a cell carrying
    #     unbackticked words must not be, or the pattern is admitting prose cells and comparing
    #     whichever column it lands on.
    named_row = f"| `src/fake.rs:4` (`alpha`) | {said} | **R** — a trigger | y | yes | C |"
    if claimed(named_row, "src/fake.rs:4") != [said]:
        bad.append(
            "a site cell naming its function in the cell was not read as a site cell — got"
            f" {claimed(named_row, 'src/fake.rs:4')}"
        )
    prosey = f"| `src/fake.rs:4` (2 sites, see below) | {said} | **R** — a trigger | y | yes | C |"
    if claimed(prosey, "src/fake.rs:4"):
        bad.append(
            "a `where` cell carrying unbackticked words was read as a site cell, so the row's"
            f" words were taken from another column — got {claimed(prosey, 'src/fake.rs:4')}"
        )
    #     And end to end: that row citing a line which does not hold its words is `misanchored`.
    #     `SELF_APART[3]` is at :4 and the phrase this row quotes is at :10, six lines away, so
    #     the radius and the bracket span cannot reach it either.
    named_cite = Cite("docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4, row=named_row)
    if [v for _, v, _ in check([named_cite], dict(apart_anchor), FakeTree(SELF_APART))] != [
        "misanchored"
    ]:
        bad.append(
            "a row whose `where` cell names its function was not judged at all, which is the 21"
            " rows SKEIN-876 found unjudged"
        )

    # 20. A RELATIVE ADDRESS IN A `where` COLUMN IS A FINDING (SKEIN-876), and the same cell with
    #     the path spelled out is not. Both directions again, and the count of cells read as well:
    #     an empty finding list is what a reader that matched nothing returns.
    triple = "| %s | nothing to reconnect | **R** — a trigger | y | yes | C |\n"
    relative = [("docs/fake.md", triple % "`src/fake.rs:4` (+ `:10`)", True)]
    spelled = [("docs/fake.md", triple % "`src/fake.rs:4`, `src/fake.rs:10`", True)]
    found, cells = stray_sites(relative)
    if [a for _, _, a in found] != [":10"] or cells != 1:
        bad.append(
            f"a `where` column naming `:10` with no path was not reported — got {found} over"
            f" {cells} site cell(s)"
        )
    found, cells = stray_sites(spelled)
    if found or cells != 1:
        bad.append(
            f"a `where` column whose second site names its file was reported anyway — got {found}"
            f" over {cells} site cell(s)"
        )
    #     AND ONLY THE FIRST CELL, which this case did not prove until reading the whole row was
    #     tried and nothing failed: the `reach` and `watchable?` columns carry the same form today
    #     — §0b writes `` (boundary `:604`) `` — so a rule that read the row rather than the
    #     column would fire on rows whose `where` column is clean, and those cells are SKEIN-877's
    #     to settle for the same reading-order reason as the prose.
    later = "| `src/fake.rs:4` | nothing to reconnect | **R** — also at `:10` | y | yes | C |\n"
    found, cells = stray_sites([("docs/fake.md", later, True)])
    if found or cells != 1:
        bad.append(
            f"a relative address OUTSIDE the `where` column was charged to it — got {found} over"
            f" {cells} site cell(s)"
        )
    #     Prose is out of scope on purpose: §10's lists write the same form across a line break,
    #     where no single line says which file it belongs to (SKEIN-877).
    in_prose = [("docs/fake.md", "The guards are at `src/fake.rs:4`, `:10` and nowhere else.\n", True)]
    found, cells = stray_sites(in_prose)
    if found or cells:
        bad.append(f"a relative address in PROSE was reported as a site cell — got {found}, {cells}")
    #     And a document this gate does not read is not read here either.
    outside = [("src/fake.rs", triple % "`src/fake.rs:4` (+ `:10`)", True)]
    if stray_sites(outside) != ([], 0):
        bad.append("stray_sites read a file outside `docs/`, which is not its scope")

    # ------------------------------------------------------------------------------------------
    # 23-26. THE DRAG (SKEIN-936). The ledger is keyed by ADDRESS, so two citations that come to
    # name one address share one entry — and the one that did not write it is judged against
    # another row's claim and moved to wherever THAT claim has got to. Every case below is built
    # on one tree with one ledger, and the ONLY difference between the citation that is moved and
    # the citation that is not is which commit wrote its document line.
    # ------------------------------------------------------------------------------------------

    class TreeAt(FakeTree):
        """A tree with a past: `at()` answers out of `past`, keyed by sha, `now()` as before."""

        def __init__(self, lines, past):
            super().__init__(lines)
            self._past = past

        def at(self, sha, rel):
            return self._past.get(sha, self._now.get(rel))

    old, new = "a" * 40, "b" * 40
    # `shifted` is SELF_TARGET with two lines added above it: the disk-full line is at :4 and the
    # reconnect line at :7. At `old` the file was four lines long and its LAST line was the
    # reconnect message — so a citation written then, to :4, meant the reconnect message, and the
    # anchor recorded under `src/fake.rs:4` is that. At `new` the file is what it is now.
    was = ["let x = 1;", "let y = 2;", "let z = 3;", SELF_TARGET[4]]
    drag_tree = TreeAt(shifted, {old: was, new: list(shifted)})
    drag_ledger = {"src/fake.rs:4": {"line": norm(SELF_TARGET[4])}}
    # The citation that recorded the entry: its document line has not been touched since, so the
    # commit that wrote it is `old` and the anchor is its own. An edit above the code left it
    # behind, and moving it is exactly right.
    settled = Cite("docs/one.md", 3, "src/fake.rs:4", "src/fake.rs", 4)
    # And the citation a PERSON re-derived, onto the same address, reading the file as it stands.
    # `docs/recovery-survey.md`'s cross-origin row was in this state on 2026-09-15 and one
    # `--relocate --write` moved it onto an unsupported-runtime refusal, green.
    by_hand = Cite("docs/two.md", 9, "src/fake.rs:4", "src/fake.rs", 4)
    seen = {"docs/one.md": {3: old}, "docs/two.md": {9: new}}
    found = check([settled, by_hand], dict(drag_ledger), drag_tree, dict(seen))
    said = {c.doc: v for c, v, _ in found}

    # 23. The citation that wrote the anchor still moves, and the one that did not is `inherited`.
    if said.get("docs/one.md") != "moved":
        bad.append(
            "the citation whose own document line recorded this anchor was not offered its"
            f" relocation — got {said.get('docs/one.md')}, so the rule convicts the innocent too"
        )
    if said.get("docs/two.md") != "inherited":
        bad.append(
            "a citation judged by an anchor another citation recorded was not `inherited` — got"
            f" {said.get('docs/two.md')}: this is the drag SKEIN-936 was found by"
        )
    elif "src/fake.rs:4" not in next(d for c, v, d in found if v == "inherited"):
        bad.append("`inherited` did not name the address the two citations contend for")

    # 24. AND IT IS NOT OFFERED A RELOCATION, which is the half that matters: the verdict is only
    #     a message, the refusal to move is the protection.
    moves, _ = relocate(found)
    if [c.doc for c, _ in moves] != ["docs/one.md"]:
        bad.append(
            "the relocation was offered to the wrong set of citations — only the one whose own"
            " document line recorded this anchor may move, and moving the other IS the drag."
            f" It was offered to {[c.doc for c, _ in moves]}"
        )

    # 25. AND THE FIXTURE HAS TO DRAG WITHOUT THE RULE, or 23 is proving nothing but its own
    #     arrangement. With the provenance switched off — which is what a shallow clone is, and
    #     what this tool did before SKEIN-936 — the same tree, ledger and citations produce two
    #     plain `moved` verdicts and the hand-corrected citation is rewritten onto the reconnect
    #     line. The verdict the rule replaces is the one this asserts.
    blind = check([settled, by_hand], dict(drag_ledger), drag_tree, dict(seen), history=False)
    if [v for _, v, _ in blind] != ["moved", "moved"] or [
        d for _, _, d in blind
    ] != ["src/fake.rs:7", "src/fake.rs:7"]:
        bad.append(
            "the drag fixture does not drag with the provenance rule off, so 23 is moot: it"
            f" reports {[(v, d) for _, v, d in blind]} where both should be moved to :7"
        )

    # 26. AND THE QUESTION IS ASKED ONLY OF A CITATION THE LEDGER WOULD MOVE. Asked of one that is
    #     stuck for another reason it reads a tree where that citation was ALREADY stuck, and says
    #     "another citation's anchor" about an entry that is its own: measured on a two-lane merge
    #     fixture on 2026-09-20, two `ambiguous` citations whose document line a sibling's
    #     relocation had rewritten were convicted that way. They are a person's either way, and
    #     `ambiguous` is the true thing to tell them.
    twice_ledger = {"src/fake.rs:4": {"line": norm(SELF_TWICE[1])}}
    stuck_cite = Cite("docs/one.md", 3, "src/fake.rs:4", "src/fake.rs", 4)
    stuck_tree = TreeAt(SELF_TWICE, {old: ["fn only() {}"] * 6})
    verdicts = [
        v for _, v, _ in check([stuck_cite], dict(twice_ledger), stuck_tree, {"docs/one.md": {3: old}})
    ]
    if verdicts != ["ambiguous"]:
        bad.append(
            "a citation whose anchor is in the file twice was told its anchor belongs to another"
            f" citation — got {verdicts}, where nothing was going to move it in the first place"
        )

    # 27. A PATH FILTER REPAIRS ONE LANE'S CITATIONS AND WITHHOLDS THE REST (SKEIN-976), and it
    #     matches at a `/` boundary for the same reason `prose.resolve` does — `--only src/fleet`
    #     must not select `src/fleet.rs`, or a lane repairs a file it does not own by typo.
    if not under("src/fleet.rs", []):
        bad.append("`under` with no filter excluded a path, so an unfiltered repair repairs nothing")
    if under("src/fleet.rs", ["src/fleet"]):
        bad.append("`--only src/fleet` selected `src/fleet.rs`, matching inside a path component")
    if not (under("src/fleet.rs", ["src"]) and under("src/fleet.rs", ["src/fleet.rs"])):
        bad.append("`under` did not match a path against its own directory or itself")
    mixed = [
        (Cite("docs/one.md", 3, "src/fake.rs:4", "src/fake.rs", 4), "moved", "src/fake.rs:9"),
        (Cite("docs/two.md", 3, "src/other.rs:4", "src/other.rs", 4), "moved", "src/other.rs:9"),
    ]
    picked, _ = relocate(mixed, select=lambda c: under(c.target, ["src/fake.rs"]))
    if [c.target for c, _ in picked] != ["src/fake.rs"]:
        bad.append(f"--only did not hold back the citation naming another file — got {picked}")
    picked, _ = relocate(mixed, select=lambda c: under(c.doc, ["docs/two.md"]))
    if [c.doc for c, _ in picked] != ["docs/two.md"]:
        bad.append(f"--in did not hold back the citation in another document — got {picked}")

    # 28. A ROW'S OTHER SITES ANSWER FOR WHERE THEY HAVE MOVED TO (SKEIN-976). `SELF_APART` holds
    #     two messages nine lines apart; the row names both and quotes both, and the file has
    #     shifted by six. Judging the first citation against the second message AT ITS OLD ADDRESS
    #     convicts it of quoting a line it never named — which is what three of the four citations
    #     in `docs/recovery-survey.md:835` reported after one insertion into `src/fleet.rs`, every
    #     one of them an ordinary `moved` that then cost a person a hand-derived line number.
    pair_row = (
        "| `src/fake.rs:4`, `src/fake.rs:10` | the disk is full · nothing to reconnect"
        " | **R** — a trigger | y | yes | C |"
    )
    two_sites = [
        Cite("docs/fake.md", 1, "src/fake.rs:4", "src/fake.rs", 4, row=pair_row),
        Cite("docs/fake.md", 1, "src/fake.rs:10", "src/fake.rs", 10, row=pair_row),
    ]
    sites_ledger = {
        "src/fake.rs:4": {"line": norm(SELF_APART[3])},
        "src/fake.rs:10": {"line": norm(SELF_APART[9])},
    }
    shifted_sites = ["// added"] * SELF_APART_GAP + SELF_APART
    found = check(two_sites, dict(sites_ledger), FakeTree(shifted_sites), {"docs/fake.md": {}})
    if [(v, d) for _, v, d in found] != [
        ("moved", "src/fake.rs:10"),
        ("moved", "src/fake.rs:16"),
    ]:
        bad.append(
            "a row naming two sites in one file, both shifted, did not report two plain `moved`"
            f" — got {[(v, d) for _, v, d in found]}"
        )
    #     And the fixture has to be the case it claims: WITHOUT the map, the second message falls
    #     onto the first citation, which is the artefact this exists to remove.
    stale = unaccounted(two_sites[0], FakeTree(shifted_sites), {})
    if list(stale.values()) != [(16,)]:
        bad.append(
            "the two-site fixture no longer charges the second message to the first citation when"
            f" the other site is subtracted at its OLD address, so 28 is moot — got {stale}"
        )

    # 29. A FILE SPLIT INTO A DIRECTORY IS FOLLOWED, NOT DROPPED (SKEIN-934). `src/fake.rs` is gone
    #     and `src/fake/` holds its two halves. Before this, the citation left the gate as
    #     unresolvable — and a citation the scan drops is an entry nothing claims, so the next
    #     `--record` or `--relocate --write` pruned its anchor: all 89 of `src/fleet.rs`'s, the day
    #     that file became `src/fleet/`.
    class FakeUnit(Tree):
        def __init__(self, files):
            super().__init__()
            self._now = {k: list(v) for k, v in files.items()}

        def now(self, rel):
            return self._now.get(rel)

        def at(self, sha, rel):
            return self._now.get(rel)

        def successors(self, rel):
            if not rel.endswith(".rs") or rel in self._now:
                return []
            return sorted(k for k in self._now if k.startswith(rel[:-3] + "/") and k.endswith(".rs"))

    halves = {"src/fake/one.rs": ["// one"] + SELF_APART[:6], "src/fake/two.rs": ["// two"] * 2 + SELF_APART[6:]}
    split = FakeUnit(halves)
    split_row = (
        "| `src/fake.rs:4`, `src/fake.rs:10` | the disk is full · nothing to reconnect"
        " | **R** — a trigger | y | yes | C |"
    )
    split_ledger = {
        "src/fake.rs:4": {"line": norm(SELF_APART[3])},
        "src/fake.rs:10": {"line": norm(SELF_APART[9])},
    }
    #     The scan keeps it, names the files it became, and counts it apart from `unresolvable`.
    found_cites, split_skipped = scan(
        [("docs/fake.md", split_row + "\n", True)], ["docs/fake.md", *halves], True, split
    )
    if [(c.key, c.heirs) for c in found_cites] != [
        ("src/fake.rs:4", sorted(halves)), ("src/fake.rs:10", sorted(halves))
    ] or split_skipped["unresolvable"] or split_skipped["split"] != 2:
        bad.append(
            "a citation into a file split into a directory was not kept with the files it became"
            f" — got {[(c.key, c.heirs) for c in found_cites]}, skipped {split_skipped}"
        )
    #     The rest is asked of the citations built here rather than of the scan's, so that each
    #     part fails on its own account and a broken scan is reported once, above.
    found_cites = [
        Cite("docs/fake.md", 1, f"src/fake.rs:{n}", "src/fake.rs", n, row=split_row) for n in (4, 10)
    ]
    for cite in found_cites:
        cite.heirs = split.successors("src/fake.rs")
    #     `--record` keeps its anchor, which is the destructive half of the failure.
    kept = dict(split_ledger)
    _, _, pruned = record(found_cites, kept, split)
    if pruned:
        bad.append(f"--record pruned the anchor of a citation into a split file — got {pruned}")
    #     Each site moves to its own FILE, and both are plain `moved`: the row's other site is
    #     followed across the split to answer for its own words, or the second message would
    #     convict the first citation (case 28's lesson, one file over).
    found = check(found_cites, dict(split_ledger), split, {"docs/fake.md": {}})
    if [(v, d) for _, v, d in found] != [("moved", "src/fake/one.rs:5"), ("moved", "src/fake/two.rs:6")]:
        bad.append(
            "the two sites of a split file did not each move to the file and line their anchor"
            f" landed on — got {[(v, d) for _, v, d in found]}"
        )
    else:
        moves, _ = relocate(found)
        fresh, collisions = rekey(dict(split_ledger), found_cites, moves)
        if collisions or set(fresh) != {"src/fake/one.rs:5", "src/fake/two.rs:6"}:
            bad.append(f"the ledger was not rekeyed onto the new files — got {sorted(fresh)}")
        #     And the DOCUMENT names the new file, read back through the gate's own reader.
        for cite, detail in moves:
            target, new = detail.rsplit(":", 1)
            after = prose.citations(rewrite_line(f"See `{cite.text}`.", cite, int(new), target), True)
            if [f"{t[1]}:{t[2]}" for t in after] != [detail]:
                bad.append(f"a split citation was rewritten to {after} where the ledger says {detail}")
    #     Both sites landing in ONE of the files is the case the crossing map is for. The row
    #     PARAPHRASES its first message ("was", where the code says "is"), so the only words of it
    #     this file holds are the second message's — and unless the second site is followed across
    #     the split to answer for them, they convict the first citation of quoting them.
    together = FakeUnit({"src/fake/one.rs": ["// one"] * 3 + SELF_APART, "src/fake/two.rs": ["fn z() {}"]})
    loose_row = split_row.replace("the disk is full", "the disk was full")
    loose = [Cite("docs/fake.md", 1, c.text, c.target, c.line, row=loose_row) for c in found_cites]
    for cite in loose:
        cite.heirs = together.successors("src/fake.rs")
    found = check(loose, dict(split_ledger), together, {"docs/fake.md": {}})
    if [(v, d) for _, v, d in found] != [("moved", "src/fake/one.rs:7"), ("moved", "src/fake/one.rs:13")]:
        bad.append(
            "two sites of one row landing in one file of a split did not both read `moved` — the"
            f" second message was charged to the first citation: {[(v, d) for _, v, d in found]}"
        )
    #     An anchor in BOTH halves is `ambiguous` — uniqueness is across the unit, not per file —
    #     and one in neither is `gone`. Neither is offered a relocation.
    both_halves = FakeUnit({**halves, "src/fake/three.rs": [SELF_APART[3]]})
    only = [c for c in found_cites if c.line == 4]
    only[0].heirs = both_halves.successors("src/fake.rs")
    found = check(only, dict(split_ledger), both_halves)
    if [v for _, v, _ in found] != ["ambiguous"] or relocate(found)[0]:
        bad.append(f"an anchor in two files of a split was not `ambiguous` — got {found}")
    neither = FakeUnit({"src/fake/one.rs": SELF_APART[:3], "src/fake/two.rs": SELF_APART[6:]})
    #     The tie provenance breaks: the line has a twin in another file, the ledger recorded no
    #     window because it was unique in the one file, and the commit that wrote the citation still
    #     has the old file — so its neighbours there name one of the two. The same twin with no
    #     history to read stays `ambiguous`, which is the half that keeps this from being a guess.
    class Remembers(FakeUnit):
        def at(self, sha, rel):
            return SELF_APART if rel == "src/fake.rs" else self._now.get(rel)

    twin = Remembers({**halves, "src/fake/three.rs": ["fn x() {}", SELF_APART[3], "fn y() {}"]})
    only[0].heirs = twin.successors("src/fake.rs")
    wrote = {"docs/fake.md": {1: "0" * 39 + "1"}}
    found = check(only, dict(split_ledger), twin, wrote)
    if [(v, d) for _, v, d in found] != [("moved", "src/fake/one.rs:5")]:
        bad.append(f"the old file's neighbours did not break a tie across the split — got {found}")
    found = check(only, dict(split_ledger), twin, dict(wrote), history=False)
    if [v for _, v, _ in found] != ["ambiguous"]:
        bad.append(f"a tie across a split was broken with no history to read — got {found}")
    only[0].heirs = neither.successors("src/fake.rs")
    found = check(only, dict(split_ledger), neither)
    if [v for _, v, _ in found] != ["gone"] or relocate(found)[0]:
        bad.append(f"an anchor in no file of a split was not `gone` — got {found}")
    #     And the misanchor rule still speaks across the move: a row quoting words the anchor's
    #     new file holds at another line is `misanchored`, not relocated onto the wrong one.
    wrong = Cite("docs/fake.md", 1, "src/fake.rs:1", "src/fake.rs", 1,
                 row=row_for("src/fake.rs:1", "the disk is full"))
    wrong.heirs = sorted(halves)
    found = check([wrong], {"src/fake.rs:1": {"line": norm(SELF_APART[0])}}, split)
    if [v for _, v, _ in found] != ["misanchored"] or "src/fake/one.rs:5" not in found[0][2]:
        bad.append(f"a split citation off its row's words was not `misanchored` — got {found}")

    # 30. A FILE ASSEMBLED FROM A DIRECTORY IS FOLLOWED INTO IT WHILE IT IS STILL ON DISK
    #     (SKEIN-1104). `src/web/index.html` is built from `src/web/app/` and is identical to the
    #     parts, so every citation of it is RIGHT about the line — and still sends its reader to the
    #     one copy they must not edit. The page stays, so nothing about it looks split; this asks
    #     the real `successors`, `scan`, `moved_path` and `rewrite_line`, with only the disk faked.
    class FakeBuilt(Tree):
        def __init__(self, files):
            super().__init__()
            self._now = {k: list(v) for k, v in files.items()}

        def now(self, rel):
            return self._now.get(rel)

        def at(self, sha, rel):
            return self._now.get(rel)

        def listing(self, rel_dir):
            return sorted(k for k in self._now if k.startswith(rel_dir + "/"))

    built_parts = {"src/web/app/one.js": SELF_APART[:6], "src/web/app/two.js": SELF_APART[6:]}
    built = FakeBuilt({"src/web/index.html": SELF_APART, **built_parts})
    built_row = "| `src/web/index.html:4`, `index.html:10` | the disk is full · nothing to reconnect |"
    built_ledger = {
        "src/web/index.html:4": {"line": norm(SELF_APART[3])},
        "src/web/index.html:10": {"line": norm(SELF_APART[9])},
    }
    built_cites, built_skipped = scan(
        [("docs/fake.md", built_row + "\n", True)],
        ["docs/fake.md", "src/web/index.html", *built_parts],
        True,
        built,
    )
    if [(c.key, c.heirs) for c in built_cites] != [
        ("src/web/index.html:4", sorted(built_parts)), ("src/web/index.html:10", sorted(built_parts))
    ] or built_skipped["split"] != 2:
        bad.append(
            "a citation of the assembled page, which is on disk and right, was not followed into"
            f" the parts it is built from — got {[(c.key, c.heirs) for c in built_cites]}"
        )
    else:
        found = check(built_cites, dict(built_ledger), built, {"docs/fake.md": {}})
        want = [("moved", "src/web/app/one.js:4"), ("moved", "src/web/app/two.js:4")]
        if [(v, d) for _, v, d in found] != want:
            bad.append(f"a citation of the assembled page did not move to its part — got {found}")
        else:
            #     The document keeps its own spelling: a full path gets the part's full path, and
            #     the tail `index.html` gets the tail `app/two.js`, which still resolves to one file.
            for cite, detail in relocate(found)[0]:
                target, new = detail.rsplit(":", 1)
                out = rewrite_line(f"See `{cite.text}`.", cite, int(new), target)
                spelled = "src/web/app/one.js:4" if cite.line == 4 else "app/two.js:4"
                if out != f"See `{spelled}`.":
                    bad.append(f"the assembled page's citation {cite.text} was rewritten as {out!r}")

    # 17. A `historical` declaration exempts the misanchor verdict too — a row that is the record
    #     of what WAS wrong is expected not to find its words in the tree.
    hist_row = {"src/fake.rs:4": {"historical": "the six copies SKEIN-756 deleted"}}
    if check([elsewhere], hist_row, FakeTree(SELF_TARGET)):
        bad.append("a citation declared `historical` was still reported as misanchored")

    return bad


def refuse(*lines):
    # The relocate path prints its moves on stdout before it can know whether the rekey is
    # possible, so without this the refusal arrives ABOVE the list it is refusing to apply.
    sys.stdout.flush()
    for line in lines:
        print(line, file=sys.stderr)
    sys.exit(2)


def main(argv):
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    everything, doing_record = "--all" in argv, "--record" in argv
    doing_relocate, writing = "--relocate" in argv, "--write" in argv
    # A FLAG THAT IS ACCEPTED AND DOES NOTHING IS THE FAILURE THIS FILE IS ABOUT. `--only` with
    # nothing after it, or with the next flag after it, would otherwise fall out of a
    # comprehension silently and the run would repair EVERYTHING while its operator believed it
    # had repaired one file — which is the SKEIN-647 shape pointed at a repair instead of a check.
    only, inside, dangling = [], [], []
    for i, flag in enumerate(argv):
        if flag not in ("--only", "--in"):
            continue
        value = argv[i + 1] if i + 1 < len(argv) else ""
        if not value or value.startswith("-"):
            dangling.append(f"{flag} {value or '(nothing)'}")
        else:
            (only if flag == "--only" else inside).append(value)
    if dangling:
        refuse(
            "line-cite-check: --only and --in each take a path, and one was given none:",
            *(f"  * {d}" for d in dangling),
            "  Refusing rather than dropping the filter, because a repair that quietly ignored",
            "  it would rewrite every document while its operator believed it rewrote one",
            "  (SKEIN-647).",
        )
    if (only or inside) and not doing_relocate:
        refuse(
            "line-cite-check: --only and --in narrow the REPAIR, never the gate.",
            "  A gate that reads part of the tree and prints `0 problems` is the check this file",
            "  refuses to be (SKEIN-647). Run the gate whole, and pass the filter to --relocate:",
            "    python3 tools/line-cite-check.py --relocate --write --only src/fleet.rs",
        )

    broken = self_check()
    if broken:
        refuse(
            "line-cite-check: its own self-check does not hold, so it cannot be trusted to read",
            "the tree. A gate that cannot demonstrate the failure it exists to catch is worse",
            "than no gate (SKEIN-647, CLAUDE.md). What did not hold:",
            *(f"  * {b}" for b in broken),
        )

    index = prose.tree_files()
    if not index:
        refuse("line-cite-check: derived no files from the tree at all. Refusing to report zero.")
    sources = prose.citation_sources()
    considered = [s for s in sources if everything or gated(s[0])]
    if not considered:
        refuse(
            "line-cite-check: derived no documents to read.",
            f"  `docs/` is the scope; `prose_check.citation_sources()` returned"
            f" {len(sources)} file(s) and none of them is one.",
            "  Refusing to report zero problems about a set it could not build (SKEIN-647).",
        )
    cites, skipped = scan(sources, index, everything)
    if not cites:
        refuse(
            f"line-cite-check: read {len(considered)} document(s) and derived no `file:line`"
            " citation at all.",
            "  That is what a broken reader looks like from the inside — `docs/` carried 452 of"
            " them on 2026-09-11.",
            "  Refusing to report zero problems (SKEIN-647).",
        )

    tree = Tree()
    ledger = read_ledger()

    # THE MISANCHOR RULE'S OWN FLOOR, and it is not the same walk as the check's: this counts
    # what the rule CAN speak about, the check counts what it convicts, and a rule that convicts
    # nothing is indistinguishable from a rule that examined nothing by its output alone
    # (SKEIN-647). `docs/` carried 127 of these at `80d9143` — 124 survey rows and 3 sentences
    # elsewhere — so zero means the reader broke, not that the documents got better.
    bodies = {}
    quoting = [c for c in cites if claimed(c.row, c.text)]
    reach = [c for c in quoting if own_words(c, tree, bodies)]
    # The full-quote half of the rule has its own count for the same reason the whole rule has a
    # floor: it is the half that speaks about a message at SEVERAL lines, and if the reader of a
    # message cell breaks, that population silently becomes zero and every family row goes back
    # to being unjudged — which is the state SKEIN-889 found them in. Printed, not refused on:
    # a tree where no document quotes a repeated message in full is a legitimate tree.
    family = [c for c in reach if quoted_whole(c.row, c.text)]
    many = [c for c in family if any(len(at) > 1 for at in own_words(c, tree, bodies).values())]
    if not reach:
        refuse(
            f"line-cite-check: not one of {len(cites)} citation(s) quotes a phrase this tree"
            " holds, so the misanchor rule examined nothing.",
            f"  {len(quoting)} citation(s) sit beside quoted words at all;"
            " `docs/` carried 269 of those, and 133 whose words could be found, at `80d9143`.",
            "  A rule that reports zero because it read nothing is worse than no rule"
            " (SKEIN-647, SKEIN-858). Refusing.",
        )

    # THE RELATIVE-ADDRESS RULE'S OWN FLOOR, for the same reason the misanchor rule has one: an
    # empty list of strays reads the same whether it examined every `where` column in `docs/` or
    # none of them (SKEIN-647).
    strays, site_cells = stray_sites(considered)
    if not site_cells:
        refuse(
            "line-cite-check: not one `where` column in the documents it read carries a citation,",
            "so the relative-address rule examined nothing. `docs/recovery-survey.md` carried 243",
            f"site cells on 2026-09-12, across {len(considered)} document(s) read here.",
            "  A rule that reports zero because it read nothing is worse than no rule (SKEIN-647,",
            "  SKEIN-876). Refusing.",
        )

    if doing_record:
        added, refused, pruned = record(cites, ledger, tree)
        write_ledger(ledger)
        print(f"{len(added)} anchor(s) recorded, {len(pruned)} pruned, into {LEDGER_REL}")
        for cite in added:
            print(f"  + {cite.key}  <- {cite.doc}:{cite.doc_line}")
        if refused:
            print(f"\n{len(refused)} citation(s) could NOT be anchored, and are still findings:")
            for cite, why in refused:
                print(f"  ? {cite}  ({why})")
            print(
                "\n  An anchor is read from the tree as it stood in the commit that wrote the\n"
                "  citation. Where that cannot be read, the citation has to be re-derived by a\n"
                "  person and re-cited, or declared `historical = \"<why>\"` in the ledger."
            )
        return 0

    # THE PROVENANCE RULE'S OWN FLOOR, and it has one for the third time for the third reason. It
    # is the rule that keeps `--relocate` from dragging a citation a person placed (SKEIN-936),
    # its failure mode is silence like the other two, and it has a switch — a shallow clone, where
    # `git blame` names one commit for every line and nothing can be derived. So `blames` is built
    # here, shared with `check`, and counted: a reader can tell "no citation is judged by another
    # citation's anchor" from "the history to tell was not there".
    history = history_is_readable()
    blames = {}
    findings = check(cites, ledger, tree, blames, history)
    recorded = [c for c in cites if c.key in ledger and "historical" not in ledger[c.key]]
    # WHAT THE PROVENANCE RULE WAS ASKED, AND WHAT IT COULD ANSWER. Its floor is not a population
    # of documents like the other two rules': the rule is asked only about a citation the ledger
    # would MOVE, so on a tree with nothing drifted the honest number is zero and says so. The
    # state it exists to make visible is the other one — citations moving while the rule that
    # stops a drag could not be consulted (SKEIN-936, SKEIN-647).
    would_move = [c for c, v, _ in findings if v in ("moved", "inherited")]
    vouched = [c for c in would_move if blames.get(c.doc)] if history else []

    if doing_relocate:
        select = (lambda c: under(c.target, only) and under(c.doc, inside)) if (only or inside) else None
        moves, _ = relocate(findings, write=False, select=select)
        all_moves, _ = relocate(findings, write=False)
        withheld = [m for m in all_moves if m not in moves]
        for cite, detail in moves:
            print(f"{cite.doc}:{cite.doc_line}  {cite.text}  ->  {detail}")
        stuck = [(c, v, d) for c, v, d in findings if v != "moved"]

        # WHAT THE FILTER HELD BACK, PRINTED WHETHER OR NOT IT IS WRITING. A filter that is
        # honoured silently turns "0 left for a person" into a statement about a subset, which is
        # the sentence SKEIN-821 was printed over.
        def say_withheld():
            if not withheld:
                return
            flags = " ".join([*(f"--only {p}" for p in only), *(f"--in {p}" for p in inside)])
            print(
                f"\n{len(withheld)} relocatable citation(s) NOT written: {flags} excludes them."
                " They are still findings, and this repository is not green until somebody"
                " repairs them:"
            )
            for cite, detail in withheld:
                print(f"  outside     {cite}  ->  {detail}")

        if not (writing and moves):
            print(f"\n{len(moves)} citation(s) relocatable; {len(stuck)} left for a person")
            for cite, verdict, detail in stuck:
                print(f"  {verdict:10s} {cite}  {detail}")
            say_withheld()
            return 0

        # AND IT WILL NOT MOVE WHAT IT CANNOT VOUCH FOR. `inherited` is what stands between
        # `--relocate --write` and a citation a person placed by hand, and it can only speak where
        # the history is readable — so a repair run that cannot read it is refused outright rather
        # than run with the guard off. This is the one place the answer differs from the gate's:
        # the gate reports what it could not check and carries on, because a shallow CI checkout
        # is a legitimate tree to check; a repair is not something CI does at all.
        unvouched = [(c, d) for c, d in moves if not (history and blames.get(c.doc))]
        if unvouched:
            refuse(
                "line-cite-check: --relocate --write cannot show that the anchors it would follow",
                "belong to the citations it would move, so it has written NOTHING. A citation a",
                "person re-derived by hand is told from one an edit left behind by the commit that",
                "wrote the document line (SKEIN-936), and that needs history this checkout has not",
                "got:" if not history else "got for these documents:",
                *(
                    ["  * this is a shallow clone — `git fetch --unshallow` and run it again"]
                    if not history
                    else [f"  * {d} is not in this repository's history" for d in sorted({c.doc for c, _ in unvouched})]
                ),
                f"  {len(unvouched)} of {len(moves)} citation(s) this run would have moved.",
            )

        # THE REKEY IS PLANNED BEFORE A BYTE IS WRITTEN, so a refusal leaves the tree exactly as
        # it was. Half a repair is worse than none here: the documents would name lines the
        # ledger has no record of, which is the state SKEIN-821 left behind.
        planned, collisions = rekey(ledger, cites, moves)
        if collisions:
            refuse(
                "line-cite-check: --relocate --write would have to record two different anchors",
                f"under one key in {LEDGER_REL}, so it has written NOTHING — not the documents",
                "and not the ledger. Two entries whose text is now the same line cannot both be",
                "that line, and choosing between them would be deciding which document's",
                "citation is the real one. Read the sentences and pick the lines by hand:",
                *(f"  * {a} and {b} both relocate to {new}" for a, b, new in collisions),
            )

        relocate(findings, write=True, select=select)
        write_ledger(planned)

        # AND NOW READ IT ALL BACK OFF THE DISK. Everything below this line is derived from the
        # documents and the ledger AS THEY NOW STAND, never from what the repair set out to do.
        # The run this replaces printed "37 citation(s) rewritten; 0 left for a person" from its
        # own intentions while it had just dropped an anchor, and the next gate run reported an
        # `unrecorded` verdict that had not existed before the repair. A repair tool that reports
        # its plan rather than its result is the defect family this repository keeps paying for
        # (SKEIN-647, 794, 804) — so the numbers here cost a second scan, on purpose.
        after_cites, _ = scan(prose.citation_sources(), index, everything)
        after = check(after_cites, read_ledger(), Tree(), {}, history)
        sites = {(c.doc, c.doc_line) for c, _ in moves}
        unrepaired = [(c, v, d) for c, v, d in after if (c.doc, c.doc_line) in sites]

        print(
            f"\n{len(moves)} citation(s) rewritten; {LEDGER_REL} rekeyed to match"
            f" ({len(planned)} entry(ies), {len(ledger) - len(planned)} pruned)"
        )
        print(
            f"{len(after)} left for a person — counted by re-reading the documents and the"
            " ledger back off the disk after the write, not from the repair's own plan"
        )
        for cite, verdict, detail in after:
            print(f"  {verdict:10s} {cite}  {detail}")
        say_withheld()
        if unrepaired:
            sys.stdout.flush()
            print(
                f"\n{len(unrepaired)} of the finding(s) above stands at a citation THIS RUN JUST"
                " REWROTE, so the repair did not take: the documents and the ledger do not agree"
                " about a line this run touched. Do not commit the tree until they do"
                " (SKEIN-821).",
                file=sys.stderr,
            )
            return 1
        return 0

    read = f"{len(cites)} citation(s) in {len(considered)} document(s)"
    extra = (
        f"; {skipped['pinned']} in a document declaring its own commit"
        f", {skipped['ambiguous']} naming several files"
        f", {skipped['unresolvable']} naming none (prose-check's finding)"
        f", {skipped['split']} naming a file split into a directory, followed into it"
    )
    # What the misanchor rule could speak about, printed whether or not it found anything. A
    # reader cannot otherwise tell "no citation names the wrong line" from "nothing was read".
    words_read = (
        f"{len(reach)} of them quote a phrase their cited file holds, and are checked against it"
        f" as well ({len(quoting)} sit beside quoted words at all); {len(family)} of those quote"
        f" their message cell IN FULL, so every line holding it is a site — {len(many)} name a"
        f" message the file holds at more than one line, which only that rule can judge"
    )
    # Printed whether or not it found one, for the same reason: a reader cannot otherwise tell
    # "no `where` column names a line by a bare `:N`" from "no `where` column was read".
    sites_read = (
        f"{site_cells} `where` cell(s) name a citation; {len(strays)} of them name a further line"
        " by a bare `:N`, which nothing can resolve"
    )
    # Printed whether or not it found one, for the third time and the third reason: a reader
    # cannot otherwise tell "no citation is being judged by another citation's anchor" from "the
    # history that tells them apart was not there" — and in a shallow clone it is not (SKEIN-936).
    whose_read = (
        f"{len(vouched)} of the {len(would_move)} citation(s) the ledger would MOVE were checked"
        " against the commit that wrote them, so none of them can drag a citation a person placed"
        f" ({len(recorded)} of the citations above are recorded at all)"
        if history
        else f"none of the {len(would_move)} citation(s) the ledger would MOVE could be checked"
        " against the commit that wrote it: this is a shallow clone, where `git blame` names one"
        " commit for every line. `--relocate --write` refuses here rather than repair with the"
        " rule that stops a drag switched off"
    )
    if not findings and not strays:
        print(f"{read} still name the line they were written against{extra}")
        print(words_read)
        print(whose_read)
        print(sites_read)
        return 0

    for label, n, addr in strays:
        print(
            f"{label}:{n}  {addr}  unresolvable-site  a `where` column names a line with no file"
        )
    by_verdict = {}
    for cite, verdict, detail in findings:
        by_verdict.setdefault(verdict, []).append((cite, detail))
    for verdict in ("misanchored", "inherited", "unrecorded", "moved", "gone", "ambiguous"):
        for cite, detail in by_verdict.get(verdict, []):
            print(f"{cite.doc}:{cite.doc_line}  {cite.text}  {verdict}" + (f"  {detail}" if detail else ""))
    print(f"\n{len(findings)} of {read} no longer name what they were written to name{extra}")
    print(words_read)
    print(whose_read)
    print(sites_read)
    print(
        "\nWhat to do, by verdict:\n"
        "  misanchored the row quotes the code's own words, and the line a relocation would\n"
        "              land this citation on is not where they are — the detail says which way\n"
        "              the anchor and the words disagree. THIS ONE IS A PERSON'S: `--relocate`\n"
        "              deliberately offers no repair, because following the anchor here would\n"
        "              move the citation further from the message its row quotes. Read the row,\n"
        "              cite the line printed above if it is the line the row means, and\n"
        "              `--record` the new anchor; or, where the row is the record of code this\n"
        f"              tree no longer has, declare it in {LEDGER_REL} as `historical = \"<why>\"`.\n"
        "              (A citation whose anchor and words moved TOGETHER is not this: it reads\n"
        "              `moved`, and one command repairs it.)\n"
        "  inherited   the anchor at this address was recorded for a DIFFERENT citation, and\n"
        "              following it would move this one onto that citation's line — which is how\n"
        "              a hand-corrected citation got dragged onto an unrelated refusal with every\n"
        "              gate green (SKEIN-936). `--relocate` offers no repair. Two rows citing one\n"
        "              address is the ordinary cause: relocate or re-cite the OTHER one first,\n"
        "              then `--record`, which reads this citation's own anchor out of the commit\n"
        "              that wrote its document line and puts it in the diff a reviewer reads.\n"
        f"  unrecorded  a citation with no anchor. `python3 tools/line-cite-check.py --record`\n"
        "              writes one from the commit that wrote the citation, and the reviewer reads\n"
        "              it in the same diff as the citation.\n"
        "  moved       the line it cited is still in the file, at the line named above.\n"
        "              `python3 tools/line-cite-check.py --relocate --write` rewrites them.\n"
        "  gone        the line it cited is not in the file any more, and the text it used to\n"
        "              hold is printed above. Re-read the sentence and re-cite it; or, if the\n"
        "              citation is MEANT to name code this tree no longer has, declare it in\n"
        f"              {LEDGER_REL} as `historical = \"<why>\"` — with the reason written out.\n"
        "  ambiguous   the cited text is in the file several times, so no repair can be chosen\n"
        "              mechanically. Read the sentence and pick the line.\n"
        "  unresolvable-site\n"
        "              a `where` column names a second site as `:N` with no path. Nothing\n"
        "              resolves that — not this gate, not `prose-check.py`, not the ledger, and\n"
        "              `--relocate` cannot move it, so the number stays whatever was typed (66 of\n"
        "              203 were wrong when SKEIN-876 looked). Spell the path: the row names the\n"
        "              sites whose words it quotes, and a larger family is given as the command\n"
        "              that enumerates it."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
