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
      It is a second OPINION though, and `misanchored` below is that measurement put to the one
      use it supports: judge only on the phrases the cited file holds EXACTLY ONCE, and be silent
      about every row that paraphrased.

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
  a citation whose anchor was   `misanchored`, and it is a DIFFERENT THING from every row above:
  never the right line          those drifted, this one was wrong when it was written. The ledger
                               cannot see it — an anchor records what the cited line SAID, not
                               whether it was the line meant — so before SKEIN-858 the gate
                               DEFENDED it: `docs/recovery-survey.md` cited
                               `src/bin/skein-server.rs:100`, a bare `}`, for a message that is at
                               `:137` and has never been anywhere else, and the gate was green
                               over it. The signal is in the document: where a row QUOTES the
                               code's own words, the line the repair would leave this citation on
                               should hold them. Reported only where the anchor and the words
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
it when they AGREE — proved with one tree where only the row's quoted words differ — and stays
silent on a paraphrase, on a phrase the file holds twice, and on a message wrapped inside the call
the citation names. Read the cases, not this sentence: a list of properties written in prose beside
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
"""

import importlib.util
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER = os.path.join(ROOT, "docs", "line-cites.toml")
LEDGER_REL = "docs/line-cites.toml"

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


# A cell that is nothing but citations — the "where" column of a survey table. `(+ `:5112`, …)`
# is part of it: `docs/recovery-survey.md:752` names six sites in one cell that way.
SITE_CELL = re.compile(r"^(?:\s*(?:\(\+)?\s*`[^`]*`\s*[,;]?\s*\)?\s*)+$")

# What a document writes where the code does not write it verbatim, so a run of the code's own
# words ends here: an interpolated `{name}`, an elision at `…`, a `[bracketed]` aside, and `·`,
# which is this document's separator between two messages quoted in one cell.
NOT_VERBATIM = re.compile(r"\{[^}]*\}|…|\.\.\.|\[[^\]]*\]|·")

# Prose, rather than a table, quoting the code: the quotation marks are the claim.
QUOTED = re.compile(r"\"([^\"\n]{4,})\"|“([^”\n]{4,})”")


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
    if row.lstrip().startswith("|"):
        cells = [c.strip() for c in row.strip().strip("|").split("|")]
        site = next(
            (i for i, c in enumerate(cells) if cite_text in c and SITE_CELL.match(c)), None
        )
        if site is not None:
            rest = cells[site + 1 :] + cells[:site][::-1]
            cell = next((c for c in rest if not prose.CITATION.search(c)), None)
            return runs(cell) if cell else []
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
    """{a run of the code's own words the row quotes: the one line of the cited file holding it}.

    Empty when the row quotes nothing, or nothing it quotes is findable — and that emptiness is
    the whole reason this check does not have to understand a sentence. It reports on the rows
    where the document and the code can be compared CHARACTER FOR CHARACTER, and says nothing
    about the rest. Measured at `80d9143`: of 424 citations in `docs/`, 269 sit beside words a
    document quotes and 133 quote a phrase their cited file holds EXACTLY ONCE — 127 of the
    survey's 228 site rows, leaving 101 of them this check says nothing about.

    `words` caches `plain`-normalised file bodies, because the same file is asked about by up to
    ninety citations.
    """
    lines = tree.now(cite.target)
    if lines is None or cite.line > len(lines):
        return {}
    if cite.target not in words:
        words[cite.target] = [plain(t) for t in lines]
    body = words[cite.target]
    sites = {}
    for run in claimed(cite.row, cite.text):
        n = sole_line(body, run)
        if n is not None:
            sites[run] = n
    return sites


# A relative address, which is how a row names a SECOND site in the same file: `(+ `:5112`)`.
# `prose-check.py`'s reader does not see these — they carry no path — so the ledger has no anchor
# for them and this is the only part of the tool that reads one.
ALSO_AT = re.compile(r"`:(\d+)(?:-\d+)?`")


def row_lines(cite):
    """Every line of `cite.target` the row names, `cite`'s own included.

    THE ROW IS THE CLAIM UNIT, not the citation. `docs/recovery-survey.md:705` cites
    `src/web/v2.html:466` and quotes two messages separated by `·`, the second of which is at
    `:526` — which the row names, in the same cell, as `(+ `:526`)`. Judging the first citation
    against the second message's line reported a correct row as a finding; that was the one false
    positive this rule produced over 424 citations, and it is the reason this function exists.

    Relative and absolute both: 33 of the survey's site cells name more than one line, and the
    extra ones are written `:5112` with no path.
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


def unaccounted(cite, tree, words):
    """{phrase: line} for the words this citation — and no other address in its row — answers for.

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
    """
    lines = tree.now(cite.target)
    sites = own_words(cite, tree, words)
    if not sites or lines is None:
        return {}
    spoken = set()
    for other in row_lines(cite) - {cite.line}:
        spoken |= (
            message_region(lines, Cite(cite.doc, cite.doc_line, "", cite.target, other))
            if other <= len(lines)
            else {other}
        )
    return {run: n for run, n in sites.items() if n not in spoken}


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


def scan(sources=None, index=None, everything=False):
    """([Cite], skipped) over the documents in scope.

    `skipped` counts what was deliberately not read: citations in a document that declares a
    `CITATIONS_AT` pin, and citations whose path names several files or none. The first two are
    somebody else's rule and the third is `prose-check.py`'s finding, not this one's.
    """
    sources = prose.citation_sources() if sources is None else sources
    index = prose.tree_files() if index is None else index
    out, skipped = [], {"pinned": 0, "ambiguous": 0, "unresolvable": 0}
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
            if target == "":
                skipped["unresolvable"] += 1
                continue
            m = re.search(r":(\d+)(?:-(\d+))?$", text)
            last = int(m.group(2)) if m.group(2) else None
            row = rows[doc_line - 1] if doc_line <= len(rows) else ""
            out.append(Cite(label, doc_line, text, target, int(m.group(1)), last, row))
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


def check(cites, ledger, tree):
    """[(cite, verdict, detail)] for every citation that is not in agreement with the ledger.

    Verdicts, and they are deliberately different things to a reader:
      `misanchored` — the row quotes the code's own words, and the line the repair would leave
                      this citation on is not where they are. `detail` is where they are AND what
                      the anchor did, because those two facts together are the finding. No
                      mechanical repair can be right, so none is offered.
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
    findings, words = [], {}
    for cite in cites:
        entry = ledger.get(cite.key)
        # A `historical` declaration is a person's written reason for a citation that points at
        # code this tree no longer has, and it exempts the citation from every verdict here —
        # including `misanchored`, because "the words are not where the row says" is the expected
        # state of a row that is the record of what WAS wrong.
        if entry is not None and "historical" in entry:
            continue
        lines = tree.now(cite.target)
        past_end = lines is None or cite.line > len(lines)
        # THE ANCHOR'S OWN VERDICT IS WORKED OUT FIRST, so the two signals can be compared before
        # either is reported. `drifted` and `hits` are what the ledger half of this gate knows.
        drifted = entry is not None and not past_end and norm(lines[cite.line - 1]) != entry["line"]
        hits = where_now(entry["line"], entry.get("window", ""), lines) if drifted else []
        mine = {} if past_end else unaccounted(cite, tree, words)
        if mine:
            # WHERE WOULD THE MECHANICAL REPAIR LEAVE THIS CITATION? At the anchor's new line when
            # that resolves uniquely, and where it is otherwise. If the row's own words are in the
            # region of THAT line, the two signals AGREE and the relocation is safe to offer —
            # which is the evidence that was missing when `b9db291` moved 162 citations at once,
            # six of them onto lines their rows did not quote (SKEIN-879). If they DISAGREE, no
            # relocation can be right and a person has to read the row (SKEIN-858).
            target = hits[0] if len(hits) == 1 else cite.line
            if not any(n in message_region(lines, at_line(cite, target)) for n in mine.values()):
                run = max(mine, key=len)
                where = ", ".join(f"{cite.target}:{n}" for n in sorted(set(mine.values())))
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
            findings.append((cite, "moved", f"{cite.target}:{hits[0]}"))
        elif hits:
            findings.append((cite, "ambiguous", f"{len(hits)} line(s) hold it"))
        else:
            findings.append((cite, "gone", entry["line"][:96]))
    return findings


def renumber(cite, new):
    """`cite.text` with its line number moved to `new`. A RANGE moves both ends by the same
    amount, so `foo.rs:10-14` shifted to 20 reads `foo.rs:20-24` and not `foo.rs:20`."""
    head = cite.text[: cite.text.rindex(":")]
    if cite.last is None:
        return f"{head}:{new}"
    return f"{head}:{new}-{cite.last + (new - cite.line)}"


def rewrite_line(text, cite, new):
    """`text` with ONE occurrence of `cite.text` renumbered to `new`.

    Anchored on the exact citation text, so a line carrying two citations has each replaced once
    and neither replacement can eat the other. Split out of `relocate` so that `self_check` can
    run the real substitution over a fixture line and read the result back with
    `prose.citations()` — the check that the document and the ledger agree on the new key has to
    exercise the code that writes the document, or it is only asking the rekey about itself.
    """
    return re.sub(
        r"(?<![A-Za-z0-9_./\\-])" + re.escape(cite.text) + r"(?![0-9])",
        renumber(cite, new),
        text,
        count=1,
    )


def relocate(findings, write=False, root=None):
    """Rewrite each `moved` citation's line number in its document. Returns (moved, left)."""
    moves = [(c, d) for c, verdict, d in findings if verdict == "moved"]
    if not write:
        return moves, []
    edits = {}
    for cite, detail in moves:
        edits.setdefault(cite.doc, []).append((cite, int(detail.rsplit(":", 1)[1])))
    for doc, items in edits.items():
        path = os.path.join(ROOT if root is None else root, doc)
        lines = open(path, encoding="utf-8").read().split("\n")
        for cite, new in items:
            lines[cite.doc_line - 1] = rewrite_line(lines[cite.doc_line - 1], cite, new)
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
    if moved_to != [4] or list(with_words.values()) != [4] or list(without.values()) != [7]:
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

    # 15. A phrase that is in the file TWICE judges nothing: it cannot say which line was meant.
    generic = Cite(
        "docs/fake.md", 1, "src/fake.rs:3", "src/fake.rs", 3,
        row=row_for("src/fake.rs:3", SELF_GENERIC_RUN),
    )
    if check([generic], {"src/fake.rs:3": {"line": "fn c() {}"}}, FakeTree(SELF_GENERIC)):
        bad.append("a phrase the cited file holds twice was used to convict a citation anyway")
    # And the fixture has to be the case it claims: the phrase twice over, and NEITHER copy
    # anywhere the citation could mean, or 15 passes whether uniqueness is enforced or not.
    copies = [n for n, t in enumerate(SELF_GENERIC, 1) if SELF_GENERIC_RUN in t]
    if len(copies) != 2 or set(copies) & message_region(SELF_GENERIC, generic):
        bad.append(
            f"the twice-over fixture no longer exercises uniqueness, so 15 is moot: copies at"
            f" {copies}, region {sorted(message_region(SELF_GENERIC, generic))}"
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
    if not where or min(abs(n - wrapped.line) for n in where.values()) <= 1:
        bad.append(
            "the wrapped-message fixture no longer exercises the bracket span, so 16 is moot:"
            f" its phrases are at {sorted(where.values())} beside a citation to :{wrapped.line}"
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
    if not reach:
        refuse(
            f"line-cite-check: not one of {len(cites)} citation(s) quotes a phrase this tree"
            " holds, so the misanchor rule examined nothing.",
            f"  {len(quoting)} citation(s) sit beside quoted words at all;"
            " `docs/` carried 269 of those, and 133 whose words could be found, at `80d9143`.",
            "  A rule that reports zero because it read nothing is worse than no rule"
            " (SKEIN-647, SKEIN-858). Refusing.",
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

    findings = check(cites, ledger, tree)

    if doing_relocate:
        moves, _ = relocate(findings, write=False)
        for cite, detail in moves:
            print(f"{cite.doc}:{cite.doc_line}  {cite.text}  ->  {detail}")
        stuck = [(c, v, d) for c, v, d in findings if v != "moved"]

        if not (writing and moves):
            print(f"\n{len(moves)} citation(s) relocatable; {len(stuck)} left for a person")
            for cite, verdict, detail in stuck:
                print(f"  {verdict:10s} {cite}  {detail}")
            return 0

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

        relocate(findings, write=True)
        write_ledger(planned)

        # AND NOW READ IT ALL BACK OFF THE DISK. Everything below this line is derived from the
        # documents and the ledger AS THEY NOW STAND, never from what the repair set out to do.
        # The run this replaces printed "37 citation(s) rewritten; 0 left for a person" from its
        # own intentions while it had just dropped an anchor, and the next gate run reported an
        # `unrecorded` verdict that had not existed before the repair. A repair tool that reports
        # its plan rather than its result is the defect family this repository keeps paying for
        # (SKEIN-647, 794, 804) — so the numbers here cost a second scan, on purpose.
        after_cites, _ = scan(prose.citation_sources(), index, everything)
        after = check(after_cites, read_ledger(), Tree())
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
    )
    # What the misanchor rule could speak about, printed whether or not it found anything. A
    # reader cannot otherwise tell "no citation names the wrong line" from "nothing was read".
    words_read = (
        f"{len(reach)} of them quote a phrase their cited file holds exactly once, and are"
        f" checked against it as well ({len(quoting)} sit beside quoted words at all)"
    )
    if not findings:
        print(f"{read} still name the line they were written against{extra}")
        print(words_read)
        return 0

    by_verdict = {}
    for cite, verdict, detail in findings:
        by_verdict.setdefault(verdict, []).append((cite, detail))
    for verdict in ("misanchored", "unrecorded", "moved", "gone", "ambiguous"):
        for cite, detail in by_verdict.get(verdict, []):
            print(f"{cite.doc}:{cite.doc_line}  {cite.text}  {verdict}" + (f"  {detail}" if detail else ""))
    print(f"\n{len(findings)} of {read} no longer name what they were written to name{extra}")
    print(words_read)
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
        "              mechanically. Read the sentence and pick the line."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
