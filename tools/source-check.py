#!/usr/bin/env python3
"""The Source law, made checkable: nothing reaches anything except through a Source.

`docs/architecture.md` §2.3 names four Sources — `enter`, `socket`, `file`, `http` — and says the
law becomes enforceable once they exist. A law nothing checks is a paragraph. This is the check,
and it is the same shape as `tools/module-check.py`: an allow-list of where each Source is spelled
today, generated from the code and then reviewed, so that a NEW way of reaching something is a line
in a diff rather than a call nobody looked at.

What it does not claim: that today's spread is right. `enter` is spelled in six files and belongs
in one. The point is that the spread cannot quietly get wider while the rewrite is under way.

**The allow-list records a COUNT, not a membership** (SKEIN-478). A list of unit names only ever
answers "may this unit reach at all", and every unit that reaches is already on it — so the reach
somebody adds tomorrow is added to a unit that is already allowed, and nothing anywhere changes.
That is the condition this gate exists to end, and for as long as the file held bare names the gate
could not see it: `fleet` was allowed to spell `sbx`, so a second, third or tenth `sbx` spawn in
`fleet` was a clean run. The count makes each one a line in a diff.

Test code is cut before matching, for the same reason module-check cuts it: a fixture that spells
`nsenter` in an assertion is describing the code, not reaching anything. The cut comes from
`tools/rustcut.py`, the one cutter all three text gates share — it is brace-matched rather than
"everything after the marker" because the cheap version stops reading at the test module and every
item below it becomes invisible, and it ends a brace-less `#[cfg(test)] const` at its `;` rather
than at the next `{`, which used to take the production function after it (WTS-9). It steps over
strings, chars and comments: `src/fleet.rs`'s test module opens with a shell fixture full of
braces, and a cutter that counted those braces closed the module 254 lines in instead of 11,822,
which is how `--show` once reported `sbx fleet(18)` where the true figure was 3.

Both crates are read: `src/` and `warden/src/`. The warden runs the privileged commands, so a
checker that stopped at skein would be silent about the reaches that matter most.

  python3 tools/source-check.py           check
  python3 tools/source-check.py --update  merge the code's counts into the allow-list
  python3 tools/source-check.py --update --prune   …and apply the removals it otherwise refuses
  python3 tools/source-check.py --show    print where each Source is spelled, and what it records
"""

import os, re, sys, collections, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rustcut  # noqa: E402 — the one cutter every gate shares, self-checked at import

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
# The warden is a separate crate (architecture §14, "separate binary"), and it is the ONE component
# whose reaches matter most: it is the process on the host that runs `sbx create` and `sbx rm`. A
# checker that stopped at `src/` would go quiet exactly there, which is the condition it exists to
# end. Its units are prefixed so `warden/doer` and `doer` can never be confused for each other.
WARDEN = os.path.join(ROOT, "warden", "src")
SPEC = os.path.join(ROOT, "docs", "sources.toml")

# Where `rustcut.test_only_files` derives the whole-file test modules from: BOTH crates, because
# both are what this gate reads. `module-check` names `src/` alone for the same reason — the
# refusal these dirs buy ("no `#[cfg(test)] mod X;` anywhere, so the reader has broken") should be
# about the tree the gate makes claims over.
CRATE_DIRS = [SRC, WARDEN]

# How each Source is spelled in Rust. Deliberately the PRIMITIVE, not the wrapper: `place.exec()` is
# skein's own front door and finding it proves nothing, while `nsenter` is the syscall dressed as a
# command and cannot be spelled by accident.
SPELLINGS = {
    "enter": [r"\bnsenter\b"],
    "socket": [r"tmux -S", r'Command::new\("tmux"\)'],
    "file": [],  # every module reads files; the law here is about what a file read may REACH, not
                 # about `fs::read`. Left empty on purpose rather than made up.
    "http": [
        r'Command::new\("curl"\)',
        r'Command::new\("gh"\)',
        r"\bTcpStream\b",
        r"\breqwest\b",
        r"\bureq\b",
    ],
    # Not a Source of its own: `sbx` is how the host reaches the SANDBOX, which is the outer shell
    # of every `enter`. Tracked separately so the two do not get confused when the transport moves
    # in-fleet and `sbx` stops being on the path at all.
    #
    # `\("sbx"` rather than `Command::new("sbx")`, because skein does not spawn it that way. It goes
    # through `run_capture_for`, `run_attached_env` and friends, and the narrower pattern saw NONE of
    # them: `fleet` spawned `sbx` six times — including the fleet create and the resize's destroy,
    # the two most privileged calls in the system — and the checker reported it as reaching nothing.
    # The program name as the first argument of a call is what "spawning it" looks like here.
    #
    # `\(\s*` and not `\(`, because the same reach goes invisible when rustfmt puts the program name
    # on its own line. Seen: SKEIN-104 split the login into a `(program, argv)` tuple, `fleet`'s
    # count silently fell from two to one, and the call it stopped counting was the one that still
    # runs `sbx`. A pattern that depends on formatting is a law that a reformat can repeal.
    "sbx": [r'\(\s*"sbx"'],
}


def units():
    """Every unit as (name, [paths]), from the one cutter — `src/<name>.rs` AND `src/<name>/**`.

    The local version enumerated with `os.listdir(SRC)` and yielded one path each, so the day
    `src/fleet.rs` becomes `src/fleet/` the unit would have vanished from this gate and every
    reach inside it with it — the allow-list would then have named a unit that no longer reaches
    anything, which reads as an improvement.
    """
    return rustcut.units(SRC, WARDEN)


def shipped_code(text):
    """The part of a unit's text that runs in production: no `#[cfg(test)]`, no comments."""
    return rustcut.uncommented(rustcut.split_tests(text)[0])


def shipped_in(paths):
    """The production half of a whole unit — and a WHOLE-FILE test module is not in it.

    `shipped_code` above reads one string and cuts the `#[cfg(test)]` written IN it. A file that is
    test code because its PARENT declares it `#[cfg(test)] mod X;` carries no attribute anywhere in
    itself, so until SKEIN-905 this gate judged all 70,000 characters of the four such files
    against the Source law as if they shipped — `src/review/testkit.rs`'s fixture HTTP server
    included.

    The law is about what production reaches, and this gate's own doc has always said so for the
    other shape: "a fixture that spells `nsenter` in an assertion is describing the code, not
    reaching anything". Which spelling of test code it is — an attribute in the file or an
    attribute in the parent — is a fact about where somebody typed it, not about what ships.

    The counts do not move today, and that is a measurement rather than a hope: of the four files,
    only `src/review/testkit.rs` spells a Source at all (`TcpStream`, at :17), and it happens to
    carry a `#[cfg(test)]` of its own so the old cutter reached it anyway. Nothing about that was
    structural — `src/prwork/testkit.rs` has no `#[cfg(test)]` item in it at all, so a fixture
    there that spawned `sbx` used to be a production reach and now is not.
    """
    return rustcut.uncommented(rustcut.split_unit(paths, CRATE_DIRS)[0])


def reaches_in(text):
    """{source: hits} for one unit's already-cut text. Split out so the self-check can count."""
    found = {}
    for source, patterns in SPELLINGS.items():
        hits = sum(len(re.findall(pattern, text)) for pattern in patterns)
        if hits:
            found[source] = hits
    return found


def read_reaches():
    """{source: Counter(unit -> hits)} over non-test, non-comment code."""
    found = {name: collections.Counter() for name in SPELLINGS}
    for unit, paths in units():
        # `source.rs` is where the Sources are DESCRIBED, and it reaches nothing. It names `nsenter`
        # in a string — the "reaches" column of §2.3's table — and counting that would put the
        # taxonomy on the list of things that cross into boxes.
        if unit == "source":
            continue
        body = shipped_in(paths)
        for source, hits in reaches_in(body).items():
            found[source][unit] += hits
    return found


def load_spec():
    if not os.path.exists(SPEC):
        return {}
    with open(SPEC, "rb") as f:
        return tomllib.load(f)


def recorded_in(spec, source):
    """{unit: count} the allow-list records for one Source, or None if it has no row at all."""
    section = spec.get(source)
    if section is None:
        return None
    return section.get("reaches")


# ---------------------------------------------------------------------------------------------
# Rendering and merging the allow-list.
#
# `--update` MERGES; it does not regenerate. It used to write the whole file out of the counts,
# which deleted every per-unit comment — the review that says why a unit is allowed to reach — on
# the command whose name says it is how you keep the file current. `tools/module-check.py` learned
# this first and the note it carries is the argument (SKEIN-614): the file is an argument, not an
# inventory, and a regenerator that keeps the numbers and drops the arguments leaves a file that
# passes its own gate having lost the point of it. Counts move far more often than names do, so
# `--update` is now a command somebody runs routinely — which is exactly when a destructive one
# does its damage.
# ---------------------------------------------------------------------------------------------

BARE_KEY = re.compile(r"[A-Za-z0-9_-]+")
HEADER = re.compile(r"^\[([^\[\]\s]+)\][ \t]*$")
# One line by construction: TOML gives an inline table no second line, so the statement can be
# found and rewritten without parsing the file around it.
REACHES = re.compile(r"^reaches[ \t]*=[ \t]*\{[^}\n]*\}[ \t]*\n?", re.M)

NEW_ROW_NOTE = (
    "# TODO: say why this Source may be spelled in each unit below. A row with no argument above\n"
    "# it is a permission nobody granted.\n"
)


def toml_key(unit):
    """A unit name as a TOML key: bare where it can be, quoted where it cannot (`warden/doer`)."""
    return unit if BARE_KEY.fullmatch(unit) else f'"{unit}"'


def render_reaches(counts):
    """The one generated statement in the file: `reaches = { unit = n, … }`, sorted by unit."""
    if not counts:
        return "reaches = {}\n"
    body = ", ".join(f"{toml_key(u)} = {n}" for u, n in sorted(counts.items()))
    return f"reaches = {{ {body} }}\n"


def split_blocks(text):
    """(preamble, [(source, text)]) — every block keeping its own bytes, in the file's own order.

    A run of comments and blank lines immediately above a header belongs to the block BELOW it:
    that is where this file's arguments are written, and where a reader expects to find them.
    """
    lines = text.splitlines(keepends=True)
    heads = [i for i, line in enumerate(lines) if HEADER.match(line)]
    if not heads:
        return text, []
    starts = []
    for n, head in enumerate(heads):
        start, floor = head, heads[n - 1] + 1 if n else 0
        while start > floor and (
            not lines[start - 1].strip() or lines[start - 1].lstrip().startswith("#")
        ):
            start -= 1
        starts.append(start)
    blocks = []
    for n, start in enumerate(starts):
        end = starts[n + 1] if n + 1 < len(starts) else len(lines)
        blocks.append((HEADER.match(lines[heads[n]]).group(1), "".join(lines[start:end])))
    return "".join(lines[: starts[0]]), blocks


def reaches_span(block):
    """({unit: count}, (start, end)) for a block's `reaches` statement; (None, None) if it has none."""
    match = REACHES.search(block)
    if not match:
        return None, None
    try:
        return tomllib.loads(match.group(0)).get("reaches", {}), match.span()
    except tomllib.TOMLDecodeError:
        return None, match.span()


def merge(text, found, prune=False):
    """The allow-list with the code's counts merged in, and every hand-written byte still in it.

    Returns `(text, applied, refused)`. Only the `reaches` statements are rewritten, in place.

    A count that MOVED is applied and printed, because the row and the sentence above it survive
    either way — but printed loudly, since a count falling from 3 to 1 usually means the prose
    above it now describes a tree that no longer exists. A unit that has LEFT is a decision about
    somebody's writing: `--update` reports it and leaves it, and only `--prune` applies it, because
    no tool can tell which sentence argued for the name it is about to delete.
    """
    preamble, blocks = split_blocks(text)
    out, applied, refused, seen = [preamble], [], [], set()
    for source, block in blocks:
        seen.add(source)
        have, span = reaches_span(block)
        want = dict(found.get(source, {}))
        if have is None:
            refused.append(
                f"[{source}] has no `reaches = {{ … }}` statement to write into"
                + (" — the one it has does not parse" if span else "")
            )
            out.append(block)
            continue
        keep = dict(want) if prune else {**have, **want}
        for unit in sorted(set(want) - set(have)):
            applied.append(
                f"wrote `{unit}` = {want[unit]} into [{source}] — a unit that reaches this way and "
                f"was not on the list. The note above it does not argue for `{unit}`"
            )
        for unit in sorted(set(want) & set(have)):
            if want[unit] != have[unit]:
                applied.append(
                    f"[{source}] `{unit}`: {have[unit]} -> {want[unit]}. Read the call sites — the "
                    f"note above this row may still be describing the old number"
                )
        for unit in sorted(set(have) - set(want)):
            if prune:
                applied.append(
                    f"removed `{unit}` from [{source}], which no code reaches this way any more — "
                    f"the note above the row may still argue for it"
                )
            else:
                refused.append(
                    f"docs/sources.toml records `{unit}` reaching by `{source}` and no code does. "
                    f"Removing the entry means the sentence that argued for it is now describing "
                    f"nothing, and no tool can tell which sentence that is — edit by hand, or "
                    f"--prune."
                )
        if keep != have:
            block = block[: span[0]] + render_reaches(keep) + block[span[1] :]
        out.append(block)
    for source in SPELLINGS:
        if source in seen:
            continue
        if not "".join(out).endswith("\n\n"):
            out.append("\n")
        out.append(f"[{source}]\n{NEW_ROW_NOTE}{render_reaches(found.get(source, {}))}")
        applied.append(f"added a row for [{source}], with a TODO where its reason goes")
    return "".join(out), applied, refused


PREAMBLE = """# Where each Source is spelled today, and HOW MANY TIMES. Read by `tools/source-check.py`,
# which fails the build on a reach from a unit that is not listed — and on a count that has moved,
# because a unit already on the list is where the next new reach hides (SKEIN-478).
#
# Merged from the code (`--update`) and then reviewed. The list is not an argument that today's
# spread is right — `enter` is spelled in several files and belongs in one. It is there so the
# spread cannot quietly get wider while the rewrite is under way.
#
# architecture.md §2.3 is the design; `src/source.rs` is the taxonomy, and its own test
# checks itself against §2.3.
"""


def render(found):
    """The whole file, for the case where there is none. `--update` MERGES into an existing one."""
    out = [PREAMBLE]
    for source in SPELLINGS:
        out.append(f"\n[{source}]\n{NEW_ROW_NOTE}{render_reaches(found.get(source, {}))}")
    return "".join(out)


# The cut this gate depends on, held as a fixture and run on every invocation. `rustcut`'s own
# self-check pins the cutter; this one pins the way THIS gate uses it, which is the pair of
# opposite errors that both end in a wrong count:
#
#   * a reach spelled in a fixture counted as production — the allow-list gets wider than the code;
#   * a reach in production code that the cut swallowed — the allow-list looks clean because part
#     of the crate is invisible. That is WTS-9: `#[cfg(test)] const TMUX_COMMAND_CEILING` at
#     `src/fleet.rs:992` is brace-less, and cutting to "the next `{`" took `fn detached_script_path`
#     (`:1012`) with it. That function spells no Source today, so the count was right by luck.
#
# The fixture's braces are UNBALANCED on purpose. They used to be `{{}}` — balanced — so the two
# assertions about them held whether or not the cutter skipped strings at all, which is this
# repository's rule 3 in the gate whose own defect was that its numbers counted fixtures
# (SKEIN-478). One stray `}` closes the module early, and everything below it — `nsenter`, a
# second `curl` — is then read as shipped code.
SELF_CHECK = r'''#[cfg(test)]
const TEST_CEILING: usize = 4;

fn detached_script_path() -> String {
    let _ = std::process::Command::new("curl");
    String::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_fixture_that_merely_NAMES_a_source_reaches_nothing() {
        let shell = format!("#!/bin/sh\nif [ -x . ]; then }} fi");
        let fixture = format!("nsenter --target {} --mount", 1);
        let _ = std::process::Command::new("curl");
    }
}

fn after_the_tests() {
    let _ = std::process::Command::new("tmux");
}
'''

# An allow-list in miniature: prose, a row whose count has moved, a row whose unit has left, and a
# row whose emptiness is the claim. Held as a fixture rather than as a comment because the failure
# it guards against is silent by construction — a file with the counts kept and the arguments gone
# still passes every check in this tool.
SELF_CHECK_ALLOW_LIST = """
[enter]
# `place` because a reader has to be able to find out WHY. This sentence is the deliverable: the
# file exists so that a reach somebody adds later is argued against a written reason.
reaches = { place = 3, gone = 1 }

[file]
# Nothing, and that is the claim rather than an accident.
reaches = {}
"""
SELF_CHECK_FOUND = {"enter": {"place": 4}, "socket": {"fleet": 1}}


def self_check():
    """This gate's own eyes, proved before it says anything about the crate.

    Each assertion names the sabotage that makes it fail: drop the `}` from the fixture's shell
    string and the first two go green whatever the cutter does, which is how the balanced version
    of this fixture came to prove nothing.
    """
    body = shipped_code(SELF_CHECK)
    if 'Command::new("curl")' not in body:
        raise SystemExit(
            "source-check: its own cut is broken — the production item after a brace-less "
            "`#[cfg(test)] const` was deleted, so a reach inside it is invisible and this gate "
            "reports a clean tree for the wrong reason (WTS-9)."
        )
    if "nsenter" in body:
        raise SystemExit(
            "source-check: its own cut is broken — a `#[cfg(test)]` fixture that merely NAMES a "
            "Source was counted as a production reach, which is how the allow-list gets wider "
            "than the code (SKEIN-412)."
        )
    if 'Command::new("tmux")' not in body:
        raise SystemExit(
            "source-check: its own cut is broken — the test module swallowed the code BELOW it, "
            "so part of the crate is invisible."
        )
    # Presence is not the measurement this gate makes. `http` is spelled twice in the fixture and
    # once in its shipped half, and only a COUNT can tell those apart — which is the whole of
    # SKEIN-478: the reach that hides is the second one in a unit that already has a first.
    if reaches_in(body) != {"http": 1, "socket": 1}:
        raise SystemExit(
            "source-check: its own counting is broken — the fixture's shipped half spells `http` "
            "once and `socket` once, and this gate counted %r. A count that includes test "
            "fixtures is how `sbx fleet(18)` was reported for a file with three reaches "
            "(SKEIN-478)." % (reaches_in(body),)
        )
    if len(body.split("\n")) != len(SELF_CHECK.split("\n")):
        raise SystemExit(
            "source-check: its own cut is broken — the cut moved line numbers, so nothing this "
            "gate reports can be located."
        )
    self_check_whole_file_tests()
    self_check_counts()
    self_check_merge()


def self_check_whole_file_tests():
    """A file that is test code because its PARENT says so is not judged against the Source law.

    Read through `shipped_in`, which is what `read_reaches` calls, and over the REAL tree rather
    than a fixture: `rustcut`'s own self-check proves the cut, and what this adds is that this gate
    is wired to it. Falsify by putting `shipped_code(rustcut.read_unit(paths))` back into
    `read_reaches` — the shape this gate had before SKEIN-905 — and the first of the files below
    comes back with its whole body in the production half.

    Derived, never listed: `rustcut.test_only_files` reads the declarations out of the tree and
    refuses to run rather than answer none, so a rename it stops recognising fails loudly here
    instead of quietly checking nothing.
    """
    for path in sorted(rustcut.test_only_files(CRATE_DIRS)):
        body = shipped_in([path])
        if body.strip():
            raise SystemExit(
                "source-check: its own cut is broken — %s is test code in its entirety (its "
                "parent declares it `#[cfg(test)] mod ...;`) and %d character(s) of it reached "
                "the production half, where a fixture that names a Source is counted as a reach "
                "(SKEIN-905)." % (os.path.relpath(path, ROOT), len(body.strip()))
            )


def self_check_counts():
    """A SECOND reach in a unit the list already allows is a finding — the whole of SKEIN-478.

    This is the case the gate could not see for as long as `docs/sources.toml` held bare names:
    `fleet` was permitted to spell `sbx`, so the reach somebody adds tomorrow is compared against
    nothing. Named so it can be watched failing: delete the `elif allowed[unit] != hits[unit]`
    branch in `check` and the second assertion goes red, which is the tool as it shipped before.
    """
    allowed = {"sbx": {"reaches": {"fleet": 1}}}
    agrees = check({"sbx": {"fleet": 1}}, allowed)
    if agrees:
        raise SystemExit(
            "source-check: its own counting is broken — a unit whose count is exactly what the "
            "allow-list records was reported as a problem: %s" % agrees[0].splitlines()[0]
        )
    moved = check({"sbx": {"fleet": 2}}, allowed)
    if len(moved) != 1 or "records 1" not in moved[0]:
        raise SystemExit(
            "source-check: its own counting is broken — a unit that reaches TWICE where the "
            "allow-list records one was not reported (%d finding(s)). A new reach added to a unit "
            "that is already on the list is exactly what this gate exists to catch (SKEIN-478)."
            % len(moved)
        )
    gone = check({"sbx": {}}, allowed)
    if len(gone) != 1 or "no longer does" not in gone[0]:
        raise SystemExit(
            "source-check: its own counting is broken — an allow-list entry that no code reaches "
            "any more was not reported, so a permission outlives the call it was granted for."
        )


def self_check_merge():
    """`--update` keeps every line a person wrote, and moves the numbers (SKEIN-614, SKEIN-478).

    Named concretely so it can be watched failing: go back to rendering the file out of the counts
    and every one of these goes red at once.
    """
    def wrong(what):
        raise SystemExit(f"source-check: its own `--update` is broken — {what}.")

    prose = [ln for ln in SELF_CHECK_ALLOW_LIST.splitlines() if ln.startswith("#")]
    merged, applied, refused = merge(SELF_CHECK_ALLOW_LIST, SELF_CHECK_FOUND)
    lost = [ln for ln in prose if ln not in merged.splitlines()]
    if lost:
        wrong(
            f"{len(lost)} of {len(prose)} hand-written line(s) did not survive the merge, starting "
            f"with {lost[0]!r}. docs/sources.toml is an argument, not an inventory (SKEIN-614)"
        )
    if "place = 4" not in merged:
        wrong("a count that moved from 3 to 4 was not written — the count IS the check (SKEIN-478)")
    if "gone = 1" not in merged:
        wrong("it deleted somebody's row without --prune, which is the whole of SKEIN-614")
    if "[socket]" not in merged or "fleet = 1" not in merged:
        wrong("a Source with no row got none, so the gate would stay red after --update")
    if "reaches = {}" not in merged:
        wrong("a row whose `reaches` is empty was dropped — that row is a claim, not an absence")
    if len(refused) != 1 or not applied:
        wrong(f"it did not report the one removal it declined to make ({len(refused)} reported)")

    pruned, applied, refused = merge(SELF_CHECK_ALLOW_LIST, SELF_CHECK_FOUND, prune=True)
    if "gone = 1" in pruned:
        wrong("--prune left a row no code reaches any more")
    if "the deliverable" not in pruned:
        wrong("--prune took the prose out with the row it removed")
    if refused or not any("gone" in line for line in applied):
        wrong("--prune removed a row without printing it")


def check(found, spec):
    """Every problem the allow-list has with the code, as sentences."""
    problems = []
    for source, hits in found.items():
        allowed = recorded_in(spec, source)
        if allowed is None:
            problems.append(
                f"source-check: docs/sources.toml has no `reaches` for `{source}`\n"
                f"              rule: a Source with no row is a Source nothing checks — run "
                f"`python3 tools/source-check.py --update` and review what it writes"
            )
            allowed = {}
        for unit in sorted(hits):
            if unit not in allowed:
                problems.append(
                    f"source-check: `{unit}` reaches by `{source}` ({hits[unit]} references), and "
                    f"docs/sources.toml does not allow it\n"
                    f"              rule: a new way of reaching something is a decision — add "
                    f"`{unit}` to [{source}] and say why, or route it through an existing Source"
                )
            elif allowed[unit] != hits[unit]:
                problems.append(
                    f"source-check: `{unit}` reaches by `{source}` {hits[unit]} time(s), and "
                    f"docs/sources.toml records {allowed[unit]}\n"
                    f"              rule: the COUNT is the check. A unit already on this list is "
                    f"exactly where the next reach hides — read the {hits[unit]} call site(s), "
                    f"then `--update` and correct the note above the row"
                )
        for unit in sorted(set(allowed) - set(hits)):
            problems.append(
                f"source-check: docs/sources.toml says `{unit}` reaches by `{source}`, and it no "
                f"longer does\n"
                f"              rule: a stale allow-list is a permission nobody granted — drop the "
                f"entry"
            )
    return problems


def show(found, spec):
    """Where each Source is spelled, with the allow-list's number beside any that disagrees.

    The disagreement is printed rather than left for a reader to notice, because `--show` and the
    document drifting apart unnoticed is the defect this gate had: the file said `fleet` spelled
    `sbx` three times for as long as it took two of the three to be deleted from the code.
    """
    for source, hits in found.items():
        allowed = recorded_in(spec, source) or {}
        cells = []
        for unit, n in sorted(hits.items()):
            says = allowed.get(unit)
            if says == n:
                cells.append(f"{unit}({n})")
            else:
                cells.append(f"{unit}({n}, " + ("not on the list)" if says is None
                                                else f"doc says {says})"))
        for unit in sorted(set(allowed) - set(hits)):
            cells.append(f"{unit}(0, doc says {allowed[unit]})")
        print(f"{source:8} " + (", ".join(cells) or "nowhere"))


def update(found, prune=False):
    where = os.path.relpath(SPEC, ROOT)
    if not os.path.exists(SPEC):
        open(SPEC, "w", encoding="utf-8").write(render(found))
        print(f"{where}: written from the code. Every TODO in it is a reason to write.")
        return 0
    text = open(SPEC, encoding="utf-8").read()
    merged, applied, refused = merge(text, found, prune)
    if merged != text:
        open(SPEC, "w", encoding="utf-8").write(merged)
    for line in applied:
        print(f"{where}: {line}")
    if not applied and not refused:
        print(f"{where}: already says what the code does; nothing written")
    for line in refused:
        print(f"source-check: {line}", file=sys.stderr)
    if refused:
        print(
            f"\n{len(refused)} thing(s) NOT done. `--update` adds and corrects; it does not delete "
            f"what somebody wrote — this file is an argument, not an inventory (SKEIN-614). "
            f"`--update --prune` applies the removals above and prints each one.",
            file=sys.stderr,
        )
    return 1 if refused else 0


def main():
    self_check()
    found = read_reaches()
    spec = load_spec()
    if "--show" in sys.argv:
        show(found, spec)
        return 0
    if "--update" in sys.argv:
        return update(found, prune="--prune" in sys.argv)

    problems = check(found, spec) if spec else [
        "source-check: docs/sources.toml is missing, so the law is unenforced\n"
        "              rule: run `python3 tools/source-check.py --update` and review it"
    ]
    for p in problems:
        print(p + "\n")
    if problems:
        print(
            f"{len(problems)} problem(s). docs/sources.toml is the allow-list; "
            f"`python3 tools/source-check.py --update` merges the code's counts into it."
        )
        return 1
    total = sum(sum(h.values()) for h in found.values())
    files = len({u for h in found.values() for u in h})
    print(f"every reach is declared in docs/sources.toml ({total} across {files} units)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
