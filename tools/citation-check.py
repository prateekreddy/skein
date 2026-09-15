#!/usr/bin/env python3
"""Every commit sha cited in `docs/` can still be followed, and there is a way back when one cannot.

CLAUDE.md rule 1 is "derive, do not assert": where a claim is about the code, cite the file and
line or give the command. A commit sha is the strongest form of that citation — it names the exact
change, not a paraphrase of it — and it is also the only one that a repository can invalidate
wholesale, without touching the document, in a single command.

That has already happened here once. A history rewrite moved every commit; all forty-four hashes
then cited in `docs/` stopped resolving in a fresh clone; and the repair — the commit whose
subject is *docs: every citation in the four design documents can be followed again* — re-derived
every one of them by hand. A second rewrite over the archive branches is scoped and not
yet run (SKEIN-652, SKEIN-605). It will do exactly the same thing.

THE TRAP, WHICH IS WHY THE OBVIOUS CHECK IS THE WRONG ONE. `git cat-file -e <sha>` — or
`-t`, or anything else that asks whether the OBJECT exists — is not the test. A rewrite leaves the
old commits in the local object database as unreachable objects. After the first rewrite thirty of
the forty-four still answered `commit` on the machine where the documents were being read, while
none of them was in any clone and a `git gc` would have taken them. So the predicate is
REACHABILITY FROM `HEAD`, which is what a reader gets:

    git merge-base --is-ancestor <sha> HEAD

`docs/delivery.md` states the same thing at the top of the file. This gate computes it once, as
membership of `git rev-list HEAD`, rather than as one `merge-base` call per citation — the same
predicate, since a commit is an ancestor of `HEAD` exactly when `rev-list HEAD` names it, and one
process instead of fifty-four. `self_check()` below proves the distinction is live by building a
real commit object that is reachable from nothing and requiring this tool to reject it while
`cat-file` accepts it. Swap the predicate for the wrong one and the tool refuses to run.

WHY A CHECKER, ON ITS OWN, IS NOT THE DELIVERABLE. Picture the morning after the next rewrite. A
gate that only knows how to say "this sha does not resolve" prints fifty-four identical failures,
every one of them true, none of them actionable, and the rational response is to switch it off.
The question a person actually has then is *which commit did this citation mean, and what is it
called now* — and the sha was the only record of that, so once it is gone the answer has to be
reconstructed from the surrounding prose. That reconstruction is what the repair commit cost.

So the durable half of this work is not the check. It is `docs/citations.toml`: a ledger recording,
for each sha cited in `docs/`, the SUBJECT and AUTHOR DATE of the commit it named. A rewrite
changes shas; it does not change what a commit was called or when it was written. The ledger is a
file in the tree, so it survives the rewrite alongside the documents that need it, and `--relocate`
turns it back into shas afterwards.

THE ANCHOR, AND WHY IT IS THIS ONE. `docs/delivery.md` already says it — "commit subjects are
stable where shas are not" — and gives `git log --oneline --all --grep='<subject>'` as the way to
re-derive one by hand. Subject alone is not quite enough: three of the thirty-eight subjects cited
today appear twice across all refs, because a rebase left a duplicate on a branch. Adding the
author date does not separate those either (a rebase preserves it); what separates them is that
only one copy of each is an ancestor of `HEAD`. So the anchor is (subject, author date) resolved
WITHIN `HEAD`'s ancestry, and the gate requires that pair to name exactly one commit — an anchor
that could not choose is a repair that will not work, and it is worth knowing that today rather
than after the rewrite.

The one thing that would break the anchor is a rewrite that edits commit MESSAGES. The scrub in
SKEIN-652's scope can do that. Measured on 2026-09-08 against `docs/residue-banned.txt`, using
`tools/residue-check.py`'s own matcher rather than a second copy of it: **0 of 974 subjects on
`HEAD` carry a banned string, and 0 of the 38 cited commits do.** So this particular rewrite has no
reason to touch a subject. That is a fact about today's history, not a property of rewrites, and it
is the assumption to re-measure before the next one — `python3 tools/residue-check.py --history`
is the measurement.

WHAT THIS COVERS

  a history rewrite        Every sha moves at once. Caught, and `--relocate` produces the
                           replacement for each one. This is the case the ledger exists for.
  an amend or a rebase     One cited commit stops being an ancestor. Caught, and relocatable,
                           because the subject and date survive both.
  a sha from a local clone  Someone cites a commit that was never pushed, or one that a squash
                           merge dissolved. Caught the moment it is written, when the author still
                           knows what they meant — which is the only time it is cheap to fix.
  a citation moved to      A sha that resolves but no longer matches what the ledger recorded for
  another commit           it. The ancestry check alone cannot see this: the new sha is a perfectly
                           good commit. Only the ledger notices.

WHAT THIS DOES NOT COVER, AND WILL NOT

  * A citation that points at a REAL and RECORDED commit which is nevertheless the wrong one for
    the sentence around it. Nothing mechanical can read the sentence. The ledger reduces this to
    "the author and the reviewer looked at it once, on the diff that added it".
  * A sha written without backticks. `docs/UX-AUDIT.md:259` names Hacker News item 44594584, which
    is eight valid hex digits, and scanning outside backticks would report it forever. Every one of
    the 54 citations in `docs/` today is a whole backtick span, so requiring the backticks costs
    nothing and removes the entire false-positive class.
  * A repository other than this one. `src/store/sync/UPSTREAM.md` cites the `sync` plugin's own
    history, which is not in this object database and never will be. Such a citation is declared in
    the ledger with `external = "<why>"` and skipped — keyed on a written reason, not on a list of
    hashes that would rot.
  * Anything outside `docs/`. `--all` widens the scan to every tracked file and is a report, not
    a gate; the reason is measured and argued at `SCOPE_NOTE` below.

MODES

    python3 tools/citation-check.py             # the gate: docs/, against HEAD and the ledger
    python3 tools/citation-check.py --all       # every tracked file. A report, not wired into CI
    python3 tools/citation-check.py --record    # add, refresh and prune docs/citations.toml
    python3 tools/citation-check.py --relocate  # re-derive every recorded citation from its anchor
    python3 tools/citation-check.py --relocate --write
                                                # and rewrite the documents and the ledger to match

`--record` NEVER OVERWRITES AN ENTRY WHOSE CITATION HAS STOPPED RESOLVING. That is the property
that keeps the ledger usable: run it by reflex after a rewrite and it records nothing and says so,
rather than replacing the mapping you are about to need with the absence of one.
"""

import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER = os.path.join(ROOT, "docs", "citations.toml")
LEDGER_REL = "docs/citations.toml"

# The ledger is not scanned for citations, and this is not tidiness. It quotes commit SUBJECTS
# verbatim, and a subject may itself contain a backticked sha; scanning it would invent citations
# that no document makes and that no `--record` could ever satisfy.
SKIP = {LEDGER_REL}

SCOPE_NOTE = """\
WHY THIS IS A REPORT AND `docs/` IS THE GATE. Measured on 2026-09-08 by this scanner: 248
citations across the tracked tree, 54 of them in `docs/`. The 54 are clean — all 38 distinct shas
are ancestors of HEAD, because the repair commit put them back. The other 194 are not. Thirty-one
of them do not resolve, and they are three different things:

  23  genuine citations in doc comments and test headers, pointing at commits that survive in
      this local object database while being reachable from no ref — which is to say, at nothing,
      for anybody who clones. One of them is cited four times, across a source file, the cockpit
      page and two browser suites. This is the same breakage the documents had, in the half of the
      tree that nobody re-derived.
   6  invented placeholders that were never citations: sequential hashes in fixture data and in
      example text, in `src/prwork/perform.rs`, `src/review/`, and `tools/prose-check.py`.
   2  `src/store/sync/UPSTREAM.md`, citing the `sync` plugin's own repository. Correct as written.

Gating that set would be red on the day it landed, with a quarter of the findings being things
nobody intends to change, and a gate in that state is switched off within a week. So `docs/` is
gated — the set that is clean, that is repairable, and whose citations carry the destination
design, the acceptance gate and the delivery plan — and this mode reports the rest, on the same
evidence, for whoever takes it on. SKEIN-708 is that work."""

# A CITATION IS A BACKTICK SPAN THAT IS NOTHING BUT THE HASH. Both halves of that were measured
# rather than assumed, and the second half is the one that earns its keep.
#
# The backticks remove an entire false-positive class: `docs/UX-AUDIT.md:259` names Hacker News
# item 44594584, which is eight valid hex digits sitting in ordinary prose, and any scan that read
# bare text would report it for ever.
#
# Requiring the hash to be the WHOLE span removes a second one, which a first draft of this file
# did not. Scanning hex runs INSIDE longer spans finds 141 further tokens across the tracked tree:
# 129 in `src/web/vendor/xterm.min.js`, and the rest decimal numbers that happen to be valid hex —
# a gibibyte, a mebibyte, the mount-namespace inode numbers quoted in `src/fleet.rs`. Four of the
# 141 are genuine, and all four are the same shape: a sha as an argument inside a quoted `git log`
# recipe, at `CHANGELOG.md:28,175` and `CONTRIBUTING.md:364,368`. So that shape IS a blind spot,
# it is named here, and it is empty inside the gated scope — every one of the 54 citations in
# `docs/` is a whole span. Writing the sha in its own span next to the recipe is the way to have a
# recipe checked.
SPAN = re.compile(r"`([^`\n]+)`")
HEX = re.compile(r"^([0-9a-fA-F]{7,40})$")

# The self-check's probe: a parentless commit on the empty tree, with a fixed identity and a fixed
# date, so that its sha is the same object on every machine and every run. It is written into the
# object database and is reachable from nothing, which is precisely the state a rewrite leaves its
# predecessors in. Two objects, about 200 bytes, written once and then found rather than rewritten.
PROBE_MESSAGE = "citation-check self-check: a commit reachable from nothing"
PROBE_IDENT = {
    "GIT_AUTHOR_NAME": "citation-check",
    "GIT_AUTHOR_EMAIL": "citation-check@invalid",
    "GIT_AUTHOR_DATE": "@0 +0000",
    "GIT_COMMITTER_NAME": "citation-check",
    "GIT_COMMITTER_EMAIL": "citation-check@invalid",
    "GIT_COMMITTER_DATE": "@0 +0000",
}


def _git(*args, stdin=None, env=None):
    """stdout of one git command with the trailing newline removed, or `None` if it failed.

    `None` is deliberately not an empty string: an empty answer and a failed call mean opposite
    things here — "no commits match" versus "this is not a repository" — and every caller that
    could confuse them checks for `None` first.
    """
    try:
        run = subprocess.run(
            ["git", "-C", ROOT, *args],
            capture_output=True,
            text=True,
            input=stdin,
            env={**os.environ, **env} if env else None,
        )
    except OSError:
        return None
    return run.stdout.rstrip("\n") if run.returncode == 0 else None


def fail(*lines):
    for line in lines:
        print(line, file=sys.stderr)
    sys.exit(2)


# ---- the history ---------------------------------------------------------------------------


class History:
    """`HEAD`'s ancestry: the set a citation must be inside, and the anchors to find it by.

    One `git log` builds all of it. The alternative — `merge-base --is-ancestor` per citation, plus
    `log -1` per citation for the anchor — is a hundred processes to answer a question that one
    walk of 974 commits answers exactly.
    """

    def __init__(self, entries):
        self.shas = [sha for sha, _, _ in entries]
        self.subject = {sha: subject for sha, _, subject in entries}
        self.date = {sha: date for sha, date, _ in entries}
        # (subject, date) -> [sha, …]. Built for every commit, not only the cited ones, because
        # the question the anchor has to answer is "which commit in this history is it", and a
        # collision with an uncited commit is just as fatal to that as a collision with a cited one.
        self.by_anchor = {}
        for sha, date, subject in entries:
            self.by_anchor.setdefault((subject, date), []).append(sha)

    def resolve(self, prefix):
        """Every ancestor of `HEAD` whose sha starts with `prefix`.

        A list rather than a sha, because the count is the answer to three different questions:
        zero means the citation cannot be followed, one means it can, and more than one means the
        abbreviation has stopped being unique as the history grew — which git itself will not warn
        a document about.
        """
        low = prefix.lower()
        return [sha for sha in self.shas if sha.startswith(low)]

    def find(self, subject, date):
        """Every ancestor of `HEAD` matching an anchor exactly."""
        return list(self.by_anchor.get((subject, date), ()))


def load_history():
    """`History` for `HEAD`, or exit.

    THE SHALLOW CHECK IS NOT A FORMALITY. `actions/checkout` clones at depth 1 by default, and on
    such a checkout `git rev-list HEAD` names one commit — so every citation in the tree would be
    reported unresolvable, with the failure looking exactly like the one this gate exists to
    report. A wrong answer that resembles the right one is worse than no answer, so this refuses to
    run instead. `.github/workflows/ci.yml` deepens the clone before calling it.
    """
    if _git("rev-parse", "--git-dir") is None:
        fail("citation-check: not a git repository, so no citation can be checked here.")
    if _git("rev-parse", "--is-shallow-repository") == "true":
        fail(
            "citation-check: this is a SHALLOW clone, and every citation would be reported",
            "                unresolvable for that reason alone — which is indistinguishable",
            "                from the failure this gate exists to find.",
            "                fix: git fetch --unshallow --filter=tree:0 origin",
            "                     (commits only: 2.0 MiB of this repository's 1021 MiB)",
        )
    out = _git("log", "--format=%H%x1f%ad%x1f%s", "--date=iso-strict", "HEAD")
    if out is None:
        fail("citation-check: `git log HEAD` failed, so HEAD's ancestry is unknown.")
    entries = []
    for line in out.split("\n"):
        if not line:
            continue
        sha, _, rest = line.partition("\x1f")
        date, _, subject = rest.partition("\x1f")
        entries.append((sha, date, subject))
    if not entries:
        fail("citation-check: HEAD has no ancestry, so there is nothing to check against.")
    return History(entries)


# ---- the citations -------------------------------------------------------------------------


def tracked(scope):
    """Tracked files under `scope`, minus the ledger. `git ls-files`, so an untracked draft is out.

    Untracked is the right boundary: a citation only has to survive for a reader of the repository,
    and a file that is not in it has no readers.
    """
    out = _git("ls-files", "-z", *([scope] if scope else []))
    if out is None:
        fail("citation-check: `git ls-files` failed, so there is nothing to scan.")
    return [p for p in out.split("\0") if p and p not in SKIP]


def citations(paths):
    """`[(path, lineno, sha, line), …]` for every backticked hex run in `paths`.

    Binary and undecodable files are skipped rather than reported: a citation is prose, and a file
    that is not text has none. Everything this scans is decided by `tracked()` above.
    """
    found = []
    for rel in paths:
        try:
            with open(os.path.join(ROOT, rel), encoding="utf-8") as fh:
                text = fh.read()
        except (OSError, UnicodeDecodeError):
            continue
        if "`" not in text:
            continue
        for number, line in enumerate(text.split("\n"), 1):
            for span in SPAN.finditer(line):
                hit = HEX.match(span.group(1).strip())
                if hit:
                    found.append((rel, number, hit.group(1), line))
    return found


# ---- the ledger ----------------------------------------------------------------------------


def load_ledger():
    """`{cited-sha: entry}` from `docs/citations.toml`, or `{}` when there is no ledger yet."""
    if not os.path.exists(LEDGER):
        return {}
    try:
        with open(LEDGER, "rb") as fh:
            return tomllib.load(fh).get("citations", {})
    except (OSError, tomllib.TOMLDecodeError) as exc:
        fail(f"citation-check: {LEDGER_REL} cannot be read: {exc}")


def _toml_string(value):
    """A TOML basic string. Subjects carry quotes, backslashes and em dashes; all three survive."""
    out = value.replace("\\", "\\\\").replace('"', '\\"')
    for raw, escaped in (("\n", "\\n"), ("\r", "\\r"), ("\t", "\\t")):
        out = out.replace(raw, escaped)
    return f'"{out}"'


def render_ledger(entries):
    """The whole file, from `{cited-sha: entry}`. Sorted, so a diff shows only what changed."""
    lines = [
        "# Which commit each sha cited in `docs/` named, at the time it was cited.",
        "#",
        "# Written and checked by `tools/citation-check.py`; that file argues why this exists. In",
        "# one line: a history rewrite invalidates every sha in the documents at once, and the",
        "# subject and author date recorded here are what `--relocate` maps back to the new shas.",
        "# Do not hand-edit an entry to make the gate pass — `--record` writes them from the",
        "# history, and an entry that disagrees with the history is the finding, not the bug.",
        "#",
        "# `external` marks a sha that belongs to another repository. It carries the reason instead",
        "# of a subject, and nothing tries to resolve it.",
        "",
    ]
    for key in sorted(entries):
        entry = entries[key]
        lines.append(f'[citations."{key}"]')
        if "external" in entry:
            lines.append(f"external = {_toml_string(entry['external'])}")
        else:
            lines.append(f"commit = {_toml_string(entry['commit'])}")
            lines.append(f"subject = {_toml_string(entry['subject'])}")
            lines.append(f"date = {_toml_string(entry['date'])}")
        lines.append("")
    return "\n".join(lines)


# ---- the gate ------------------------------------------------------------------------------


def findings(cited, ledger, history):
    """`[(where, what, rule), …]` — everything wrong with this set of citations.

    Order of the checks matters to the person reading the output, and one order in particular:
    RESOLUTION IS TESTED BEFORE THE LEDGER. A sha pasted from somebody's own clone is both
    unresolvable and unrecorded, and "run --record" is the wrong thing to tell them — `--record`
    would decline to record it anyway. The useful sentence is that no reader can follow it.

    An unresolvable sha is then reported once and nothing further is said about it: piling "and the
    ledger's subject no longer matches" on top after a rewrite would treble the noise on the one
    morning when this output most needs to be readable.
    """
    out = []
    seen = set()
    for path, number, sha, _line in cited:
        seen.add(sha)
        where = f"{path}:{number}"
        entry = ledger.get(sha)
        # Outside `docs/` there is no ledger and no claim that there should be one, so the only
        # question `--all` can honestly ask of a citation is whether it resolves. Asking the rest
        # would print 194 demands for entries that a `--record` run would immediately prune.
        ledgered = path.startswith("docs/")
        if entry and "external" in entry:
            continue
        matches = history.resolve(sha)
        if not matches:
            out.append(
                (
                    where,
                    f"`{sha}` is not an ancestor of HEAD — no reader of this repository can"
                    " follow it",
                    "`git cat-file -e` is NOT the test and will often say this is fine: a"
                    " rewrite leaves the old commits in the local object database, unreachable,"
                    " where a clone has none of them."
                    + (
                        " run: python3 tools/citation-check.py --relocate"
                        if ledgered
                        else " nothing outside docs/ is recorded, so there is no anchor to"
                        " relocate this one by: re-derive it from the prose around it, or"
                        " replace the sha with the commit's subject."
                    ),
                )
            )
            continue
        if len(matches) > 1:
            out.append(
                (
                    where,
                    f"`{sha}` is ambiguous — {len(matches)} ancestors of HEAD start with it",
                    "the history has grown past this abbreviation. write more of the sha:"
                    f" {', '.join(m[: len(sha) + 4] for m in matches[:3])}",
                )
            )
            continue
        full = matches[0]
        subject, date = history.subject[full], history.date[full]
        if not ledgered:
            continue
        if entry is None:
            out.append(
                (
                    where,
                    f"`{sha}` is cited by no entry in {LEDGER_REL}",
                    "every cited sha is recorded with the subject and date of the commit it"
                    " names, so that a history rewrite can be undone by machine rather than by"
                    " hand. run: python3 tools/citation-check.py --record",
                )
            )
            continue
        if (subject, date) != (entry.get("subject"), entry.get("date")):
            out.append(
                (
                    where,
                    f"`{sha}` now names a different commit than {LEDGER_REL} recorded",
                    f"recorded: {entry.get('subject')!r} ({entry.get('date')}); it now"
                    f" resolves to: {subject!r} ({date}). either the citation was edited, or"
                    " the commit was amended. decide which is right, then --record.",
                )
            )
            continue
        anchors = history.find(subject, date)
        if len(anchors) != 1:
            out.append(
                (
                    where,
                    f"`{sha}` has no unique anchor — its subject and date name"
                    f" {len(anchors)} commits on HEAD",
                    "the anchor is what a rewrite is undone by, so an anchor that cannot"
                    " choose is a repair that will not work. the duplicate is usually a"
                    " rebase copy merged back in; reword one of the two subjects.",
                )
            )
    for key in sorted(set(ledger) - seen):
        if "external" in ledger[key]:
            # An `external` entry is not a relocation mapping for a docs/ citation — it is a
            # standing exemption keyed by sha, good wherever that sha is cited, docs/ or not
            # (findings() above skips resolution for it on that basis, regardless of `ledgered`).
            # So "not cited under docs/" is not a defect for one of these: SKEIN-708 declared a
            # handful for citations outside docs/ (fixture placeholders, another repository's own
            # commits), and this scan — docs/-scoped by default — would otherwise flag every one
            # of them as an orphan on every run.
            continue
        out.append(
            (
                LEDGER_REL,
                f"`{key}` is recorded but cited by nothing under docs/",
                "a ledger entry is a mapping for a citation that exists. run:"
                " python3 tools/citation-check.py --record",
            )
        )
    return out


def report(cited, found, scope_label):
    """Print the findings and return the exit code.

    Grouped by rule, and the rule printed once. The ungrouped version of this is what a rewrite
    turns into fifty-four copies of the same paragraph, and a wall of identical text is read as
    one failure however many it is — which is the mode of failure this whole file is about.
    """
    if not found:
        print(
            f"citation-check: {len(cited)} commit citations in {scope_label}, "
            f"{len({c[2] for c in cited})} distinct, all reachable from HEAD and anchored."
        )
        return 0
    grouped = {}
    for where, what, rule in found:
        grouped.setdefault(rule, []).append((where, what))
    for rule, hits in grouped.items():
        for where, what in hits:
            print(f"citation-check: {where}  {what}")
        print(f"                rule: {rule}")
        print()
    print(
        f"citation-check: {len(found)} finding(s) across {len(cited)} citations in {scope_label}."
    )
    return 1


# ---- recording -----------------------------------------------------------------------------


def record(cited, ledger, history):
    """Add, refresh and prune `docs/citations.toml` from what the history says today.

    The one rule that matters here is stated in the module docstring and enforced by the `continue`
    below: an entry whose citation NO LONGER RESOLVES is left exactly as it is. `--record` is the
    command somebody runs when a gate has just failed, and a rewrite is when the gate fails for
    every citation at once — so the version of this that "refreshes everything" would delete the
    entire mapping at the moment it becomes the only copy of it.
    """
    keys = {sha for _, _, sha, _ in cited}
    updated, added, pruned, kept = dict(ledger), [], [], []
    for key in sorted(set(ledger) - keys):
        if "external" in ledger[key]:
            # Same reasoning as the orphan check in `findings()`: an `external` entry is a
            # standing exemption for a sha, not a relocation mapping for a docs/ citation, so a
            # docs/-scoped scan finding it uncited here is not evidence it is unused (SKEIN-708).
            continue
        del updated[key]
        pruned.append(key)
    for sha in sorted(keys):
        if "external" in ledger.get(sha, {}):
            continue
        matches = history.resolve(sha)
        if len(matches) != 1:
            if sha in ledger:
                kept.append(sha)
            else:
                print(
                    f"citation-check: `{sha}` resolves to {len(matches)} commits on HEAD and was"
                    " NOT recorded — a mapping to nothing is worse than a missing one."
                )
            continue
        full = matches[0]
        fresh = {
            "commit": full,
            "subject": history.subject[full],
            "date": history.date[full],
        }
        if updated.get(sha) != fresh:
            added.append(sha)
            updated[sha] = fresh
    with open(LEDGER, "w", encoding="utf-8") as fh:
        fh.write(render_ledger(updated))
    print(f"citation-check: {LEDGER_REL} — {len(updated)} entries.")
    for label, keys_ in (("recorded", added), ("pruned", pruned), ("left alone", kept)):
        if keys_:
            print(f"                {label}: {' '.join(keys_)}")
    if kept:
        print(
            "                (left alone because they no longer resolve — their recorded anchor"
        )
        print("                is the only remaining record of what they meant. --relocate next.)")
    return 0


# ---- relocation ----------------------------------------------------------------------------


def relocate(cited, ledger, history, write):
    """Re-derive every recorded citation from its anchor, and optionally rewrite the documents.

    This is the mode the ledger exists for, and it is also the mode that is hardest to trust,
    because the day it matters is the one day nobody can check its output against the old shas. So
    it does not wait for a rewrite to be exercised: it re-derives EVERY entry, not only the broken
    ones, and prints `same` for each one that comes back as the sha already written. Run today,
    against a history nothing has rewritten, it must print `same` 38 times — and does. That is a
    dry run of the repair, available at any moment, on the real data.

    The fallback to `--all` when `HEAD` has no candidate is for the amend/rebase case, where the
    commit is still somewhere in the repository. It is reported as `elsewhere` rather than applied:
    a sha that is on a branch and not on `HEAD` is not something a reader can follow either, so
    substituting it would turn a loud failure into a quiet one.
    """
    by_sha = {}
    for path, number, sha, line in cited:
        by_sha.setdefault(sha, []).append((path, number, line))
    plan, unresolved = {}, 0
    for key in sorted(ledger):
        entry = ledger[key]
        if "external" in entry:
            print(f"citation-check: `{key}`  external — {entry['external']}")
            continue
        subject, date = entry.get("subject"), entry.get("date")
        matches = history.find(subject, date)
        if len(matches) == 1:
            new = matches[0]
            if new.startswith(key.lower()):
                print(f"citation-check: `{key}`  same     {subject}")
                continue
            # Keep the width the document already uses, and widen only if the new history has
            # made it ambiguous — a repair that silently lengthened every citation would show up
            # as a diff nobody could read past.
            width = len(key)
            while width < 40 and len(history.resolve(new[:width])) != 1:
                width += 1
            plan[key] = new[:width]
            print(f"citation-check: `{key}`  -> `{new[:width]}`  {subject}")
            continue
        unresolved += 1
        if len(matches) > 1:
            print(
                f"citation-check: `{key}`  AMBIGUOUS — the anchor names {len(matches)} commits"
                f" on HEAD: {subject}"
            )
            continue
        loose = _git("log", "--all", "--format=%H%x1f%s")
        elsewhere = [
            ln.split("\x1f")[0]
            for ln in (loose or "").split("\n")
            if ln.partition("\x1f")[2] == subject
        ]
        if elsewhere:
            print(
                f"citation-check: `{key}`  elsewhere — on a ref that is not HEAD"
                f" ({elsewhere[0][:12]}), so a reader still cannot follow it: {subject}"
            )
        else:
            print(f"citation-check: `{key}`  NOT FOUND — no commit in this repository is: {subject}")
    if not write:
        if plan:
            print(
                f"citation-check: {len(plan)} citation(s) would move. Re-run with --write to apply."
            )
        return 1 if unresolved else 0
    edits = 0
    for path in sorted({p for sha in plan for p, _, _ in by_sha.get(sha, [])}):
        full = os.path.join(ROOT, path)
        with open(full, encoding="utf-8") as fh:
            text = fh.read()
        for old, new in plan.items():
            text = text.replace(f"`{old}`", f"`{new}`")
        with open(full, "w", encoding="utf-8") as fh:
            fh.write(text)
        edits += 1
    moved = {}
    for key, entry in ledger.items():
        moved[plan.get(key, key)] = entry
    for key, new in plan.items():
        matches = history.resolve(new)
        if len(matches) == 1:
            moved[new] = {
                "commit": matches[0],
                "subject": history.subject[matches[0]],
                "date": history.date[matches[0]],
            }
    with open(LEDGER, "w", encoding="utf-8") as fh:
        fh.write(render_ledger(moved))
    print(f"citation-check: rewrote {len(plan)} citation(s) across {edits} file(s), and the ledger.")
    return 1 if unresolved else 0


# ---- the self-check ------------------------------------------------------------------------


def self_check():
    """Prove, on every run, that this tool is asking the question it claims to ask.

    The whole value of this gate is one distinction — reachable from `HEAD` versus present in the
    object database — and the wrong side of it LOOKS CORRECT. Thirty of forty-four dangling shas
    answered `git cat-file -t` with `commit` after the last rewrite, which is how the breakage
    stayed invisible. A future edit that "simplifies" the predicate back to `cat-file` would pass
    every existing citation and report a clean tree for ever.

    So the probe is a real commit object, written here, reachable from nothing: `cat-file` accepts
    it, this tool must reject it. Both directions are asserted, because a predicate that rejects
    everything would pass the negative half on its own.
    """
    tree = _git("hash-object", "-t", "tree", "-w", "--stdin", stdin="")
    probe = None
    if tree is not None:
        probe = _git("commit-tree", tree, "-m", PROBE_MESSAGE, env=PROBE_IDENT)
    if probe is None:
        fail(
            "citation-check: SELF-CHECK COULD NOT RUN — this checkout would not let a probe",
            "                object be written, so the one distinction this gate is built on",
            "                (reachable, versus merely present) is unproven. Refusing to report",
            "                a clean tree on an unproven predicate.",
        )
    if _git("cat-file", "-t", probe) != "commit":
        fail(
            "citation-check: SELF-CHECK FAILED — the probe is not a commit object, so the",
            "                comparison below proves nothing about the two predicates.",
        )
    history = load_history()
    if history.resolve(probe):
        fail(
            "citation-check: SELF-CHECK FAILED — a commit that is reachable from NO ref was",
            "                accepted. That is the exact state a history rewrite leaves its old",
            "                commits in, and `git cat-file` accepts it too — which is why this",
            "                check must be ancestry and not existence.",
        )
    head = _git("rev-parse", "HEAD")
    if head is None or history.resolve(head[:8]) != [head]:
        fail(
            "citation-check: SELF-CHECK FAILED — HEAD itself was not recognised as reachable",
            "                from HEAD, so this predicate rejects everything and every clean",
            "                report it has ever produced was luck.",
        )
    # This file may not spell a sha, and that is not fastidiousness — it is the tool declining to
    # write itself a way to brick. `--all` scans every tracked file, this one included, so any sha
    # in the prose above becomes a citation of its own; the empty tree and the probe are not
    # commits at all and would be permanently unresolvable ones; and a sha that IS a good commit
    # today stops being one the morning of the rewrite, which is the morning this must still run.
    # So the docstring names the repair commit by its subject, which is the anchor this whole file
    # argues for, and every hash it needs is computed at run time. `tools/prose-check.py` learned
    # the same lesson from the other end: a checker whose own source contains what it looks for
    # goes quiet about it.
    mine = os.path.relpath(os.path.abspath(__file__), ROOT)
    if os.path.exists(os.path.join(ROOT, mine)) and citations([mine]):
        fail(
            "citation-check: SELF-CHECK FAILED — this file now contains a backticked sha of its",
            "                own, which --all would report as a citation for ever. Describe the",
            "                commit instead of spelling it.",
        )
    return history


# ---- main ----------------------------------------------------------------------------------


def main():
    argv = sys.argv[1:]
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    unknown = [a for a in argv if a not in ("--all", "--record", "--relocate", "--write")]
    if unknown:
        fail(f"citation-check: unknown argument(s): {' '.join(unknown)}. Try --help.")
    # The ledger is `docs/`-scoped by construction — `--record` prunes any entry no document
    # cites — so recording or relocating under `--all` would write the rest of the tree in and
    # then, on the next plain run, prune it all back out. Refuse rather than oscillate.
    if "--all" in argv and ("--record" in argv or "--relocate" in argv):
        fail(
            "citation-check: --all is a report over the whole tree; --record and --relocate",
            "                write the docs/-scoped ledger. Run them separately.",
        )
    if "--write" in argv and "--relocate" not in argv:
        fail("citation-check: --write only means anything with --relocate.")
    history = self_check()
    scope = None if "--all" in argv else "docs"
    label = "the tracked tree" if scope is None else "docs/"
    cited = citations(tracked(scope))
    ledger = load_ledger()
    if "--record" in argv:
        return record(cited, ledger, history)
    if "--relocate" in argv:
        return relocate(cited, ledger, history, "--write" in argv)
    if scope is None:
        print(SCOPE_NOTE)
        print()
    return report(cited, findings(cited, ledger, history), label)


if __name__ == "__main__":
    sys.exit(main())
